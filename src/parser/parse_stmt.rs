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

    /// M6 (audit-syntax 2026-08-03): lookahead for the abolished `delegate`
    /// forms. Returns true only when the token immediately following the
    /// current `delegate` token is `view`, `mutate`, or `consume` — the three
    /// keywords of the clause-2-removed delegation syntax. A bare `delegate`
    /// used as an identifier (`delegate()`, `delegate.field`) returns false so
    /// the match guard falls through to the expression path.
    fn delegate_followed_by_abolished_kw(&self) -> bool {
        let next = self.pos + 1;
        if next >= self.tokens.len() {
            return false;
        }
        match &self.tokens[next].kind {
            TokenKind::View | TokenKind::Mutate => true,
            // `consume` was removed from the keyword table (softened to Ident).
            TokenKind::Ident(s) => s == "consume",
            _ => false,
        }
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
            // v0.34.10a (SD-9): `ieee_float` is a soft keyword — expression/
            // statement position, binding position still an identifier.
            TokenKind::Ident(name) if name == "ieee_float" => self.parse_ieee_float(),
            TokenKind::Shared => self.parse_shared_let(SharedKind::Shared),
            TokenKind::Weak => self.parse_shared_let(SharedKind::Weak),
            // 0.35.13 (DX backlog #10 trivia-ization): desc:/rule:/mms{}
            // no longer produce AST statements. Block loops consume them as
            // trivia; reaching them outside a block is a syntax error.
            // 0.35.39: desc/rule/mms are no longer keywords — they lex as
            // identifiers and are consumed as trivia by the block loop above.
            TokenKind::LBrace => {
                self.advance();
                Ok(Stmt::Block(self.parse_block()?))
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
            TokenKind::Defer => {
                self.advance();
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{`")?;
                let body = self.parse_block()?;
                self.match_semi();
                Ok(Stmt::Defer(body))
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
            TokenKind::Ident(s) if s == "delegate" && self.delegate_followed_by_abolished_kw() => {
                // v0.34.1 / golden §1.1+§1.4: `delegate` abolished by amendment
                // clause 2 (no nested Flow delegation) and removed from the
                // keyword table (softened to an identifier, like subflow/consume,
                // 89→80 keyword diet). It is still recognized in statement
                // position to reject with a clause-referencing diagnostic
                // (governance §9.2: every abolished syntax keeps a negative test).
                //
                // M6 (audit-syntax 2026-08-03): reject ONLY the abolished forms
                // `delegate view|mutate|consume(...)`. A bare `delegate` used as
                // a real identifier (e.g. `delegate()`, `delegate.x = 1`) must
                // fall through to the expression path — the previous blanket
                // rejection clobbered legitimate identifiers. The guard
                // (delegate_followed_by_abolished_kw) keeps the clause-2
                // negative tests green while freeing the identifier.
                self.advance();
                let tok = self.tokens[self.pos.saturating_sub(1)].clone();
                return Err(ParseError::new(
                    "`delegate` was abolished by architecture amendment clause 2 \
                     (nested Flow delegation removed). Express delegation as an explicit \
                     transition to a target state instead. \
                     See devdocs/v0.31/architecture-amendment-1.0.md §条款 2.",
                    tok.line,
                    tok.col,
                ));
            }
            TokenKind::Pinned => {
                self.advance();
                self.expect(TokenKind::LParen, "`(`")?;
                let expr = self.parse_expr(0)?;
                // Architecture amendment clause 10: synchronous pinned timeout
                // is abolished. Reject `pinned(expr, timeout = N)` with a clear
                // diagnostic pointing users to ForeignTask async timeout.
                if self.at(&TokenKind::Comma) {
                    return Err(ParseError::new(
                        "pinned(timeout) was abolished by architecture amendment clause 10. \
                         Async FFI timeout (spawn_foreign_task) is planned for 0.2 — FFI calls \
                         are synchronous today. \
                         See devdocs/v0.31/architecture-amendment-1.0.md §条款 10.",
                        self.tokens[self.pos.saturating_sub(1)].line,
                        self.tokens[self.pos.saturating_sub(1)].col,
                    ));
                }
                self.expect(TokenKind::RParen, "`)`")?;
                let var = if self.at(&TokenKind::PipeArrow) {
                    // M8 (audit-syntax 2026-08-03): `|>` was abolished as a
                    // separator by ADR-002; the pinned binder EBNF is
                    // `[ '|' Ident '|' ]`. Reject explicitly instead of
                    // silently accepting the zombie spelling.
                    let tok = self.peek();
                    return Err(ParseError::new(
                        "`|>` was abolished as a separator (ADR-002); pinned binder syntax is \
                         `pinned(expr) | name | { ... }` (use `|`)",
                        tok.line,
                        tok.col,
                    ));
                } else if self.at(&TokenKind::BitOr) {
                    self.advance();
                    let v = self.expect_ident()?;
                    if self.at(&TokenKind::PipeArrow) {
                        let tok = self.peek();
                        return Err(ParseError::new(
                            "`|>` was abolished as a separator (ADR-002); pinned binder syntax is \
                             `pinned(expr) | name | { ... }` (use `|`)",
                            tok.line,
                            tok.col,
                        ));
                    }
                    // Full-audit 2026-08-05: the closing `|` is mandatory
                    // (ADR-002 binder form `| name |`); `pinned(x) |v { }`
                    // silently accepted a half-open binder before.
                    if !self.at(&TokenKind::BitOr) {
                        let tok = self.peek();
                        return Err(ParseError::new(
                            "expected `|` to close the pinned binder (ADR-002): syntax is \
                             `pinned(expr) | name | { ... }`",
                            tok.line,
                            tok.col,
                        ));
                    }
                    self.advance();
                    Some(v)
                } else {
                    None
                };
                self.skip_newlines();
                self.expect(TokenKind::LBrace, "`{`")?;
                let body = self.parse_block()?;
                Ok(Stmt::Pinned { expr, var, body })
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

    /// v0.34.10a (SD-9): `ieee_float { block }` — IEEE 754 escape hatch.
    /// Float results inside may be NaN/Inf without the finiteness trap.
    fn parse_ieee_float(&mut self) -> Result<Stmt, ParseError> {
        self.advance(); // consume `ieee_float` ident
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{` for ieee_float block")?;
        let body = self.parse_block()?;
        self.match_semi();
        Ok(Stmt::IeeeFloat(body))
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

    fn parse_mms_block(&mut self) -> Result<(), ParseError> {
        self.expect_ident_name("mms")?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        if matches!(self.peek_kind(), TokenKind::String(_)) {
            self.expect_string()?;
        } else {
            // 0.35.13 trivia-ization: consume the body validating only the
            // brace structure — the text is a super-comment, no AST node.
            let mut depth = 1u32;
            while !self.at(&TokenKind::Eof) {
                match self.peek_kind() {
                    TokenKind::LBrace => depth += 1,
                    TokenKind::RBrace => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                self.advance();
            }
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        self.match_semi();
        // 0.35.13 trivia-ization: the content is parsed (validating the
        // brace structure) but discarded — mms{} is a super-comment.
        Ok(())
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
                            SharedKind::Weak => "weak",
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
        // Wave-2 stress fix (scripts/stress-test.sh big-if-else): `else if`
        // chains used to recurse one depth-guard level per link
        // (parse_if → parse_if_inner → parse_if → …), so long FLAT chains
        // burnt parser stack proportional to chain length and hit the
        // recursion limit (2000 links ≫ 128 cap). Chains are semantically
        // flat: collect the links iteratively, then fold right-to-left into
        // exactly the right-nested AST the recursive form produced. Inner
        // links still carry no statement metadata (the recursive form never
        // wrapped them) and parse_stmt still wraps the whole chain once.
        enum IfHead {
            Cond(Expr),
            // v0.34.3: `if let pattern = expr { } else { }` — pattern-match
            // guard (pattern bindings visible in the then-block).
            Let { pat: Pattern, init: Expr },
        }
        let mut links: Vec<(IfHead, Block)> = Vec::new();
        let tail: Option<Block>;
        loop {
            self.expect(TokenKind::If, "`if`")?;
            self.skip_newlines();
            let head = if self.at(&TokenKind::Let) {
                self.advance();
                let pat = self.parse_pattern()?;
                self.skip_newlines();
                self.expect(TokenKind::Eq, "`=`")?;
                let init = self.parse_expr(0)?;
                IfHead::Let { pat, init }
            } else {
                IfHead::Cond(self.parse_expr(0)?)
            };
            self.skip_newlines();
            self.expect(TokenKind::LBrace, "`{`")?;
            let then_ = self.parse_block()?;
            links.push((head, then_));
            self.skip_newlines();
            if !self.at(&TokenKind::Else) {
                tail = None;
                break;
            }
            self.advance(); // consume `else`
            self.skip_newlines();
            if self.at(&TokenKind::If) {
                continue; // next chain link — iterative, no recursion
            }
            self.expect(TokenKind::LBrace, "`{`")?;
            tail = Some(self.parse_block()?);
            break;
        }
        // Fold right-to-left; the loop above guarantees ≥1 link. 0.35.12
        // (DX backlog #2): invariant violation surfaces as a diagnostic
        // instead of a panic — user input must never abort the compiler.
        let mut iter = links.into_iter().rev();
        let Some((head, then_)) = iter.next() else {
            return Err(ParseError::new(
                "internal: `if` chain lost its first link",
                self.peek().line,
                self.peek().col,
            ));
        };
        let mut current = match head {
            IfHead::Cond(cond) => Stmt::If {
                cond,
                then_,
                else_: tail,
            },
            IfHead::Let { pat, init } => Stmt::IfLet {
                pat,
                init,
                then_,
                else_: tail,
            },
        };
        for (head, then_) in iter {
            let else_ = Some(vec![current]);
            current = match head {
                IfHead::Cond(cond) => Stmt::If { cond, then_, else_ },
                IfHead::Let { pat, init } => Stmt::IfLet {
                    pat,
                    init,
                    then_,
                    else_,
                },
            };
        }
        Ok(current)
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
        // v0.34.3: `for (k, v) in m` destructuring — parse a full pattern.
        // Single identifiers still parse as Pattern::Variable.
        let var = self.parse_pattern()?;
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
                    // §1-#10 (audit 2026-08-05, closed 2026-08-07): mirror
                    // the lexer's quote-aware interpolation scan — braces
                    // inside quoted literals (`f"{ "}" }"`) must not close
                    // the interpolation. Skip the whole literal, honoring
                    // backslash escapes.
                    if c == '"' || c == '\'' {
                        let quote = c;
                        expr_str.push(c);
                        chars.next();
                        while let Some(&qc) = chars.peek() {
                            expr_str.push(qc);
                            chars.next();
                            if qc == '\\' {
                                if let Some(&escaped) = chars.peek() {
                                    expr_str.push(escaped);
                                    chars.next();
                                }
                            } else if qc == quote {
                                break;
                            }
                        }
                        continue;
                    }
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
                // 0.35.25 (C2, audit-triage-0.35.25.md): 子解析器继承外层
                // recursion_depth——修复前嵌套 f-string 每层新建实例时从 0
                // 重数，深度守卫对跨实例链完全失察（6000 层嵌套 ~30KB 源码
                // SIGSEGV，默认 8MB 栈打穿；libtest 2MB 栈 40 层即炸）。
                // 继承后守卫跨实例持续计数，达到预算返回 ParseError。
                sub.recursion_depth.set(self.recursion_depth.get());
                // 通用预算（DEPTH_MAX_DEFAULT=128）对本路径太宽：嵌套每层
                // 栈成本 ~53KB，128 预算允许 64 层嵌套 ~3.4MB 仍会爆 2MB
                // libtest 栈。f-string 专用预算 DEPTH_MAX_FSTRING=64
                // （≈32 层嵌套，~1.7MB，带余量）。
                self.check_depth_with(super::helpers::DEPTH_MAX_FSTRING)?;
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
                        'n' => {
                            current_text.push('\n');
                            chars.next();
                        }
                        't' => {
                            current_text.push('\t');
                            chars.next();
                        }
                        'r' => {
                            current_text.push('\r');
                            chars.next();
                        }
                        '0' => {
                            current_text.push('\0');
                            chars.next();
                        }
                        '\\' => {
                            current_text.push('\\');
                            chars.next();
                        }
                        '"' => {
                            current_text.push('"');
                            chars.next();
                        }
                        '{' => {
                            current_text.push('{');
                            chars.next();
                        }
                        '}' => {
                            current_text.push('}');
                            chars.next();
                        }
                        // Full-audit 2026-08-05: \xNN / \uXXXX / \u{...} are
                        // validated by scan_fstring but were never decoded here
                        // (f"\x41" stayed the literal 4 chars while "\x41" = "A").
                        // Decode exactly like the normal-string path
                        // (LexerPos::scan_string in lexer/flow.rs).
                        'x' => {
                            chars.next(); // consume 'x'
                            let mut hex = String::with_capacity(2);
                            for _ in 0..2 {
                                match chars.peek() {
                                    Some(&h) if h.is_ascii_hexdigit() => {
                                        hex.push(h);
                                        chars.next();
                                    }
                                    _ => break,
                                }
                            }
                            if hex.len() != 2 {
                                return Err(ParseError::new(
                                    "invalid \\x escape in f-string (expected 2 hex digits)",
                                    base_line,
                                    base_col,
                                ));
                            }
                            // Infallible: exactly 2 ASCII hex digits validated above.
                            let value = u8::from_str_radix(&hex, 16).map_err(|_| {
                                ParseError::new(
                                    "invalid \\x escape in f-string (expected 2 hex digits)",
                                    base_line,
                                    base_col,
                                )
                            })?;
                            current_text.push(value as char);
                        }
                        'u' => {
                            chars.next(); // consume 'u'
                            let mut code = String::new();
                            if chars.peek() == Some(&'{') {
                                chars.next();
                                while let Some(&ch) = chars.peek() {
                                    if ch.is_ascii_hexdigit() || ch == '_' {
                                        code.push(ch);
                                        chars.next();
                                    } else {
                                        break;
                                    }
                                }
                                if chars.peek() != Some(&'}') {
                                    return Err(ParseError::new(
                                        "invalid \\u{ escape in f-string (expected `}`)",
                                        base_line,
                                        base_col,
                                    ));
                                }
                                chars.next(); // consume '}'
                            } else {
                                for _ in 0..4 {
                                    match chars.peek() {
                                        Some(&ch) if ch.is_ascii_hexdigit() => {
                                            code.push(ch);
                                            chars.next();
                                        }
                                        _ => break,
                                    }
                                }
                                if code.len() != 4 {
                                    return Err(ParseError::new(
                                        "invalid \\u escape in f-string (expected 4 hex digits)",
                                        base_line,
                                        base_col,
                                    ));
                                }
                            }
                            let cleaned: String = code.chars().filter(|ch| *ch != '_').collect();
                            let value = u32::from_str_radix(&cleaned, 16).map_err(|_| {
                                ParseError::new(
                                    "invalid \\u escape in f-string (expected hex digits)",
                                    base_line,
                                    base_col,
                                )
                            })?;
                            match char::from_u32(value) {
                                Some(ch) => current_text.push(ch),
                                // Lexer validation cannot exclude surrogates
                                // (\uD800) or out-of-range braces (\u{110000}).
                                None => {
                                    return Err(ParseError::new(
                                        "invalid \\u escape in f-string (not a valid Unicode scalar value)",
                                        base_line,
                                        base_col,
                                    ))
                                }
                            }
                        }
                        other => {
                            // Unknown escape: keep both chars so diagnostics remain visible.
                            current_text.push('\\');
                            current_text.push(other);
                            chars.next();
                        }
                    }
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
            // 0.35.13 (DX backlog #10 trivia-ization): desc:/rule:/mms{}
            // are consumed as trivia — validated but never enter the AST.
            // 0.35.39: desc/rule/mms no longer lex as keywords — they are
            // ordinary identifiers matched here by name.
            if self.at_ident_name("desc") {
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    self.parse_brace_block_content()?;
                } else {
                    self.expect_string()?;
                    self.match_semi();
                }
                continue;
            }
            if self.at_ident_name("rule") {
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    self.parse_brace_block_content()?;
                } else {
                    self.expect_string()?;
                    self.match_semi();
                }
                continue;
            }
            if self.at_ident_name("mms") {
                self.parse_mms_block()?;
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
                        // Full-audit 2026-08-05: a failed parse can stop with
                        // the cursor ON the math block's own `}` (or EOF). The
                        // old blind advance() then ate that terminator, pulling
                        // every following statement into the math node (silent
                        // AST corruption on the LSP/recovery path).
                        let pos_before = self.pos;
                        match self.parse_expr(0) {
                            Ok(expr) => {
                                exprs.push(expr);
                                self.match_semi();
                                self.skip_newlines();
                            }
                            Err(_) => {
                                if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                                    break;
                                }
                                // Only force progress when the failed parse made
                                // none; otherwise the failure already advanced
                                // and skipping again would drop a token.
                                if self.pos == pos_before {
                                    self.advance();
                                }
                            }
                        }
                    }
                    let _ = self.expect(TokenKind::RBrace, "`}`");
                    self.match_semi();
                    stmts.push(self.parsed_stmt_from(start_pos, Stmt::Math(exprs)));
                }
                continue;
            }
            // 0.35.13 trivia-ization (recovery loop): consume-and-discard.
            if self.at_ident_name("desc") {
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    let _ = self.parse_brace_block_content();
                } else if self.expect_string().is_ok() {
                    self.match_semi();
                }
                continue;
            }
            if self.at_ident_name("rule") {
                self.advance();
                if self.at(&TokenKind::LBrace) {
                    let _ = self.parse_brace_block_content();
                } else if self.expect_string().is_ok() {
                    self.match_semi();
                }
                continue;
            }
            if self.at_ident_name("mms") {
                let _ = self.parse_mms_block();
                continue;
            }
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(e) => {
                    // PR-H1: sync to block terminator / statement boundary instead
                    // of single-token skip (which causes cascade errors).
                    self.errors.push(e);
                    let pos_before_recovery = self.pos;
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
                    // P-1 (full-audit 2026-08-05-0656): guarantee forward
                    // progress. When the failed statement leaves the cursor
                    // ON a sync token that is neither ';'/newline nor this
                    // block's terminator (an orphan `}` in sketch mode,
                    // where the terminator is Dedent), recover_to_sync
                    // returns immediately and nothing above advances — the
                    // outer loop would retry parse_stmt on the same token
                    // forever (infinite hang; latent because no production
                    // caller combined sketch mode with recovery until now).
                    if self.pos == pos_before_recovery
                        && !self.at(&terminator)
                        && !self.at(&TokenKind::Eof)
                    {
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
