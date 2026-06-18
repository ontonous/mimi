use super::*;

impl Parser {
    pub(crate) fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.skip_newlines();
        match self.peek_kind() {
            TokenKind::Let => self.parse_let(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Break => {
                self.advance();
                let val = if self.peek_kind() == &TokenKind::Semi || self.peek_kind() == &TokenKind::Newline || self.peek_kind() == &TokenKind::RBrace || self.peek_kind() == &TokenKind::Eof {
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
            TokenKind::For => self.parse_for(),
            TokenKind::Arena => self.parse_arena(),
            TokenKind::Unsafe => self.parse_unsafe(),
            TokenKind::Alloc if self.is_alloc_block() => self.parse_alloc(),
            TokenKind::Shared => self.parse_shared_let(SharedKind::Shared),
            TokenKind::LocalShared => self.parse_shared_let(SharedKind::LocalShared),
            TokenKind::Weak => self.parse_shared_let(SharedKind::Weak),
            TokenKind::Mms => self.parse_mms_block(),
            TokenKind::LBrace => {
                self.advance();
                Ok(Stmt::Block(self.parse_block()?))
            }
            TokenKind::Desc => {
                self.advance();
                let s = self.expect_string()?;
                self.match_semi();
                Ok(Stmt::Desc(s))
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
                Ok(Stmt::Parasteps(body))
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
                let expr = self.parse_expr(0)?;
                if self.at(&TokenKind::Eq) {
                    self.advance();
                    let value = self.parse_expr(0)?;
                    self.match_semi();
                    Ok(Stmt::Assign { target: expr, value })
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
                    self.match_semi();
                    let op = match op_token {
                        TokenKind::PlusEq => BinOp::Add,
                        TokenKind::MinusEq => BinOp::Sub,
                        TokenKind::StarEq => BinOp::Mul,
                        TokenKind::SlashEq => BinOp::Div,
                        TokenKind::BitAndEq => BinOp::BitAnd,
                        TokenKind::BitOrEq => BinOp::BitOr,
                        TokenKind::BitXorEq => BinOp::BitXor,
                        _ => unreachable!(),
                    };
                    let rhs = Expr::Binary(op, Box::new(expr.clone()), Box::new(value));
                    Ok(Stmt::Assign { target: expr, value: rhs })
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
        Ok(Stmt::Arena(body))
    }

    fn parse_unsafe(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::Unsafe, "`unsafe`")?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let body = self.parse_block()?;
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
                    _ => return Err(ParseError::new(
                        format!("expected allocator type (System, Arena, Bump), found `{}`", name),
                        kind_tok.line,
                        kind_tok.col,
                    )),
                }
            }
            TokenKind::Arena => {
                self.advance();
                AllocKind::Arena
            }
            _ => return Err(ParseError::new(
                format!("expected allocator type (System, Arena, Bump), found {}", kind_tok.kind),
                kind_tok.line,
                kind_tok.col,
            )),
        };
        self.expect(TokenKind::RParen, "`)`")?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let body = self.parse_block()?;
        Ok(Stmt::Alloc { kind, body })
    }

    fn parse_mms_block(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::Mms, "`mms`")?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let content = if self.at(&TokenKind::String("".into())) {
            self.expect_string()?
        } else {
            let mut text = String::new();
            let mut depth = 1;
            let mut first_line = None;
            let mut first_col = None;
            while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
                let tok = self.peek();
                match &tok.kind {
                    TokenKind::LBrace => depth += 1,
                    TokenKind::RBrace => {
                        depth -= 1;
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
                    let base_col = first_col.unwrap_or(0);
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
        let ast = Self::try_parse_mimispec_with_timeout(&content);
        Ok(Stmt::MmsBlock { content, ast })
    }

    fn try_parse_mimispec_with_timeout(content: &str) -> Option<mimispec::ast::File> {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let content_owned = content.to_string();
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let result = mimispec::parse(&content_owned);
            let _ = tx.send(result);
        });

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => {
                if result.errors.is_empty() {
                    Some(result.file)
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    }

    fn parse_shared_let(&mut self, kind: SharedKind) -> Result<Stmt, ParseError> {
        self.advance();
        let tok = self.peek().clone();
        let name = match &tok.kind {
            TokenKind::Ident(s) => {
                self.advance();
                s.clone()
            }
            _ => return Err(ParseError::new(
                format!("expected variable name after '{}'", match kind {
                    SharedKind::Shared => "shared",
                    SharedKind::LocalShared => "local_shared",
                    SharedKind::Weak | SharedKind::WeakLocal => "weak",
                }),
                tok.line,
                tok.col,
            )),
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
        Ok(Stmt::SharedLet { kind, name, ty, init })
    }

    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::Let, "`let`")?;
        let mut_ = self.at(&TokenKind::Mut);
        if mut_ {
            self.advance();
        }
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
        let cond = self.parse_expr(0)?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::For, "`for`")?;
        let var = self.expect_ident()?;
        self.expect(TokenKind::In, "`in`")?;
        let iterable = self.parse_expr(0)?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let body = self.parse_block()?;
        Ok(Stmt::For { var, iterable, body })
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

    pub(crate) fn parse_fstring_parts(&self, raw: &str) -> Result<Vec<crate::ast::FStringPart>, ParseError> {
        use crate::ast::FStringPart;
        let mut parts = Vec::new();
        let mut chars = raw.chars().peekable();
        let mut current_text = String::new();
        
        while let Some(&c) = chars.peek() {
            if c == '{' {
                if !current_text.is_empty() {
                    parts.push(FStringPart::Text(current_text.clone()));
                    current_text.clear();
                }
                chars.next();
                let mut expr_str = String::new();
                let mut depth = 1;
                while let Some(&c) = chars.peek() {
                    if c == '{' { depth += 1; }
                    else if c == '}' {
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
                    return Err(ParseError::new("unterminated interpolation in f-string", 0, 0));
                }
                let tokens = crate::lexer::Lexer::new(&expr_str).tokenize()
                    .map_err(|e| ParseError::new(&e, 0, 0))?;
                let expr = Parser::new(tokens).parse_expr(0)?;
                parts.push(FStringPart::Interp(expr));
            } else if c == '\\' {
                chars.next();
                if let Some(&esc) = chars.peek() {
                    match esc {
                        'n' => current_text.push('\n'),
                        't' => current_text.push('\t'),
                        'r' => current_text.push('\r'),
                        '\\' => current_text.push('\\'),
                        '"' => current_text.push('"'),
                        '{' => current_text.push('{'),
                        '}' => current_text.push('}'),
                        other => { current_text.push('\\'); current_text.push(other); }
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
}