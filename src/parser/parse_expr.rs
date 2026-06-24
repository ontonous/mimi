// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::*;

impl Parser {
    pub(crate) fn parse_expr(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        self.check_depth()?;
        self.inc_depth();
        let result = self.parse_expr_inner(min_prec);
        self.dec_depth();
        result
    }

    fn parse_expr_inner(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            // Check for pipe operator |> before binary ops
            // Pipe has lowest precedence (0), so skip if min_prec > 0
            if self.at(&TokenKind::PipeArrow) && min_prec == 0 {
                self.advance();
                let rhs = self.parse_expr(1)?;
                lhs = self.desugar_pipe(lhs, rhs)?;
                continue;
            }
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
            lhs = lhs.binary(op, rhs);
        }
        Ok(lhs)
    }

    /// Parse an expression without consuming `..` (used for slice start parsing)
    fn parse_expr_without_range(&mut self) -> Result<Expr, ParseError> {
        self.check_depth()?;
        self.inc_depth();
        let mut lhs = self.parse_unary()?;
        loop {
            if self.at(&TokenKind::PipeArrow) {
                self.advance();
                let rhs = self.parse_expr(1)?;
                lhs = self.desugar_pipe(lhs, rhs)?;
                continue;
            }
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
            lhs = lhs.binary(op, rhs);
        }
        self.dec_depth();
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        self.check_depth()?;
        self.inc_depth();
        let result = self.parse_unary_inner();
        self.dec_depth();
        result
    }

    fn parse_unary_inner(&mut self) -> Result<Expr, ParseError> {
        match self.peek_kind() {
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Minus => {
                self.advance();
                Ok(self.parse_unary()?.unary(UnOp::Neg))
            }
            TokenKind::Bang | TokenKind::NotOp | TokenKind::Not => {
                self.advance();
                Ok(self.parse_unary()?.unary(UnOp::Not))
            }
            TokenKind::BitAnd => {
                self.advance();
                if self.at(&TokenKind::Mut) {
                    self.advance();
                    Ok(self.parse_unary()?.unary(UnOp::RefMut))
                } else {
                    Ok(self.parse_unary()?.unary(UnOp::Ref))
                }
            }
            TokenKind::Star => {
                self.advance();
                Ok(self.parse_unary()?.unary(UnOp::Deref))
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
                    e = e.call(args);
                }
                return self.parse_postfix(e);
            }
            TokenKind::Ident(name) => self.parse_ident_primary(name)?,
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
            TokenKind::LBracket => self.parse_bracket_primary()?,
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
                let e = self.parse_expr(12)?;
                return self.parse_postfix(Expr::Spawn(Box::new(e)));
            }
            TokenKind::Await => {
                self.advance();
                let e = self.parse_expr(12)?;
                return self.parse_postfix(Expr::Await(Box::new(e)));
            }
            TokenKind::Arena => {
                self.advance();
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for arena block")?;
                let body = self.parse_block()?;
                return self.parse_postfix(Expr::Arena(body));
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
            TokenKind::LBrace => {
                self.advance();
                // Try to parse as map literal {"key": value, ...}
                if let Some(entries) = self.try_parse_map_literal() {
                    return self.parse_postfix(Expr::MapLiteral { entries });
                }
                // Try to parse as set literal {1, 2, 3, ...}
                if let Some(elems) = self.try_parse_set_literal() {
                    return self.parse_postfix(Expr::SetLiteral(elems));
                }
                let block = self.parse_block()?;
                return self.parse_postfix(Expr::Block(block));
            }
            // Keywords as identifiers in expression context (e.g., Func, Module for enum comparison)
            ref kw if is_keyword_token(kw) => {
                let name = kind.source_text().to_string();
                self.advance();
                return self.parse_postfix(Expr::Ident(name));
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
        // ? after an identifier: check for a separate Question token.
        if self.at(&TokenKind::Question) {
            self.advance();
            expr = expr.try_expr();
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
                    expr = expr.call(args);
                } else if self.at(&TokenKind::Dot) {
                    self.advance();
                    // Check for numeric tuple index: t.0, t.1, etc.
                    if let TokenKind::Int(s) = &self.peek().kind {
                        let idx = s.replace('_', "").parse::<usize>()
                            .map_err(|_| ParseError::new("invalid tuple index", self.peek().line, self.peek().col))?;
                        self.advance();
                        expr = expr.tuple_index(idx);
                    } else {
                        let field = self.expect_ident()?;
                        expr = expr.field(field);
                }
            } else if self.at(&TokenKind::LBracket) {
                expr = self.parse_slice_or_index(expr)?;
            } else {
                break;
            }
        }
        if self.at(&TokenKind::Question) {
            self.advance();
            expr = expr.try_expr();
        }
        Ok(expr)
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        if self.at(&TokenKind::RParen) {
            return Ok(args);
        }
        loop {
            // Check for named arg: `ident = expr`
            if let Some(name) = self.peek_named_arg() {
                self.expect_ident()?;
                self.expect(TokenKind::Eq, "`=`")?;
                let value = self.parse_expr(0)?;
                args.push(Expr::NamedArg(name, Box::new(value)));
            } else {
                args.push(self.parse_expr(0)?);
            }
            if !self.at(&TokenKind::Comma) {
                break;
            }
            self.advance();
        }
        Ok(args)
    }

    /// Peek at the next tokens to detect a named argument pattern: `ident = expr`
    /// Returns the identifier name if it looks like a named arg, None otherwise.
    fn peek_named_arg(&mut self) -> Option<String> {
        let save = self.pos();
        let tok = self.peek();
        let name = match &tok.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => return None,
        };
        // Check if followed by `=`
        if save + 1 < self.tokens.len() && matches!(&self.tokens[save + 1].kind, TokenKind::Eq) {
            Some(name)
        } else {
            None
        }
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
            Ok(target.with_slice(None, end))
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
                Ok(target.with_slice(Some(Box::new(first)), end))
            } else {
                // expr[i] — regular index
                self.expect(TokenKind::RBracket, "`]`")?;
                Ok(target.index(first))
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
    fn parse_ident_primary(&mut self, name: String) -> Result<Expr, ParseError> {
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
                return Ok(Expr::Turbofish(name, type_args, args));
            } else {
                let field = self.expect_ident()?;
                let mut e = Expr::Ident(name).field(field);
                while self.at(&TokenKind::ColonColon) {
                    self.advance();
                    let field = self.expect_ident()?;
                    e = e.field(field);
                }
                if self.at(&TokenKind::LParen) {
                    self.advance();
                    let args = self.parse_args()?;
                    self.expect(TokenKind::RParen, "`)`")?;
                    e = e.call(args);
                }
                return Ok(e);
            }
        }
        let mut e = Expr::Ident(name);
        loop {
            if self.at(&TokenKind::LParen) {
                self.advance();
                let args = self.parse_args()?;
                self.expect(TokenKind::RParen, "`)`")?;
                e = e.call(args);
            } else if self.at(&TokenKind::Dot) {
                self.advance();
                if let TokenKind::Int(s) = &self.peek().kind {
                    let idx = s.replace('_', "").parse::<usize>()
                        .map_err(|_| ParseError::new("invalid tuple index", self.peek().line, self.peek().col))?;
                    self.advance();
                    e = e.tuple_index(idx);
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
                    e = e.field(field);
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
        Ok(e)
    }

    /// Parse a `[expr]` / `[expr for var in iter]` / `[expr, ...]` primary expression.
    fn parse_bracket_primary(&mut self) -> Result<Expr, ParseError> {
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
            self.parse_postfix(Expr::Comprehension {
                expr: Box::new(first_expr),
                var,
                iter: Box::new(iter),
                guard,
            })
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
            self.parse_postfix(Expr::List(elems))
        }
    }

    /// Try to parse a map literal `{expr: expr, ...}` from the current position.
    /// Returns None if the token stream doesn't start a map literal (fallback to block parsing).
    fn try_parse_map_literal(&mut self) -> Option<Vec<(Expr, Expr)>> {
        let saved = self.pos;
        // Quick first-token check to avoid expensive parsing attempts
        let first = self.peek_kind().clone();
        if is_stmt_start_keyword(&first) || matches!(first, TokenKind::RBrace) {
            return None;
        }
        // Save depth state
        let saved_depth = self.recursion_depth.get();
        // Try to parse a single expression
        let first_key = match self.parse_expr(0) {
            Ok(key) => key,
            Err(_) => { self.pos = saved; self.recursion_depth.set(saved_depth); return None; }
        };
        // The colon after the first expression distinguishes map literal from block
        if !self.at(&TokenKind::Colon) {
            self.pos = saved;
            self.recursion_depth.set(saved_depth);
            return None;
        }
        self.advance(); // consume ':'
        let first_val = match self.parse_expr(0) {
            Ok(val) => val,
            Err(_) => { self.pos = saved; self.recursion_depth.set(saved_depth); return None; }
        };
        let mut entries = vec![(first_key, first_val)];
        // Parse remaining entries
        loop {
            self.skip_newlines();
            if self.at(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
                if self.at(&TokenKind::RBrace) {
                    break; // trailing comma ok
                }
                let key = match self.parse_expr(0) {
                    Ok(k) => k,
                    Err(_) => { self.pos = saved; self.recursion_depth.set(saved_depth); return None; }
                };
                if !self.at(&TokenKind::Colon) {
                    self.pos = saved;
                    self.recursion_depth.set(saved_depth);
                    return None;
                }
                self.advance();
                let val = match self.parse_expr(0) {
                    Ok(v) => v,
                    Err(_) => { self.pos = saved; self.recursion_depth.set(saved_depth); return None; }
                };
                entries.push((key, val));
            } else if self.at(&TokenKind::RBrace) {
                break;
            } else {
                self.pos = saved;
                self.recursion_depth.set(saved_depth);
                return None;
            }
        }
        self.advance(); // consume '}'
        Some(entries)
    }

    /// Try to parse a set literal `{expr, expr, ...}` from the current position.
    /// Returns None if the token stream doesn't start a set literal (fallback to block parsing).
    /// Disambiguation: `{expr}` is always a block; `{expr, ...}` with 2+ elements is a set.
    fn try_parse_set_literal(&mut self) -> Option<Vec<Expr>> {
        let saved = self.pos;
        let saved_depth = self.recursion_depth.get();
        // Quick check for stmt-start keyword or closing brace
        let first = self.peek_kind().clone();
        if is_stmt_start_keyword(&first) || matches!(first, TokenKind::RBrace) {
            return None;
        }
        // Parse first expression
        let first_elem = match self.parse_expr(0) {
            Ok(e) => e,
            Err(_) => { self.pos = saved; self.recursion_depth.set(saved_depth); return None; }
        };
        // Must have a comma to be a set literal (single expr is block)
        if !self.at(&TokenKind::Comma) {
            self.pos = saved;
            self.recursion_depth.set(saved_depth);
            return None;
        }
        self.advance(); // consume ','
        let mut elems = vec![first_elem];
        // Parse remaining elements
        loop {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) {
                break; // trailing comma ok
            }
            match self.parse_expr(0) {
                Ok(e) => elems.push(e),
                Err(_) => { self.pos = saved; self.recursion_depth.set(saved_depth); return None; }
            }
            self.skip_newlines();
            if self.at(&TokenKind::Comma) {
                self.advance();
            } else if self.at(&TokenKind::RBrace) {
                break;
            } else {
                self.pos = saved;
                self.recursion_depth.set(saved_depth);
                return None;
            }
        }
        self.advance(); // consume '}'
        Some(elems)
    }

    /// Desugar a |> b into b(a) or b(a, ...).
    /// If b is a call expression, a is inserted as the first argument.
    /// If b is an identifier, it becomes a call with a as the only argument.
    fn desugar_pipe(&mut self, lhs: Expr, rhs: Expr) -> Result<Expr, ParseError> {
        match rhs {
            Expr::Call(callee, args) => {
                let mut new_args = vec![lhs];
                new_args.extend(args);
                Ok(Expr::Call(callee, new_args))
            }
            Expr::Ident(_) | Expr::Field(_, _) | Expr::Turbofish(_, _, _) => {
                Ok(Expr::Call(Box::new(rhs), vec![lhs]))
            }
            _ => {
                let line = self.peek().line;
                let col = self.peek().col;
                Err(ParseError::new(
                    "right side of |> must be a function call or name".to_string(),
                    line,
                    col,
                ))
            }
        }
    }
}

/// Returns true if the token kind starts a statement (and therefore parsing as block).
fn is_stmt_start_keyword(kind: &TokenKind) -> bool {
    matches!(kind,
        TokenKind::Let | TokenKind::If | TokenKind::While | TokenKind::For |
        TokenKind::Return | TokenKind::Break | TokenKind::Continue |
        TokenKind::Match | TokenKind::Arena | TokenKind::Unsafe |
        TokenKind::Spawn | TokenKind::Await | TokenKind::Alloc |
        TokenKind::Drop | TokenKind::Steps | TokenKind::Parasteps |
        TokenKind::Failure | TokenKind::Requires | TokenKind::Ensures |
        TokenKind::Math | TokenKind::Invariant | TokenKind::Desc | TokenKind::Rule |
        TokenKind::Loop
    )
}

/// Delegates to the canonical keyword check in `lexer::keywords`.
/// Add new keywords there, not here.
fn is_keyword_token(kind: &TokenKind) -> bool {
    crate::lexer::is_keyword_kind(kind)
}