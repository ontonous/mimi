// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::*;

impl Parser {
    fn attribute_error(&self, token: &Token, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            line: token.line,
            col: token.col,
            source_id: self.source_id,
            span: Some(
                Span::new(token.line, token.col, token.end_line, token.end_col)
                    .with_source(self.source_id),
            ),
        }
    }

    pub(crate) fn parse_import(&mut self) -> Result<Import, ParseError> {
        let start_pos = self.pos;
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
        Ok(Import {
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            path,
            alias,
        })
    }

    pub(crate) fn parse_item(&mut self) -> Result<Item, ParseError> {
        let start_pos = self.pos;
        let mut item = self.parse_item_kind()?;
        let meta = self.consumed_meta(start_pos, AstOrigin::User);
        match &mut item {
            Item::Func(def) => def.meta = meta,
            Item::Module(def) => def.meta = meta,
            Item::Type(def) => def.meta = meta,
            Item::Actor(def) => def.meta = meta,
            Item::Cap(def) => def.meta = meta,
            Item::Trait(def) => def.meta = meta,
            Item::Impl(def) => def.meta = meta,
            Item::ExternBlock(def) => def.meta = meta,
            Item::Const {
                meta: const_meta, ..
            } => *const_meta = meta,
            Item::Flow(def) => def.meta = meta,
            Item::Protocol(def) => def.meta = meta,
            Item::Session(def) => def.meta = meta,
        }
        Ok(item)
    }

    fn parse_item_kind(&mut self) -> Result<Item, ParseError> {
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
        let mut type_attribute_token = None;
        let mut no_panic_attribute_token = None;
        while self.at(&TokenKind::Hash)
            && self.pos + 1 < self.tokens.len()
            && self.tokens[self.pos + 1].kind == TokenKind::LBracket
        {
            self.advance(); // skip #
            self.advance(); // skip [
            if self.at(&TokenKind::Ident("derive".to_string())) {
                type_attribute_token.get_or_insert_with(|| self.peek().clone());
                self.advance(); // skip "derive"
                self.expect(TokenKind::LParen, "`(`")?;
                if self.at(&TokenKind::RParen) {
                    let token = self.peek().clone();
                    return Err(self.attribute_error(
                        &token,
                        "`#[derive(...)]` requires at least one derive name",
                    ));
                }
                while !self.at(&TokenKind::RParen) && !self.at(&TokenKind::Eof) {
                    let derive_token = self.peek().clone();
                    let name = self.expect_ident()?;
                    match name.as_str() {
                        "Debug" | "Clone" | "Eq" => derives.push(name),
                        "Copy" | "Default" => {
                            return Err(self.attribute_error(
                                &derive_token,
                                format!(
                                    "derive `{}` is reserved but not implemented; supported derives: Debug, Clone, Eq",
                                    name
                                ),
                            ));
                        }
                        _ => {
                            return Err(self.attribute_error(
                                &derive_token,
                                format!(
                                    "unknown derive `{}`; supported derives: Debug, Clone, Eq",
                                    name
                                ),
                            ));
                        }
                    }
                    if self.at(&TokenKind::Comma) {
                        self.advance();
                    }
                }
                self.expect(TokenKind::RParen, "`)`")?;
            } else if self.at(&TokenKind::Ident("repr".to_string())) {
                type_attribute_token.get_or_insert_with(|| self.peek().clone());
                self.advance(); // skip "repr"
                self.expect(TokenKind::LParen, "`(`")?;
                let repr_token = self.peek().clone();
                let repr_name = self.expect_ident()?;
                match repr_name.as_str() {
                    "C" => attributes.push(crate::ast::TypeAttribute::ReprC),
                    "transparent" => attributes.push(crate::ast::TypeAttribute::ReprTransparent),
                    _ => {
                        return Err(self.attribute_error(
                            &repr_token,
                            format!(
                                "unknown repr `{}`; supported representations: C, transparent",
                                repr_name
                            ),
                        ));
                    }
                }
                self.expect(TokenKind::RParen, "`)`")?;
            } else if self.at(&TokenKind::Ident("no_panic".to_string())) {
                no_panic_attribute_token.get_or_insert_with(|| self.peek().clone());
                self.advance(); // skip "no_panic"
                no_panic_block = true;
            } else {
                let token = self.peek().clone();
                return Err(self.attribute_error(
                    &token,
                    format!("unknown attribute `{}`", token.kind.source_text()),
                ));
            }
            self.expect(TokenKind::RBracket, "`]`")?;
            self.skip_newlines();
        }
        if let Some(token) = &type_attribute_token {
            if !matches!(self.peek_kind(), TokenKind::Type | TokenKind::Newtype) {
                return Err(self.attribute_error(
                    token,
                    format!(
                        "attribute `{}` is only supported on type declarations",
                        token.kind.source_text()
                    ),
                ));
            }
        }
        if let Some(token) = &no_panic_attribute_token {
            if !matches!(self.peek_kind(), TokenKind::Extern | TokenKind::Unsafe) {
                return Err(self.attribute_error(
                    token,
                    "attribute `no_panic` is only supported on extern blocks",
                ));
            }
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
                t.derives = derives;
                t.attributes = attributes;
                Ok(Item::Type(t))
            }
            TokenKind::Actor => {
                let mut a = self.parse_actor_def()?;
                a.pub_ = pub_;
                Ok(Item::Actor(a))
            }
            TokenKind::Const => {
                let start_pos = self.pos;
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
                    meta: self.consumed_meta(start_pos, AstOrigin::User),
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
                        if let Some(token) = &no_panic_attribute_token {
                            return Err(self.attribute_error(
                                token,
                                "attribute `no_panic` is only supported on extern blocks, not extern functions",
                            ));
                        }
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
        let start_pos = self.pos;
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
            combined_with,
        })
    }

    fn parse_trait_def(&mut self) -> Result<TraitDef, ParseError> {
        let start_pos = self.pos;
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
            let method_start = self.pos;
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
                meta: self.consumed_meta(method_start, AstOrigin::User),
                name: method_name,
                generics: method_generics,
                params,
                ret,
            });
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(TraitDef {
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
            methods,
            generics,
        })
    }

    fn parse_impl_def(&mut self) -> Result<ImplDef, ParseError> {
        let start_pos = self.pos;
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
        let (type_name, type_args) = match impl_type.into_unlocated() {
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
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
        let start_pos = self.pos;
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
            let func_start = self.pos;
            self.expect(TokenKind::Func, "`func`")?;
            let name = self.expect_ident()?;
            self.expect(TokenKind::LParen, "`(`")?;
            let mut params = Vec::new();
            if !self.at(&TokenKind::RParen) {
                loop {
                    let param_start = self.pos;
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
                        meta: self.consumed_meta(param_start, AstOrigin::User),
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
                meta: self.consumed_meta(func_start, AstOrigin::User),
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            abi,
            funcs,
            no_panic,
            unsafe_: false,
        })
    }

    fn parse_actor_def(&mut self) -> Result<ActorDef, ParseError> {
        let start_pos = self.pos;
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
                let field_start = self.pos;
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
                        meta: self.consumed_meta(field_start, AstOrigin::User),
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
            pub_: false,
            fields,
            methods,
        })
    }

    fn parse_module(&mut self) -> Result<ModuleDef, ParseError> {
        let start_pos = self.pos;
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
            imports,
            items,
        })
    }

    pub(super) fn parse_func(&mut self) -> Result<FuncDef, ParseError> {
        let start_pos = self.pos;
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
                let clause_start = self.pos;
                let type_param = self.expect_ident()?;
                self.expect(TokenKind::Colon, "`:`")?;
                let mut bounds = Vec::new();
                bounds.push(self.expect_ident()?);
                while self.at(&TokenKind::Plus) {
                    self.advance();
                    bounds.push(self.expect_ident()?);
                }
                clauses.push(WhereClause {
                    meta: self.consumed_meta(clause_start, AstOrigin::User),
                    type_param,
                    bounds,
                });
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
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
                let param_start = self.pos;
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
                params.push(GenericParam {
                    meta: self.consumed_meta(param_start, AstOrigin::User),
                    name,
                    bounds,
                });
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
            let param_start = self.pos;
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
                meta: self.consumed_meta(param_start, AstOrigin::User),
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
        let start_pos = self.pos;
        self.expect_keyword(TokenKind::Flow)?;
        let name = self.expect_ident()?;
        let generics = self.parse_generic_params()?;
        // Parse optional annotations: @mailbox(depth = 2048) etc.
        let mut annotations = Vec::new();
        while self.at(&TokenKind::At) {
            let annotation_start = self.pos;
            self.advance();
            let ann_name = self.expect_ident()?;
            self.expect(TokenKind::LParen, "`(`")?;
            let kind = match ann_name.as_str() {
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
                        FlowAnnotationKind::MailboxDepth(depth)
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
                        FlowAnnotationKind::MaxChildren(n)
                    } else {
                        let tok = self.peek();
                        return Err(ParseError::new(
                            "expected integer in @max_children(...), e.g. @max_children(10)",
                            tok.line,
                            tok.col,
                        ));
                    }
                }
                _ => {
                    let tok = self.peek();
                    return Err(ParseError::new(
                        format!(
                            "unknown flow annotation '@{}' — expected @mailbox(...) or @max_children(...)",
                            ann_name
                        ),
                        tok.line,
                        tok.col,
                    ));
                }
            };
            self.expect(TokenKind::RParen, "`)`")?;
            annotations.push(FlowAnnotation::new(
                self.consumed_meta(annotation_start, AstOrigin::User),
                kind,
            ));
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
                    let annotation_start = self.pos;
                    self.advance();
                    let ann_name = self.expect_ident()?;
                    self.expect(TokenKind::LParen, "`(`")?;
                    let kind = match ann_name.as_str() {
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
                                FlowAnnotationKind::MailboxDepth(depth)
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
                                FlowAnnotationKind::MaxChildren(n)
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
                    };
                    self.expect(TokenKind::RParen, "`)`")?;
                    annotations.push(FlowAnnotation::new(
                        self.consumed_meta(annotation_start, AstOrigin::User),
                        kind,
                    ));
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
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
        let start_pos = self.pos;
        self.expect_keyword(TokenKind::State)?;
        let name = self.expect_ident()?;
        let payload = if self.at(&TokenKind::LBrace) {
            self.advance();
            let mut fields = Vec::new();
            while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
                let field_start = self.pos;
                let fname = self.expect_ident()?;
                self.expect(TokenKind::Colon, "`:`")?;
                let fty = self.parse_type()?;
                fields.push(Field {
                    meta: self.consumed_meta(field_start, AstOrigin::User),
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
            payload,
        })
    }

    fn parse_transition_def(&mut self) -> Result<TransitionDef, ParseError> {
        let start_pos = self.pos;
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
                let param_start = self.pos;
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
                    meta: self.consumed_meta(param_start, AstOrigin::User),
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
            from_state,
            params,
            to_states,
            body,
            is_fallback: false,
            is_ffi_pinned: false,
        })
    }

    fn parse_protocol_def(&mut self) -> Result<ProtocolDef, ParseError> {
        let start_pos = self.pos;
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
                    let state_start = self.pos;
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
                        meta: self.consumed_meta(state_start, AstOrigin::User),
                        name,
                        payload_name,
                        payload_type,
                    });
                }
                TokenKind::Transition => {
                    let transition_start = self.pos;
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
                        meta: self.consumed_meta(transition_start, AstOrigin::User),
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
            meta: self.consumed_meta(start_pos, AstOrigin::User),
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
        let start_pos = self.pos;
        self.expect_keyword(TokenKind::Session)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::Eq, "`=` after session name")?;
        self.skip_newlines();
        let body = self.parse_session_type()?;
        self.match_semi();
        Ok(SessionDef {
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
            pub_: false,
            body,
        })
    }

    /// Parse a session type expression starting at the current token.
    pub(crate) fn parse_session_type(&mut self) -> Result<SessionType, ParseError> {
        let start_pos = self.pos;
        // `!T . cont`  or  `?T . cont`  or  `dual(...)`  or  `end`  or  `Name`
        if self.at(&TokenKind::Bang) || self.at(&TokenKind::NotOp) {
            self.advance(); // consume `!`
            let payload = self.parse_type()?;
            self.expect(TokenKind::Dot, "`.` after send payload type")?;
            self.skip_newlines();
            let cont = self.parse_session_type()?;
            return Ok(SessionType::Send(payload, Box::new(cont))
                .with_meta(self.consumed_meta(start_pos, AstOrigin::User)));
        }
        if self.at(&TokenKind::Question) {
            self.advance(); // consume `?`
            let payload = self.parse_type()?;
            self.expect(TokenKind::Dot, "`.` after recv payload type")?;
            self.skip_newlines();
            let cont = self.parse_session_type()?;
            return Ok(SessionType::Recv(payload, Box::new(cont))
                .with_meta(self.consumed_meta(start_pos, AstOrigin::User)));
        }
        if self.at(&TokenKind::Dual) {
            self.advance();
            self.expect(TokenKind::LParen, "`(` after dual")?;
            let inner = self.parse_session_type()?;
            self.expect(TokenKind::RParen, "`)` after dual(...)")?;
            return Ok(SessionType::Dual(Box::new(inner))
                .with_meta(self.consumed_meta(start_pos, AstOrigin::User)));
        }
        if self.at(&TokenKind::End) {
            self.advance();
            return Ok(SessionType::End.with_meta(self.consumed_meta(start_pos, AstOrigin::User)));
        }
        // Named session reference
        if matches!(self.peek_kind(), TokenKind::Ident(_)) {
            let name = self.expect_ident()?;
            return Ok(
                SessionType::Name(name).with_meta(self.consumed_meta(start_pos, AstOrigin::User))
            );
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

#[cfg(test)]
mod attribute_tests {
    use super::*;
    use crate::diagnostic::Severity;
    use crate::lexer::Lexer;
    use crate::span::SourceId;

    fn parse_with_source(source: &str, source_id: SourceId) -> Result<File, ParseError> {
        let tokens = Lexer::new(source).tokenize().expect("lex source");
        Parser::new_with_source(tokens, source_id).parse_file()
    }

    fn span_for(source: &str, fragment: &str, source_id: SourceId) -> Span {
        let offset = source
            .find(fragment)
            .expect("fragment must occur in source");
        let mut line = 1;
        let mut col = 1;
        for ch in source[..offset].chars() {
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        let (start_line, start_col) = (line, col);
        for ch in fragment.chars() {
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        Span::new(start_line, start_col, line, col).with_source(source_id)
    }

    fn assert_user_meta(meta: AstNodeMeta, expected: Span) {
        assert_eq!(meta.origin, AstOrigin::User);
        assert_eq!(meta.span, expected);
    }

    fn assert_attribute_error(
        source: &str,
        expected_message: &str,
        expected_span: Span,
    ) -> ParseError {
        let source_id = expected_span.source_id;
        let error = parse_with_source(source, source_id).expect_err("attribute must fail closed");
        assert!(
            error.message.contains(expected_message),
            "unexpected message: {}",
            error.message
        );
        assert_eq!(error.source_id, source_id);
        assert_eq!(error.span, Some(expected_span));
        let diagnostic = error.to_diagnostic();
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.span, expected_span);
        error
    }

    #[test]
    fn unknown_repr_is_a_source_aware_parse_diagnostic() {
        let source_id = SourceId::new(81);
        assert_attribute_error(
            "#[repr(packed)]\ntype Packet { value: i32 }",
            "unknown repr `packed`",
            Span::new(1, 8, 1, 14).with_source(source_id),
        );
    }

    #[test]
    fn unknown_and_reserved_derives_fail_closed() {
        let source_id = SourceId::new(82);
        assert_attribute_error(
            "#[derive(Serialize)]\ntype Packet { value: i32 }",
            "unknown derive `Serialize`",
            Span::new(1, 10, 1, 19).with_source(source_id),
        );
        assert_attribute_error(
            "#[derive(Copy)]\ntype Packet { value: i32 }",
            "derive `Copy` is reserved but not implemented",
            Span::new(1, 10, 1, 14).with_source(source_id),
        );
    }

    #[test]
    fn empty_and_unknown_attributes_fail_closed() {
        let source_id = SourceId::new(83);
        assert_attribute_error(
            "#[derive()]\ntype Packet { value: i32 }",
            "requires at least one derive name",
            Span::new(1, 10, 1, 11).with_source(source_id),
        );
        assert_attribute_error(
            "#[mystery]\ntype Packet { value: i32 }",
            "unknown attribute `mystery`",
            Span::new(1, 3, 1, 10).with_source(source_id),
        );
    }

    #[test]
    fn implemented_attributes_on_unsupported_declarations_fail_closed() {
        let source_id = SourceId::new(85);
        assert_attribute_error(
            "#[derive(Debug)]\nfunc main() -> i32 { 1 }",
            "only supported on type declarations",
            Span::new(1, 3, 1, 9).with_source(source_id),
        );
        assert_attribute_error(
            "#[no_panic]\nfunc main() -> i32 { 1 }",
            "only supported on extern blocks",
            Span::new(1, 3, 1, 11).with_source(source_id),
        );
    }

    #[test]
    fn implemented_derive_and_repr_attributes_are_preserved() {
        let source_id = SourceId::new(84);
        let attributed_source =
            "#[derive(Debug, Clone, Eq)]\n#[repr(C)]\ntype Point { x: i32, y: i32 }";
        let file = parse_with_source(attributed_source, source_id)
            .expect("supported attributes must parse");
        let Item::Type(type_def) = &file.items[0] else {
            panic!("expected type definition");
        };
        assert_eq!(type_def.derives, ["Debug", "Clone", "Eq"]);
        assert_eq!(type_def.attributes, [TypeAttribute::ReprC]);
        assert_user_meta(
            type_def.meta,
            span_for(attributed_source, attributed_source, source_id),
        );

        let transparent = parse_with_source("#[repr(transparent)]\ntype UserId = i64", source_id)
            .expect("transparent repr must remain supported");
        let Item::Type(type_def) = &transparent.items[0] else {
            panic!("expected transparent type definition");
        };
        assert_eq!(type_def.attributes, [TypeAttribute::ReprTransparent]);

        let newtype = parse_with_source(
            "#[derive(Clone)]\n#[repr(transparent)]\nnewtype UserId = i64",
            source_id,
        )
        .expect("type attributes must be preserved on newtypes");
        let Item::Type(type_def) = &newtype.items[0] else {
            panic!("expected newtype definition");
        };
        assert_eq!(type_def.derives, ["Clone"]);
        assert_eq!(type_def.attributes, [TypeAttribute::ReprTransparent]);
    }

    #[test]
    fn declaration_and_signature_children_have_exact_metadata() {
        let source_id = SourceId::new(86);

        let import_source = "use std::io as console;";
        let imports = parse_with_source(import_source, source_id).expect("parse import");
        assert_user_meta(
            imports.imports[0].meta,
            span_for(import_source, import_source, source_id),
        );

        let type_source = "type Pair<T: Clone> { left: T, right: i32 }";
        let types = parse_with_source(type_source, source_id).expect("parse record type");
        let Item::Type(pair) = &types.items[0] else {
            panic!("expected type");
        };
        assert_user_meta(pair.meta, span_for(type_source, type_source, source_id));
        assert_user_meta(
            pair.generics[0].meta,
            span_for(type_source, "T: Clone", source_id),
        );
        let TypeDefKind::Record(fields) = &pair.kind else {
            panic!("expected record");
        };
        assert_user_meta(fields[0].meta, span_for(type_source, "left: T", source_id));
        assert_user_meta(
            fields[1].meta,
            span_for(type_source, "right: i32", source_id),
        );

        let enum_source = "type Choice { Some(i32), Empty }";
        let enums = parse_with_source(enum_source, source_id).expect("parse enum type");
        let Item::Type(choice) = &enums.items[0] else {
            panic!("expected enum");
        };
        let TypeDefKind::Enum(variants) = &choice.kind else {
            panic!("expected enum variants");
        };
        assert_user_meta(
            variants[0].meta,
            span_for(enum_source, "Some(i32)", source_id),
        );
        assert_user_meta(variants[1].meta, span_for(enum_source, "Empty", source_id));

        let func_source =
            "func choose<T: Clone>(mut value: T = fallback) -> T where T: Clone + Eq { value }";
        let funcs = parse_with_source(func_source, source_id).expect("parse function");
        let Item::Func(func) = &funcs.items[0] else {
            panic!("expected function");
        };
        assert_user_meta(func.meta, span_for(func_source, func_source, source_id));
        assert_user_meta(
            func.generics[0].meta,
            span_for(func_source, "T: Clone", source_id),
        );
        assert_user_meta(
            func.params[0].meta,
            span_for(func_source, "mut value: T = fallback", source_id),
        );
        assert_user_meta(
            func.where_clause[0].meta,
            span_for(func_source, "T: Clone + Eq", source_id),
        );

        let modified_source = "pub async func work(value: i32) -> i32 { value }";
        let modified =
            parse_with_source(modified_source, source_id).expect("parse modified function");
        let Item::Func(work) = &modified.items[0] else {
            panic!("expected modified function");
        };
        assert_user_meta(
            work.meta,
            span_for(modified_source, modified_source, source_id),
        );
    }

    #[test]
    fn nested_declarations_have_exact_metadata() {
        let source_id = SourceId::new(87);

        let trait_source = "trait Show<T> { func show(value: T) -> string; }";
        let traits = parse_with_source(trait_source, source_id).expect("parse trait");
        let Item::Trait(trait_def) = &traits.items[0] else {
            panic!("expected trait");
        };
        assert_user_meta(
            trait_def.meta,
            span_for(trait_source, trait_source, source_id),
        );
        assert_user_meta(
            trait_def.methods[0].meta,
            span_for(trait_source, "func show(value: T) -> string;", source_id),
        );
        assert_user_meta(
            trait_def.methods[0].params[0].meta,
            span_for(trait_source, "value: T", source_id),
        );

        let extern_source = "extern \"C\" { func add(left: i32, right: i32) -> i32; }";
        let externs = parse_with_source(extern_source, source_id).expect("parse extern block");
        let Item::ExternBlock(block) = &externs.items[0] else {
            panic!("expected extern block");
        };
        assert_user_meta(
            block.meta,
            span_for(extern_source, extern_source, source_id),
        );
        assert_user_meta(
            block.funcs[0].meta,
            span_for(
                extern_source,
                "func add(left: i32, right: i32) -> i32;",
                source_id,
            ),
        );
        assert_user_meta(
            block.funcs[0].params[0].meta,
            span_for(extern_source, "left: i32", source_id),
        );
        assert_user_meta(
            block.funcs[0].params[1].meta,
            span_for(extern_source, "right: i32", source_id),
        );

        let actor_source = "actor Worker { mut count: i32 = 0; }";
        let actors = parse_with_source(actor_source, source_id).expect("parse actor");
        let Item::Actor(actor) = &actors.items[0] else {
            panic!("expected actor");
        };
        assert_user_meta(actor.meta, span_for(actor_source, actor_source, source_id));
        assert_user_meta(
            actor.fields[0].meta,
            span_for(actor_source, "mut count: i32 = 0;", source_id),
        );

        let module_source = "module nested { const LIMIT: i32 = 3; }";
        let modules = parse_with_source(module_source, source_id).expect("parse module");
        let Item::Module(module) = &modules.items[0] else {
            panic!("expected module");
        };
        assert_user_meta(
            module.meta,
            span_for(module_source, module_source, source_id),
        );
        let Item::Const { meta, .. } = &module.items[0] else {
            panic!("expected nested const");
        };
        assert_user_meta(
            *meta,
            span_for(module_source, "const LIMIT: i32 = 3;", source_id),
        );

        let cap_source = "cap Read;";
        let caps = parse_with_source(cap_source, source_id).expect("parse capability");
        let Item::Cap(cap) = &caps.items[0] else {
            panic!("expected capability");
        };
        assert_user_meta(cap.meta, span_for(cap_source, cap_source, source_id));

        let impl_source = "impl Show for Pair { func show(value: Pair) -> string { \"pair\" } }";
        let impls = parse_with_source(impl_source, source_id).expect("parse impl");
        let Item::Impl(impl_def) = &impls.items[0] else {
            panic!("expected impl");
        };
        assert_user_meta(impl_def.meta, span_for(impl_source, impl_source, source_id));
        assert_user_meta(
            impl_def.methods[0].meta,
            span_for(
                impl_source,
                "func show(value: Pair) -> string { \"pair\" }",
                source_id,
            ),
        );
    }

    #[test]
    fn flow_protocol_and_session_children_have_exact_metadata() {
        let source_id = SourceId::new(88);
        let flow_source = "flow Counter @mailbox(depth=8) { state Ready { count: i32 } transition tick(Ready, by: i32) -> Ready { return Ready { count: by } } }";
        let flows = parse_with_source(flow_source, source_id).expect("parse flow");
        let Item::Flow(flow) = &flows.items[0] else {
            panic!("expected flow");
        };
        assert_user_meta(flow.meta, span_for(flow_source, flow_source, source_id));
        assert_user_meta(
            flow.annotations[0].meta,
            span_for(flow_source, "@mailbox(depth=8)", source_id),
        );
        assert_eq!(
            flow.annotations[0],
            FlowAnnotation::synthetic(
                FlowAnnotationKind::MailboxDepth(8),
                AstOrigin::RuntimeSystem("test.semantic_equality"),
            ),
            "FlowAnnotation semantic equality must ignore metadata",
        );
        let ready = flow
            .states
            .iter()
            .find(|state| state.meta.origin == AstOrigin::User)
            .expect("user state");
        assert_user_meta(
            ready.meta,
            span_for(flow_source, "state Ready { count: i32 }", source_id),
        );
        assert_user_meta(
            ready.payload.as_ref().unwrap()[0].meta,
            span_for(flow_source, "count: i32", source_id),
        );
        let tick = flow
            .transitions
            .iter()
            .find(|transition| transition.meta.origin == AstOrigin::User)
            .expect("user transition");
        assert_user_meta(
            tick.meta,
            span_for(
                flow_source,
                "transition tick(Ready, by: i32) -> Ready { return Ready { count: by } }",
                source_id,
            ),
        );
        assert_user_meta(
            tick.params[0].meta,
            span_for(flow_source, "by: i32", source_id),
        );

        let protocol_source =
            "protocol Wire<T> { state Open { data: T }; transition send(Open) -> Open; }";
        let protocols =
            parse_with_source(protocol_source, source_id).expect("parse protocol declaration");
        let Item::Protocol(protocol) = &protocols.items[0] else {
            panic!("expected protocol");
        };
        assert_user_meta(
            protocol.meta,
            span_for(protocol_source, protocol_source, source_id),
        );
        assert_user_meta(
            protocol.states[0].meta,
            span_for(protocol_source, "state Open { data: T };", source_id),
        );
        assert_user_meta(
            protocol.transitions[0].meta,
            span_for(protocol_source, "transition send(Open) -> Open;", source_id),
        );

        let session_source = "session Stream = !i32 . ?string . end;";
        let sessions = parse_with_source(session_source, source_id).expect("parse session");
        let Item::Session(session) = &sessions.items[0] else {
            panic!("expected session");
        };
        assert_user_meta(
            session.meta,
            span_for(session_source, session_source, source_id),
        );
        assert_user_meta(
            session.body.meta().expect("session body metadata"),
            span_for(session_source, "!i32 . ?string . end", source_id),
        );
        assert_eq!(
            session.body,
            SessionType::Send(
                Type::Name("i32".into(), vec![]),
                Box::new(SessionType::Recv(
                    Type::Name("string".into(), vec![]),
                    Box::new(SessionType::End),
                )),
            ),
            "SessionType semantic equality must ignore metadata",
        );
        let SessionType::Send(_, recv) = session.body.unlocated() else {
            panic!("expected send");
        };
        assert_user_meta(
            recv.meta().expect("recv metadata"),
            span_for(session_source, "?string . end", source_id),
        );
        let SessionType::Recv(_, end) = recv.unlocated() else {
            panic!("expected recv");
        };
        assert_user_meta(
            end.meta().expect("end metadata"),
            span_for(session_source, "end", source_id),
        );
    }
}
