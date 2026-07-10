use crate::lexer;
use crate::lsp::LspServer;

impl LspServer {
    /// Compute semantic tokens for the document
    pub fn compute_semantic_tokens(&self, text: &str) -> Vec<u32> {
        let mut tokens = Vec::new();

        if let Ok(lexer_tokens) = lexer::Lexer::new(text).tokenize() {
            let mut prev_line = 0u32;
            let mut prev_start = 0u32;

            for tok in &lexer_tokens {
                let line = (tok.line as u32).saturating_sub(1);
                let start = (tok.col as u32).saturating_sub(1);

                // Calculate token length from kind
                let len = match &tok.kind {
                    lexer::TokenKind::Ident(s) => s.len() as u32,
                    lexer::TokenKind::Int(s) => s.len() as u32,
                    lexer::TokenKind::Float(s) => s.len() as u32,
                    lexer::TokenKind::String(s) => s.len() as u32 + 2, // include quotes
                    lexer::TokenKind::FString(s) => s.len() as u32 + 2,
                    _ => {
                        // For keywords/operators, calculate from token kind
                        match tok.kind {
                            lexer::TokenKind::Module => 6,
                            lexer::TokenKind::Type => 4,
                            lexer::TokenKind::Func => 4,
                            lexer::TokenKind::Fn => 2,
                            lexer::TokenKind::Actor => 5,
                            lexer::TokenKind::Let => 3,
                            lexer::TokenKind::Mut => 3,
                            lexer::TokenKind::Return => 6,
                            lexer::TokenKind::If => 2,
                            lexer::TokenKind::Else => 4,
                            lexer::TokenKind::While => 5,
                            lexer::TokenKind::For => 3,
                            lexer::TokenKind::Match => 5,
                            lexer::TokenKind::Spawn => 5,
                            lexer::TokenKind::Await => 5,
                            lexer::TokenKind::Extern => 6,
                            lexer::TokenKind::Trait => 5,
                            lexer::TokenKind::Impl => 4,
                            lexer::TokenKind::Cap => 3,
                            lexer::TokenKind::Async => 5,
                            lexer::TokenKind::True => 4,
                            lexer::TokenKind::False => 5,
                            _ => 1,
                        }
                    }
                };

                let (token_type, modifiers) = match &tok.kind {
                    lexer::TokenKind::Func
                    | lexer::TokenKind::Type
                    | lexer::TokenKind::Module
                    | lexer::TokenKind::Actor
                    | lexer::TokenKind::Trait
                    | lexer::TokenKind::Impl
                    | lexer::TokenKind::Newtype => (0, vec![0]), // keyword + declaration
                    lexer::TokenKind::If
                    | lexer::TokenKind::Else
                    | lexer::TokenKind::While
                    | lexer::TokenKind::For
                    | lexer::TokenKind::Return
                    | lexer::TokenKind::Let
                    | lexer::TokenKind::Mut
                    | lexer::TokenKind::Match
                    | lexer::TokenKind::Spawn
                    | lexer::TokenKind::Await
                    | lexer::TokenKind::Extern
                    | lexer::TokenKind::Cap
                    | lexer::TokenKind::Async
                    | lexer::TokenKind::True
                    | lexer::TokenKind::False
                    | lexer::TokenKind::In
                    | lexer::TokenKind::Break
                    | lexer::TokenKind::Continue
                    | lexer::TokenKind::Use
                    | lexer::TokenKind::Pub
                    | lexer::TokenKind::Drop
                    | lexer::TokenKind::Invariant => (0, vec![]), // keyword
                    lexer::TokenKind::Int(_) | lexer::TokenKind::Float(_) => (4, vec![]), // number
                    lexer::TokenKind::String(_) | lexer::TokenKind::FString(_) => (5, vec![]), // string
                    lexer::TokenKind::Ident(s) => {
                        if s.starts_with(|c: char| c.is_uppercase()) {
                            (2, vec![]) // type/constructor (semantic token type 2 = class)
                        } else if s == "true" || s == "false" {
                            (4, vec![]) // boolean literal (number type)
                        } else {
                            (3, vec![]) // variable/function
                        }
                    }
                    lexer::TokenKind::Plus
                    | lexer::TokenKind::Minus
                    | lexer::TokenKind::Star
                    | lexer::TokenKind::Slash
                    | lexer::TokenKind::Percent
                    | lexer::TokenKind::Eq
                    | lexer::TokenKind::Ne
                    | lexer::TokenKind::Lt
                    | lexer::TokenKind::Gt
                    | lexer::TokenKind::Le
                    | lexer::TokenKind::Ge
                    | lexer::TokenKind::And
                    | lexer::TokenKind::Or
                    | lexer::TokenKind::Not => (7, vec![]), // operator
                    _ => continue,
                };

                let delta_line = line.saturating_sub(prev_line);
                let delta_start = if delta_line == 0 {
                    start.saturating_sub(prev_start)
                } else {
                    start
                };

                tokens.push(delta_line);
                tokens.push(delta_start);
                tokens.push(len);
                tokens.push(token_type);
                tokens.push(modifiers.iter().fold(0u32, |acc, m| acc | (1 << m)));

                prev_line = line;
                prev_start = start;
            }
        }

        tokens
    }
}
