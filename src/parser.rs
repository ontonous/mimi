use crate::ast::*;
use crate::lexer::{Token, TokenKind};

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl ParseError {
    fn new(message: impl Into<String>, line: usize, col: usize) -> Self {
        Self {
            message: message.into(),
            line,
            col,
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}:{}", self.message, self.line, self.col)
    }
}

impl std::error::Error for ParseError {}

impl From<ParseError> for String {
    fn from(e: ParseError) -> Self {
        e.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    Production,
    Sketch,
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    mode: ParseMode,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self::with_mode(tokens, ParseMode::Production)
    }

    pub fn new_sketch(tokens: Vec<Token>) -> Self {
        Self::with_mode(tokens, ParseMode::Sketch)
    }

    fn with_mode(tokens: Vec<Token>, mode: ParseMode) -> Self {
        Self { tokens, pos: 0, mode }
    }

    fn is_sketch(&self) -> bool {
        self.mode == ParseMode::Sketch
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos];
        if !matches!(tok.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        &self.tokens[self.pos.saturating_sub(1)]
    }

    fn at(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(self.peek_kind()) == std::mem::discriminant(kind)
    }

    fn expect(&mut self, kind: TokenKind, expected: &str) -> Result<&Token, ParseError> {
        if self.at(&kind) {
            Ok(self.advance())
        } else {
            let tok = self.peek();
            Err(ParseError::new(
                format!("expected {}, found {}", expected, tok.kind),
                tok.line,
                tok.col,
            ))
        }
    }

