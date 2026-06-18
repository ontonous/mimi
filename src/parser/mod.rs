use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::lexer::{Token, TokenKind};
use crate::span::Span;

mod parse_type;
mod parse_expr;
mod parse_stmt;

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
    pub span: Option<Span>,
}

impl ParseError {
    fn new(message: impl Into<String>, line: usize, col: usize) -> Self {
        Self {
            message: message.into(),
            line,
            col,
            span: None,
        }
    }

    #[allow(dead_code)]
    fn with_span(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            line: span.start_line,
            col: span.start_col,
            span: Some(span),
        }
    }

    /// Convert to the new Diagnostic type.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let span = self.span.unwrap_or_else(|| Span::single(self.line, self.col));
        Diagnostic::error(&self.message, span)
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
    recovery_mode: bool,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self::with_mode(tokens, ParseMode::Production)
    }

    pub fn new_sketch(tokens: Vec<Token>) -> Self {
        Self::with_mode(tokens, ParseMode::Sketch)
    }

    fn with_mode(tokens: Vec<Token>, mode: ParseMode) -> Self {
        Self { tokens, pos: 0, mode, recovery_mode: false }
    }

    /// Create a parser in recovery mode: statement-level errors are caught and skipped.
    #[allow(dead_code)]
    pub fn new_with_recovery(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0, mode: ParseMode::Production, recovery_mode: true }
    }

    /// Skip tokens until we reach a synchronization point.
    /// Returns true if we found a sync point, false if we reached EOF.
    /// Always consumes the sync token to guarantee progress.
    fn recover_to_sync(&mut self, sync_tokens: &[TokenKind]) -> bool {
        let max_skip = 100;
        let mut skipped = 0;
        while !self.at(&TokenKind::Eof) && skipped < max_skip {
            for sync in sync_tokens {
                if self.at(sync) {
                    self.advance(); // consume sync token to guarantee progress
                    return true;
                }
            }
            self.advance();
            skipped += 1;
        }
        !self.at(&TokenKind::Eof)
    }

    /// Get the current token's span.
    #[allow(dead_code)]
    fn current_span(&self) -> Span {
        let tok = self.peek();
        Span::single(tok.line, tok.col)
    }

    /// Get a span from start token to current position.
    #[allow(dead_code)]
    fn span_from(&self, start_line: usize, start_col: usize) -> Span {
        let tok = self.peek();
        Span::new(start_line, start_col, tok.line, tok.col)
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

    /// Check if current position is `alloc(Arena) {` or `alloc(System) {` or `alloc(Bump) {`
    fn is_alloc_block(&self) -> bool {
        if !self.at(&TokenKind::Alloc) {
            return false;
        }
        // Peek ahead: alloc must be followed by LParen
        if self.pos + 1 >= self.tokens.len() {
            return false;
        }
        if self.tokens[self.pos + 1].kind != TokenKind::LParen {
            return false;
        }
        // Check the token after LParen: must be Arena/System/Bump identifier
        if self.pos + 2 >= self.tokens.len() {
            return false;
        }
        matches!(
            &self.tokens[self.pos + 2].kind,
            TokenKind::Arena
        ) || matches!(
            &self.tokens[self.pos + 2].kind,
            TokenKind::Ident(name) if name == "System" || name == "Bump" || name == "Arena"
        )
    }

    fn match_semi(&mut self) {
        // SIF (Semicolon Inference): both explicit `;` and newline act as statement terminators
        if matches!(self.peek_kind(), TokenKind::Semi | TokenKind::Newline) {
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

    /// Parse a file with error recovery, collecting multiple errors.
    /// Returns the parsed file (possibly partial) and all errors encountered.
    pub fn parse_file_with_recovery(mut self) -> (File, Vec<ParseError>) {
        self.recovery_mode = true;
        let mut errors = Vec::new();

        self.skip_newlines();
        let mut imports = Vec::new();
        while self.at(&TokenKind::Use) {
            match self.parse_import() {
                Ok(imp) => imports.push(imp),
                Err(e) => {
                    errors.push(e);
                    // Skip to next top-level sync point
                    self.recover_to_sync(&[
                        TokenKind::Func, TokenKind::Type, TokenKind::Module,
                        TokenKind::Actor, TokenKind::Cap, TokenKind::Trait,
                        TokenKind::Impl, TokenKind::Extern, TokenKind::Use,
                        TokenKind::RBrace, TokenKind::Eof,
                    ]);
                }
            }
            self.skip_newlines();
        }

        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::Eof) {
                break;
            }
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(e) => {
                    errors.push(e);
                    // Skip to next top-level sync point
                    self.recover_to_sync(&[
                        TokenKind::Func, TokenKind::Type, TokenKind::Module,
                        TokenKind::Actor, TokenKind::Cap, TokenKind::Trait,
                        TokenKind::Impl, TokenKind::Extern, TokenKind::Use,
                        TokenKind::RBrace, TokenKind::Eof,
                    ]);
                }
            }
        }

        (File { imports, items }, errors)
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
        // Parse optional #[derive(...)] and #[repr(...)] attributes
        let mut derives = Vec::new();
        let mut attributes = Vec::new();
        while self.at(&TokenKind::Hash) && self.pos + 1 < self.tokens.len() && self.tokens[self.pos + 1].kind == TokenKind::LBracket {
            self.advance(); // skip #
            self.advance(); // skip [
            if self.at(&TokenKind::Ident("derive".to_string())) {
                self.advance(); // skip "derive"
                self.expect(TokenKind::LParen, "`(`")?;
                while !self.at(&TokenKind::RParen) && !self.at(&TokenKind::Eof) {
                    let name = self.expect_ident()?;
                    derives.push(name);
                    if self.at(&TokenKind::Comma) {
                        self.advance();
                    }
                }
                self.expect(TokenKind::RParen, "`)`")?;
            } else if self.at(&TokenKind::Ident("repr".to_string())) {
                self.advance(); // skip "repr"
                self.expect(TokenKind::LParen, "`(`")?;
                let repr_name = self.expect_ident()?;
                match repr_name.as_str() {
                    "C" => attributes.push(crate::ast::TypeAttribute::ReprC),
                    "transparent" => attributes.push(crate::ast::TypeAttribute::ReprTransparent),
                    _ => { /* unknown repr, ignore */ }
                }
                self.expect(TokenKind::RParen, "`)`")?;
            }
            self.expect(TokenKind::RBracket, "`]`")?;
            self.skip_newlines();
        }
        match self.peek_kind() {
            TokenKind::Comptime => {
                // comptime func ... — comptime function modifier
                self.advance();
                let mut f = self.parse_func()?;
                f.pub_ = pub_;
                f.is_comptime = true;
                Ok(Item::Func(f))
            }
            TokenKind::Async => {
                // async func ... — async function modifier
                self.advance();
                let mut f = self.parse_func()?;
                f.pub_ = pub_;
                f.is_async = true;
                Ok(Item::Func(f))
            }
            TokenKind::Func => {
                let mut f = self.parse_func()?;
                f.pub_ = pub_;
                Ok(Item::Func(f))
            }
            TokenKind::Module => Ok(Item::Module(self.parse_module()?)),
            TokenKind::Type => {
                let mut t = self.parse_type_def(derives, attributes)?;
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
            TokenKind::Trait => Ok(Item::Trait(self.parse_trait_def()?)),
            TokenKind::Impl => Ok(Item::Impl(self.parse_impl_def()?)),
            TokenKind::Extern => Ok(Item::ExternBlock(self.parse_extern_block()?)),
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

    fn parse_trait_def(&mut self) -> Result<TraitDef, ParseError> {
        let commitment = self.expect_keyword(TokenKind::Trait)?;
        let name = self.expect_ident()?;
        let generics = self.parse_generic_params()?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut methods = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            // Parse method signature (no body)
            self.expect(TokenKind::Func, "`func`")?;
            let method_name = self.expect_ident()?;
            self.expect(TokenKind::LParen, "`(`")?;
            let params = self.parse_params()?;
            self.expect(TokenKind::RParen, "`)`")?;
            let ret = if self.at(&TokenKind::Arrow) {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            self.match_semi();
            methods.push(TraitMethod {
                name: method_name,
                params,
                ret,
            });
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(TraitDef {
            name,
            commitment,
            methods,
            generics,
        })
    }

    fn parse_impl_def(&mut self) -> Result<ImplDef, ParseError> {
        self.expect(TokenKind::Impl, "`impl`")?;
        let trait_name = self.expect_ident()?;
        self.expect(TokenKind::For, "`for`")?;
        let type_name = self.expect_ident()?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut methods = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            methods.push(self.parse_func()?);
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(ImplDef {
            trait_name,
            type_name,
            methods,
        })
    }

    fn parse_extern_block(&mut self) -> Result<ExternBlock, ParseError> {
        self.expect(TokenKind::Extern, "`extern`")?;
        // Parse optional ABI string: extern "C" { ... }
        let abi = if self.at(&TokenKind::String("".into())) {
            // Get the actual string value
            let tok = self.peek().clone();
            if let TokenKind::String(s) = &tok.kind {
                let abi = s.clone();
                self.advance();
                abi
            } else {
                "C".to_string()
            }
        } else {
            "C".to_string()
        };
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "`{`")?;
        let mut funcs = Vec::new();
        self.skip_newlines();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
                break;
            }
            // Parse extern function signature
            self.expect(TokenKind::Func, "`func`")?;
            let name = self.expect_ident()?;
            self.expect(TokenKind::LParen, "`(`")?;
            let mut params = Vec::new();
            if !self.at(&TokenKind::RParen) {
                loop {
                    // Check for cap @ annotation
                    let cap_mode = if self.at(&TokenKind::Cap) {
                        self.advance();
                        self.expect(TokenKind::At, "`@`")?;
                        Some(CapMode::Move) // Default to move
                    } else if self.at(&TokenKind::BitAnd) {
                        // & means borrow
                        self.advance();
                        Some(CapMode::Borrow)
                    } else {
                        None
                    };
                    let param_name = self.expect_ident()?;
                    self.expect(TokenKind::Colon, "`:`")?;
                    let ty = self.parse_type()?;
                    params.push(ExternParam {
                        name: param_name,
                        ty,
                        cap_mode,
                    });
                    if !self.at(&TokenKind::Comma) {
                        break;
                    }
                    self.advance();
                }
            }
            self.expect(TokenKind::RParen, "`)`")?;
            let ret = if self.at(&TokenKind::Arrow) {
                self.advance();
                Some(self.parse_type()?)
            } else {
                None
            };
            // Parse optional requires/ensures contracts
            let mut requires = None;
            let mut ensures = None;
            loop {
                if self.at(&TokenKind::Requires) {
                    self.advance();
                    requires = Some(self.parse_expr(0)?);
                } else if self.at(&TokenKind::Ensures) {
                    self.advance();
                    ensures = Some(self.parse_expr(0)?);
                } else {
                    break;
                }
            }
            self.match_semi();
            funcs.push(ExternFunc {
                name,
                params,
                ret,
                requires,
                ensures,
            });
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(ExternBlock { abi, funcs })
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
        // Parse optional generic parameters: <T> or <T: Trait>
        let generics = self.parse_generic_params()?;
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
        // Parse where clause if present
        let where_clause = if self.at(&TokenKind::Where) {
            self.advance();
            let type_param = self.expect_ident()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let mut bounds = Vec::new();
            bounds.push(self.expect_ident()?);
            while self.at(&TokenKind::Plus) {
                self.advance();
                bounds.push(self.expect_ident()?);
            }
            Some(WhereClause { type_param, bounds })
        } else {
            None
        };
        self.skip_newlines();
        if self.is_sketch() {
            self.expect(TokenKind::Colon, "`:`")?;
            self.skip_newlines();
        }
        // Parse effects if present: with Effect1, Effect2
        let effects = if self.at(&TokenKind::With) {
            self.advance();
            let mut effects = Vec::new();
            effects.push(self.expect_ident()?);
            while self.at(&TokenKind::Comma) {
                self.advance();
                effects.push(self.expect_ident()?);
            }
            effects
        } else {
            Vec::new()
        };
        self.expect_block_start("function body")?;
        let body = self.parse_block()?;
        Ok(FuncDef {
            name,
            commitment,
            pub_: false,
            params,
            ret,
            body,
            where_clause,
            generics,
            effects,
            is_comptime: false,
            is_async: false,
        })
    }

    fn parse_generic_params(&mut self) -> Result<Vec<GenericParam>, ParseError> {
        if !self.at(&TokenKind::Lt) {
            return Ok(Vec::new());
        }
        self.advance();
        let mut params = Vec::new();
        if !self.at(&TokenKind::Gt) {
            loop {
                let name = self.expect_ident()?;
                let bounds = if self.at(&TokenKind::Colon) {
                    self.advance();
                    let mut b = vec![self.expect_ident()?];
                    while self.at(&TokenKind::Plus) {
                        self.advance();
                        b.push(self.expect_ident()?);
                    }
                    b
                } else {
                    Vec::new()
                };
                params.push(GenericParam { name, bounds });
                if !self.at(&TokenKind::Comma) {
                    break;
                }
                self.advance();
            }
        }
        self.expect(TokenKind::Gt, "`>`")?;
        Ok(params)
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
        if self.is_sketch() {
            self.parse_indent_block()
        } else {
            self.parse_brace_block()
        }
    }

    fn parse_block_with_terminator(&mut self, terminator: TokenKind, label: &str) -> Result<Block, ParseError> {
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
                let span = Span::single(self.peek().line, self.peek().col);
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                let expr = self.parse_expr(0)?;
                stmts.push(Stmt::Requires(expr, span));
                continue;
            }
            if self.at(&TokenKind::Ensures) {
                let span = Span::single(self.peek().line, self.peek().col);
                self.advance();
                self.expect(TokenKind::Colon, "`:`")?;
                let expr = self.parse_expr(0)?;
                stmts.push(Stmt::Ensures(expr, span));
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
        self.expect(terminator, label)?;
        Ok(stmts)
    }

    /// Parse a block with error recovery: catches statement errors and continues.
    /// Always returns Ok with partial results; errors are collected internally.
    fn parse_block_with_recovery(&mut self, terminator: TokenKind, label: &str) -> Result<Block, ParseError> {
        let mut stmts = Vec::new();
        self.skip_newlines();
        while !self.at(&terminator) && !self.at(&TokenKind::Eof) {
            self.skip_newlines();
            if self.at(&terminator) || self.at(&TokenKind::Eof) {
                break;
            }
            if self.at(&TokenKind::Requires) {
                let span = Span::single(self.peek().line, self.peek().col);
                self.advance();
                if self.expect(TokenKind::Colon, "`:`").is_ok() {
                    if let Ok(expr) = self.parse_expr(0) {
                        stmts.push(Stmt::Requires(expr, span));
                    }
                }
                continue;
            }
            if self.at(&TokenKind::Ensures) {
                let span = Span::single(self.peek().line, self.peek().col);
                self.advance();
                if self.expect(TokenKind::Colon, "`:`").is_ok() {
                    if let Ok(expr) = self.parse_expr(0) {
                        stmts.push(Stmt::Ensures(expr, span));
                    }
                }
                continue;
            }
            if self.at(&TokenKind::Desc) {
                self.advance();
                if let Ok(s) = self.expect_string() {
                    stmts.push(Stmt::Desc(s));
                }
                continue;
            }
            if self.at(&TokenKind::Rule) {
                self.advance();
                if let Ok(s) = self.expect_string() {
                    stmts.push(Stmt::Desc(format!("rule: {}", s)));
                }
                continue;
            }
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(_) => {
                    self.advance();
                }
            }
        }
        let _ = self.expect(terminator, label);
        Ok(stmts)
    }

    fn parse_brace_block(&mut self) -> Result<Block, ParseError> {
        self.parse_block_with_terminator(TokenKind::RBrace, "`}`")
    }

    fn parse_indent_block(&mut self) -> Result<Block, ParseError> {
        self.parse_block_with_terminator(TokenKind::Dedent, "dedent")
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