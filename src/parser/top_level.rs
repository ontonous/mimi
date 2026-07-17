// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::*;

impl Parser {
    pub(crate) fn parse_import(&mut self) -> Result<Import, ParseError> {
        self.expect(TokenKind::Use, "`use`")?;
        let mut path = vec![self.expect_ident()?];
        while self.at(&TokenKind::ColonColon) {
            self.advance();
            path.push(self.expect_ident()?);
        }
        let alias = if self.at(&TokenKind::As) {
            self.advance();
            Some(self.expect_ident()?)
        } else {
            None
        };
        self.match_semi();
        Ok(Import { path, alias })
    }

    pub(crate) fn parse_item(&mut self) -> Result<Item, ParseError> {
        let pub_ = if self.at(&TokenKind::Pub) {
            self.advance();
            true
        } else {
            false
        };
        // Parse optional #[derive(...)], #[repr(...)], and #[no_panic] attributes
        let mut derives = Vec::new();
        let mut attributes = Vec::new();
        let mut no_panic_block = false;
        while self.at(&TokenKind::Hash)
            && self.pos + 1 < self.tokens.len()
            && self.tokens[self.pos + 1].kind == TokenKind::LBracket
        {
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
            } else if self.at(&TokenKind::Ident("no_panic".to_string())) {
                self.advance(); // skip "no_panic"
                no_panic_block = true;
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
            TokenKind::Const => {
                self.advance(); // consume `const`
                let name = self.expect_ident()?;
                let ty = if self.at(&TokenKind::Colon) {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                self.expect(TokenKind::Eq, "`=` after const name")?;
                let value = self.parse_expr(0)?;
                self.match_semi();
                Ok(Item::Const {
                    name,
                    ty,
                    value,
                    pub_,
                })
            }
            TokenKind::Cap => Ok(Item::Cap(self.parse_cap_def()?)),
            TokenKind::Trait => Ok(Item::Trait(self.parse_trait_def()?)),
            TokenKind::Impl => Ok(Item::Impl(self.parse_impl_def()?)),
            TokenKind::Flow => {
                let mut f = self.parse_flow_def()?;
                f.pub_ = pub_;
                Ok(Item::Flow(f))
            }
            TokenKind::Protocol => Ok(Item::Protocol(self.parse_protocol_def()?)),
            TokenKind::Session => {
                let mut s = self.parse_session_def()?;
                s.pub_ = pub_;
                Ok(Item::Session(s))
            }
            TokenKind::Unsafe => {
                // unsafe extern "C" { ... } — bypass passport-type checking
                self.advance(); // consume `unsafe`
                self.skip_newlines();
                let mut extern_block = self.parse_extern_block_with_no_panic(no_panic_block)?;
                extern_block.unsafe_ = true;
                Ok(Item::ExternBlock(extern_block))
            }
            TokenKind::Extern => {
                // Check if this is `extern "C" func` (export) or `extern "C" { ... }` (import)
                // Peek at the token AFTER `extern` to see if it's a string literal
                let has_abi_string = self
                    .tokens
                    .get(self.pos + 1)
                    .map(|t| matches!(t.kind, TokenKind::String(_)))
                    .unwrap_or(false);
                if has_abi_string {
                    // Peek past the string to see if next is `func`
                    let after_abi = self.tokens.get(self.pos + 2).map(|t| &t.kind);
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
                Ok(Item::ExternBlock(
                    self.parse_extern_block_with_no_panic(no_panic_block)?,
                ))
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
        self.expect_keyword(TokenKind::Cap)?;
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
            combined_with,
        })
    }

    fn parse_trait_def(&mut self) -> Result<TraitDef, ParseError> {
        self.expect_keyword(TokenKind::Trait)?;
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
                    tok.line,
                    tok.col,
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

    fn parse_extern_block_with_no_panic(
        &mut self,
        no_panic: bool,
    ) -> Result<ExternBlock, ParseError> {
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
                        self.advance();
                        // &mut is parsed but mutability isn't tracked in ExternParam
                        if self.at(&TokenKind::Mut) {
                            self.advance();
                        }
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
                no_panic,
            });
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(ExternBlock {
            abi,
            funcs,
            no_panic,
            unsafe_: false,
        })
    }

    fn parse_actor_def(&mut self) -> Result<ActorDef, ParseError> {
        self.expect_keyword(TokenKind::Actor)?;
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
                    fields.push(ActorField {
                        name: fname,
                        ty: fty,
                        mut_,
                        init,
                    });
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
        Ok(ActorDef {
            name,
            pub_: false,
            fields,
            methods,
        })
    }

    fn parse_module(&mut self) -> Result<ModuleDef, ParseError> {
        self.expect_keyword(TokenKind::Module)?;
        let name = self.expect_ident()?;
        self.skip_newlines();
        if self.is_sketch() {
            self.expect(TokenKind::Colon, "`:`")?;
            self.skip_newlines();
        }
        self.expect_block_start("module body")?;
        // Module bodies may start with their own `use` imports.
        let mut imports = Vec::new();
        self.skip_newlines();
        while self.at(&TokenKind::Use) {
            imports.push(self.parse_import()?);
            self.skip_newlines();
        }
        let items = self.parse_item_block()?;
        Ok(ModuleDef {
            name,
            imports,
            items,
        })
    }

    pub(super) fn parse_func(&mut self) -> Result<FuncDef, ParseError> {
        let pos = (self.peek().line, self.peek().col);
        self.expect_keyword(TokenKind::Func)?;
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
        // Parse where clause(s) if present: `where T: Bound1 + Bound2, U: Bound3`
        let where_clause = if self.at(&TokenKind::Where) {
            self.advance();
            let mut clauses = Vec::new();
            loop {
                let type_param = self.expect_ident()?;
                self.expect(TokenKind::Colon, "`:`")?;
                let mut bounds = Vec::new();
                bounds.push(self.expect_ident()?);
                while self.at(&TokenKind::Plus) {
                    self.advance();
                    bounds.push(self.expect_ident()?);
                }
                clauses.push(WhereClause { type_param, bounds });
                if self.at(&TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            clauses
        } else {
            Vec::new()
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
        self.skip_newlines();
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
            // v0.29.23: optional `view` / `mutate` borrow mode before the type.
            let borrow = if self.at(&TokenKind::View) {
                self.advance();
                Some(ParamBorrow::View)
            } else if self.at(&TokenKind::Mutate) {
                self.advance();
                Some(ParamBorrow::Mutate)
            } else {
                None
            };
            let ty = self.parse_type()?;
            let default_value = if self.at(&TokenKind::Eq) {
                self.advance();
                Some(self.parse_expr(0)?)
            } else {
                None
            };
            // mutate implies mut_ for assignment checking inside the callee.
            let mut_ = mut_ || matches!(borrow, Some(ParamBorrow::Mutate));
            params.push(Param {
                name,
                ty,
                mut_,
                default_value,
                borrow,
            });
            self.skip_newlines();
            if !self.at(&TokenKind::Comma) {
                break;
            }
            self.advance();
            self.skip_newlines();
            // Allow trailing comma: if the next token is `)`, stop here.
            if self.at(&TokenKind::RParen) {
                break;
            }
        }
        self.skip_newlines();
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

    fn parse_flow_def(&mut self) -> Result<FlowDef, ParseError> {
        let pos = (self.peek().line, self.peek().col);
        self.expect_keyword(TokenKind::Flow)?;
        let name = self.expect_ident()?;
        let generics = self.parse_generic_params()?;
        // Parse optional annotations: @mailbox(depth = 2048) etc.
        let mut annotations = Vec::new();
        while self.at(&TokenKind::At) {
            self.advance();
            let ann_name = self.expect_ident()?;
            self.expect(TokenKind::LParen, "`(`")?;
            match ann_name.as_str() {
                "mailbox" => {
                    if matches!(self.peek_kind(), TokenKind::Ident(_))
                        && self.pos + 1 < self.tokens.len()
                        && self.tokens[self.pos + 1].kind == TokenKind::Eq
                    {
                        self.expect_ident()?; // skip "depth"
                        self.expect(TokenKind::Eq, "`=`")?;
                    }
                    if let TokenKind::Int(s) = &self.peek().kind {
                        // PR-C1: parse failure is a hard error, not silent default.
                        let tok = self.peek();
                        let (line, col) = (tok.line, tok.col);
                        let depth = s.parse::<usize>().map_err(|_| {
                            ParseError::new(
                                format!(
                                    "invalid @mailbox depth '{}': expected non-negative integer",
                                    s
                                ),
                                line,
                                col,
                            )
                        })?;
                        self.advance();
                        annotations.push(FlowAnnotation::MailboxDepth(depth));
                    } else {
                        let tok = self.peek();
                        return Err(ParseError::new(
                            "expected integer depth in @mailbox(...), e.g. @mailbox(depth=2048)",
                            tok.line,
                            tok.col,
                        ));
                    }
                }
                "max_children" => {
                    if matches!(self.peek_kind(), TokenKind::Ident(_))
                        && self.pos + 1 < self.tokens.len()
                        && self.tokens[self.pos + 1].kind == TokenKind::Eq
                    {
                        self.expect_ident()?; // skip "children"
                        self.expect(TokenKind::Eq, "`=`")?;
                    }
                    if let TokenKind::Int(s) = &self.peek().kind {
                        // PR-C1: parse failure is a hard error, not silent default.
                        let tok = self.peek();
                        let (line, col) = (tok.line, tok.col);
                        let n = s.parse::<usize>().map_err(|_| {
                            ParseError::new(
                                format!(
                                    "invalid @max_children value '{}': expected non-negative integer",
                                    s
                                ),
                                line,
                                col,
                            )
                        })?;
                        self.advance();
                        annotations.push(FlowAnnotation::MaxChildren(n));
                    } else {
                        let tok = self.peek();
                        return Err(ParseError::new(
                            "expected integer in @max_children(...), e.g. @max_children(10)",
                            tok.line,
                            tok.col,
                        ));
                    }
                }
                _ => {}
            }
            self.expect(TokenKind::RParen, "`)`")?;
            self.skip_newlines();
        }
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut states = Vec::new();
        let mut transitions = Vec::new();
        let mut impl_protocols = Vec::new();
        let mut persistent_fields = Vec::new();
        let mut transactional_fields = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            // Check for `impl ProtocolName`
            if self.at(&TokenKind::Impl) {
                self.advance();
                let proto = self.expect_ident()?;
                self.match_semi();
                impl_protocols.push(proto);
                continue;
            }
            // Check for `persistent` modifier or `@` annotation
            // `@transactional` may appear without `()` before `persistent state`.
            let mut state_all_transactional = false;
            if self.at(&TokenKind::At) {
                // Peek: @transactional without paren → field/state attribute
                let saved = self.pos;
                self.advance();
                let ann_name = self.expect_ident()?;
                if ann_name == "transactional" && !self.at(&TokenKind::LParen) {
                    state_all_transactional = true;
                    self.skip_newlines();
                    // fall through to persistent/state parsing
                } else {
                    // Restore and parse as flow annotation @name(...)
                    self.pos = saved;
                    self.advance();
                    let ann_name = self.expect_ident()?;
                    self.expect(TokenKind::LParen, "`(`")?;
                    match ann_name.as_str() {
                        "mailbox" => {
                            if matches!(self.peek_kind(), TokenKind::Ident(_))
                                && self.pos + 1 < self.tokens.len()
                                && self.tokens[self.pos + 1].kind == TokenKind::Eq
                            {
                                self.expect_ident()?;
                                self.expect(TokenKind::Eq, "`=`")?;
                            }
                            if let TokenKind::Int(s) = &self.peek().kind {
                                // PR-C1: parse failure is a hard error, not silent default.
                                let tok = self.peek();
                                let (line, col) = (tok.line, tok.col);
                                let depth = s.parse::<usize>().map_err(|_| {
                                    ParseError::new(
                                        format!(
                                            "invalid @mailbox depth '{}': expected non-negative integer",
                                            s
                                        ),
                                        line,
                                        col,
                                    )
                                })?;
                                self.advance();
                                annotations.push(FlowAnnotation::MailboxDepth(depth));
                            } else {
                                let tok = self.peek();
                                return Err(ParseError::new(
                                    "expected integer depth in @mailbox(...), e.g. @mailbox(depth=2048)",
                                    tok.line,
                                    tok.col,
                                ));
                            }
                        }
                        "max_children" => {
                            if matches!(self.peek_kind(), TokenKind::Ident(_))
                                && self.pos + 1 < self.tokens.len()
                                && self.tokens[self.pos + 1].kind == TokenKind::Eq
                            {
                                self.expect_ident()?;
                                self.expect(TokenKind::Eq, "`=`")?;
                            }
                            if let TokenKind::Int(s) = &self.peek().kind {
                                // PR-C1: parse failure is a hard error, not silent default.
                                let tok = self.peek();
                                let (line, col) = (tok.line, tok.col);
                                let n = s.parse::<usize>().map_err(|_| {
                                    ParseError::new(
                                        format!(
                                            "invalid @max_children value '{}': expected non-negative integer",
                                            s
                                        ),
                                        line,
                                        col,
                                    )
                                })?;
                                self.advance();
                                annotations.push(FlowAnnotation::MaxChildren(n));
                            } else {
                                let tok = self.peek();
                                return Err(ParseError::new(
                                    "expected integer in @max_children(...), e.g. @max_children(10)",
                                    tok.line,
                                    tok.col,
                                ));
                            }
                        }
                        "transactional" => {
                            // PR-H3: @transactional(...) with parens is invalid —
                            // only bare `@transactional` before state/fields is supported.
                            let tok = self.peek();
                            return Err(ParseError::new(
                                "`@transactional(...)` takes no arguments; write bare `@transactional` before a state or fields",
                                tok.line,
                                tok.col,
                            ));
                        }
                        _ => {
                            // PR-H2: unknown @annotations must surface as parse errors
                            // (not eprintln!) so LSP/check can report them with span.
                            let tok = self.peek();
                            return Err(ParseError::new(
                                format!(
                                    "unknown flow annotation '@{}' — expected @mailbox(...), @max_children(...), or bare @transactional",
                                    ann_name
                                ),
                                tok.line,
                                tok.col,
                            ));
                        }
                    }
                    self.expect(TokenKind::RParen, "`)`")?;
                    continue;
                }
            }
            // Check for `persistent` modifier
            let is_persistent = self.at(&TokenKind::Persistent);
            if is_persistent {
                self.advance();
            }
            match self.peek_kind() {
                TokenKind::State => {
                    let state = self.parse_state_def()?;
                    if is_persistent {
                        if let Some(ref payload) = state.payload {
                            for field in payload {
                                if !persistent_fields.contains(&field.name) {
                                    persistent_fields.push(field.name.clone());
                                }
                                // @transactional before persistent state → all fields WAL-backed
                                if state_all_transactional
                                    && !transactional_fields.contains(&field.name)
                                {
                                    transactional_fields.push(field.name.clone());
                                }
                            }
                        }
                    }
                    states.push(state);
                }
                TokenKind::Transition => {
                    if is_persistent {
                        return Err(ParseError::new(
                            "`persistent` cannot be applied to a transition",
                            self.peek().line,
                            self.peek().col,
                        ));
                    }
                    if state_all_transactional {
                        return Err(ParseError::new(
                            "`@transactional` cannot be applied to a transition",
                            self.peek().line,
                            self.peek().col,
                        ));
                    }
                    transitions.push(self.parse_transition_def()?);
                }
                _ => {
                    let tok = self.peek();
                    return Err(ParseError::new(
                        format!(
                            "expected `state` or `transition` in flow body, found {}",
                            tok.kind
                        ),
                        tok.line,
                        tok.col,
                    ));
                }
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(FlowDef {
            name,
            pos,
            origin: AstOrigin::User,
            pub_: false,
            generics,
            annotations,
            states,
            transitions,
            impl_protocols,
            persistent_fields,
            transactional_fields,
            metadata_shadow_fields: vec![],
        })
    }

    fn parse_state_def(&mut self) -> Result<StateDef, ParseError> {
        let pos = (self.peek().line, self.peek().col);
        self.expect_keyword(TokenKind::State)?;
        let name = self.expect_ident()?;
        let payload = if self.at(&TokenKind::LBrace) {
            self.advance();
            let mut fields = Vec::new();
            while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
                let fname = self.expect_ident()?;
                self.expect(TokenKind::Colon, "`:`")?;
                let fty = self.parse_type()?;
                fields.push(Field {
                    name: fname,
                    ty: fty,
                });
                if self.at(&TokenKind::Comma) {
                    self.advance();
                }
                self.skip_newlines();
            }
            self.expect(TokenKind::RBrace, "`}`")?;
            Some(fields)
        } else {
            None
        };
        self.match_semi();
        Ok(StateDef {
            name,
            pos,
            origin: AstOrigin::User,
            payload,
        })
    }

    fn parse_transition_def(&mut self) -> Result<TransitionDef, ParseError> {
        let pos = (self.peek().line, self.peek().col);
        self.expect_keyword(TokenKind::Transition)?;
        let name = self.expect_ident()?;
        // Parse: (FromState) or (FromState, event_param, ...)
        self.expect(TokenKind::LParen, "`(`")?;
        let from_state = self.expect_ident()?;
        let mut params = Vec::new();
        self.skip_newlines();
        // Check for event params (after from_state)
        if self.at(&TokenKind::Comma) {
            self.advance();
            self.skip_newlines();
            loop {
                let mut_ = self.at(&TokenKind::Mut);
                if mut_ {
                    self.advance();
                }
                let pname = self.expect_ident()?;
                self.expect(TokenKind::Colon, "`:`")?;
                let borrow = if self.at(&TokenKind::View) {
                    self.advance();
                    Some(ParamBorrow::View)
                } else if self.at(&TokenKind::Mutate) {
                    self.advance();
                    Some(ParamBorrow::Mutate)
                } else {
                    None
                };
                let pty = self.parse_type()?;
                let mut_ = mut_ || matches!(borrow, Some(ParamBorrow::Mutate));
                params.push(Param {
                    name: pname,
                    ty: pty,
                    mut_,
                    default_value: None,
                    borrow,
                });
                self.skip_newlines();
                if !self.at(&TokenKind::Comma) {
                    break;
                }
                self.advance();
                self.skip_newlines();
                if self.at(&TokenKind::RParen) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen, "`)`")?;
        // Parse -> ToState or -> ToState1 | ToState2
        self.skip_newlines();
        self.expect(TokenKind::Arrow, "`->`")?;
        let mut to_states = Vec::new();
        loop {
            let target = self.expect_ident()?;
            to_states.push(target);
            self.skip_newlines();
            if self.at(&TokenKind::PipeArrow) || self.at(&TokenKind::BitOr) {
                self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }
        // Parse optional body: { do { ... } }
        let body = if self.at(&TokenKind::LBrace) {
            self.expect(TokenKind::LBrace, "`{`")?;
            Some(self.parse_block()?)
        } else if self.at(&TokenKind::Semi) {
            self.advance();
            None
        } else {
            self.match_semi();
            None
        };
        Ok(TransitionDef {
            name,
            from_state,
            params,
            to_states,
            body,
            pos,
            is_fallback: false,
            is_ffi_pinned: false,
        })
    }

    fn parse_protocol_def(&mut self) -> Result<ProtocolDef, ParseError> {
        self.expect_keyword(TokenKind::Protocol)?;
        let name = self.expect_ident()?;
        let generics = self.parse_generic_params()?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut states = Vec::new();
        let mut transitions = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            match self.peek_kind() {
                TokenKind::State => {
                    self.advance();
                    let name = self.expect_ident()?;
                    let (payload_name, payload_type) = if self.at(&TokenKind::LBrace) {
                        self.advance();
                        // Parse single payload type: { data: f32 }
                        let field_name = self.expect_ident()?;
                        self.expect(TokenKind::Colon, "`:`")?;
                        let fty = self.parse_type()?;
                        self.expect(TokenKind::RBrace, "`}`")?;
                        (Some(field_name), Some(fty))
                    } else {
                        (None, None)
                    };
                    self.match_semi();
                    states.push(ProtocolStateDef {
                        name,
                        payload_name,
                        payload_type,
                    });
                }
                TokenKind::Transition => {
                    self.advance();
                    let tname = self.expect_ident()?;
                    self.expect(TokenKind::LParen, "`(`")?;
                    let from_state = self.expect_ident()?;
                    self.expect(TokenKind::RParen, "`)`")?;
                    self.skip_newlines();
                    self.expect(TokenKind::Arrow, "`->`")?;
                    let to_state = self.expect_ident()?;
                    self.match_semi();
                    transitions.push(ProtocolTransitionDef {
                        name: tname,
                        from_state,
                        to_state,
                    });
                }
                _ => {
                    let tok = self.peek();
                    return Err(ParseError::new(
                        format!(
                            "expected `state` or `transition` in protocol body, found {}",
                            tok.kind
                        ),
                        tok.line,
                        tok.col,
                    ));
                }
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(ProtocolDef {
            name,
            generics,
            states,
            transitions,
        })
    }

    /// Parse `session Name = SessionTypeExpr ;`
    ///
    /// Session type grammar (v0.29.19):
    /// ```text
    /// S ::= ! Type . S | ? Type . S | dual ( S ) | end | Name
    /// ```
    fn parse_session_def(&mut self) -> Result<SessionDef, ParseError> {
        self.expect_keyword(TokenKind::Session)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::Eq, "`=` after session name")?;
        self.skip_newlines();
        let body = self.parse_session_type()?;
        self.match_semi();
        Ok(SessionDef {
            name,
            pub_: false,
            body,
        })
    }

    /// Parse a session type expression starting at the current token.
    pub(crate) fn parse_session_type(&mut self) -> Result<SessionType, ParseError> {
        // `!T . cont`  or  `?T . cont`  or  `dual(...)`  or  `end`  or  `Name`
        if self.at(&TokenKind::Bang) || self.at(&TokenKind::NotOp) {
            self.advance(); // consume `!`
            let payload = self.parse_type()?;
            self.expect(TokenKind::Dot, "`.` after send payload type")?;
            self.skip_newlines();
            let cont = self.parse_session_type()?;
            return Ok(SessionType::Send(payload, Box::new(cont)));
        }
        if self.at(&TokenKind::Question) {
            self.advance(); // consume `?`
            let payload = self.parse_type()?;
            self.expect(TokenKind::Dot, "`.` after recv payload type")?;
            self.skip_newlines();
            let cont = self.parse_session_type()?;
            return Ok(SessionType::Recv(payload, Box::new(cont)));
        }
        if self.at(&TokenKind::Dual) {
            self.advance();
            self.expect(TokenKind::LParen, "`(` after dual")?;
            let inner = self.parse_session_type()?;
            self.expect(TokenKind::RParen, "`)` after dual(...)")?;
            return Ok(SessionType::Dual(Box::new(inner)));
        }
        if self.at(&TokenKind::End) {
            self.advance();
            return Ok(SessionType::End);
        }
        // Named session reference
        if matches!(self.peek_kind(), TokenKind::Ident(_)) {
            let name = self.expect_ident()?;
            return Ok(SessionType::Name(name));
        }
        let tok = self.peek();
        Err(ParseError::new(
            format!(
                "expected session type (`!T . S`, `?T . S`, `dual(S)`, `end`, or name), found {}",
                tok.kind
            ),
            tok.line,
            tok.col,
        ))
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
