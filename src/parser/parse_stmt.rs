// Parser uses .expect() on self.expect() returns as an intentional pattern.
#![cfg_attr(not(test), allow(clippy::unwrap_used))]
use super::*;

impl Parser {
    pub(crate) fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.skip_newlines();
        let start_pos = self.pos;
        let stmt = self.parse_stmt_kind()?;
        Ok(self.parsed_stmt_from(start_pos, stmt))
    }

    fn parsed_stmt_from(&self, start_pos: usize, stmt: Stmt) -> Stmt {
        stmt.with_meta(self.consumed_meta(start_pos, AstOrigin::User))
    }

    fn parse_stmt_kind(&mut self) -> Result<Stmt, ParseError> {
        match self.peek_kind() {
            TokenKind::Let | TokenKind::Const => self.parse_let(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Break => {
                self.advance();
                let val = if self.peek_kind() == &TokenKind::Semi
                    || self.peek_kind() == &TokenKind::Newline
                    || self.peek_kind() == &TokenKind::RBrace
                    || self.peek_kind() == &TokenKind::Eof
                {
                    None
                } else {
                    Some(self.parse_expr(0)?)
                };
                self.match_semi();
                Ok(Stmt::Break(val))
            }
            TokenKind::Continue => {
                self.advance();
                self.match_semi();
                Ok(Stmt::Continue)
            }
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::Loop => self.parse_loop(),
            TokenKind::For => self.parse_for(),
            TokenKind::Arena => self.parse_arena(),
            TokenKind::Unsafe => self.parse_unsafe(),
            TokenKind::Alloc if self.is_alloc_block() => self.parse_alloc(),
            TokenKind::Shared => self.parse_shared_let(SharedKind::Shared),
            TokenKind::LocalShared => self.parse_shared_let(SharedKind::LocalShared),
            TokenKind::Weak => self.parse_shared_let(SharedKind::Weak),
            TokenKind::WeakLocal => self.parse_shared_let(SharedKind::WeakLocal),
            TokenKind::Mms => self.parse_mms_block(),
            TokenKind::LBrace => {
                self.advance();
                Ok(Stmt::Block(self.parse_block()?))
            }
            TokenKind::Desc => {
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    let s = self.parse_brace_block_content()?;
                    Ok(Stmt::Desc(s, span))
                } else {
                    let s = self.expect_string()?;
                    self.match_semi();
                    Ok(Stmt::Desc(s, span))
                }
            }
            TokenKind::Rule => {
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    let s = self.parse_brace_block_content()?;
                    Ok(Stmt::Rule(s, span))
                } else {
                    let s = self.expect_string()?;
                    self.match_semi();
                    Ok(Stmt::Rule(s, span))
                }
            }
            TokenKind::Ellipsis => {
                self.advance();
                if !self.is_sketch() {
                    return Err(ParseError::new(
                        "`...` placeholder is not allowed in production mode (.mimi); implement or use sketch mode (.mms)",
                        self.tokens[self.pos.saturating_sub(1)].line,
                        self.tokens[self.pos.saturating_sub(1)].col,
                    ));
                }
                self.match_semi();
                Ok(Stmt::Ellipsis)
            }
            TokenKind::Drop => {
                self.advance();
                self.expect(TokenKind::LParen, "`(`")?;
                let expr = self.parse_expr(0)?;
                self.expect(TokenKind::RParen, "`)`")?;
                self.match_semi();
                Ok(Stmt::Drop(expr))
            }
            TokenKind::Parasteps => {
                self.advance();
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{`")?;
                let body = self.parse_block()?;
                self.match_semi();
                Ok(Stmt::Parasteps(body))
            }
            TokenKind::Func => {
                let func = self.parse_func()?;
                self.match_semi();
                Ok(Stmt::Func(func))
            }
            TokenKind::Do => {
                self.advance();
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{`")?;
                let body = self.parse_block()?;
                Ok(Stmt::Do(body))
            }
            TokenKind::Delegate => {
                self.advance();
                let kind = match self.peek_kind() {
                    k if *k == TokenKind::View => {
                        self.advance();
                        DelegateKind::View
                    }
                    k if *k == TokenKind::Mutate => {
                        self.advance();
                        DelegateKind::Mutate
                    }
                    k if *k == TokenKind::Consume => {
                        self.advance();
                        DelegateKind::Consume
                    }
                    _ => {
                        let tok = self.peek();
                        return Err(ParseError::new(
                            "expected `view`, `mutate`, or `consume` after `delegate`",
                            tok.line,
                            tok.col,
                        ));
                    }
                };
                self.expect(TokenKind::LParen, "`(`")?;
                let expr = self.parse_expr(0)?;
                self.expect(TokenKind::RParen, "`)`")?;
                // expect "to" keyword
                if !matches!(self.peek_kind(), TokenKind::Ident(s) if s == "to") {
                    let tok = self.peek();
                    return Err(ParseError::new(
                        format!("expected `to`, found {}", tok.kind),
                        tok.line,
                        tok.col,
                    ));
                }
                self.advance();
                let target = self.expect_ident()?;
                self.match_semi();
                Ok(Stmt::Delegate { kind, expr, target })
            }
            TokenKind::Pinned => {
                self.advance();
                self.expect(TokenKind::LParen, "`(`")?;
                let expr = self.parse_expr(0)?;
                let timeout = if self.at(&TokenKind::Comma) {
                    self.advance();
                    // parse timeout = 5s
                    self.expect_ident()?; // skip "timeout"
                    self.expect(TokenKind::Eq, "`=`")?;
                    let t = self.parse_expr(0)?;
                    Some(t)
                } else {
                    None
                };
                self.expect(TokenKind::RParen, "`)`")?;
                let var = if self.at(&TokenKind::PipeArrow) || self.at(&TokenKind::BitOr) {
                    self.advance();
                    let v = self.expect_ident()?;
                    if self.at(&TokenKind::PipeArrow) || self.at(&TokenKind::BitOr) {
                        self.advance();
                    }
                    Some(v)
                } else {
                    None
                };
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{`")?;
                let body = self.parse_block()?;
                Ok(Stmt::Pinned {
                    expr,
                    timeout,
                    var,
                    body,
                })
            }
            TokenKind::Ident(s) if s == "on" => {
                self.advance();
                self.expect(TokenKind::Failure, "`failure`")?;
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{`")?;
                let body = self.parse_block()?;
                Ok(Stmt::OnFailure(body))
            }
            _ => {
                let target_start = self.pos;
                let expr = self.parse_expr(0)?;
                if self.at(&TokenKind::Eq) {
                    self.advance();
                    let value = self.parse_expr(0)?;
                    self.match_semi();
                    Ok(Stmt::Assign {
                        target: expr,
                        value,
                    })
                } else if self.at(&TokenKind::PlusEq)
                    || self.at(&TokenKind::MinusEq)
                    || self.at(&TokenKind::StarEq)
                    || self.at(&TokenKind::SlashEq)
                    || self.at(&TokenKind::BitAndEq)
                    || self.at(&TokenKind::BitOrEq)
                    || self.at(&TokenKind::BitXorEq)
                {
                    let op_token = self.peek().kind.clone();
                    self.advance();
                    let value = self.parse_expr(0)?;
                    let op = match op_token {
                        TokenKind::PlusEq => BinOp::Add,
                        TokenKind::MinusEq => BinOp::Sub,
                        TokenKind::StarEq => BinOp::Mul,
                        TokenKind::SlashEq => BinOp::Div,
                        TokenKind::BitAndEq => BinOp::BitAnd,
                        TokenKind::BitOrEq => BinOp::BitOr,
                        TokenKind::BitXorEq => BinOp::BitXor,
                        _ => {
                            return Err(ParseError::new(
                                "unexpected token in statement parsing".to_string(),
                                0,
                                0,
                            ))
                        }
                    };
                    // The compound-assignment desugaring clones the target
                    // expression so the original user identifier is retained
                    // as the assignment target. The cloned operand inside the
                    // binary RHS must be re-tagged as Desugared so the resolved
                    // IR gives it a generated NodeId (origin + rule + role)
                    // instead of duplicating the user identifier's NodeId
                    // (which is keyed on the user span and would collide).
                    // Preserve the target's span for diagnostics but mark the
                    // origin as Desugared so it is not treated as a second
                    // user-written occurrence of the same identifier.
                    let cloned_target = match expr.meta() {
                        Some(meta) => expr.clone().with_meta(AstNodeMeta::inherited(
                            meta.span,
                            AstOrigin::Desugared("parser.compound_assignment.operand"),
                        )),
                        None => expr.clone(),
                    };
                    let binary = cloned_target.binary(op, value);
                    let rhs = binary.with_meta(self.consumed_meta(
                        target_start,
                        AstOrigin::Desugared("parser.compound_assignment"),
                    ));
                    self.match_semi();
                    Ok(Stmt::Assign {
                        target: expr,
                        value: rhs,
                    })
                } else {
                    self.match_semi();
                    Ok(Stmt::Expr(expr))
                }
            }
        }
    }

    fn parse_arena(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::Arena, "`arena`")?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let body = self.parse_block()?;
        self.match_semi();
        Ok(Stmt::Arena(body))
    }

    fn parse_unsafe(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::Unsafe, "`unsafe`")?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let body = self.parse_block()?;
        self.match_semi();
        Ok(Stmt::Unsafe(body))
    }

    fn parse_alloc(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::Alloc, "`alloc`")?;
        self.expect(TokenKind::LParen, "`(`")?;
        let kind_tok = self.peek().clone();
        let kind = match &kind_tok.kind {
            TokenKind::Ident(name) => {
                self.advance();
                match name.as_str() {
                    "System" => AllocKind::System,
                    "Arena" => AllocKind::Arena,
                    "Bump" => AllocKind::Bump,
                    _ => {
                        return Err(ParseError::new(
                            format!(
                                "expected allocator type (System, Arena, Bump), found `{}`",
                                name
                            ),
                            kind_tok.line,
                            kind_tok.col,
                        ))
                    }
                }
            }
            TokenKind::Arena => {
                self.advance();
                AllocKind::Arena
            }
            _ => {
                return Err(ParseError::new(
                    format!(
                        "expected allocator type (System, Arena, Bump), found {}",
                        kind_tok.kind
                    ),
                    kind_tok.line,
                    kind_tok.col,
                ))
            }
        };
        self.expect(TokenKind::RParen, "`)`")?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let body = self.parse_block()?;
        Ok(Stmt::Alloc { kind, body })
    }

    /// Parse content inside { ... } as raw text (for desc/rule blocks)
    fn parse_brace_block_content(&mut self) -> Result<String, ParseError> {
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut text = String::new();
        let mut depth = 1u32;
        let mut first_line = None;
        let mut first_col = None;
        while !self.at(&TokenKind::Eof) {
            let tok = self.peek();
            match &tok.kind {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            let t = tok.kind.source_text();
            if t == "\n" {
                text.push('\n');
            } else if !t.is_empty() {
                if first_line.is_none() {
                    first_line = Some(tok.line);
                    first_col = Some(tok.col);
                }
                let base_col = first_col.unwrap_or(tok.col);
                let relative_col = tok.col.saturating_sub(base_col);
                if text.ends_with('\n') || text.is_empty() {
                    text.push_str(&" ".repeat(relative_col));
                } else {
                    text.push(' ');
                }
                text.push_str(t);
            }
            self.advance();
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        self.match_semi();
        Ok(text.trim().to_string())
    }

    fn parse_mms_block(&mut self) -> Result<Stmt, ParseError> {
        let mms_line = self.peek().line;
        let mms_col = self.peek().col;
        self.expect(TokenKind::Mms, "`mms`")?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let content = if matches!(self.peek_kind(), TokenKind::String(_)) {
            self.expect_string()?
        } else {
            let mut text = String::new();
            let mut depth = 1u32;
            let mut first_line = None;
            let mut first_col = None;
            while !self.at(&TokenKind::Eof) {
                let tok = self.peek();
                match &tok.kind {
                    TokenKind::LBrace => depth += 1,
                    TokenKind::RBrace => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                let t = tok.kind.source_text();
                if t == "\n" {
                    text.push('\n');
                } else if !t.is_empty() {
                    if first_line.is_none() {
                        first_line = Some(tok.line);
                        first_col = Some(tok.col);
                    }
                    let base_col = first_col.unwrap_or(tok.col);
                    let relative_col = tok.col.saturating_sub(base_col);
                    if text.ends_with('\n') || text.is_empty() {
                        text.push_str(&" ".repeat(relative_col));
                    } else {
                        text.push(' ');
                    }
                    text.push_str(t);
                }
                self.advance();
            }
            text.trim().to_string()
        };
        self.expect(TokenKind::RBrace, "`}`")?;
        self.match_semi();
        let span = self.single_span(mms_line, mms_col);
        Ok(Stmt::MmsBlock { content, span })
    }

    fn parse_shared_let(&mut self, kind: SharedKind) -> Result<Stmt, ParseError> {
        self.advance();
        let tok = self.peek().clone();
        let name = match &tok.kind {
            TokenKind::Ident(s) => {
                self.advance();
                s.clone()
            }
            _ => {
                return Err(ParseError::new(
                    format!(
                        "expected variable name after '{}'",
                        match kind {
                            SharedKind::Shared => "shared",
                            SharedKind::LocalShared => "local_shared",
                            SharedKind::Weak | SharedKind::WeakLocal => "weak",
                        }
                    ),
                    tok.line,
                    tok.col,
                ))
            }
        };
        let ty = if self.at(&TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(TokenKind::Eq, "`=`")?;
        let init = self.parse_expr(0)?;
        self.match_semi();
        Ok(Stmt::SharedLet {
            kind,
            name,
            ty,
            init,
        })
    }

    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        let is_const = self.at(&TokenKind::Const);
        if is_const {
            self.advance();
        } else {
            self.expect(TokenKind::Let, "`let`")?;
        }
        let mut_ = if is_const {
            false
        } else {
            let m = self.at(&TokenKind::Mut);
            if m {
                self.advance();
            }
            m
        };
        let ref_ = self.at(&TokenKind::Ref);
        if ref_ {
            self.advance();
        }
        let pat = self.parse_pattern()?;
        let ty = if self.at(&TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let init = if self.at(&TokenKind::Eq) {
            self.advance();
            self.skip_newlines(); // PA-C4: allow newline after `=` in let binding
            if self.at(&TokenKind::Semi) || self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof)
            {
                return Err(ParseError::new(
                    "expected expression after `=` in let binding",
                    self.peek().line,
                    self.peek().col,
                ));
            }
            Some(self.parse_expr(0)?)
        } else {
            None
        };
        self.match_semi();
        Ok(Stmt::Let {
            pat,
            ty,
            init,
            mut_,
            ref_,
        })
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::Return, "`return`")?;
        let expr = if self.at(&TokenKind::Semi)
            || self.at(&TokenKind::Newline)
            || self.at(&TokenKind::RBrace)
        {
            None
        } else {
            Some(self.parse_expr(0)?)
        };
        self.match_semi();
        Ok(Stmt::Return(expr))
    }

    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        self.check_depth()?;
        self.inc_depth();
        let result = self.parse_if_inner();
        self.dec_depth();
        result
    }

    fn parse_if_inner(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::If, "`if`")?;
        let cond = self.parse_expr(0)?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let then_ = self.parse_block()?;
        self.skip_newlines();
        let else_ = if self.at(&TokenKind::Else) {
            self.advance();
            self.skip_newlines();
            if self.at(&TokenKind::If) {
                let elif = self.parse_if()?;
                Some(vec![elif])
            } else {
                self.expect(TokenKind::LBrace, "`{`")?;
                Some(self.parse_block()?)
            }
        } else {
            None
        };
        Ok(Stmt::If { cond, then_, else_ })
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::While, "`while`")?;
        self.skip_newlines();
        // Check for while-let: `while let pattern = expr { body }`
        if self.at(&TokenKind::Let) {
            self.advance(); // consume 'let'
            let pat = self.parse_pattern()?;
            self.skip_newlines();
            self.expect(TokenKind::Eq, "`=`")?;
            let init = self.parse_expr(0)?;
            self.skip_newlines();
            self.expect(TokenKind::LBrace, "`{`")?;
            let body = self.parse_block()?;
            Ok(Stmt::WhileLet { pat, init, body })
        } else {
            let cond = self.parse_expr(0)?;
            self.skip_newlines();
            self.expect(TokenKind::LBrace, "`{`")?;
            let body = self.parse_block()?;
            Ok(Stmt::While { cond, body })
        }
    }

    fn parse_loop(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::Loop, "`loop`")?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let body = self.parse_block()?;
        Ok(Stmt::Loop(body))
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::For, "`for`")?;
        let var = self.expect_ident()?;
        self.expect(TokenKind::In, "`in`")?;
        let iterable = self.parse_expr(0)?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let body = self.parse_block()?;
        Ok(Stmt::For {
            var,
            iterable,
            body,
        })
    }

    pub(crate) fn expect_string(&mut self) -> Result<String, ParseError> {
        let tok = self.peek();
        match &tok.kind {
            TokenKind::String(s) => {
                let s = s.clone();
                self.advance();
                Ok(s)
            }
            _ => Err(ParseError::new(
                format!("expected string literal, found {}", tok.kind),
                tok.line,
                tok.col,
            )),
        }
    }

    pub(crate) fn parse_fstring_parts(
        &self,
        raw: &str,
        base_line: usize,
        base_col: usize,
    ) -> Result<Vec<crate::ast::FStringPart>, ParseError> {
        use crate::ast::FStringPart;
        let mut parts = Vec::new();
        let mut chars = raw.chars().peekable();
        let mut current_text = String::new();
        let raw_char_count = raw.chars().count();

        while let Some(&c) = chars.peek() {
            if c == '{' {
                let open_offset = raw_char_count - chars.clone().count();
                if !current_text.is_empty() {
                    parts.push(FStringPart::Text(current_text.clone()));
                    current_text.clear();
                }
                chars.next();
                let mut expr_str = String::new();
                let mut depth = 1;
                while let Some(&c) = chars.peek() {
                    if c == '{' {
                        depth += 1;
                    } else if c == '}' {
                        depth -= 1;
                        if depth == 0 {
                            chars.next();
                            break;
                        }
                    }
                    expr_str.push(c);
                    chars.next();
                }
                if depth != 0 {
                    return Err(ParseError::new(
                        "unterminated interpolation in f-string",
                        base_line,
                        base_col,
                    ));
                }
                // LX-H8: empty interpolation f"{}" is invalid.
                if expr_str.trim().is_empty() {
                    return Err(ParseError::new(
                        "empty interpolation in f-string (f\"{}\" is not allowed)",
                        base_line,
                        base_col,
                    ));
                }
                let mut tokens = crate::lexer::Lexer::new(&expr_str)
                    .tokenize()
                    .map_err(|e| ParseError::new(e.to_string(), base_line, base_col))?;
                // The interpolation lexer starts at 1:1 in its fragment. Rebase
                // every token (including its exact half-open end) onto the
                // enclosing f-string so nested Expr metadata names the real
                // source and coordinates rather than an anonymous fragment.
                let mut expr_line = base_line;
                let mut expr_col = base_col + 2; // skip the leading `f"`
                for ch in raw.chars().take(open_offset + 1) {
                    if ch == '\n' {
                        expr_line += 1;
                        expr_col = 1;
                    } else {
                        expr_col += 1;
                    }
                }
                let rebase = |line: usize, col: usize| {
                    if line == 1 {
                        (expr_line, expr_col + col.saturating_sub(1))
                    } else {
                        (expr_line + line - 1, col)
                    }
                };
                for token in &mut tokens {
                    let (line, col) = rebase(token.line, token.col);
                    let (end_line, end_col) = rebase(token.end_line, token.end_col);
                    token.line = line;
                    token.col = col;
                    token.end_line = end_line;
                    token.end_col = end_col;
                }
                // F-H2: interpolation sub-parser must consume the entire fragment.
                let mut sub = Parser::new_with_source(tokens, self.source_id);
                let expr = sub.parse_expr(0)?;
                if !sub.at(&TokenKind::Eof) {
                    return Err(ParseError::new(
                        format!("trailing tokens in f-string interpolation: `{}`", expr_str),
                        base_line,
                        base_col,
                    ));
                }
                parts.push(FStringPart::Interp(expr));
            } else if c == '\\' {
                chars.next();
                if let Some(&esc) = chars.peek() {
                    match esc {
                        'n' => current_text.push('\n'),
                        't' => current_text.push('\t'),
                        'r' => current_text.push('\r'),
                        '0' => current_text.push('\0'),
                        '\\' => current_text.push('\\'),
                        '"' => current_text.push('"'),
                        '{' => current_text.push('{'),
                        '}' => current_text.push('}'),
                        other => {
                            // Unknown escape: keep both chars so diagnostics remain visible.
                            current_text.push('\\');
                            current_text.push(other);
                        }
                    }
                    chars.next();
                }
            } else {
                current_text.push(c);
                chars.next();
            }
        }
        if !current_text.is_empty() {
            parts.push(FStringPart::Text(current_text));
        }
        Ok(parts)
    }

    pub(crate) fn parse_brace_block(&mut self) -> Result<Block, ParseError> {
        self.parse_block_with_terminator(TokenKind::RBrace, "`}`")
    }

    pub(crate) fn parse_indent_block(&mut self) -> Result<Block, ParseError> {
        self.parse_block_with_terminator(TokenKind::Dedent, "dedent")
    }

    fn parse_block_with_terminator(
        &mut self,
        terminator: TokenKind,
        label: &str,
    ) -> Result<Block, ParseError> {
        // In recovery mode, catch statement errors and continue
        if self.recovery_mode {
            return self.parse_block_with_recovery(terminator, label);
        }
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&terminator) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&terminator) || self.at(&TokenKind::Eof) {
                break;
            }
            if self.at(&TokenKind::Requires) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                let expr = self.parse_expr(0)?;
                // CRITICAL #16 fix: consume trailing semicolons after
                // contract clauses. Previously, `requires: x > 0;` would
                // leave the `;` unconsumed, causing cascade parse errors.
                while self.at(&TokenKind::Semi) {
                    self.advance();
                }
                stmts.push(self.parsed_stmt_from(start_pos, Stmt::Requires(expr, span)));
                continue;
            }
            if self.at(&TokenKind::Ensures) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                let expr = self.parse_expr(0)?;
                // CRITICAL #16 fix: consume trailing semicolons.
                while self.at(&TokenKind::Semi) {
                    self.advance();
                }
                stmts.push(self.parsed_stmt_from(start_pos, Stmt::Ensures(expr, span)));
                continue;
            }
            if self.at(&TokenKind::Invariant) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                let expr = self.parse_expr(0)?;
                // CRITICAL #16 fix: consume trailing semicolons.
                while self.at(&TokenKind::Semi) {
                    self.advance();
                }
                stmts.push(self.parsed_stmt_from(start_pos, Stmt::Invariant(expr, span)));
                continue;
            }
            if self.at(&TokenKind::Math) {
                let start_pos = self.pos;
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{` for math block")?;
                let mut exprs = Vec::new();
                self.skip_newlines();
                while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
                    exprs.push(self.parse_expr(0)?);
                    self.match_semi();
                    self.skip_newlines();
                }
                self.expect(TokenKind::RBrace, "`}`")?;
                self.match_semi();
                stmts.push(self.parsed_stmt_from(start_pos, Stmt::Math(exprs)));
                continue;
            }
            if self.at(&TokenKind::Desc) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    let s = self.parse_brace_block_content()?;
                    stmts.push(self.parsed_stmt_from(start_pos, Stmt::Desc(s, span)));
                } else {
                    let s = self.expect_string()?;
                    self.match_semi();
                    stmts.push(self.parsed_stmt_from(start_pos, Stmt::Desc(s, span)));
                }
                continue;
            }
            if self.at(&TokenKind::Rule) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    let s = self.parse_brace_block_content()?;
                    stmts.push(self.parsed_stmt_from(start_pos, Stmt::Rule(s, span)));
                } else {
                    let s = self.expect_string()?;
                    self.match_semi();
                    stmts.push(self.parsed_stmt_from(start_pos, Stmt::Rule(s, span)));
                }
                continue;
            }
            stmts.push(self.parse_stmt()?);
        }
        self.expect(terminator, label)?;
        Ok(stmts)
    }

    /// Parse a block with error recovery: catches statement errors and continues.
    /// Always returns Ok with partial results; errors are collected internally.
    fn parse_block_with_recovery(
        &mut self,
        terminator: TokenKind,
        label: &str,
    ) -> Result<Block, ParseError> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&terminator) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&terminator) || self.at(&TokenKind::Eof) {
                break;
            }
            if self.at(&TokenKind::Requires) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                // F-H3: surface malformed contract clauses instead of swallowing them.
                match self.expect(TokenKind::Colon, "`:` after requires") {
                    Ok(_) => match self.parse_expr(0) {
                        Ok(expr) => {
                            stmts.push(self.parsed_stmt_from(start_pos, Stmt::Requires(expr, span)))
                        }
                        Err(e) => self.errors.push(e),
                    },
                    Err(e) => self.errors.push(e),
                }
                continue;
            }
            if self.at(&TokenKind::Ensures) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                match self.expect(TokenKind::Colon, "`:` after ensures") {
                    Ok(_) => match self.parse_expr(0) {
                        Ok(expr) => {
                            stmts.push(self.parsed_stmt_from(start_pos, Stmt::Ensures(expr, span)))
                        }
                        Err(e) => self.errors.push(e),
                    },
                    Err(e) => self.errors.push(e),
                }
                continue;
            }
            if self.at(&TokenKind::Invariant) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                if self.expect(TokenKind::Colon, "`:`").is_ok() {
                    if let Ok(expr) = self.parse_expr(0) {
                        stmts.push(self.parsed_stmt_from(start_pos, Stmt::Invariant(expr, span)));
                    }
                }
                continue;
            }
            // BUG-7 fix: Math branch was missing in recovery mode, causing math blocks
            // to be silently dropped and parsed as expression statements instead.
            if self.at(&TokenKind::Math) {
                let start_pos = self.pos;
                self.advance();
                if self.expect(TokenKind::Colon, "`:`").is_ok()
                    && self.expect(TokenKind::LBrace, "`{` for math block").is_ok()
                {
                    let mut exprs = Vec::new();
                    self.skip_newlines();
                    while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
                        match self.parse_expr(0) {
                            Ok(expr) => {
                                exprs.push(expr);
                                self.match_semi();
                                self.skip_newlines();
                            }
                            Err(_) => {
                                self.advance();
                            }
                        }
                    }
                    let _ = self.expect(TokenKind::RBrace, "`}`");
                    self.match_semi();
                    stmts.push(self.parsed_stmt_from(start_pos, Stmt::Math(exprs)));
                }
                continue;
            }
            if self.at(&TokenKind::Desc) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    if let Ok(s) = self.parse_brace_block_content() {
                        stmts.push(self.parsed_stmt_from(start_pos, Stmt::Desc(s, span)));
                    }
                } else if let Ok(s) = self.expect_string() {
                    self.match_semi();
                    stmts.push(self.parsed_stmt_from(start_pos, Stmt::Desc(s, span)));
                }
                continue;
            }
            if self.at(&TokenKind::Rule) {
                let start_pos = self.pos;
                let span = self.single_span(self.peek().line, self.peek().col);
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    if let Ok(s) = self.parse_brace_block_content() {
                        stmts.push(self.parsed_stmt_from(start_pos, Stmt::Rule(s, span)));
                    }
                } else if let Ok(s) = self.expect_string() {
                    self.match_semi();
                    stmts.push(self.parsed_stmt_from(start_pos, Stmt::Rule(s, span)));
                }
                continue;
            }
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(e) => {
                    // PR-H1: sync to block terminator / statement boundary instead
                    // of single-token skip (which causes cascade errors).
                    self.errors.push(e);
                    let sync = [
                        TokenKind::Semi,
                        TokenKind::Newline,
                        terminator.clone(),
                        TokenKind::RBrace,
                        TokenKind::Dedent,
                        TokenKind::Func,
                        TokenKind::Eof,
                    ];
                    if !self.recover_to_sync(&sync) {
                        break;
                    }
                    // Consume the sync token when it is ';' or newline so the
                    // next iteration starts at the following statement.
                    if self.at(&TokenKind::Semi) || self.at(&TokenKind::Newline) {
                        self.advance();
                    }
                }
            }
        }
        let _ = self.expect(terminator, label);
        Ok(stmts)
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::span::{SourceId, Span};

    fn parse_single_stmt(source: &str, source_id: SourceId) -> Stmt {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        Parser::new_with_source(tokens, source_id)
            .parse_stmt()
            .expect("parse statement")
    }

    fn assert_user_stmt_span(stmt: &Stmt, expected: Span) {
        let meta = stmt.meta().expect("parsed Stmt must have metadata");
        assert_eq!(meta.origin, AstOrigin::User);
        assert_eq!(meta.span, expected);
    }

    #[test]
    fn compound_assignment_desugaring_keeps_user_metadata() {
        let source_id = SourceId::new(75);
        let tokens = Lexer::new("counter += delta;").tokenize().expect("lex");
        let mut parser = Parser::new_with_source(tokens, source_id);
        let stmt = parser.parse_stmt().expect("parse statement");
        assert_user_stmt_span(&stmt, Span::new(1, 1, 1, 18).with_source(source_id));
        let Stmt::Assign { target, value } = stmt.unlocated() else {
            panic!("expected assignment");
        };

        let target_meta = target.meta().expect("target metadata");
        assert_eq!(target_meta.origin, AstOrigin::User);
        assert_eq!(
            target_meta.span,
            Span::new(1, 1, 1, 8).with_source(source_id)
        );
        let value_meta = value.meta().expect("desugared value metadata");
        assert_eq!(
            value_meta.origin,
            AstOrigin::Desugared("parser.compound_assignment")
        );
        assert_eq!(
            value_meta.span,
            Span::new(1, 1, 1, 17).with_source(source_id)
        );
        let Expr::Binary(BinOp::Add, left, right) = value.unlocated() else {
            panic!("expected desugared addition");
        };
        assert!(left.meta().is_some());
        assert_eq!(
            right.meta().expect("right metadata").span,
            Span::new(1, 12, 1, 17).with_source(source_id)
        );
    }

    #[test]
    fn simple_and_control_flow_statements_have_exact_source_aware_spans() {
        let source_id = SourceId::new(76);

        let let_stmt = parse_single_stmt("let x: i32 = 1;", source_id);
        assert!(matches!(let_stmt.unlocated(), Stmt::Let { .. }));
        assert_user_stmt_span(&let_stmt, Span::new(1, 1, 1, 16).with_source(source_id));

        let return_stmt = parse_single_stmt("return x;", source_id);
        assert!(matches!(return_stmt.unlocated(), Stmt::Return(Some(_))));
        assert_user_stmt_span(&return_stmt, Span::new(1, 1, 1, 10).with_source(source_id));

        let expr_stmt = parse_single_stmt("consume(x);", source_id);
        assert!(matches!(expr_stmt.unlocated(), Stmt::Expr(_)));
        assert_user_stmt_span(&expr_stmt, Span::new(1, 1, 1, 12).with_source(source_id));

        let assign_stmt = parse_single_stmt("x = y;", source_id);
        assert!(matches!(assign_stmt.unlocated(), Stmt::Assign { .. }));
        assert_user_stmt_span(&assign_stmt, Span::new(1, 1, 1, 7).with_source(source_id));

        let if_stmt = parse_single_stmt("if ready {\n    return value;\n}", source_id);
        let Stmt::If { then_, .. } = if_stmt.unlocated() else {
            panic!("expected if statement");
        };
        assert_user_stmt_span(&if_stmt, Span::new(1, 1, 3, 2).with_source(source_id));
        assert_user_stmt_span(&then_[0], Span::new(2, 5, 2, 18).with_source(source_id));

        let while_stmt = parse_single_stmt("while ok { break; }", source_id);
        assert!(matches!(while_stmt.unlocated(), Stmt::While { .. }));
        assert_user_stmt_span(&while_stmt, Span::new(1, 1, 1, 20).with_source(source_id));

        let for_stmt = parse_single_stmt("for item in items { continue; }", source_id);
        assert!(matches!(for_stmt.unlocated(), Stmt::For { .. }));
        assert_user_stmt_span(&for_stmt, Span::new(1, 1, 1, 32).with_source(source_id));
    }

    #[test]
    fn contract_and_math_statements_include_trailing_delimiters() {
        let source_id = SourceId::new(77);
        let source = "func sample(x: i32) -> i32 {\n    requires: x > 0;\n    ensures: result > 0;\n    invariant: x >= 0;\n    math: { x + 1; };\n    return x;\n}";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new_with_source(tokens, source_id)
            .parse_file()
            .expect("parse file");
        let Item::Func(func) = &file.items[0] else {
            panic!("expected function");
        };

        assert!(matches!(func.body[0].unlocated(), Stmt::Requires(..)));
        assert_user_stmt_span(&func.body[0], Span::new(2, 5, 2, 21).with_source(source_id));
        assert!(matches!(func.body[1].unlocated(), Stmt::Ensures(..)));
        assert_user_stmt_span(&func.body[1], Span::new(3, 5, 3, 25).with_source(source_id));
        assert!(matches!(func.body[2].unlocated(), Stmt::Invariant(..)));
        assert_user_stmt_span(&func.body[2], Span::new(4, 5, 4, 23).with_source(source_id));
        assert!(matches!(func.body[3].unlocated(), Stmt::Math(..)));
        assert_user_stmt_span(&func.body[3], Span::new(5, 5, 5, 22).with_source(source_id));
    }
}
