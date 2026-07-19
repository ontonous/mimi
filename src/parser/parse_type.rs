// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::*;

impl Parser {
    pub(crate) fn parse_type(&mut self) -> Result<Type, ParseError> {
        self.check_depth()?;
        let start_pos = self.pos;
        self.inc_depth();
        let result = self.parse_type_optional(false);
        self.dec_depth();
        result.map(|ty| ty.with_meta(self.consumed_meta(start_pos, AstOrigin::User)))
    }

    fn parse_type_optional(&mut self, allow_func: bool) -> Result<Type, ParseError> {
        let start_pos = self.pos;
        let mut ty = self.parse_type_atom()?;
        loop {
            if self.at(&TokenKind::Lt) {
                self.advance();
                self.skip_newlines();
                let mut args = Vec::new();
                if !self.at(&TokenKind::Gt) {
                    loop {
                        args.push(self.parse_type()?);
                        if !self.at(&TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                        self.skip_newlines();
                    }
                }
                self.skip_newlines();
                self.expect_gt("`>`")?;
                ty = match ty.into_unlocated() {
                    Type::Name(name, _) => Type::Name(name, args),
                    _ => {
                        let tok = self.peek();
                        return Err(ParseError::new(
                            "type arguments only allowed on named types",
                            tok.line,
                            tok.col,
                        ));
                    }
                };
            } else if self.at(&TokenKind::Question) {
                // The wrapped type is also a first-class AST node. Capture its
                // span before consuming `?`, including a preceding generic
                // application rebuilt above.
                ty = ty.with_meta(self.consumed_meta(start_pos, AstOrigin::User));
                self.advance();
                ty = Type::Option(Box::new(ty));
            } else {
                break;
            }
        }
        if allow_func && self.at(&TokenKind::Arrow) {
            ty = ty.with_meta(self.consumed_meta(start_pos, AstOrigin::User));
            self.advance();
            let ret = self.parse_type()?;
            ty = Type::Func(vec![ty], Box::new(ret));
        }
        Ok(ty)
    }

    fn parse_type_atom(&mut self) -> Result<Type, ParseError> {
        let start_pos = self.pos;
        let tok = self.peek();
        let result = match tok.kind {
            TokenKind::Ident(ref name) if name == "CBuffer" => {
                self.advance();
                self.expect(TokenKind::Lt, "`<`")?;
                let inner = self.parse_type()?;
                self.expect_gt("`>`")?;
                Ok(Type::CBuffer(Box::new(inner)))
            }
            TokenKind::Ident(ref name) if name == "_" => {
                self.advance();
                Ok(Type::Infer)
            }
            TokenKind::Ident(ref name) => {
                let name = name.clone();
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
                // Check for &'a lifetime annotation
                let lifetime = if self.at(&TokenKind::Tick) {
                    self.advance();
                    let name = self.expect_ident()?;
                    Some(name)
                } else {
                    None
                };
                let mut_ = self.at(&TokenKind::Mut);
                if mut_ {
                    self.advance();
                }
                // Check for &[T] slice type
                if self.at(&TokenKind::LBracket) {
                    self.advance();
                    let elem_type = self.parse_type()?;
                    self.expect(TokenKind::RBracket, "`]`")?;
                    Ok(Type::Slice(Box::new(elem_type)))
                } else {
                    let inner = self.parse_type()?;
                    if mut_ {
                        Ok(Type::RefMut(lifetime, Box::new(inner)))
                    } else {
                        Ok(Type::Ref(lifetime, Box::new(inner)))
                    }
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
                // Empty tuple `()` is the unit type — use the canonical Name form
                // so it unifies with other unit representations (default return type,
                // Lit::Unit literal, etc.)
                if elems.is_empty() {
                    Ok(Type::Name("unit".into(), vec![]))
                } else {
                    Ok(Type::Tuple(elems))
                }
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
            TokenKind::WeakLocal => {
                self.advance();
                let inner = self.parse_type()?;
                Ok(Type::WeakLocal(Box::new(inner)))
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
                    _ => {
                        return Err(ParseError::new(
                            "expected capability name after `cap`",
                            name_tok.line,
                            name_tok.col,
                        ))
                    }
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
                    Type::Name("unit".to_string(), vec![]).synthetic_with_origin(
                        AstOrigin::Desugared("parser.function_type.unit_return"),
                    )
                };
                Ok(Type::Func(param_types, Box::new(ret_type)))
            }
            TokenKind::Extern => {
                // extern "C" fn(ArgType, ...) -> RetType
                self.advance();
                self.expect(TokenKind::String("C".to_string()), "\"C\"")?;
                if !self.at(&TokenKind::Fn) && !self.at(&TokenKind::Func) {
                    let tok = self.peek();
                    return Err(ParseError::new(
                        format!(
                            "expected `fn` or `func` after `extern \"C\"`, found {}",
                            tok.kind
                        ),
                        tok.line,
                        tok.col,
                    ));
                }
                self.advance();
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
                    Type::Name("unit".to_string(), vec![]).synthetic_with_origin(
                        AstOrigin::Desugared("parser.extern_function_type.unit_return"),
                    )
                };
                Ok(Type::ExternFunc(param_types, Box::new(ret_type)))
            }
            TokenKind::Impl => {
                self.advance();
                let mut traits = Vec::new();
                let trait_tok = self.peek();
                let trait_name = match &trait_tok.kind {
                    TokenKind::Ident(n) => n.clone(),
                    _ => {
                        return Err(ParseError::new(
                            "expected trait name after `impl`",
                            trait_tok.line,
                            trait_tok.col,
                        ))
                    }
                };
                self.advance();
                traits.push(trait_name);
                // Parse additional traits: impl Trait1 + Trait2
                while self.at(&TokenKind::Plus) {
                    self.advance();
                    let next_tok = self.peek();
                    let next_name = match &next_tok.kind {
                        TokenKind::Ident(n) => n.clone(),
                        _ => {
                            return Err(ParseError::new(
                                "expected trait name after `+`",
                                next_tok.line,
                                next_tok.col,
                            ))
                        }
                    };
                    self.advance();
                    traits.push(next_name);
                }
                Ok(Type::ImplTrait(traits))
            }
            TokenKind::Dyn => {
                self.advance();
                let mut traits = Vec::new();
                let first_tok = self.peek();
                let first_name = match &first_tok.kind {
                    TokenKind::Ident(n) => n.clone(),
                    _ => {
                        return Err(ParseError::new(
                            "expected trait name after `dyn`",
                            first_tok.line,
                            first_tok.col,
                        ))
                    }
                };
                self.advance();
                traits.push(first_name);
                // Parse additional traits: dyn Trait1 + Trait2
                while self.at(&TokenKind::Plus) {
                    self.advance();
                    let next_tok = self.peek();
                    let next_name = match &next_tok.kind {
                        TokenKind::Ident(n) => n.clone(),
                        _ => {
                            return Err(ParseError::new(
                                "expected trait name after `+`",
                                next_tok.line,
                                next_tok.col,
                            ))
                        }
                    };
                    self.advance();
                    traits.push(next_name);
                }
                Ok(Type::DynTrait(traits))
            }
            TokenKind::LBracket => {
                self.advance();
                let elem_type = self.parse_type()?;
                if self.at(&TokenKind::Semi) {
                    self.advance();
                    // [T; n] — fixed-size array
                    let size_tok = self.peek();
                    let size = match &size_tok.kind {
                        TokenKind::Int(s) => s.parse::<usize>().map_err(|_| {
                            ParseError::new(
                                "array size must be a non-negative integer",
                                size_tok.line,
                                size_tok.col,
                            )
                        })?,
                        _ => {
                            return Err(ParseError::new(
                                "expected integer array size after `;`",
                                size_tok.line,
                                size_tok.col,
                            ))
                        }
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
        };
        result.map(|ty| ty.with_meta(self.consumed_meta(start_pos, AstOrigin::User)))
    }

    pub(crate) fn parse_type_def(
        &mut self,
        derives: Vec<String>,
        attributes: Vec<crate::ast::TypeAttribute>,
    ) -> Result<TypeDef, ParseError> {
        let start_pos = self.pos;
        self.expect_keyword(TokenKind::Type)?;
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
            return Ok(TypeDef {
                meta: self.consumed_meta(start_pos, AstOrigin::User),
                name,
                pub_: false,
                kind,
                generics,
                derives,
                attributes,
            });
        }
        if self.at(&TokenKind::Eq) {
            self.advance();
            // Check for `= union { ... }` syntax (must match both variant AND value)
            if matches!(self.peek_kind(), TokenKind::Ident(name) if name == "union") {
                self.advance();
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for union definition")?;
                self.skip_newlines();
                let fields = self.parse_record_fields()?;
                self.skip_newlines();
                self.expect(TokenKind::RBrace, "`}`")?;
                return Ok(TypeDef {
                    meta: self.consumed_meta(start_pos, AstOrigin::User),
                    name,
                    pub_: false,
                    kind: TypeDefKind::Union(fields),
                    generics,
                    derives,
                    attributes,
                });
            }
            let ty = self.parse_type()?;
            self.match_semi();
            return Ok(TypeDef {
                meta: self.consumed_meta(start_pos, AstOrigin::User),
                name,
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
        Ok(TypeDef {
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
            pub_: false,
            kind,
            generics,
            derives,
            attributes,
        })
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
        while !self.at(&TokenKind::RBrace)
            && !self.at(&TokenKind::Dedent)
            && !self.at(&TokenKind::Eof)
        {
            let field_start = self.pos;
            let fname = self.expect_ident()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let fty = self.parse_type()?;
            fields.push(Field {
                meta: self.consumed_meta(field_start, AstOrigin::User),
                name: fname,
                ty: fty,
            });
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
            if self.at(&TokenKind::RBrace)
                || self.at(&TokenKind::Dedent)
                || self.at(&TokenKind::Eof)
            {
                break;
            }
            let variant_start = self.pos;
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
            variants.push(Variant {
                meta: self.consumed_meta(variant_start, AstOrigin::User),
                name: vname,
                payload,
            });
            if matches!(
                self.peek_kind(),
                TokenKind::BitOr | TokenKind::Comma | TokenKind::Newline
            ) {
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
        let start_pos = self.pos;
        self.expect_keyword(TokenKind::Newtype)?;
        let name = self.expect_ident()?;
        let generics = self.parse_generic_params()?;
        self.expect(TokenKind::Eq, "`=`")?;
        let ty = self.parse_type()?;
        self.match_semi();
        Ok(TypeDef {
            meta: self.consumed_meta(start_pos, AstOrigin::User),
            name,
            pub_: false,
            kind: TypeDefKind::Newtype(ty),
            generics,
            derives: Vec::new(),
            attributes: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::token::TokenKind;
    use crate::lexer::Lexer;
    use crate::span::{SourceId, Span};

    fn parse_source_type(source: &str, source_id: SourceId) -> Type {
        let tokens = Lexer::new(source).tokenize().expect("lex type");
        let mut parser = Parser::new_with_source(tokens, source_id);
        let ty = parser.parse_type().expect("parse type");
        assert!(parser.at(&TokenKind::Eof), "type left trailing tokens");
        ty
    }

    fn span_for(source: &str, fragment: &str, source_id: SourceId) -> Span {
        let offset = source.find(fragment).expect("fragment must occur");
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

    fn assert_user_span(ty: &Type, expected: Span) {
        let meta = ty.meta().expect("parsed Type must have metadata");
        assert_eq!(meta.origin, AstOrigin::User);
        assert_eq!(meta.span, expected);
    }

    #[test]
    fn nested_composite_types_have_exact_source_aware_half_open_spans() {
        let source = "Result<List<i32>,\n  Option<&'a mut string>>?";
        let source_id = SourceId::new(91);
        let ty = parse_source_type(source, source_id);
        assert_user_span(&ty, span_for(source, source, source_id));

        let Type::Option(result) = ty.unlocated() else {
            panic!("expected postfix option type");
        };
        let result_source = source.strip_suffix('?').expect("option suffix");
        assert_user_span(result, span_for(source, result_source, source_id));

        let Type::Name(result_name, result_args) = result.unlocated() else {
            panic!("expected Result application");
        };
        assert_eq!(result_name, "Result");
        assert_eq!(result_args.len(), 2);

        let list = &result_args[0];
        assert_user_span(list, span_for(source, "List<i32>", source_id));
        let Type::Name(list_name, list_args) = list.unlocated() else {
            panic!("expected List application");
        };
        assert_eq!(list_name, "List");
        assert_user_span(&list_args[0], span_for(source, "i32", source_id));

        let option = &result_args[1];
        assert_user_span(
            option,
            span_for(source, "Option<&'a mut string>", source_id),
        );
        let Type::Name(option_name, option_args) = option.unlocated() else {
            panic!("expected Option application");
        };
        assert_eq!(option_name, "Option");
        let reference = &option_args[0];
        assert_user_span(reference, span_for(source, "&'a mut string", source_id));
        let Type::RefMut(lifetime, inner) = reference.unlocated() else {
            panic!("expected mutable reference");
        };
        assert_eq!(lifetime.as_deref(), Some("a"));
        assert_user_span(inner, span_for(source, "string", source_id));
    }

    #[test]
    fn type_semantic_equality_ignores_outer_and_nested_metadata() {
        let first_source = SourceId::new(92);
        let second_source = SourceId::new(93);
        let first = Type::Name(
            "List".into(),
            vec![Type::Name("i32".into(), vec![]).with_meta(AstNodeMeta::new(
                Span::new(1, 6, 1, 9).with_source(first_source),
                AstOrigin::User,
            ))],
        )
        .with_meta(AstNodeMeta::new(
            Span::new(1, 1, 1, 10).with_source(first_source),
            AstOrigin::User,
        ));
        let second = Type::Name(
            "List".into(),
            vec![Type::Name("i32".into(), vec![]).with_meta(AstNodeMeta::new(
                Span::new(8, 20, 8, 23).with_source(second_source),
                AstOrigin::User,
            ))],
        )
        .with_meta(AstNodeMeta::new(
            Span::new(8, 15, 8, 24).with_source(second_source),
            AstOrigin::User,
        ));

        assert_eq!(first, second);
    }
}
