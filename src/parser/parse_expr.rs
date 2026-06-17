use super::*;

impl Parser {
    pub(crate) fn parse_expr(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let (op, prec, right_assoc) = match self.peek_kind() {
                TokenKind::OrOr => (BinOp::Or, 1, false),
                TokenKind::AndAnd => (BinOp::And, 2, false),
                TokenKind::EqEq => (BinOp::EqCmp, 3, false),
                TokenKind::Ne => (BinOp::NeCmp, 3, false),
                TokenKind::Lt => (BinOp::Lt, 4, false),
                TokenKind::Gt => (BinOp::Gt, 4, false),
                TokenKind::Le => (BinOp::Le, 4, false),
                TokenKind::Ge => (BinOp::Ge, 4, false),
                TokenKind::BitOr => (BinOp::BitOr, 5, false),
                TokenKind::BitXor => (BinOp::BitXor, 6, false),
                TokenKind::BitAnd => (BinOp::BitAnd, 7, false),
                TokenKind::Shl => (BinOp::Shl, 8, false),
                TokenKind::Shr => (BinOp::Shr, 8, false),
                TokenKind::Plus => (BinOp::Add, 9, false),
                TokenKind::Minus => (BinOp::Sub, 9, false),
                TokenKind::Star => (BinOp::Mul, 10, false),
                TokenKind::Slash => (BinOp::Div, 10, false),
                TokenKind::Percent => (BinOp::Mod, 10, false),
                TokenKind::Pow => (BinOp::Pow, 11, true),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.advance();
            let next_min = if right_assoc { prec } else { prec + 1 };
            let rhs = self.parse_expr(next_min)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek_kind() {
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Minus => {
                self.advance();
                Ok(Expr::Unary(UnOp::Neg, Box::new(self.parse_unary()?)))
            }
            TokenKind::Bang | TokenKind::NotOp | TokenKind::Not => {
                self.advance();
                Ok(Expr::Unary(UnOp::Not, Box::new(self.parse_unary()?)))
            }
            TokenKind::BitAnd => {
                self.advance();
                if self.at(&TokenKind::Mut) {
                    self.advance();
                    Ok(Expr::Unary(UnOp::RefMut, Box::new(self.parse_unary()?)))
                } else {
                    Ok(Expr::Unary(UnOp::Ref, Box::new(self.parse_unary()?)))
                }
            }
            TokenKind::Star => {
                self.advance();
                Ok(Expr::Unary(UnOp::Deref, Box::new(self.parse_unary()?)))
            }
            TokenKind::Old => {
                self.advance();
                self.expect(TokenKind::LParen, "`(`")?;
                let expr = self.parse_expr(0)?;
                self.expect(TokenKind::RParen, "`)`")?;
                Ok(Expr::Old(Box::new(expr)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_if_expr(&mut self) -> Result<Expr, ParseError> {
        self.advance(); // consume `if`
        let cond = self.parse_expr(0)?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{` for if expression")?;
        let then_ = self.parse_block()?;
        let else_ = if self.at(&TokenKind::Else) {
            self.advance();
            self.skip_newlines();
            if self.at(&TokenKind::LBrace) {
                self.advance();
                let else_body = self.parse_block()?;
                Some(else_body)
            } else if self.at(&TokenKind::If) {
                let elif = self.parse_if_expr()?;
                Some(vec![Stmt::Expr(elif)])
            } else {
                return Err(ParseError::new("`{` or `if` expected after `else`", self.peek().line, self.peek().col));
            }
        } else {
            None
        };
        Ok(Expr::If {
            cond: Box::new(cond),
            then_,
            else_,
        })
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let kind = self.peek().kind.clone();
        let mut expr = match kind {
            TokenKind::Int(s) => {
                let (line, col) = (self.peek().line, self.peek().col);
                self.advance();
                let v = s
                    .replace('_', "")
                    .parse::<i64>()
                    .map_err(|_| ParseError::new("invalid integer", line, col))?;
                Expr::Literal(Lit::Int(v))
            }
            TokenKind::Float(s) => {
                let (line, col) = (self.peek().line, self.peek().col);
                self.advance();
                let v = s
                    .replace('_', "")
                    .parse::<f64>()
                    .map_err(|_| ParseError::new("invalid float", line, col))?;
                Expr::Literal(Lit::Float(v))
            }
            TokenKind::String(s) => {
                self.advance();
                Expr::Literal(Lit::String(s))
            }
            TokenKind::FString(raw) => {
                let raw = raw.clone();
                self.advance();
                let parts = self.parse_fstring_parts(&raw)?;
                Expr::Literal(Lit::FString(parts))
            }
            TokenKind::True => {
                self.advance();
                Expr::Literal(Lit::Bool(true))
            }
            TokenKind::False => {
                self.advance();
                Expr::Literal(Lit::Bool(false))
            }
            TokenKind::Unit => {
                self.advance();
                Expr::Literal(Lit::Unit)
            }
            TokenKind::Alloc => {
                self.advance();
                let mut e = Expr::Ident("alloc".to_string());
                if self.at(&TokenKind::LParen) {
                    self.advance();
                    let args = self.parse_args()?;
                    self.expect(TokenKind::RParen, "`)`")?;
                    e = Expr::Call(Box::new(e), args);
                }
                loop {
                    if self.at(&TokenKind::LParen) {
                        self.advance();
                        let args = self.parse_args()?;
                        self.expect(TokenKind::RParen, "`)`")?;
                        e = Expr::Call(Box::new(e), args);
                    } else if self.at(&TokenKind::Dot) {
                        self.advance();
                        let field = self.expect_ident()?;
                        e = Expr::Field(Box::new(e), field);
                    } else if self.at(&TokenKind::LBracket) {
                        self.advance();
                        // Check for slice syntax: arr[start..end], arr[..end], arr[start..]
                        if self.at(&TokenKind::DotDot) {
                            // arr[..end]
                            self.advance();
                            let end = if self.at(&TokenKind::RBracket) {
                                None
                            } else {
                                Some(Box::new(self.parse_expr(0)?))
                            };
                            self.expect(TokenKind::RBracket, "`]`")?;
                            e = Expr::SliceExpr {
                                target: Box::new(e),
                                start: None,
                                end,
                            };
                        } else {
                            let first = self.parse_expr(0)?;
                            if self.at(&TokenKind::DotDot) {
                                // arr[start..end] or arr[start..]
                                self.advance();
                                let end = if self.at(&TokenKind::RBracket) {
                                    None
                                } else {
                                    Some(Box::new(self.parse_expr(0)?))
                                };
                                self.expect(TokenKind::RBracket, "`]`")?;
                                e = Expr::SliceExpr {
                                    target: Box::new(e),
                                    start: Some(Box::new(first)),
                                    end,
                                };
                            } else {
                                // arr[i] — regular index
                                self.expect(TokenKind::RBracket, "`]`")?;
                                e = Expr::Index(Box::new(e), Box::new(first));
                            }
                        }
                    } else {
                        break;
                    }
                }
                e
            }
            TokenKind::Ident(name) => {
                self.advance();
                if name == "type_name" && self.at(&TokenKind::LParen) {
                    self.advance();
                    let inner = self.parse_expr(0)?;
                    self.expect(TokenKind::RParen, "`)` for type_name")?;
                    return Ok(Expr::TypeOf(Box::new(inner)));
                }
                if name == "type_info" && self.at(&TokenKind::LParen) {
                    self.advance();
                    let ty = self.parse_type()?;
                    self.expect(TokenKind::RParen, "`)` for type_info")?;
                    return Ok(Expr::TypeInfo(ty));
                }
                if self.at(&TokenKind::ColonColon) {
                    self.advance();
                    if self.at(&TokenKind::Lt) {
                        self.advance();
                        let mut type_args = Vec::new();
                        if !self.at(&TokenKind::Gt) {
                            loop {
                                type_args.push(self.parse_type()?);
                                if !self.at(&TokenKind::Comma) {
                                    break;
                                }
                                self.advance();
                            }
                        }
                        self.expect(TokenKind::Gt, "`>`")?;
                        self.expect(TokenKind::LParen, "`(`")?;
                        let args = self.parse_args()?;
                        self.expect(TokenKind::RParen, "`)`")?;
                        Expr::Turbofish(name, type_args, args)
                    } else {
                        let field = self.expect_ident()?;
                        let mut e = Expr::Field(Box::new(Expr::Ident(name)), field);
                        while self.at(&TokenKind::ColonColon) {
                            self.advance();
                            let field = self.expect_ident()?;
                            e = Expr::Field(Box::new(e), field);
                        }
                        if self.at(&TokenKind::LParen) {
                            self.advance();
                            let args = self.parse_args()?;
                            self.expect(TokenKind::RParen, "`)`")?;
                            e = Expr::Call(Box::new(e), args);
                        }
                        e
                    }
                } else {
                let mut e = Expr::Ident(name);
                loop {
                    if self.at(&TokenKind::LParen) {
                        self.advance();
                        let args = self.parse_args()?;
                        self.expect(TokenKind::RParen, "`)`")?;
                        e = Expr::Call(Box::new(e), args);
                    } else if self.at(&TokenKind::Dot) {
                        self.advance();
                        let field = if self.at(&TokenKind::Ident("".into())) {
                            self.expect_ident()
                        } else if self.at(&TokenKind::Spawn) {
                            self.advance();
                            Ok("spawn".to_string())
                        } else if self.at(&TokenKind::Await) {
                            self.advance();
                            Ok("await".to_string())
                        } else if self.at(&TokenKind::Quote) {
                            self.advance();
                            Ok("quote".to_string())
                        } else {
                            self.expect_ident()
                        }?;
                        e = Expr::Field(Box::new(e), field);
                    } else if self.at(&TokenKind::LBracket) {
                        self.advance();
                        // Check for slice syntax: arr[start..end], arr[..end], arr[start..]
                        if self.at(&TokenKind::DotDot) {
                            // arr[..end]
                            self.advance();
                            let end = if self.at(&TokenKind::RBracket) {
                                None
                            } else {
                                Some(Box::new(self.parse_expr(0)?))
                            };
                            self.expect(TokenKind::RBracket, "`]`")?;
                            e = Expr::SliceExpr {
                                target: Box::new(e),
                                start: None,
                                end,
                            };
                        } else {
                            let first = self.parse_expr(0)?;
                            if self.at(&TokenKind::DotDot) {
                                // arr[start..end] or arr[start..]
                                self.advance();
                                let end = if self.at(&TokenKind::RBracket) {
                                    None
                                } else {
                                    Some(Box::new(self.parse_expr(0)?))
                                };
                                self.expect(TokenKind::RBracket, "`]`")?;
                                e = Expr::SliceExpr {
                                    target: Box::new(e),
                                    start: Some(Box::new(first)),
                                    end,
                                };
                            } else {
                                // arr[i] — regular index
                                self.expect(TokenKind::RBracket, "`]`")?;
                                e = Expr::Index(Box::new(e), Box::new(first));
                            }
                        }
                    } else if self.at(&TokenKind::LBrace) {
                        if let Expr::Ident(ty_name) = &e {
                            if ty_name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                                let ty_name = ty_name.clone();
                                self.advance();
                                let fields = self.parse_record_expr_fields()?;
                                self.expect(TokenKind::RBrace, "`}`")?;
                                e = Expr::Record {
                                    ty: Some(ty_name),
                                    fields,
                                };
                                continue;
                            }
                        }
                        break;
                    } else {
                        break;
                    }
                }
                e
                }
            }
            TokenKind::LParen => {
                self.advance();
                self.skip_newlines();
                if self.at(&TokenKind::RParen) {
                    self.advance();
                    return Ok(Expr::Literal(Lit::Unit));
                }
                let e = self.parse_expr(0)?;
                self.skip_newlines();
                if self.at(&TokenKind::Comma) {
                    let mut elems = vec![e];
                    while self.at(&TokenKind::Comma) {
                        self.advance();
                        self.skip_newlines();
                        elems.push(self.parse_expr(0)?);
                        self.skip_newlines();
                    }
                    self.expect(TokenKind::RParen, "`)`")?;
                    return Ok(Expr::Tuple(elems));
                }
                self.expect(TokenKind::RParen, "`)`")?;
                e
            }
            TokenKind::LBracket => {
                self.advance();
                self.skip_newlines();
                let first_expr = if self.at(&TokenKind::RBracket) {
                    self.advance();
                    return Ok(Expr::List(vec![]));
                } else {
                    self.parse_expr(0)?
                };
                self.skip_newlines();
                if self.at(&TokenKind::For) {
                    self.advance();
                    let var = self.expect_ident()?;
                    self.expect(TokenKind::In, "`in`")?;
                    let iter = self.parse_expr(0)?;
                    self.skip_newlines();
                    let guard = if self.at(&TokenKind::If) {
                        self.advance();
                        Some(Box::new(self.parse_expr(0)?))
                    } else {
                        None
                    };
                    self.expect(TokenKind::RBracket, "`]`")?;
                    Expr::Comprehension {
                        expr: Box::new(first_expr),
                        var,
                        iter: Box::new(iter),
                        guard,
                    }
                } else {
                    let mut elems = vec![first_expr];
                    loop {
                        self.skip_newlines();
                        if !self.at(&TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                        self.skip_newlines();
                        if self.at(&TokenKind::RBracket) {
                            break;
                        }
                        elems.push(self.parse_expr(0)?);
                    }
                    self.expect(TokenKind::RBracket, "`]`")?;
                    Expr::List(elems)
                }
            }
            TokenKind::Match => {
                self.advance();
                let e = self.parse_expr(0)?;
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{`")?;
                let arms = self.parse_match_arms()?;
                self.expect(TokenKind::RBrace, "`}`")?;
                Expr::Match(Box::new(e), arms)
            }
            TokenKind::Spawn => {
                self.advance();
                let e = self.parse_expr(0)?;
                Expr::Spawn(Box::new(e))
            }
            TokenKind::Await => {
                self.advance();
                let e = self.parse_expr(0)?;
                Expr::Await(Box::new(e))
            }
            TokenKind::Comptime => {
                self.advance();
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for comptime block")?;
                let body = self.parse_block()?;
                Expr::Comptime(body)
            }
            TokenKind::Quote => {
                self.advance();
                if self.at(&TokenKind::Bang) {
                    self.advance();
                }
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for quote! body")?;
                let body = self.parse_quote_block()?;
                Expr::Quote(body)
            }
            TokenKind::DollarParen => {
                self.advance();
                let inner = self.parse_expr(0)?;
                self.expect(TokenKind::RParen, "`)` for quote interpolation")?;
                Expr::QuoteInterpolate(Box::new(inner))
            }
            TokenKind::Fn => {
                self.advance();
                self.expect(TokenKind::LParen, "`(`")?;
                let params = self.parse_params()?;
                self.expect(TokenKind::RParen, "`)`")?;
                let ret = if self.at(&TokenKind::Arrow) {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                self.skip_newlines();
                if self.is_sketch() {
                    self.expect(TokenKind::Colon, "`:`")?;
                    self.skip_newlines();
                }
                self.expect_block_start("closure body")?;
                let body = self.parse_block()?;
                Expr::Lambda { params, ret, body }
            }
            _ => {
                let (line, col) = (self.peek().line, self.peek().col);
                return Err(ParseError::new(
                    format!("unexpected token {}", kind),
                    line,
                    col,
                ));
            }
        };
        if self.at(&TokenKind::Question) {
            self.advance();
            expr = Expr::Try(Box::new(expr));
        }
        Ok(expr)
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        if self.at(&TokenKind::RParen) {
            return Ok(args);
        }
        loop {
            args.push(self.parse_expr(0)?);
            if !self.at(&TokenKind::Comma) {
                break;
            }
            self.advance();
        }
        Ok(args)
    }

    fn parse_record_expr_fields(&mut self) -> Result<Vec<RecordFieldExpr>, ParseError> {
        let mut fields = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let name = self.expect_ident()?;
            if self.at(&TokenKind::Colon) {
                self.advance();
                let value = self.parse_expr(0)?;
                fields.push(RecordFieldExpr { name, value });
            } else {
                // Shorthand: field_name instead of field_name: field_name
                fields.push(RecordFieldExpr { name: name.clone(), value: Expr::Ident(name) });
            }
            self.skip_newlines();
            if self.at(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }
        Ok(fields)
    }
}