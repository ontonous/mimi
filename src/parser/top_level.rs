use super::*;

impl Parser {
    pub(crate) fn parse_import(&mut self) -> Result<Import, ParseError> {
        self.expect(TokenKind::Use, "`use`")?;
        let mut path = vec![self.expect_ident()?];
        while self.at(&TokenKind::ColonColon) {
            self.advance();
            path.push(self.expect_ident()?);
        }
        self.match_semi();
        Ok(Import { path })
    }

    pub(crate) fn parse_item(&mut self) -> Result<Item, ParseError> {
        let pub_ = if self.at(&TokenKind::Pub) {
            self.advance();
            true
        } else {
            false
        };
        // Parse optional #[derive(...)] and #[repr(...)] attributes
        let mut derives = Vec::new();
        let mut attributes = Vec::new();
        while self.at(&TokenKind::Hash) && self.pos + 1 < self.tokens.len() && self.tokens[self.pos + 1].kind == TokenKind::LBracket {
            self.advance(); // skip #
            self.advance(); // skip [
            if self.at(&TokenKind::Ident("derive".to_string())) {
                self.advance(); // skip "derive"
                self.expect(TokenKind::LParen, "`(`")?;
                while !self.at(&TokenKind::RParen) && !self.at(&TokenKind::Eof) {
                    let name = self.expect_ident()?;
                    derives.push(name);
                    if self.at(&TokenKind::Comma) {
                        self.advance();
                    }
                }
                self.expect(TokenKind::RParen, "`)`")?;
            } else if self.at(&TokenKind::Ident("repr".to_string())) {
                self.advance(); // skip "repr"
                self.expect(TokenKind::LParen, "`(`")?;
                let repr_name = self.expect_ident()?;
                match repr_name.as_str() {
                    "C" => attributes.push(crate::ast::TypeAttribute::ReprC),
                    "transparent" => attributes.push(crate::ast::TypeAttribute::ReprTransparent),
                    _ => { /* unknown repr, ignore */ }
                }
                self.expect(TokenKind::RParen, "`)`")?;
            }
            self.expect(TokenKind::RBracket, "`]`")?;
            self.skip_newlines();
        }
        match self.peek_kind() {
            TokenKind::Comptime => {
                // comptime func ... — comptime function modifier
                self.advance();
                let mut f = self.parse_func()?;
                f.pub_ = pub_;
                f.is_comptime = true;
                Ok(Item::Func(f))
            }
            TokenKind::Async => {
                // async func ... — async function modifier
                self.advance();
                let mut f = self.parse_func()?;
                f.pub_ = pub_;
                f.is_async = true;
                Ok(Item::Func(f))
            }
            TokenKind::Func => {
                let mut f = self.parse_func()?;
                f.pub_ = pub_;
                Ok(Item::Func(f))
            }
            TokenKind::Module => Ok(Item::Module(self.parse_module()?)),
            TokenKind::Type => {
                let mut t = self.parse_type_def(derives, attributes)?;
                t.pub_ = pub_;
                Ok(Item::Type(t))
            }
            TokenKind::Newtype => {
                let mut t = self.parse_newtype()?;
                t.pub_ = pub_;
                Ok(Item::Type(t))
            }
            TokenKind::Actor => {
                let mut a = self.parse_actor_def()?;
                a.pub_ = pub_;
                Ok(Item::Actor(a))
            }
            TokenKind::Cap => Ok(Item::Cap(self.parse_cap_def()?)),
            TokenKind::Trait => Ok(Item::Trait(self.parse_trait_def()?)),
            TokenKind::Impl => Ok(Item::Impl(self.parse_impl_def()?)),
            TokenKind::Extern => {
                // Check if this is `extern "C" func` (export) or `extern "C" { ... }` (import)
                // Peek at the token AFTER `extern` to see if it's a string literal
                let has_abi_string = self.tokens.get(self.pos + 1)
                    .map(|t| matches!(t.kind, TokenKind::String(_)))
                    .unwrap_or(false);
                if has_abi_string {
                    // Peek past the string to see if next is `func`
                    let after_abi = self.tokens.get(self.pos + 2)
                        .map(|t| &t.kind);
                    if matches!(after_abi, Some(TokenKind::Func)) {
                        // extern "C" func ... { body } — Mimi → C export
                        self.advance(); // consume `extern`
                        let abi = {
                            let tok = self.advance().clone(); // consume string
                            if let TokenKind::String(s) = &tok.kind {
                                s.clone()
                            } else {
                                "C".to_string()
                            }
                        };
                        let mut f = self.parse_func()?;
                        f.pub_ = pub_;
                        f.extern_abi = Some(abi);
                        return Ok(Item::Func(f));
                    }
                }
                Ok(Item::ExternBlock(self.parse_extern_block()?))
            }
            TokenKind::Rule => {
                let span = Span::single(self.peek().line, self.peek().col);
                self.advance();
                let s = self.expect_string()?;
                self.match_semi();
                Ok(Item::Rule(s, span))
            }
            TokenKind::Desc => {
                let span = Span::single(self.peek().line, self.peek().col);
                self.advance();
                let s = self.expect_string()?;
                self.match_semi();
                Ok(Item::Desc(s, span))
            }
            _ => {
                let tok = self.peek();
                Err(ParseError::new(
                    format!("unexpected token {} at top level", tok.kind),
                    tok.line,
                    tok.col,
                ))
            }
        }
    }

