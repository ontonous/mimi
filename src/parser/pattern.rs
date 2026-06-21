use super::*;

impl Parser {
    pub(crate) fn parse_match_arms(&mut self) -> Result<Vec<MatchArm>, ParseError> {
        let mut arms = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let pat = self.parse_pattern()?;
            let guard = if self.at(&TokenKind::If) {
                self.advance();
                Some(self.parse_expr(0)?)
            } else {
                None
            };
            self.expect(TokenKind::FatArrow, "`=>`")?;
            let body = if self.at(&TokenKind::LBrace) {
                self.advance();
                let stmts = self.parse_block()?;
                if let Some(last) = stmts.last() {
                    if let Stmt::Expr(e) = last {
                        e.clone()
                    } else {
                        return Err(ParseError::new("match arm block must end with an expression", self.peek().line, self.peek().col));
                    }
                } else {
                    return Err(ParseError::new("match arm block must not be empty", self.peek().line, self.peek().col));
                }
            } else {
                self.parse_expr(0)?
            };
            arms.push(MatchArm { pat, guard, body });
            self.skip_newlines();
            if self.at(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        Ok(arms)
    }

    pub(crate) fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        self.skip_newlines();
        let tok = self.peek();
        match tok.kind.clone() {
            TokenKind::Ident(name) => {
                self.advance();
                if self.at(&TokenKind::LParen) {
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
                    Ok(Pattern::Constructor(name, pats))
                } else if self.at(&TokenKind::LBrace) {
                    self.advance();
                    let mut fields = Vec::new();
                    if !self.at(&TokenKind::RBrace) {
                        loop {
                            let fname = self.expect_ident()?;
                            let pat = if self.at(&TokenKind::Colon) {
                                self.advance();
                                self.parse_pattern()?
                            } else {
                                Pattern::Variable(fname.clone())
                            };
                            fields.push((fname, pat));
                            if !self.at(&TokenKind::Comma) {
                                break;
                            }
                            self.advance();
                        }
                    }
                    self.expect(TokenKind::RBrace, "`}`")?;
                    Ok(Pattern::Constructor(name, fields.into_iter().map(|(_, p)| p).collect()))
                } else if name == "_" {
                    Ok(Pattern::Wildcard)
                } else {
                    Ok(Pattern::Variable(name))
                }
            }
            TokenKind::Int(v) => {
                let (line, col) = (self.peek().line, self.peek().col);
                self.advance();
                let val = v.replace('_', "").parse::<i64>().map_err(|_| ParseError::new("invalid integer", line, col))?;
                Ok(Pattern::Literal(Lit::Int(val)))
            }
            TokenKind::String(v) => {
                self.advance();
                Ok(Pattern::Literal(Lit::String(v)))
            }
            TokenKind::True => {
                self.advance();
                Ok(Pattern::Literal(Lit::Bool(true)))
            }
            TokenKind::False => {
                self.advance();
                Ok(Pattern::Literal(Lit::Bool(false)))
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
                Ok(Pattern::Tuple(pats))
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
                    Ok(Pattern::Slice(pats, rest))
                } else {
                    Ok(Pattern::Array(pats))
                }
            }
            _ => Err(ParseError::new(format!("unexpected token in pattern {}", tok.kind), tok.line, tok.col)),
        }
    }
}
