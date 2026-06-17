use super::*;

impl Parser {
    pub(crate) fn parse_type(&mut self) -> Result<Type, ParseError> {
        self.parse_type_optional(false)
    }

    fn parse_type_optional(&mut self, allow_func: bool) -> Result<Type, ParseError> {
        let mut ty = self.parse_type_atom()?;
        loop {
            if self.at(&TokenKind::Lt) {
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
                self.expect(TokenKind::Gt, "`>`")?;
                if let Type::Name(name, _) = ty {
                    ty = Type::Name(name, args);
                } else {
                    let tok = self.peek();
                    return Err(ParseError::new(
                        "type arguments only allowed on named types",
                        tok.line,
                        tok.col,
                    ));
                }
            } else if self.at(&TokenKind::Question) {
                self.advance();
                ty = Type::Option(Box::new(ty));
            } else {
                break;
            }
        }
        if allow_func && self.at(&TokenKind::Arrow) {
            self.advance();
            let ret = self.parse_type()?;
            ty = Type::Func(vec![ty], Box::new(ret));
        }
        Ok(ty)
    }

    fn parse_type_atom(&mut self) -> Result<Type, ParseError> {
        let tok = self.peek();
        match tok.kind {
            TokenKind::Ident(ref name) => {
                let name = name.clone();
                self.advance();
                Ok(Type::Name(name, Vec::new()))
            }
            TokenKind::I32 | TokenKind::I64 | TokenKind::F64 | TokenKind::Bool | TokenKind::StringKw => {
                let name = tok.kind.to_string();
                self.advance();
                Ok(Type::Name(name, Vec::new()))
            }
            TokenKind::Alloc => {
                self.advance();
                Ok(Type::Allocator)
            }
            TokenKind::Nothing => {
                self.advance();
                Ok(Type::Nothing)
            }
            TokenKind::BitAnd => {
                self.advance();
                let mut_ = self.at(&TokenKind::Mut);
                if mut_ {
                    self.advance();
                }
                // Check for &[T] slice type
                if self.at(&TokenKind::LBracket) {
                    self.advance();
                    let elem_type = self.parse_type()?;
                    self.expect(TokenKind::RBracket, "`]`")?;
                    return Ok(Type::Slice(Box::new(elem_type)));
                }
                let inner = self.parse_type()?;
                if mut_ {
                    Ok(Type::RefMut(Box::new(inner)))
                } else {
                    Ok(Type::Ref(Box::new(inner)))
                }
            }
            TokenKind::LParen => {
                self.advance();
                let mut elems = Vec::new();
                if !self.at(&TokenKind::RParen) {
                    loop {
                        elems.push(self.parse_type()?);
                        if !self.at(&TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                    }
                }
                self.expect(TokenKind::RParen, "`)`")?;
                Ok(Type::Tuple(elems))
            }
            TokenKind::Shared => {
                self.advance();
                let inner = self.parse_type()?;
                Ok(Type::Shared(Box::new(inner)))
            }
            TokenKind::LocalShared => {
                self.advance();
                let inner = self.parse_type()?;
                Ok(Type::LocalShared(Box::new(inner)))
            }
            TokenKind::Weak => {
                self.advance();
                let inner = self.parse_type()?;
                Ok(Type::Weak(Box::new(inner)))
            }
            TokenKind::CShared => {
                self.advance();
                let inner = self.parse_type()?;
                Ok(Type::CShared(Box::new(inner)))
            }
            TokenKind::CBorrow => {
                self.advance();
                let inner = self.parse_type()?;
                Ok(Type::CBorrow(Box::new(inner)))
            }
            TokenKind::CBorrowMut => {
                self.advance();
                let inner = self.parse_type()?;
                Ok(Type::CBorrowMut(Box::new(inner)))
            }
            TokenKind::RawString => {
                self.advance();
                Ok(Type::RawString)
            }
            TokenKind::Star => {
                self.advance();
                let mut_ = self.at(&TokenKind::Mut);
                if mut_ {
                    self.advance();
                }
                let inner = self.parse_type()?;
                if mut_ {
                    Ok(Type::RawPtrMut(Box::new(inner)))
                } else {
                    Ok(Type::RawPtr(Box::new(inner)))
                }
            }
            TokenKind::Cap => {
                self.advance();
                let name_tok = self.peek();
                let name = match &name_tok.kind {
                    TokenKind::Ident(n) => n.clone(),
                    _ => return Err(ParseError::new(
                        "expected capability name after `cap`",
                        name_tok.line,
                        name_tok.col,
                    )),
                };
                self.advance();
                Ok(Type::Cap(name))
            }
            TokenKind::Func => {
                self.advance();
                // func(ArgType, ...) -> RetType
                self.expect(TokenKind::LParen, "`(` for function type parameters")?;
                let mut param_types = Vec::new();
                if !self.at(&TokenKind::RParen) {
                    loop {
                        param_types.push(self.parse_type()?);
                        if !self.at(&TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                    }
                }
                self.expect(TokenKind::RParen, "`)`")?;
                let ret_type = if self.at(&TokenKind::Arrow) {
                    self.advance();
                    self.parse_type()?
                } else {
                    Type::Name("unit".to_string(), vec![])
                };
                Ok(Type::Func(param_types, Box::new(ret_type)))
            }
            TokenKind::Impl => {
                self.advance();
                let mut traits = Vec::new();
                let trait_tok = self.peek();
                let trait_name = match &trait_tok.kind {
                    TokenKind::Ident(n) => n.clone(),
                    _ => return Err(ParseError::new(
                        "expected trait name after `impl`",
                        trait_tok.line,
                        trait_tok.col,
                    )),
                };
                self.advance();
                traits.push(trait_name);
                // Parse additional traits: impl Trait1 + Trait2
                while self.at(&TokenKind::Plus) {
                    self.advance();
                    let next_tok = self.peek();
                    let next_name = match &next_tok.kind {
                        TokenKind::Ident(n) => n.clone(),
                        _ => return Err(ParseError::new(
                            "expected trait name after `+`",
                            next_tok.line,
                            next_tok.col,
                        )),
                    };
                    self.advance();
                    traits.push(next_name);
                }
                Ok(Type::ImplTrait(traits))
            }
            TokenKind::LBracket => {
                self.advance();
                let elem_type = self.parse_type()?;
                if self.at(&TokenKind::Semi) {
                    self.advance();
                    // [T; n] — fixed-size array
                    let size_tok = self.peek();
                    let size = match &size_tok.kind {
                        TokenKind::Int(s) => s.parse::<usize>().map_err(|_| ParseError::new(
                            "array size must be a non-negative integer",
                            size_tok.line,
                            size_tok.col,
                        ))?,
                        _ => return Err(ParseError::new(
                            "expected integer array size after `;`",
                            size_tok.line,
                            size_tok.col,
                        )),
                    };
                    self.advance();
                    self.expect(TokenKind::RBracket, "`]`")?;
                    Ok(Type::Array(Box::new(elem_type), size))
                } else {
                    Err(ParseError::new(
                        "expected `;` for array type `[T; n]`",
                        self.peek().line,
                        self.peek().col,
                    ))
                }
            }
            _ => Err(ParseError::new(
                format!("expected type, found {}", tok.kind),
                tok.line,
                tok.col,
            )),
        }
    }

    pub(crate) fn parse_type_def(&mut self, derives: Vec<String>, attributes: Vec<crate::ast::TypeAttribute>) -> Result<TypeDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Type)?;
        let name = self.expect_ident()?;
        let generics = self.parse_generic_params()?;
        self.skip_newlines();
        if self.is_sketch() {
            self.expect(TokenKind::Colon, "`:`")?;
            self.skip_newlines();
            let kind = if self.at(&TokenKind::Indent) {
                self.advance();
                self.skip_newlines();
                let is_record = self.peek_kind().clone();
                let is_record = if let TokenKind::Ident(_) = is_record {
                    let mut pos = self.pos + 1;
                    while pos < self.tokens.len() {
                        match &self.tokens[pos].kind {
                            TokenKind::Newline | TokenKind::Ident(_) => {}
                            TokenKind::Colon => break,
                            _ => break,
                        }
                        pos += 1;
                    }
                    matches!(&self.tokens[pos].kind, TokenKind::Colon)
                } else {
                    false
                };
                if is_record {
                    let fields = self.parse_record_fields()?;
                    self.expect(TokenKind::Dedent, "dedent")?;
                    TypeDefKind::Record(fields)
                } else {
                    let variants = self.parse_enum_variants()?;
                    self.expect(TokenKind::Dedent, "dedent")?;
                    TypeDefKind::Enum(variants)
                }
            } else {
                let variants = self.parse_enum_variants()?;
                TypeDefKind::Enum(variants)
            };
            return Ok(TypeDef { name, commitment, pub_: false, kind, generics, derives, attributes });
        }
        if self.at(&TokenKind::Eq) {
            self.advance();
            let ty = self.parse_type()?;
            self.match_semi();
            return Ok(TypeDef {
                name,
                commitment,
                pub_: false,
                kind: TypeDefKind::Alias(ty),
                generics,
                derives,
                attributes,
            });
        }
        self.expect(TokenKind::LBrace, "`{`")?;
        self.skip_newlines();
        let kind = if self.lookahead_is_record() {
            let fields = self.parse_record_fields()?;
            TypeDefKind::Record(fields)
        } else {
            let variants = self.parse_enum_variants()?;
            TypeDefKind::Enum(variants)
        };
        self.skip_newlines();
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(TypeDef { name, commitment, pub_: false, kind, generics, derives, attributes })
    }