    fn parse_cap_def(&mut self) -> Result<CapDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Cap)?;
        let name = self.expect_ident()?;
        let combined_with = if self.at(&TokenKind::Plus) {
            // cap A + B syntax
            self.advance();
            let combined_name = self.expect_ident()?;
            Some(combined_name)
        } else if self.at(&TokenKind::Eq) {
            // cap A = B + C syntax
            self.advance();
            let first = self.expect_ident()?;
            if self.at(&TokenKind::Plus) {
                self.advance();
                let second = self.expect_ident()?;
                // Store as "first + second" in combined_with
                Some(format!("{} + {}", first, second))
            } else {
                Some(first)
            }
        } else {
            None
        };
        self.match_semi();
        Ok(CapDef {
            name,
            commitment,
            combined_with,
        })
    }

    fn parse_trait_def(&mut self) -> Result<TraitDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Trait)?;
        let name = self.expect_ident()?;
        let generics = self.parse_generic_params()?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut methods = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            // Parse method signature (no body)
            self.expect(TokenKind::Func, "`func`")?;
            let method_name = self.expect_ident()?;
            let method_generics = self.parse_generic_params()?;
            self.expect(TokenKind::LParen, "`(`")?;
            let params = self.parse_params()?;
            self.expect(TokenKind::RParen, "`)`")?;
            let ret = if self.at(&TokenKind::Arrow) {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            self.match_semi();
            methods.push(TraitMethod {
                name: method_name,
                generics: method_generics,
                params,
                ret,
            });
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(TraitDef {
            name,
            commitment,
            methods,
            generics,
        })
    }

    fn parse_impl_def(&mut self) -> Result<ImplDef, ParseError> {
        self.expect(TokenKind::Impl, "`impl`")?;
        let generics = self.parse_generic_params()?;
        let trait_name = self.expect_ident()?;
        let trait_args = if self.at(&TokenKind::Lt) {
            self.advance();
            let mut args = Vec::new();
            if !self.at(&TokenKind::Gt) {
                loop {
                    args.push(self.parse_type()?);
                    if !self.at(&TokenKind::Comma) {
                        break;
                    }
                    self.advance();
                }
            }
            self.expect_gt("`>`")?;
            args
        } else {
            Vec::new()
        };
        self.expect(TokenKind::For, "`for`")?;
        // Parse the type using parse_type() to support List<T>, Result<T,E>, etc.
        let impl_type = self.parse_type()?;
        let (type_name, type_args) = match impl_type {
            Type::Name(name, args) => (name, args),
            _ => {
                let tok = self.peek();
                return Err(ParseError::new(
                    "expected a named type after `for`",
                    tok.line, tok.col,
                ));
            }
        };
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut methods = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            methods.push(self.parse_func()?);
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(ImplDef {
            generics,
            trait_name,
            trait_args,
            type_name,
            type_args,
            methods,
        })
    }

    fn parse_extern_block(&mut self) -> Result<ExternBlock, ParseError> {
        self.expect(TokenKind::Extern, "`extern`")?;
        // Parse optional ABI string: extern "C" { ... }
        let abi = if matches!(self.peek_kind(), TokenKind::String(_)) {
            // Get the actual string value
            let tok = self.peek().clone();
            if let TokenKind::String(s) = &tok.kind {
                let abi = s.clone();
                self.advance();
                abi
            } else {
                "C".to_string()
            }
        } else {
            "C".to_string()
        };
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut funcs = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            // Parse extern function signature
            self.expect(TokenKind::Func, "`func`")?;
            let name = self.expect_ident()?;
            self.expect(TokenKind::LParen, "`(`")?;
            let mut params = Vec::new();
            if !self.at(&TokenKind::RParen) {
                loop {
                    // Check for cap @ annotation
                    let cap_mode = if self.at(&TokenKind::Cap) {
                        self.advance();
                        self.expect(TokenKind::At, "`@`")?;
                        Some(CapMode::Move) // Default to move
                    } else if self.at(&TokenKind::BitAnd) {
                        // & means borrow
                        self.advance();
                        Some(CapMode::Borrow)
                    } else {
                        None
                    };
                    let param_name = self.expect_ident()?;
                    self.expect(TokenKind::Colon, "`:`")?;
                    let ty = self.parse_type()?;
                    params.push(ExternParam {
                        name: param_name,
                        ty,
                        cap_mode,
                    });
                    if !self.at(&TokenKind::Comma) {
                        break;
                    }
                    self.advance();
                }
            }
            // Check for variadic `...`
            let variadic = if self.at(&TokenKind::Ellipsis) {
                self.advance();
                true
            } else {
                false
            };
            self.expect(TokenKind::RParen, "`)`")?;
            let ret = if self.at(&TokenKind::Arrow) {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            // Parse optional requires/ensures contracts
            self.skip_newlines();
            let mut requires = None;
            let mut ensures = None;
            loop {
                if self.at(&TokenKind::Requires) {
                    self.advance();
                    self.expect(TokenKind::Colon, "`:` after requires")?;
                    requires = Some(self.parse_expr(0)?);
                    self.skip_newlines();
                } else if self.at(&TokenKind::Ensures) {
                    self.advance();
                    self.expect(TokenKind::Colon, "`:` after ensures")?;
                    ensures = Some(self.parse_expr(0)?);
                    self.skip_newlines();
                } else {
                    break;
                }
            }
            self.match_semi();
            funcs.push(ExternFunc {
                name,
                params,
                ret,
                requires,
                ensures,
                variadic,
            });
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(ExternBlock { abi, funcs })
    }

    fn parse_actor_def(&mut self) -> Result<ActorDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Actor)?;
        let name = self.expect_ident()?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        self.skip_newlines();

        let mut fields = Vec::new();
        let mut methods = Vec::new();

        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            // Check if it's a method (func keyword) or field
            if self.at(&TokenKind::Mut) || matches!(self.peek_kind(), TokenKind::Ident(_)) {
                // Could be field: [mut] name: Type [= expr]
                let mut_ = self.at(&TokenKind::Mut);
                if mut_ {
                    self.advance();
                }
                let fname = self.expect_ident()?;
                if self.at(&TokenKind::Colon) {
                    // It's a field
                    self.advance();
                    let fty = self.parse_type()?;
                    let init = if self.at(&TokenKind::Eq) {
                        self.advance();
                        Some(self.parse_expr(0)?)
                    } else {
                        None
                    };
                    self.match_semi();
                    fields.push(ActorField { name: fname, ty: fty, mut_, init });
                } else {
                    // Not a field - error
                    let tok = self.peek();
                    return Err(ParseError::new(
                        "expected `:` for field type",
                        tok.line,
                        tok.col,
                    ));
                }
            } else if self.at(&TokenKind::Func) {
                methods.push(self.parse_func()?);
            } else {
                let tok = self.peek();
                return Err(ParseError::new(
                    format!("unexpected token {} in actor body", tok.kind),
                    tok.line,
                    tok.col,
                ));
            }
            self.skip_newlines();
        }

        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(ActorDef { name, commitment, pub_: false, fields, methods })
    }

    fn parse_module(&mut self) -> Result<ModuleDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Module)?;
        let name = self.expect_ident()?;
        self.skip_newlines();
        if self.is_sketch() {
            self.expect(TokenKind::Colon, "`:`")?;
            self.skip_newlines();
        }
        self.expect_block_start("module body")?;
        let items = self.parse_item_block()?;
        Ok(ModuleDef {
            name,
            commitment,
            items,
        })
    }

    fn parse_func(&mut self) -> Result<FuncDef, ParseError> {
        let pos = (self.peek().line, self.peek().col);
        let commitment = self.expect_keyword(TokenKind::Func)?;
        let name = self.expect_ident()?;
        // Parse optional generic parameters: <T> or <T: Trait>
        let generics = self.parse_generic_params()?;
        let params = if self.is_sketch() && !self.at(&TokenKind::LParen) {
            Vec::new()
        } else {
            self.expect(TokenKind::LParen, "`(`")?;
            let p = self.parse_params()?;
            self.expect(TokenKind::RParen, "`)`")?;
            p
        };
        let ret = if self.at(&TokenKind::Arrow) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        // Parse where clause if present
        let where_clause = if self.at(&TokenKind::Where) {
            self.advance();
            let type_param = self.expect_ident()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let mut bounds = Vec::new();
            bounds.push(self.expect_ident()?);
            while self.at(&TokenKind::Plus) {
                self.advance();
                bounds.push(self.expect_ident()?);
            }
            Some(WhereClause { type_param, bounds })
        } else {
            None
        };
        self.skip_newlines();
        if self.is_sketch() {
            self.expect(TokenKind::Colon, "`:`")?;
            self.skip_newlines();
        }
        // Parse effects if present: with Effect1, Effect2
        let effects = if self.at(&TokenKind::With) {
            self.advance();
            let mut effects = Vec::new();
            effects.push(self.expect_ident()?);
            while self.at(&TokenKind::Comma) {
                self.advance();
                effects.push(self.expect_ident()?);
            }
            effects
        } else {
            Vec::new()
        };
        self.expect_block_start("function body")?;
        let body = self.parse_block()?;
        Ok(FuncDef {
            name,
            commitment,
            pub_: false,
            params,
            ret,
            body,
            where_clause,
            generics,
            effects,
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            pos,
        })
    }

    pub(crate) fn parse_generic_params(&mut self) -> Result<Vec<GenericParam>, ParseError> {
        if !self.at(&TokenKind::Lt) {
            return Ok(Vec::new());
        }
        self.advance();
        let mut params = Vec::new();
        if !self.at(&TokenKind::Gt) {
            loop {
                let name = self.expect_ident()?;
                let bounds = if self.at(&TokenKind::Colon) {
                    self.advance();
                    let mut b = vec![self.expect_ident()?];
                    while self.at(&TokenKind::Plus) {
                        self.advance();
                        b.push(self.expect_ident()?);
                    }
                    b
                } else {
                    Vec::new()
                };
                params.push(GenericParam { name, bounds });
                if !self.at(&TokenKind::Comma) {
                    break;
                }
                self.advance();
            }
        }
        self.expect_gt("`>`")?;
        Ok(params)
    }

    pub(crate) fn parse_params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if self.at(&TokenKind::RParen) {
            return Ok(params);
        }
        loop {
            let mut_ = self.at(&TokenKind::Mut);
            if mut_ {
                self.advance();
            }
            let name = self.expect_ident()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let ty = self.parse_type()?;
            params.push(Param { name, ty, mut_ });
            if !self.at(&TokenKind::Comma) {
                break;
            }
            self.advance();
        }
        Ok(params)
    }

    pub(crate) fn expect_block_start(&mut self, context: &str) -> Result<(), ParseError> {
        self.skip_newlines();
        if self.is_sketch() {
            self.expect(TokenKind::Indent, &format!("indented {}", context))?;
        } else {
            self.expect(TokenKind::LBrace, &format!("`{{` for {}", context))?;
        }
        Ok(())
    }

    fn parse_item_block(&mut self) -> Result<Vec<Item>, ParseError> {
        let mut items = Vec::new();
        self.skip_newlines();
        let end = if self.is_sketch() {
            TokenKind::Dedent
        } else {
            TokenKind::RBrace
        };
        while !self.at(&end) && !self.at(&TokenKind::Eof) {
            items.push(self.parse_item()?);
            self.skip_newlines();
        }
        self.expect(end, if self.is_sketch() { "dedent" } else { "`}`" })?;
        Ok(items)
    }
}
