// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::*;

impl Parser {
    pub(crate) fn parse_match_arms(&mut self) -> Result<Vec<MatchArm>, ParseError> {
        let mut arms = Vec::new();
        self.skip_newlines();
        // CRITICAL #6 fix: ensure record literals are allowed in match arm
        // bodies. The match scrutinee parser temporarily disables
        // allow_record_literal to disambiguate `match Foo { ... }` (scrutinee
        // Foo, body `{...}`) from `match Foo { x: 1 }` (scrutinee = record).
        // But the save/restore in parse_expr.rs only covers the scrutinee;
        // arm bodies must re-enable it explicitly.
        let saved_allow_record = self.allow_record_literal;
        self.allow_record_literal = true;
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let arm_start = self.pos;
            let pat = self.parse_pattern()?;
            let guard = if self.at(&TokenKind::If) {
                self.advance();
                Some(self.parse_expr(0)?)
            } else {
                None
            };
            self.expect(TokenKind::FatArrow, "`=>`")?;
            let body_start = self.pos;
            let body = if self.at(&TokenKind::LBrace) {
                self.advance();
                let stmts = self.parse_block()?;
                self.parsed_expr_from(body_start, Expr::Block(stmts))
            } else {
                let value = self.parse_expr(0)?;
                let meta = value
                    .meta()
                    .map(|meta| {
                        AstNodeMeta::new(
                            meta.span,
                            AstOrigin::Desugared("parser.match_arm.expression_body"),
                        )
                    })
                    .unwrap_or_else(|| {
                        AstNodeMeta::synthetic(AstOrigin::Desugared(
                            "parser.match_arm.expression_body",
                        ))
                    });
                self.parsed_expr_from(
                    body_start,
                    Expr::Block(vec![Stmt::Expr(value).with_meta(meta)]),
                )
            };
            arms.push(MatchArm {
                meta: self.consumed_meta(arm_start, AstOrigin::User),
                pat,
                guard,
                body,
            });
            self.skip_newlines();
            if self.at(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        self.allow_record_literal = saved_allow_record;
        Ok(arms)
    }

    pub(crate) fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.skip_newlines();
        let start_pos = self.pos;
        let tok = self.peek();
        let kind = match tok.kind.clone() {
            TokenKind::Ident(name) => {
                self.advance();
                if self.at(&TokenKind::LParen) {
                    self.advance();
                    let mut pats = Vec::new();
                    let mut idx = 0usize;
                    if !self.at(&TokenKind::RParen) {
                        loop {
                            let p = self.parse_pattern()?;
                            pats.push((format!("_{}", idx), p));
                            idx += 1;
                            if !self.at(&TokenKind::Comma) {
                                break;
                            }
                            self.advance();
                        }
                    }
                    self.expect(TokenKind::RParen, "`)")?;
                    Ok(PatternKind::Constructor(name, pats))
                } else if self.at(&TokenKind::LBrace) {
                    self.advance();
                    let mut fields = Vec::new();
                    if !self.at(&TokenKind::RBrace) {
                        loop {
                            let field_start = self.pos;
                            let fname = self.expect_ident()?;
                            let pat = if self.at(&TokenKind::Colon) {
                                self.advance();
                                self.parse_pattern()?
                            } else {
                                self.pattern_from(field_start, PatternKind::Variable(fname.clone()))
                            };
                            fields.push((fname, pat));
                            if !self.at(&TokenKind::Comma) {
                                break;
                            }
                            self.advance();
                        }
                    }
                    self.expect(TokenKind::RBrace, "`}`")?;
                    Ok(PatternKind::Constructor(name, fields))
                } else if name == "_" {
                    Ok(PatternKind::Wildcard)
                } else {
                    Ok(PatternKind::Variable(name))
                }
            }
            TokenKind::Int(v) => {
                let (line, col) = (self.peek().line, self.peek().col);
                self.advance();
                // F-H10: support 0x/0b/0o bases in match patterns (same as expr lits).
                let cleaned = v.replace('_', "");
                let val = if cleaned.starts_with("0x") || cleaned.starts_with("0X") {
                    i64::from_str_radix(&cleaned[2..], 16)
                        .map_err(|_| ParseError::new("invalid hex integer", line, col))?
                } else if cleaned.starts_with("0b") || cleaned.starts_with("0B") {
                    i64::from_str_radix(&cleaned[2..], 2)
                        .map_err(|_| ParseError::new("invalid binary integer", line, col))?
                } else if cleaned.starts_with("0o") || cleaned.starts_with("0O") {
                    i64::from_str_radix(&cleaned[2..], 8)
                        .map_err(|_| ParseError::new("invalid octal integer", line, col))?
                } else {
                    cleaned
                        .parse::<i64>()
                        .map_err(|_| ParseError::new("invalid integer", line, col))?
                };
                Ok(PatternKind::Literal(Lit::Int(val)))
            }
            TokenKind::String(v) => {
                self.advance();
                Ok(PatternKind::Literal(Lit::String(v)))
            }
            TokenKind::True => {
                self.advance();
                Ok(PatternKind::Literal(Lit::Bool(true)))
            }
            TokenKind::False => {
                self.advance();
                Ok(PatternKind::Literal(Lit::Bool(false)))
            }
            TokenKind::LParen => {
                self.advance();
                let mut pats = Vec::new();
                if !self.at(&TokenKind::RParen) {
                    loop {
                        pats.push(self.parse_pattern()?);
                        if !self.at(&TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                    }
                }
                self.expect(TokenKind::RParen, "`)")?;
                Ok(PatternKind::Tuple(pats))
            }
            TokenKind::LBracket => {
                self.advance();
                let mut pats = Vec::new();
                let mut rest = None;
                if !self.at(&TokenKind::RBracket) {
                    loop {
                        if self.at(&TokenKind::DotDot) {
                            // [p1, ..rest] — rest pattern
                            self.advance();
                            if !self.at(&TokenKind::RBracket) {
                                rest = Some(Box::new(self.parse_pattern()?));
                            }
                            break;
                        }
                        pats.push(self.parse_pattern()?);
                        if !self.at(&TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                    }
                }
                self.expect(TokenKind::RBracket, "`]`")?;
                if rest.is_some() {
                    Ok(PatternKind::Slice(pats, rest))
                } else {
                    Ok(PatternKind::Array(pats))
                }
            }
            // Soft keywords allowed as binding names in pattern context.
            // Special syntax (delegate view/mutate/consume, do { }, etc.)
            // is handled by statement parsers before patterns are reached.
            TokenKind::Old
            | TokenKind::View
            | TokenKind::Mutate
            | TokenKind::Consume
            | TokenKind::Do
            | TokenKind::Persistent
            | TokenKind::Subflow
            | TokenKind::Session
            | TokenKind::Dual
            | TokenKind::End => {
                let name = tok.kind.source_text().to_string();
                self.advance();
                Ok(PatternKind::Variable(name))
            }
            _ => Err(ParseError::new(
                format!("unexpected token in pattern {}", tok.kind),
                tok.line,
                tok.col,
            )),
        }?;
        Ok(self.pattern_from(start_pos, kind))
    }

    fn pattern_from(&self, start_pos: usize, kind: PatternKind) -> Pattern {
        Pattern::new(self.consumed_meta(start_pos, AstOrigin::User), kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::span::SourceId;

    #[test]
    fn parsed_patterns_keep_source_and_nested_spans() {
        let source_id = SourceId::new(42);
        let tokens = Lexer::new("(x, [1, ..rest])").tokenize().expect("lex");
        let mut parser = Parser::new_with_source(tokens, source_id);
        let pattern = parser.parse_pattern().expect("parse pattern");

        assert_eq!(pattern.meta.span.source_id, source_id);
        assert_eq!(pattern.meta.origin, AstOrigin::User);
        assert_eq!(
            pattern.meta.span,
            Span::new(1, 1, 1, 17).with_source(source_id)
        );
        let PatternKind::Tuple(items) = &pattern.kind else {
            panic!("expected tuple");
        };
        assert_eq!(
            items[0].meta.span,
            Span::new(1, 2, 1, 3).with_source(source_id)
        );
        assert_eq!(items[1].meta.span.source_id, source_id);
    }
}