    fn expect_keyword(&mut self, kind: TokenKind) -> Result<Commitment, ParseError> {
        let tok = self.expect(kind, "keyword")?;
        Ok(tok.commitment)
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        let tok = self.peek();
        match &tok.kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(ParseError::new(
                format!("expected identifier, found {}", tok.kind),
                tok.line,
                tok.col,
            )),
        }
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek_kind(), TokenKind::Newline) {
            self.advance();
        }
    }

    fn match_semi(&mut self) {
        if matches!(self.peek_kind(), TokenKind::Semi) {
            self.advance();
        }
    }

    pub fn parse_file(mut self) -> Result<File, ParseError> {
        self.skip_newlines();
        let mut imports = Vec::new();
        while self.at(&TokenKind::Use) {
            imports.push(self.parse_import()?);
            self.skip_newlines();
        }
        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::Eof) {
                break;
            }
            items.push(self.parse_item()?);
        }
        Ok(File { imports, items })
    }

    fn parse_import(&mut self) -> Result<Import, ParseError> {
        self.expect(TokenKind::Use, "`use`")?;
        let mut path = vec![self.expect_ident()?];
        while self.at(&TokenKind::ColonColon) {
            self.advance();
            path.push(self.expect_ident()?);
        }
        self.match_semi();
        Ok(Import { path })
    }

    fn parse_item(&mut self) -> Result<Item, ParseError> {
        let pub_ = if self.at(&TokenKind::Pub) {
            self.advance();
            true
        } else {
            false
        };
        match self.peek_kind() {
            TokenKind::Func => {
                let mut f = self.parse_func()?;
                f.pub_ = pub_;
                Ok(Item::Func(f))
            }
            TokenKind::Module => Ok(Item::Module(self.parse_module()?)),
            TokenKind::Type => {
                let mut t = self.parse_type_def()?;
                t.pub_ = pub_;
                Ok(Item::Type(t))
            }
            TokenKind::Newtype => {
                let mut t = self.parse_newtype()?;
                t.pub_ = pub_;
                Ok(Item::Type(t))
            }
            TokenKind::Actor => {
                let mut a = self.parse_actor_def()?;
                a.pub_ = pub_;
                Ok(Item::Actor(a))
            }
            TokenKind::Cap => Ok(Item::Cap(self.parse_cap_def()?)),
            TokenKind::Rule => {
                self.advance();
                let s = self.expect_string()?;
                self.match_semi();
                Ok(Item::Rule(s))
            }
            TokenKind::Desc => {
                self.advance();
                let s = self.expect_string()?;
                self.match_semi();
                Ok(Item::Desc(s))
            }
            _ => {
                let tok = self.peek();
                Err(ParseError::new(
                    format!("unexpected token {} at top level", tok.kind),
                    tok.line,
                    tok.col,
                ))
            }
        }
    }

    fn parse_cap_def(&mut self) -> Result<CapDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Cap)?;
        let name = self.expect_ident()?;
        let combined_with = if self.at(&TokenKind::Plus) {
            // cap A + B syntax
            self.advance();
            let combined_name = self.expect_ident()?;
            Some(combined_name)
        } else if self.at(&TokenKind::Eq) {
            // cap A = B + C syntax
            self.advance();
            let first = self.expect_ident()?;
            if self.at(&TokenKind::Plus) {
                self.advance();
                let second = self.expect_ident()?;
                // Store as "first + second" in combined_with
                Some(format!("{} + {}", first, second))
            } else {
                Some(first)
            }
        } else {
            None
        };
        self.match_semi();
        Ok(CapDef {
            name,
            commitment,
            combined_with,
        })
    }

    fn parse_actor_def(&mut self) -> Result<ActorDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Actor)?;
        let name = self.expect_ident()?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        self.skip_newlines();

        let mut fields = Vec::new();
        let mut methods = Vec::new();

        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) {
                break;
            }
            // Check if it's a method (func keyword) or field
            if self.at(&TokenKind::Mut) || matches!(self.peek_kind(), TokenKind::Ident(_)) {
                // Could be field: [mut] name: Type [= expr]
                let mut_ = self.at(&TokenKind::Mut);
                if mut_ {
                    self.advance();
                }
                let fname = self.expect_ident()?;
                if self.at(&TokenKind::Colon) {
                    // It's a field
                    self.advance();
                    let fty = self.parse_type()?;
                    let init = if self.at(&TokenKind::Eq) {
                        self.advance();
                        Some(self.parse_expr(0)?)
                    } else {
                        None
                    };
                    self.match_semi();
                    fields.push(ActorField { name: fname, ty: fty, mut_, init });
                } else {
                    // Not a field - error
                    let tok = self.peek();
                    return Err(ParseError::new(
                        "expected `:` for field type",
                        tok.line,
                        tok.col,
                    ));
                }
            } else if self.at(&TokenKind::Func) {
                methods.push(self.parse_func()?);
            } else {
                let tok = self.peek();
                return Err(ParseError::new(
                    format!("unexpected token {} in actor body", tok.kind),
                    tok.line,
                    tok.col,
                ));
            }
            self.skip_newlines();
        }

        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(ActorDef { name, commitment, pub_: false, fields, methods })
    }

    fn parse_module(&mut self) -> Result<ModuleDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Module)?;
        let name = self.expect_ident()?;
        self.skip_newlines();
        if self.is_sketch() {
            self.expect(TokenKind::Colon, "`:`")?;
            self.skip_newlines();
        }
        self.expect_block_start("module body")?;
        let items = self.parse_item_block()?;
        Ok(ModuleDef {
            name,
            commitment,
            items,
        })
    }

    fn parse_func(&mut self) -> Result<FuncDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Func)?;
        let name = self.expect_ident()?;
        let params = if self.is_sketch() && !self.at(&TokenKind::LParen) {
            Vec::new()
        } else {
            self.expect(TokenKind::LParen, "`(`")?;
            let p = self.parse_params()?;
            self.expect(TokenKind::RParen, "`)`")?;
            p
        };
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
        self.expect_block_start("function body")?;
        let body = self.parse_block()?;
        Ok(FuncDef {
            name,
            commitment,
            pub_: false,
            params,
            ret,
            body,
        })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if self.at(&TokenKind::RParen) {
            return Ok(params);
        }
        loop {
            let mut_ = self.at(&TokenKind::Mut);
            if mut_ {
                self.advance();
            }
            let name = self.expect_ident()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let ty = self.parse_type()?;
            params.push(Param { name, ty, mut_ });
            if !self.at(&TokenKind::Comma) {
                break;
            }
            self.advance();
        }
        Ok(params)
    }

    fn parse_type(&mut self) -> Result<Type, ParseError> {
        self.parse_type_optional(false)
    }

    fn parse_type_optional(&mut self, allow_func: bool) -> Result<Type, ParseError> {
        let mut ty = self.parse_type_atom()?;
        loop {
            if self.at(&TokenKind::Lt) {
                self.advance();
                let mut args = Vec::new();
                if !self.at(&TokenKind::Gt) {
                    loop {
                        args.push(self.parse_type()?);
                        if !self.at(&TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                    }
                }
                self.expect(TokenKind::Gt, "`>`")?;
                if let Type::Name(name, _) = ty {
                    ty = Type::Name(name, args);
                } else {
                    let tok = self.peek();
                    return Err(ParseError::new(
                        "type arguments only allowed on named types",
                        tok.line,
                        tok.col,
                    ));
                }
            } else if self.at(&TokenKind::Question) {
                self.advance();
                ty = Type::Option(Box::new(ty));
            } else {
                break;
            }
        }
        if allow_func && self.at(&TokenKind::Arrow) {
            self.advance();
            let ret = self.parse_type()?;
            ty = Type::Func(vec![ty], Box::new(ret));
        }
        Ok(ty)
    }

    fn parse_type_atom(&mut self) -> Result<Type, ParseError> {
        let tok = self.peek();
        match tok.kind {
            TokenKind::Ident(ref name) => {
                let name = name.clone();
                self.advance();
                Ok(Type::Name(name, Vec::new()))
            }
            TokenKind::I32 | TokenKind::I64 | TokenKind::F64 | TokenKind::Bool | TokenKind::StringKw => {
                let name = tok.kind.to_string();
                self.advance();
                Ok(Type::Name(name, Vec::new()))
            }
            TokenKind::Nothing => {
                self.advance();
                Ok(Type::Nothing)
            }
            TokenKind::BitAnd => {
                self.advance();
                let mut_ = self.at(&TokenKind::Mut);
                if mut_ {
                    self.advance();
                }
                let inner = self.parse_type()?;
                if mut_ {
                    Ok(Type::RefMut(Box::new(inner)))
                } else {
                    Ok(Type::Ref(Box::new(inner)))
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
                Ok(Type::Tuple(elems))
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
            _ => Err(ParseError::new(
                format!("expected type, found {}", tok.kind),
                tok.line,
                tok.col,
            )),
        }
    }

    fn expect_block_start(&mut self, context: &str) -> Result<(), ParseError> {
        self.skip_newlines();
        if self.is_sketch() {
            self.expect(TokenKind::Indent, &format!("indented {}", context))?;
        } else {
            self.expect(TokenKind::LBrace, &format!("`{{` for {}", context))?;
        }
        Ok(())
    }

    fn parse_item_block(&mut self) -> Result<Vec<Item>, ParseError> {
        let mut items = Vec::new();
        self.skip_newlines();
        let end = if self.is_sketch() {
            TokenKind::Dedent
        } else {
            TokenKind::RBrace
        };
        while !self.at(&end) && !self.at(&TokenKind::Eof) {
            items.push(self.parse_item()?);
            self.skip_newlines();
        }
        self.expect(end, if self.is_sketch() { "dedent" } else { "`}`" })?;
        Ok(items)
    }

    fn parse_block(&mut self) -> Result<Block, ParseError> {
        let block = if self.is_sketch() {
            self.parse_indent_block()
        } else {
            self.parse_brace_block()
        };
        block
    }

    fn parse_brace_block(&mut self) -> Result<Block, ParseError> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            // Contract prefixes inside block
            if self.at(&TokenKind::Requires) {
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                let expr = self.parse_expr(0)?;
                stmts.push(Stmt::Requires(expr));
                continue;
            }
            if self.at(&TokenKind::Ensures) {
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                let expr = self.parse_expr(0)?;
                stmts.push(Stmt::Ensures(expr));
                continue;
            }
            if self.at(&TokenKind::Math) {
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
                stmts.push(Stmt::Math(exprs));
                continue;
            }
            if self.at(&TokenKind::Desc) {
                self.advance();
                let s = self.expect_string()?;
                self.match_semi();
                stmts.push(Stmt::Desc(s));
                continue;
            }
            if self.at(&TokenKind::Rule) {
                self.advance();
                let s = self.expect_string()?;
                self.match_semi();
                // Rule is metadata-only in v1.0, stored as Desc for simplicity
                stmts.push(Stmt::Desc(format!("rule: {}", s)));
                continue;
            }
            stmts.push(self.parse_stmt()?);
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(stmts)
    }

    fn parse_indent_block(&mut self) -> Result<Block, ParseError> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::Dedent) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::Dedent) || self.at(&TokenKind::Eof) {
                break;
            }
            // Contract prefixes inside block
            if self.at(&TokenKind::Requires) {
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                let expr = self.parse_expr(0)?;
                stmts.push(Stmt::Requires(expr));
                continue;
            }
            if self.at(&TokenKind::Ensures) {
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                let expr = self.parse_expr(0)?;
                stmts.push(Stmt::Ensures(expr));
                continue;
            }
            if self.at(&TokenKind::Math) {
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
                stmts.push(Stmt::Math(exprs));
                continue;
            }
            if self.at(&TokenKind::Desc) {
                self.advance();
                let s = self.expect_string()?;
                self.match_semi();
                stmts.push(Stmt::Desc(s));
                continue;
            }
            if self.at(&TokenKind::Rule) {
                self.advance();
                let s = self.expect_string()?;
                self.match_semi();
                stmts.push(Stmt::Desc(format!("rule: {}", s)));
                continue;
            }
            stmts.push(self.parse_stmt()?);
        }
        self.expect(TokenKind::Dedent, "dedent")?;
        Ok(stmts)
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

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.skip_newlines();
        match self.peek_kind() {
            TokenKind::Let => self.parse_let(),
            TokenKind::Return => self.parse_return(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Arena => self.parse_arena(),
            TokenKind::Shared => self.parse_shared_let(SharedKind::Shared),
            TokenKind::LocalShared => self.parse_shared_let(SharedKind::LocalShared),
            TokenKind::Weak => self.parse_shared_let(SharedKind::Weak),
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
            TokenKind::On => {
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

    fn parse_shared_let(&mut self, kind: SharedKind) -> Result<Stmt, ParseError> {
        // Consume the shared/local_shared/weak keyword
        self.advance();
        // Parse variable name
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
        // Optional type annotation
        let ty = if self.at(&TokenKind::Colon) {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        // Expect '='
        self.expect(TokenKind::Eq, "`=`")?;
        // Parse initializer expression
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
        let else_ = if self.at(&TokenKind::Else) {
            self.advance();
            self.skip_newlines();
            if self.at(&TokenKind::If) {
                // else if: parse as a single-statement else block containing the next if
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

    fn expect_string(&mut self) -> Result<String, ParseError> {
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

    fn parse_expr(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
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
            _ => self.parse_primary(),
        }
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
            TokenKind::Ident(name) => {
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
                        // Accept identifier or keyword (like spawn, await) as field name
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
                        let idx = self.parse_expr(0)?;
                        self.expect(TokenKind::RBracket, "`]`")?;
                        e = Expr::Index(Box::new(e), Box::new(idx));
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
                let mut elems = Vec::new();
                if !self.at(&TokenKind::RBracket) {
                    loop {
                        elems.push(self.parse_expr(0)?);
                        self.skip_newlines();
                        if !self.at(&TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                        self.skip_newlines();
                    }
                }
                self.expect(TokenKind::RBracket, "`]`")?;
                Expr::List(elems)
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
            TokenKind::Quote => {
                self.advance();
                // Allow optional ! after quote (quote! syntax)
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
        // Handle postfix `?` operator for Result/Option
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
            self.expect(TokenKind::Colon, "`:`")?;
            let value = self.parse_expr(0)?;
            fields.push(RecordFieldExpr { name, value });
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

    fn parse_type_def(&mut self) -> Result<TypeDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Type)?;
        let name = self.expect_ident()?;
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
            return Ok(TypeDef { name, commitment, pub_: false, kind });
        }
        if self.at(&TokenKind::Eq) {
            self.advance();
            let ty = self.parse_type()?;
            self.match_semi();
            return Ok(TypeDef {
                name,
                commitment,
                pub_: false,
                kind: TypeDefKind::Alias(ty),
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
        Ok(TypeDef { name, commitment, pub_: false, kind })
    }

    fn lookahead_is_record(&self) -> bool {
        // A record field looks like `ident: type`.
        // Stop at newline to avoid scanning into next variant
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
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Dedent) && !self.at(&TokenKind::Eof) {
            let fname = self.expect_ident()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let fty = self.parse_type()?;
            fields.push(Field { name: fname, ty: fty });
            if self.at(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            } else if self.at(&TokenKind::Newline) {
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
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Dedent) || self.at(&TokenKind::Eof) {
                break;
            }
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
            variants.push(Variant { name: vname, payload });
            if self.at(&TokenKind::BitOr) {
                self.advance();
                self.skip_newlines();
            } else if self.at(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            } else if self.at(&TokenKind::Newline) {
                self.advance();
                self.skip_newlines();
            } else if matches!(self.peek_kind(), TokenKind::Ident(_)) {
                // Adjacent variants inside braces: Circle(f64) Rectangle(f64, f64)
                self.skip_newlines();
            } else {
                break;
            }
        }
        Ok(variants)
    }

    fn parse_match_arms(&mut self) -> Result<Vec<MatchArm>, ParseError> {
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
            let body = self.parse_expr(0)?;
            arms.push(MatchArm { pat, guard, body });
            self.skip_newlines();
            if self.at(&TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
            }
        }
        Ok(arms)
    }

    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
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
            _ => Err(ParseError::new(format!("unexpected token in pattern {}", tok.kind), tok.line, tok.col)),
        }
    }

    fn parse_newtype(&mut self) -> Result<TypeDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Newtype)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::Eq, "`=`")?;
        let ty = self.parse_type()?;
        self.match_semi();
        Ok(TypeDef {
            name,
            commitment,
            pub_: false,
            kind: TypeDefKind::Newtype(ty),
        })
    }
}
