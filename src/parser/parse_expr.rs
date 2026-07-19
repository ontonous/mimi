// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::*;

impl Parser {
    pub(super) fn parsed_expr_from(&self, start_pos: usize, expr: Expr) -> Expr {
        let origin = expr
            .meta()
            .map(|meta| meta.origin)
            .unwrap_or(AstOrigin::User);
        expr.with_meta(self.consumed_meta(start_pos, origin))
    }

    pub(crate) fn parse_expr(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let start_pos = self.pos;
        self.check_depth()?;
        self.inc_depth();
        let result = self.parse_expr_inner(min_prec, start_pos);
        self.dec_depth();
        result.map(|expr| self.parsed_expr_from(start_pos, expr))
    }

    fn parse_expr_inner(&mut self, min_prec: u8, start_pos: usize) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            // Check for pipe operator |> before binary ops
            // Pipe has lowest precedence (0), so skip if min_prec > 0
            if self.at(&TokenKind::PipeArrow) && min_prec == 0 {
                self.advance();
                let rhs = self.parse_expr(1)?;
                let piped = self.desugar_pipe(lhs, rhs)?;
                lhs = piped
                    .with_meta(self.consumed_meta(start_pos, AstOrigin::Desugared("parser.pipe")));
                continue;
            }
            // Skip newlines before low-precedence boolean operators (multiline ||/&& chains)
            self.try_skip_newlines_for_boolean_op();
            let (op, prec, right_assoc) = match self.peek_kind() {
                TokenKind::OrOr | TokenKind::Or => (BinOp::Or, 1, false),
                TokenKind::AndAnd | TokenKind::And => (BinOp::And, 2, false),
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
            // Skip newlines after binary operator before RHS (multiline expressions)
            self.skip_newlines();
            let next_min = if right_assoc { prec } else { prec + 1 };
            let rhs = self.parse_expr(next_min)?;
            let binary = lhs.binary(op, rhs);
            lhs = self.parsed_expr_from(start_pos, binary);
        }
        Ok(lhs)
    }

    /// Parse an expression without consuming `..` (used for slice start parsing)
    fn parse_expr_without_range(&mut self) -> Result<Expr, ParseError> {
        let start_pos = self.pos;
        self.check_depth()?;
        self.inc_depth();
        let mut lhs = self.parse_unary()?;
        loop {
            if self.at(&TokenKind::PipeArrow) {
                self.advance();
                let rhs = self.parse_expr(1)?;
                let piped = self.desugar_pipe(lhs, rhs)?;
                lhs = piped
                    .with_meta(self.consumed_meta(start_pos, AstOrigin::Desugared("parser.pipe")));
                continue;
            }
            // Skip newlines before low-precedence boolean operators (multiline ||/&& chains)
            self.try_skip_newlines_for_boolean_op();
            let (op, prec, right_assoc) = match self.peek_kind() {
                TokenKind::OrOr | TokenKind::Or => (BinOp::Or, 1, false),
                TokenKind::AndAnd | TokenKind::And => (BinOp::And, 2, false),
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
            let binary = lhs.binary(op, rhs);
            lhs = self.parsed_expr_from(start_pos, binary);
        }
        self.dec_depth();
        Ok(self.parsed_expr_from(start_pos, lhs))
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        let start_pos = self.pos;
        self.check_depth()?;
        self.inc_depth();
        let result = self.parse_unary_inner();
        self.dec_depth();
        result.map(|expr| self.parsed_expr_from(start_pos, expr))
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
                // Soft keyword: old(expr) is contract snapshot, bare 'old' is identifier
                if self.pos + 1 < self.tokens.len()
                    && self.tokens[self.pos + 1].kind == TokenKind::LParen
                {
                    self.advance(); // consume 'old'
                    self.advance(); // consume '('
                    let expr = self.parse_expr(0)?;
                    self.expect(TokenKind::RParen, "`)`")?;
                    Ok(Expr::Old(Box::new(expr)))
                } else {
                    self.parse_primary()
                }
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_if_expr(&mut self) -> Result<Expr, ParseError> {
        let start_pos = self.pos;
        self.check_depth()?;
        self.inc_depth();
        let result = self.parse_if_expr_inner();
        self.dec_depth();
        result.map(|expr| self.parsed_expr_from(start_pos, expr))
    }

    fn parse_if_expr_inner(&mut self) -> Result<Expr, ParseError> {
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
                let meta = elif
                    .meta()
                    .map(|meta| {
                        AstNodeMeta::new(
                            meta.span,
                            AstOrigin::Desugared("parser.else_if.statement"),
                        )
                    })
                    .unwrap_or_else(|| {
                        AstNodeMeta::synthetic(AstOrigin::Desugared("parser.else_if.statement"))
                    });
                Some(vec![Stmt::Expr(elif).with_meta(meta)])
            } else {
                return Err(ParseError::new(
                    "`{` or `if` expected after `else`",
                    self.peek().line,
                    self.peek().col,
                ));
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
        let start_pos = self.pos;
        let kind = self.peek().kind.clone();
        let expr = match kind {
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
                    cleaned
                        .parse::<i64>()
                        .map_err(|_| ParseError::new("invalid integer", line, col))?
                };
                let literal = self.parsed_expr_from(start_pos, Expr::Literal(Lit::Int(v)));
                return self.parse_postfix(literal, start_pos);
            }
            TokenKind::Float(s) => {
                let (line, col) = (self.peek().line, self.peek().col);
                self.advance();
                let v = s
                    .replace('_', "")
                    .parse::<f64>()
                    .map_err(|_| ParseError::new("invalid float", line, col))?;
                let literal = self.parsed_expr_from(start_pos, Expr::Literal(Lit::Float(v)));
                return self.parse_postfix(literal, start_pos);
            }
            TokenKind::String(s) => {
                self.advance();
                let literal = self.parsed_expr_from(start_pos, Expr::Literal(Lit::String(s)));
                return self.parse_postfix(literal, start_pos);
            }
            TokenKind::FString(raw) => {
                let (line, col) = (self.peek().line, self.peek().col);
                let raw = raw.clone();
                self.advance();
                let parts = self.parse_fstring_parts(&raw, line, col)?;
                let literal = self.parsed_expr_from(start_pos, Expr::Literal(Lit::FString(parts)));
                return self.parse_postfix(literal, start_pos);
            }
            TokenKind::True => {
                self.advance();
                let literal = self.parsed_expr_from(start_pos, Expr::Literal(Lit::Bool(true)));
                return self.parse_postfix(literal, start_pos);
            }
            TokenKind::False => {
                self.advance();
                let literal = self.parsed_expr_from(start_pos, Expr::Literal(Lit::Bool(false)));
                return self.parse_postfix(literal, start_pos);
            }
            TokenKind::Unit => {
                self.advance();
                let literal = self.parsed_expr_from(start_pos, Expr::Literal(Lit::Unit));
                return self.parse_postfix(literal, start_pos);
            }
            TokenKind::Alloc => {
                self.advance();
                let mut e = self.parsed_expr_from(start_pos, Expr::Ident("alloc".to_string()));
                if self.at(&TokenKind::LParen) {
                    self.advance();
                    let args = self.parse_args()?;
                    self.expect(TokenKind::RParen, "`)`")?;
                    let call = e.call(args);
                    e = self.parsed_expr_from(start_pos, call);
                }
                return self.parse_postfix(e, start_pos);
            }
            TokenKind::Ident(name) => self.parse_ident_primary(name)?,
            TokenKind::LParen => {
                self.advance();
                self.skip_newlines();
                if self.at(&TokenKind::RParen) {
                    self.advance();
                    return Ok(self.parsed_expr_from(start_pos, Expr::Literal(Lit::Unit)));
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
                    return Ok(self.parsed_expr_from(start_pos, Expr::Tuple(elems)));
                }
                self.expect(TokenKind::RParen, "`)`")?;
                let grouped = self.parsed_expr_from(start_pos, e);
                return self.parse_postfix(grouped, start_pos);
            }
            TokenKind::LBracket => self.parse_bracket_primary()?,
            TokenKind::Match => {
                self.advance();
                let saved = self.allow_record_literal;
                self.allow_record_literal = false;
                let e = self.parse_expr(0)?;
                self.allow_record_literal = saved;
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{`")?;
                let arms = self.parse_match_arms()?;
                self.expect(TokenKind::RBrace, "`}`")?;
                let match_expr = self.parsed_expr_from(start_pos, Expr::Match(Box::new(e), arms));
                return self.parse_postfix(match_expr, start_pos);
            }
            TokenKind::Spawn => {
                self.advance();
                let e = self.parse_expr(12)?;
                let spawn = self.parsed_expr_from(start_pos, Expr::Spawn(Box::new(e)));
                return self.parse_postfix(spawn, start_pos);
            }
            TokenKind::Await => {
                self.advance();
                let e = self.parse_expr(12)?;
                let await_expr = self.parsed_expr_from(start_pos, Expr::Await(Box::new(e)));
                return self.parse_postfix(await_expr, start_pos);
            }
            TokenKind::Arena => {
                self.advance();
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for arena block")?;
                let body = self.parse_block()?;
                let arena = self.parsed_expr_from(start_pos, Expr::Arena(body));
                return self.parse_postfix(arena, start_pos);
            }
            TokenKind::Comptime => {
                self.advance();
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for comptime block")?;
                let body = self.parse_block()?;
                let comptime = self.parsed_expr_from(start_pos, Expr::Comptime(body));
                return self.parse_postfix(comptime, start_pos);
            }
            TokenKind::Quote => {
                self.advance();
                if self.at(&TokenKind::Bang) {
                    self.advance();
                }
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for quote! body")?;
                let body = self.parse_quote_block()?;
                let quote = self.parsed_expr_from(start_pos, Expr::Quote(body));
                return self.parse_postfix(quote, start_pos);
            }
            TokenKind::DollarParen => {
                self.advance();
                let inner = self.parse_expr(0)?;
                // v0.28.21 — The `$(` token is a single DollarParen
                // token, so the closing `)` is still in the stream and
                // must be consumed here. `parse_postfix` does not eat
                // stray `)`s, so this is the canonical place.
                self.expect(TokenKind::RParen, "`)` to close $(...) interpolation")?;
                let interpolation =
                    self.parsed_expr_from(start_pos, Expr::QuoteInterpolate(Box::new(inner)));
                return self.parse_postfix(interpolation, start_pos);
            }
            TokenKind::Old => {
                self.advance();
                let ident = self.parsed_expr_from(start_pos, Expr::Ident("old".to_string()));
                return self.parse_postfix(ident, start_pos);
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
                let lambda = self.parsed_expr_from(start_pos, Expr::Lambda { params, ret, body });
                return self.parse_postfix(lambda, start_pos);
            }
            TokenKind::LBrace => {
                self.advance();
                // Try to parse as map literal {"key": value, ...}
                if let Some(entries) = self.try_parse_map_literal() {
                    let map = self.parsed_expr_from(start_pos, Expr::MapLiteral { entries });
                    return self.parse_postfix(map, start_pos);
                }
                // Try to parse as set literal {1, 2, 3, ...}
                if let Some(elems) = self.try_parse_set_literal() {
                    let set = self.parsed_expr_from(start_pos, Expr::SetLiteral(elems));
                    return self.parse_postfix(set, start_pos);
                }
                let block = self.parse_block()?;
                let block = self.parsed_expr_from(start_pos, Expr::Block(block));
                return self.parse_postfix(block, start_pos);
            }
            // Keywords as identifiers in expression context (e.g., Func, Module for enum comparison)
            ref kw if is_keyword_token(kw) => {
                let name = kind.source_text().to_string();
                self.advance();
                let ident = self.parsed_expr_from(start_pos, Expr::Ident(name));
                return self.parse_postfix(ident, start_pos);
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
        Ok(self.parsed_expr_from(start_pos, expr))
    }

    /// Parse postfix operations (calls, field access, indexing) on a base expression
    fn parse_postfix(&mut self, mut expr: Expr, start_pos: usize) -> Result<Expr, ParseError> {
        loop {
            // PA-H3 (audit): handle `?.field` (optional chain) and `?` (try)
            // INSIDE the postfix loop so they work after any expression and
            // can be chained: `a?.b?.c`, `foo()?.bar`, `arr[0]?.name`.
            if self.at(&TokenKind::Question) {
                let next_is_dot = self.pos + 1 < self.tokens.len()
                    && self.tokens[self.pos + 1].kind == TokenKind::Dot;
                if next_is_dot {
                    self.advance(); // consume `?`
                    self.advance(); // consume `.`
                                    // Support both `?.field` and `?.0` (optional tuple index)
                    if let TokenKind::Int(s) = &self.peek().kind {
                        let idx = s.replace('_', "").parse::<usize>().map_err(|_| {
                            ParseError::new(
                                "invalid tuple index",
                                self.peek().line,
                                self.peek().col,
                            )
                        })?;
                        self.advance();
                        // OptionalChain doesn't have a tuple-index variant;
                        // desugar to Field with numeric name for now.
                        let optional = Expr::OptionalChain(Box::new(expr), idx.to_string());
                        expr = self.parsed_expr_from(start_pos, optional);
                    } else {
                        let field = self.expect_ident()?;
                        let optional = Expr::OptionalChain(Box::new(expr), field);
                        expr = self.parsed_expr_from(start_pos, optional);
                    }
                    continue; // allow chaining: a?.b?.c
                } else {
                    self.advance();
                    let try_expr = expr.try_expr();
                    expr = self.parsed_expr_from(start_pos, try_expr);
                    continue;
                }
            }
            if self.at(&TokenKind::LParen) {
                self.advance();
                let args = self.parse_args()?;
                self.skip_newlines();
                self.expect(TokenKind::RParen, "`)`")?;
                let call = expr.call(args);
                expr = self.parsed_expr_from(start_pos, call);
            } else if self.at(&TokenKind::Dot) {
                self.advance();
                // Check for numeric tuple index: t.0, t.1, etc.
                if let TokenKind::Int(s) = &self.peek().kind {
                    let idx = s.replace('_', "").parse::<usize>().map_err(|_| {
                        ParseError::new("invalid tuple index", self.peek().line, self.peek().col)
                    })?;
                    self.advance();
                    let tuple_index = expr.tuple_index(idx);
                    expr = self.parsed_expr_from(start_pos, tuple_index);
                } else {
                    let field = self.expect_ident()?;
                    let field_expr = expr.field(field);
                    expr = self.parsed_expr_from(start_pos, field_expr);
                }
            } else if self.at(&TokenKind::LBracket) {
                expr = self.parse_slice_or_index(expr, start_pos)?;
            } else {
                break;
            }
        }
        // Type cast: expr as Type
        if self.at(&TokenKind::As) {
            self.advance();
            let target_type = self.parse_type()?;
            let cast = Expr::Cast(Box::new(expr), target_type);
            expr = self.parsed_expr_from(start_pos, cast);
        }
        Ok(expr)
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        self.skip_newlines();
        if self.at(&TokenKind::RParen) {
            return Ok(args);
        }
        loop {
            self.skip_newlines();
            // Check for named arg: `ident = expr`
            if let Some(name) = self.peek_named_arg() {
                let arg_start = self.pos;
                self.expect_ident()?;
                self.expect(TokenKind::Eq, "`=`")?;
                let value = self.parse_expr(0)?;
                let named = Expr::NamedArg(name, Box::new(value));
                args.push(self.parsed_expr_from(arg_start, named));
            } else {
                args.push(self.parse_expr(0)?);
            }
            self.skip_newlines();
            if !self.at(&TokenKind::Comma) {
                break;
            }
            self.advance();
            self.skip_newlines();
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
    fn parse_slice_or_index(
        &mut self,
        target: Expr,
        target_start: usize,
    ) -> Result<Expr, ParseError> {
        self.advance(); // skip [
        self.skip_newlines();
        if self.at(&TokenKind::DotDot) {
            // expr[..end]
            self.advance();
            self.skip_newlines();
            let end = if self.at(&TokenKind::RBracket) {
                None
            } else {
                Some(Box::new(self.parse_expr(0)?))
            };
            self.skip_newlines();
            self.expect(TokenKind::RBracket, "`]`")?;
            let slice = target.with_slice(None, end);
            Ok(self.parsed_expr_from(target_start, slice))
        } else {
            // Parse start, stopping before `..` to handle slice syntax
            let first = self.parse_expr_without_range()?;
            self.skip_newlines();
            if self.at(&TokenKind::DotDot) {
                // expr[start..end] or expr[start..]
                self.advance();
                let end = if self.at(&TokenKind::RBracket) {
                    None
                } else {
                    self.skip_newlines();
                    Some(Box::new(self.parse_expr(0)?))
                };
                self.skip_newlines();
                self.expect(TokenKind::RBracket, "`]`")?;
                let slice = target.with_slice(Some(Box::new(first)), end);
                Ok(self.parsed_expr_from(target_start, slice))
            } else {
                // expr[i] — regular index
                self.expect(TokenKind::RBracket, "`]`")?;
                let index = target.index(first);
                Ok(self.parsed_expr_from(target_start, index))
            }
        }
    }

    fn parse_record_expr_fields(&mut self) -> Result<Vec<RecordFieldExpr>, ParseError> {
        let mut fields = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let field_start = self.pos;
            let name = self.expect_ident()?;
            if self.at(&TokenKind::Colon) {
                self.advance();
                let value = self.parse_expr(0)?;
                fields.push(RecordFieldExpr {
                    meta: self.consumed_meta(field_start, AstOrigin::User),
                    name,
                    value,
                });
            } else {
                // Shorthand: field_name instead of field_name: field_name
                fields.push(RecordFieldExpr {
                    meta: self.consumed_meta(field_start, AstOrigin::User),
                    name: name.clone(),
                    value: Expr::Ident(name).with_meta(self.consumed_meta(
                        field_start,
                        AstOrigin::Desugared("parser.record_shorthand"),
                    )),
                });
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
                let interpolation_start = self.pos;
                self.advance();
                let inner = self.parse_expr(0)?;
                self.expect(TokenKind::RParen, "`)`")?;
                let interpolation = Expr::QuoteInterpolate(Box::new(inner));
                let expr = self.parsed_expr_from(interpolation_start, interpolation);
                stmts.push(
                    Stmt::Expr(expr)
                        .with_meta(self.consumed_meta(interpolation_start, AstOrigin::User)),
                );
            } else {
                stmts.push(self.parse_stmt()?);
            }
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(stmts)
    }
    fn parse_ident_primary(&mut self, name: String) -> Result<Expr, ParseError> {
        let start_pos = self.pos;
        self.advance();
        let ident = self.parsed_expr_from(start_pos, Expr::Ident(name.clone()));
        if name == "type_name" && self.at(&TokenKind::LParen) {
            self.advance();
            let inner = self.parse_expr(0)?;
            self.expect(TokenKind::RParen, "`)` for type_name")?;
            return Ok(self.parsed_expr_from(start_pos, Expr::TypeOf(Box::new(inner))));
        }
        if name == "type_info" && self.at(&TokenKind::LParen) {
            self.advance();
            let ty = self.parse_type()?;
            self.expect(TokenKind::RParen, "`)` for type_info")?;
            return Ok(self.parsed_expr_from(start_pos, Expr::TypeInfo(ty)));
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
                return Ok(self.parsed_expr_from(start_pos, Expr::Turbofish(name, type_args, args)));
            } else {
                let field = self.expect_ident()?;
                let field_expr = ident.field(field);
                let mut e = self.parsed_expr_from(start_pos, field_expr);
                while self.at(&TokenKind::ColonColon) {
                    self.advance();
                    let field = self.expect_ident()?;
                    let field_expr = e.field(field);
                    e = self.parsed_expr_from(start_pos, field_expr);
                }
                if self.at(&TokenKind::LParen) {
                    self.advance();
                    let args = self.parse_args()?;
                    self.expect(TokenKind::RParen, "`)`")?;
                    let call = e.call(args);
                    e = self.parsed_expr_from(start_pos, call);
                }
                return Ok(e);
            }
        }
        let mut e = ident;
        loop {
            if self.at(&TokenKind::LParen) {
                self.advance();
                let args = self.parse_args()?;
                self.expect(TokenKind::RParen, "`)`")?;
                let call = e.call(args);
                e = self.parsed_expr_from(start_pos, call);
            } else if self.at(&TokenKind::Dot) {
                self.advance();
                if let TokenKind::Int(s) = &self.peek().kind {
                    let idx = s.replace('_', "").parse::<usize>().map_err(|_| {
                        ParseError::new("invalid tuple index", self.peek().line, self.peek().col)
                    })?;
                    self.advance();
                    let tuple_index = e.tuple_index(idx);
                    e = self.parsed_expr_from(start_pos, tuple_index);
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
                    let field_expr = e.field(field);
                    e = self.parsed_expr_from(start_pos, field_expr);
                }
            } else if self.at(&TokenKind::LBracket) {
                e = self.parse_slice_or_index(e, start_pos)?;
            } else if self.at(&TokenKind::LBrace) && self.allow_record_literal {
                if let Some(ty_name) = record_literal_type_name(&e) {
                    if ty_name
                        .chars()
                        .next()
                        .map(|c| c.is_uppercase())
                        .unwrap_or(false)
                    {
                        self.advance();
                        let fields = self.parse_record_expr_fields()?;
                        self.expect(TokenKind::RBrace, "`}`")?;
                        let record = Expr::Record {
                            ty: Some(ty_name),
                            fields,
                        };
                        e = self.parsed_expr_from(start_pos, record);
                        continue;
                    }
                }
                break;
            } else {
                break;
            }
        }
        // Delegate to parse_postfix for `?` / `?.field` / `as Type` handling.
        // parse_ident_primary handles `.` `(` `[` `{` inline above, but the
        // `?` and `?.` operators need to be handled after the ident-primary
        // loop exits. parse_postfix will loop again but the tokens it looks
        // for (`?`, `(`, `.`, `[`) will already be consumed if they appeared
        // above, so it will quickly break on the first non-matching token.
        self.parse_postfix(e, start_pos)
    }

    /// Parse a `[expr]` / `[expr for var in iter]` / `[expr, ...]` primary expression.
    fn parse_bracket_primary(&mut self) -> Result<Expr, ParseError> {
        let start_pos = self.pos;
        self.advance();
        self.skip_newlines();
        let first_expr = if self.at(&TokenKind::RBracket) {
            self.advance();
            let list = self.parsed_expr_from(start_pos, Expr::List(vec![]));
            return self.parse_postfix(list, start_pos);
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
            let comprehension = self.parsed_expr_from(
                start_pos,
                Expr::Comprehension {
                    expr: Box::new(first_expr),
                    var,
                    iter: Box::new(iter),
                    guard,
                },
            );
            self.parse_postfix(comprehension, start_pos)
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
            let list = self.parsed_expr_from(start_pos, Expr::List(elems));
            self.parse_postfix(list, start_pos)
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
            Err(_) => {
                self.pos = saved;
                self.recursion_depth.set(saved_depth);
                return None;
            }
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
            Err(_) => {
                self.pos = saved;
                self.recursion_depth.set(saved_depth);
                return None;
            }
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
                    Err(_) => {
                        self.pos = saved;
                        self.recursion_depth.set(saved_depth);
                        return None;
                    }
                };
                if !self.at(&TokenKind::Colon) {
                    self.pos = saved;
                    self.recursion_depth.set(saved_depth);
                    return None;
                }
                self.advance();
                let val = match self.parse_expr(0) {
                    Ok(v) => v,
                    Err(_) => {
                        self.pos = saved;
                        self.recursion_depth.set(saved_depth);
                        return None;
                    }
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
            Err(_) => {
                self.pos = saved;
                self.recursion_depth.set(saved_depth);
                return None;
            }
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
                Err(_) => {
                    self.pos = saved;
                    self.recursion_depth.set(saved_depth);
                    return None;
                }
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
        if matches!(rhs.unlocated(), Expr::Ident(_) | Expr::Field(_, _)) {
            return Ok(Expr::Call(Box::new(rhs), vec![lhs]));
        }

        match rhs.into_unlocated() {
            Expr::Call(callee, args) => {
                let mut new_args = vec![lhs];
                new_args.extend(args);
                Ok(Expr::Call(callee, new_args))
            }
            Expr::Turbofish(func_name, type_args, mut turbofish_args) => {
                // PA-C2: a |> name::<T>(b, c) → Turbofish(name, [T], [a, b, c])
                // lhs is inserted as the first argument of the turbofish call
                turbofish_args.insert(0, lhs);
                Ok(Expr::Turbofish(func_name, type_args, turbofish_args))
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

/// Extract the type name from an expression that could start a record literal.
/// Handles `MyStruct`, `MyModule::MyStruct`, etc.
fn record_literal_type_name(expr: &Expr) -> Option<String> {
    match expr.unlocated() {
        Expr::Ident(name) => Some(name.clone()),
        Expr::Field(obj, field) => {
            let prefix = record_literal_type_name(obj)?;
            Some(format!("{}::{}", prefix, field))
        }
        _ => None,
    }
}

/// Returns true if the token kind starts a statement (and therefore parsing as block).
fn is_stmt_start_keyword(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Let
            | TokenKind::If
            | TokenKind::While
            | TokenKind::For
            | TokenKind::Return
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Match
            | TokenKind::Arena
            | TokenKind::Unsafe
            | TokenKind::Spawn
            | TokenKind::Await
            | TokenKind::Alloc
            | TokenKind::Drop
            | TokenKind::Steps
            | TokenKind::Parasteps
            | TokenKind::Failure
            | TokenKind::Requires
            | TokenKind::Ensures
            | TokenKind::Math
            | TokenKind::Invariant
            | TokenKind::Desc
            | TokenKind::Rule
            | TokenKind::Loop
            | TokenKind::Do
            | TokenKind::Pinned
            | TokenKind::Delegate
            | TokenKind::Shared
            | TokenKind::Const
            | TokenKind::Func
            | TokenKind::Type
            | TokenKind::Module
            | TokenKind::Extern
            | TokenKind::Use
    )
}

/// Delegates to the canonical keyword check in `lexer::keywords`.
/// Add new keywords there, not here.
fn is_keyword_token(kind: &TokenKind) -> bool {
    crate::lexer::is_keyword_kind(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::token::TokenKind;
    use crate::lexer::Lexer;
    use crate::span::{SourceId, Span};

    fn parse_source_expr(source: &str, source_id: SourceId) -> Expr {
        let tokens = Lexer::new(source).tokenize().expect("lex expression");
        let mut parser = Parser::new_with_source(tokens, source_id);
        let expr = parser.parse_expr(0).expect("parse expression");
        assert!(
            parser.at(&TokenKind::Eof),
            "expression left trailing tokens"
        );
        expr
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

    fn assert_user_span(expr: &Expr, expected: Span) {
        let meta = expr.meta().expect("parsed Expr must have metadata");
        assert_eq!(meta.origin, AstOrigin::User);
        assert_eq!(meta.span, expected);
    }

    #[test]
    fn nested_call_field_index_record_and_named_arg_have_exact_metadata() {
        let source = "root.call(left + 2, named = Rec { field: arr[index], short }).tail";
        let source_id = SourceId::new(71);
        let expr = parse_source_expr(source, source_id);
        assert_user_span(&expr, span_for(source, source, source_id));

        let Expr::Field(call, tail) = expr.unlocated() else {
            panic!("expected trailing field access");
        };
        assert_eq!(tail, "tail");
        let call_source = source.strip_suffix(".tail").expect("suffix");
        assert_user_span(call, span_for(source, call_source, source_id));

        let Expr::Call(callee, args) = call.unlocated() else {
            panic!("expected call");
        };
        assert_eq!(args.len(), 2);
        assert_user_span(callee, span_for(source, "root.call", source_id));
        let Expr::Field(receiver, field) = callee.unlocated() else {
            panic!("expected method field");
        };
        assert_eq!(field, "call");
        assert_user_span(receiver, span_for(source, "root", source_id));

        let binary = &args[0];
        assert_user_span(binary, span_for(source, "left + 2", source_id));
        let Expr::Binary(BinOp::Add, left, right) = binary.unlocated() else {
            panic!("expected binary argument");
        };
        assert_user_span(left, span_for(source, "left", source_id));
        assert_user_span(right, span_for(source, "2", source_id));

        let named = &args[1];
        assert_user_span(
            named,
            span_for(
                source,
                "named = Rec { field: arr[index], short }",
                source_id,
            ),
        );
        let Expr::NamedArg(name, record) = named.unlocated() else {
            panic!("expected named argument");
        };
        assert_eq!(name, "named");
        assert_user_span(
            record,
            span_for(source, "Rec { field: arr[index], short }", source_id),
        );
        let Expr::Record { fields, .. } = record.unlocated() else {
            panic!("expected record value");
        };
        assert_eq!(fields.len(), 2);
        assert_eq!(
            fields[0].meta,
            AstNodeMeta::new(
                span_for(source, "field: arr[index]", source_id),
                AstOrigin::User,
            )
        );
        assert_eq!(
            fields[1].meta,
            AstNodeMeta::new(span_for(source, "short", source_id), AstOrigin::User)
        );
        let indexed = &fields[0].value;
        assert_user_span(indexed, span_for(source, "arr[index]", source_id));
        let Expr::Index(target, index) = indexed.unlocated() else {
            panic!("expected indexed field value");
        };
        assert_user_span(target, span_for(source, "arr", source_id));
        assert_user_span(index, span_for(source, "index", source_id));
        let shorthand_meta = fields[1]
            .value
            .meta()
            .expect("record shorthand value metadata");
        assert_eq!(
            shorthand_meta.origin,
            AstOrigin::Desugared("parser.record_shorthand")
        );
        assert_eq!(shorthand_meta.span, span_for(source, "short", source_id));
    }

    #[test]
    fn match_arm_bodies_and_nested_calls_have_exact_metadata() {
        let source = "match value {\n    Some(x) if x > 0 => {\n        build(x)\n    },\n    _ => fallback\n}";
        let source_id = SourceId::new(72);
        let expr = parse_source_expr(source, source_id);
        let Expr::Match(scrutinee, arms) = expr.unlocated() else {
            panic!("expected match");
        };
        assert_user_span(scrutinee, span_for(source, "value", source_id));
        assert_eq!(arms.len(), 2);
        assert_eq!(
            arms[0].meta,
            AstNodeMeta::new(
                span_for(
                    source,
                    "Some(x) if x > 0 => {\n        build(x)\n    }",
                    source_id,
                ),
                AstOrigin::User,
            )
        );
        assert_eq!(
            arms[1].meta,
            AstNodeMeta::new(
                span_for(source, "_ => fallback", source_id),
                AstOrigin::User
            )
        );

        let guard = arms[0].guard.as_ref().expect("guard");
        assert_user_span(guard, span_for(source, "x > 0", source_id));
        assert_user_span(
            &arms[0].body,
            span_for(source, "{\n        build(x)\n    }", source_id),
        );
        let Expr::Block(first_body) = arms[0].body.unlocated() else {
            panic!("expected braced arm block");
        };
        let Stmt::Expr(call) = first_body[0].unlocated() else {
            panic!("expected call expression statement");
        };
        assert_user_span(call, span_for(source, "build(x)", source_id));
        let Expr::Call(callee, args) = call.unlocated() else {
            panic!("expected arm call");
        };
        assert_user_span(callee, span_for(source, "build", source_id));
        assert_user_span(&args[0], Span::new(3, 15, 3, 16).with_source(source_id));

        assert_user_span(&arms[1].body, span_for(source, "fallback", source_id));
        let Expr::Block(second_body) = arms[1].body.unlocated() else {
            panic!("expected normalized expression arm block");
        };
        let Stmt::Expr(fallback) = second_body[0].unlocated() else {
            panic!("expected fallback expression statement");
        };
        assert_user_span(fallback, span_for(source, "fallback", source_id));
    }

    #[test]
    fn escaped_string_and_multiline_binary_use_lexer_end_positions() {
        let source_id = SourceId::new(73);
        let escaped_source = r#""line\n\"quoted\"""#;
        let escaped = parse_source_expr(escaped_source, source_id);
        assert_user_span(
            &escaped,
            span_for(escaped_source, escaped_source, source_id),
        );

        let multiline_source = "alpha +\n    beta * gamma";
        let multiline = parse_source_expr(multiline_source, source_id);
        assert_user_span(
            &multiline,
            span_for(multiline_source, multiline_source, source_id),
        );
        let Expr::Binary(BinOp::Add, alpha, product) = multiline.unlocated() else {
            panic!("expected multiline addition");
        };
        assert_user_span(alpha, span_for(multiline_source, "alpha", source_id));
        assert_user_span(
            product,
            span_for(multiline_source, "beta * gamma", source_id),
        );
        let Expr::Binary(BinOp::Mul, beta, gamma) = product.unlocated() else {
            panic!("expected nested product");
        };
        assert_user_span(beta, span_for(multiline_source, "beta", source_id));
        assert_user_span(gamma, span_for(multiline_source, "gamma", source_id));
    }

    #[test]
    fn fstring_interpolation_metadata_is_rebased_to_enclosing_source() {
        let source = "f\"sum={left +\n right}\"";
        let source_id = SourceId::new(74);
        let expr = parse_source_expr(source, source_id);
        assert_user_span(&expr, span_for(source, source, source_id));
        let Expr::Literal(Lit::FString(parts)) = expr.unlocated() else {
            panic!("expected f-string literal");
        };
        let interpolation = parts
            .iter()
            .find_map(|part| match part {
                FStringPart::Interp(expr) => Some(expr),
                FStringPart::Text(_) => None,
            })
            .expect("interpolation");
        assert_user_span(interpolation, span_for(source, "left +\n right", source_id));
        let Expr::Binary(BinOp::Add, left, right) = interpolation.unlocated() else {
            panic!("expected interpolation binary");
        };
        assert_user_span(left, span_for(source, "left", source_id));
        assert_user_span(right, span_for(source, "right", source_id));
    }

    #[test]
    fn is_stmt_start_keyword_covers_all_keywords() {
        // audit (MEDIUM): is_stmt_start_keyword previously omitted many
        // keywords (Loop, Do, Pinned, Delegate, Shared, Const, Func, Type,
        // Module, Extern, Use). This test ensures they are recognized so
        // that block expressions containing these statements as their first
        // element are correctly parsed as blocks, not as map/set literals.
        for kind in [
            TokenKind::Let,
            TokenKind::If,
            TokenKind::While,
            TokenKind::For,
            TokenKind::Return,
            TokenKind::Break,
            TokenKind::Continue,
            TokenKind::Match,
            TokenKind::Loop,
            TokenKind::Do,
            TokenKind::Pinned,
            TokenKind::Delegate,
            TokenKind::Shared,
            TokenKind::Const,
            TokenKind::Func,
            TokenKind::Type,
            TokenKind::Module,
            TokenKind::Extern,
            TokenKind::Use,
            TokenKind::Arena,
            TokenKind::Unsafe,
            TokenKind::Spawn,
            TokenKind::Await,
            TokenKind::Alloc,
            TokenKind::Drop,
            TokenKind::Steps,
            TokenKind::Parasteps,
            TokenKind::Failure,
            TokenKind::Requires,
            TokenKind::Ensures,
            TokenKind::Math,
            TokenKind::Invariant,
            TokenKind::Desc,
            TokenKind::Rule,
        ] {
            assert!(
                is_stmt_start_keyword(&kind),
                "{kind:?} should be a statement-start keyword"
            );
        }
    }
}