    fn lookahead_is_record(&self) -> bool {
        if let TokenKind::Ident(_) = self.peek_kind() {
            let mut pos = self.pos + 1;
            while pos < self.tokens.len() {
                match &self.tokens[pos].kind {
                    TokenKind::Colon => return true,
                    TokenKind::Newline | TokenKind::RBrace | TokenKind::Eof => return false,
                    _ => {}
                }
                pos += 1;
            }
        }
        false
    }

    fn parse_record_fields(&mut self) -> Result<Vec<Field>, ParseError> {
        let mut fields = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Dedent) && !self.at(&TokenKind::Eof) {
            let fname = self.expect_ident()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let fty = self.parse_type()?;
            fields.push(Field { name: fname, ty: fty });
            if matches!(self.peek_kind(), TokenKind::Comma | TokenKind::Newline) {
                self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }
        Ok(fields)
    }

    fn parse_enum_variants(&mut self) -> Result<Vec<Variant>, ParseError> {
        let mut variants = Vec::new();
        self.skip_newlines();
        loop {
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Dedent) || self.at(&TokenKind::Eof) {
                break;
            }
            let vname = self.expect_ident()?;
            let payload = if self.at(&TokenKind::LParen) {
                self.advance();
                let mut types = Vec::new();
                if !self.at(&TokenKind::RParen) {
                    loop {
                        types.push(self.parse_type()?);
                        if !self.at(&TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                    }
                }
                self.expect(TokenKind::RParen, "`)`")?;
                Some(VariantPayload::Tuple(types))
            } else if self.at(&TokenKind::LBrace) {
                self.advance();
                let fields = self.parse_record_fields()?;
                self.expect(TokenKind::RBrace, "`}`")?;
                Some(VariantPayload::Record(fields))
            } else {
                None
            };
            variants.push(Variant { name: vname, payload });
            if matches!(self.peek_kind(), TokenKind::BitOr | TokenKind::Comma | TokenKind::Newline) {
                self.advance();
                self.skip_newlines();
            } else if matches!(self.peek_kind(), TokenKind::Ident(_)) {
                self.skip_newlines();
            } else {
                break;
            }
        }
        Ok(variants)
    }

    pub(crate) fn parse_newtype(&mut self) -> Result<TypeDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Newtype)?;
        let name = self.expect_ident()?;
        let generics = self.parse_generic_params()?;
        self.expect(TokenKind::Eq, "`=`")?;
        let ty = self.parse_type()?;
        self.match_semi();
        Ok(TypeDef {
            name,
            commitment,
            pub_: false,
            kind: TypeDefKind::Newtype(ty),
            generics,
            derives: Vec::new(),
            attributes: Vec::new(),
        })
    }
}