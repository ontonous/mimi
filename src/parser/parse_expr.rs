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
                TokenKind::DotDot => (BinOp::Range, 3, false),
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

    /// Parse an expression without consuming `..` (used for slice start parsing)
    fn parse_expr_without_range(&mut self) -> Result<Expr, ParseError> {
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
                // Stop before `..` to allow slice syntax parsing
                TokenKind::DotDot => break,
                _ => break,
            };
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
        let commitment = self.peek().commitment;
        let mut expr = match kind {
            TokenKind::Int(s) => {
                let (line, col) = (self.peek().line, self.peek().col);
                self.advance();
                let cleaned = s.replace('_', "");
                let v = if cleaned.starts_with("0x") || cleaned.starts_with("0X") {
                    i64::from_str_radix(&cleaned[2..], 16)
                        .map_err(|_| ParseError::new("invalid hex integer", line, col))?
                } else if cleaned.starts_with("0b") || cleaned.starts_with("0B") {
                    i64::from_str_radix(&cleaned[2..], 2)
                        .map_err(|_| ParseError::new("invalid binary integer", line, col))?
                } else if cleaned.starts_with("0o") || cleaned.starts_with("0O") {
                    i64::from_str_radix(&cleaned[2..], 8)
                        .map_err(|_| ParseError::new("invalid octal integer", line, col))?
                } else {
                    cleaned.parse::<i64>()
                        .map_err(|_| ParseError::new("invalid integer", line, col))?
                };
                return self.parse_postfix(Expr::Literal(Lit::Int(v)));
            }
            TokenKind::Float(s) => {
                let (line, col) = (self.peek().line, self.peek().col);
                self.advance();
                let v = s
                    .replace('_', "")
                    .parse::<f64>()
                    .map_err(|_| ParseError::new("invalid float", line, col))?;
                return self.parse_postfix(Expr::Literal(Lit::Float(v)));
            }
            TokenKind::String(s) => {
                self.advance();
                return self.parse_postfix(Expr::Literal(Lit::String(s)));
            }
            TokenKind::FString(raw) => {
                let raw = raw.clone();
                self.advance();
                let parts = self.parse_fstring_parts(&raw)?;
                return self.parse_postfix(Expr::Literal(Lit::FString(parts)));
            }
            TokenKind::True => {
                self.advance();
                return self.parse_postfix(Expr::Literal(Lit::Bool(true)));
            }
            TokenKind::False => {
                self.advance();
                return self.parse_postfix(Expr::Literal(Lit::Bool(false)));
            }
            TokenKind::Unit => {
                self.advance();
                return self.parse_postfix(Expr::Literal(Lit::Unit));
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
                        e = self.parse_slice_or_index(e)?;
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
                        self.expect_gt("`>`")?;
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
                        if let TokenKind::Int(s) = &self.peek().kind {
                            let idx = s.replace('_', "").parse::<usize>()
                                .map_err(|_| ParseError::new("invalid tuple index", self.peek().line, self.peek().col))?;
                            self.advance();
                            e = Expr::TupleIndex(Box::new(e), idx);
                        } else {
                            let field = if matches!(self.peek_kind(), TokenKind::Ident(_)) {
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
                        }
                    } else if self.at(&TokenKind::LBracket) {
                        e = self.parse_slice_or_index(e)?;
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
                return self.parse_postfix(e);
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
                    return self.parse_postfix(Expr::Comprehension {
                        expr: Box::new(first_expr),
                        var,
                        iter: Box::new(iter),
                        guard,
                    });
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
                    return self.parse_postfix(Expr::List(elems));
                }
            }
            TokenKind::Match => {
                self.advance();
                let e = self.parse_expr(0)?;
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{`")?;
                let arms = self.parse_match_arms()?;
                self.expect(TokenKind::RBrace, "`}`")?;
                return self.parse_postfix(Expr::Match(Box::new(e), arms));
            }
            TokenKind::Spawn => {
                self.advance();
                let e = self.parse_expr(0)?;
                return self.parse_postfix(Expr::Spawn(Box::new(e)));
            }
            TokenKind::Await => {
                self.advance();
                let e = self.parse_expr(0)?;
                return self.parse_postfix(Expr::Await(Box::new(e)));
            }
            TokenKind::Comptime => {
                self.advance();
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for comptime block")?;
                let body = self.parse_block()?;
                return self.parse_postfix(Expr::Comptime(body));
            }
            TokenKind::Quote => {
                self.advance();
                if self.at(&TokenKind::Bang) {
                    self.advance();
                }
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for quote! body")?;
                let body = self.parse_quote_block()?;
                return self.parse_postfix(Expr::Quote(body));
            }
            TokenKind::DollarParen => {
                self.advance();
                let inner = self.parse_expr(0)?;
                return self.parse_postfix(Expr::QuoteInterpolate(Box::new(inner)));
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
                return self.parse_postfix(Expr::Lambda { params, ret, body });
            }
            // Keywords as identifiers in expression context (e.g., Func, Module for enum comparison)
            ref kw if is_keyword_token(kw) => {
                let name = kind.source_text().to_string();
                self.advance();
                let mut e = Expr::Ident(name);
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
                        let first = self.parse_expr(0)?;
                        if self.at(&TokenKind::DotDot) {
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
                            self.expect(TokenKind::RBracket, "`]`")?;
                            e = Expr::Index(Box::new(e), Box::new(first));
                        }
                    } else {
                        break;
                    }
                }
                e
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
        // ? after an identifier: the lexer may attach it as commitment:Question
        // (via scan_commitment called from scan_ident), so check both the
        // commitment field on the primary token AND a separate Question token.
        let try_from_commitment = commitment == Commitment::Question
            || commitment == Commitment::QuestionQuestion;
        if try_from_commitment || self.at(&TokenKind::Question) {
            if !try_from_commitment {
                self.advance();
            }
            expr = Expr::Try(Box::new(expr));
        }
        Ok(expr)
    }

    /// Parse postfix operations (calls, field access, indexing) on a base expression
    fn parse_postfix(&mut self, mut expr: Expr) -> Result<Expr, ParseError> {
        loop {
            if self.at(&TokenKind::LParen) {
                self.advance();
                let args = self.parse_args()?;
                self.expect(TokenKind::RParen, "`)`")?;
                expr = Expr::Call(Box::new(expr), args);
            } else if self.at(&TokenKind::Dot) {
                self.advance();
                // Check for numeric tuple index: t.0, t.1, etc.
                if let TokenKind::Int(s) = &self.peek().kind {
                    let idx = s.replace('_', "").parse::<usize>()
                        .map_err(|_| ParseError::new("invalid tuple index", self.peek().line, self.peek().col))?;
                    self.advance();
                    expr = Expr::TupleIndex(Box::new(expr), idx);
                } else {
                    let field = self.expect_ident()?;
                    expr = Expr::Field(Box::new(expr), field);
                }
            } else if self.at(&TokenKind::LBracket) {
                self.advance();
                let first = self.parse_expr(0)?;
                if self.at(&TokenKind::DotDot) {
                    self.advance();
                    let end = if self.at(&TokenKind::RBracket) { None } else { Some(Box::new(self.parse_expr(0)?)) };
                    self.expect(TokenKind::RBracket, "`]`")?;
                    expr = Expr::SliceExpr { target: Box::new(expr), start: Some(Box::new(first)), end };
                } else {
                    self.expect(TokenKind::RBracket, "`]`")?;
                    expr = Expr::Index(Box::new(expr), Box::new(first));
                }
            } else {
                break;
            }
        }
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

    /// Parse `expr[i]` (index) or `expr[start..end]` / `expr[..end]` / `expr[start..]` (slice).
    /// Shared by the primary expression and identifier postfix paths.
    fn parse_slice_or_index(&mut self, target: Expr) -> Result<Expr, ParseError> {
        self.advance(); // skip [
        if self.at(&TokenKind::DotDot) {
            // expr[..end]
            self.advance();
            let end = if self.at(&TokenKind::RBracket) {
                None
            } else {
                Some(Box::new(self.parse_expr(0)?))
            };
            self.expect(TokenKind::RBracket, "`]`")?;
            Ok(Expr::SliceExpr {
                target: Box::new(target),
                start: None,
                end,
            })
        } else {
            // Parse start, stopping before `..` to handle slice syntax
            let first = self.parse_expr_without_range()?;
            if self.at(&TokenKind::DotDot) {
                // expr[start..end] or expr[start..]
                self.advance();
                let end = if self.at(&TokenKind::RBracket) {
                    None
                } else {
                    Some(Box::new(self.parse_expr(0)?))
                };
                self.expect(TokenKind::RBracket, "`]`")?;
                Ok(Expr::SliceExpr {
                    target: Box::new(target),
                    start: Some(Box::new(first)),
                    end,
                })
            } else {
                // expr[i] — regular index
                self.expect(TokenKind::RBracket, "`]`")?;
                Ok(Expr::Index(Box::new(target), Box::new(first)))
            }
        }
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

    fn parse_quote_block(&mut self) -> Result<Block, ParseError> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            if self.at(&TokenKind::DollarParen) {
                self.advance();
                let inner = self.parse_expr(0)?;
                self.expect(TokenKind::RParen, "`)`")?;
                stmts.push(Stmt::Expr(Expr::QuoteInterpolate(Box::new(inner))));
            } else {
                stmts.push(self.parse_stmt()?);
            }
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(stmts)
    }
}

/// Check if a token kind is a keyword that can be used as an identifier in expression context.
fn is_keyword_token(kind: &TokenKind) -> bool {
    matches!(kind,
        TokenKind::Module | TokenKind::Type | TokenKind::Func | TokenKind::Fn |
        TokenKind::Actor | TokenKind::Newtype | TokenKind::Let | TokenKind::Mut |
        TokenKind::Ref | TokenKind::Shared | TokenKind::LocalShared | TokenKind::Weak |
        TokenKind::WeakLocal | TokenKind::CShared | TokenKind::CBorrow |
        TokenKind::CBorrowMut | TokenKind::RawString |
        TokenKind::Arena | TokenKind::Alloc | TokenKind::Cap | TokenKind::Trait | TokenKind::Impl |
        TokenKind::Dyn | TokenKind::Where | TokenKind::Extern | TokenKind::Unsafe |
        TokenKind::Use | TokenKind::Pub | TokenKind::In |
        TokenKind::Drop | TokenKind::Steps | TokenKind::Parasteps | TokenKind::Failure |
        TokenKind::Requires | TokenKind::Ensures | TokenKind::Math | TokenKind::Desc |
        TokenKind::Rule | TokenKind::Mms | TokenKind::With | TokenKind::And |
        TokenKind::Or | TokenKind::Not | TokenKind::Async | TokenKind::Comptime |
        TokenKind::Spawn | TokenKind::Await | TokenKind::Quote | TokenKind::Old |
        TokenKind::I32 | TokenKind::I64 |
        TokenKind::F64 | TokenKind::Bool | TokenKind::StringKw | TokenKind::Nothing |
        TokenKind::True | TokenKind::False | TokenKind::Unit
    )
}