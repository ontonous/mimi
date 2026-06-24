use crate::lexer::token::TokenKind;

/// Check if a TokenKind represents a language keyword (not an identifier).
/// This is the single source of truth for keyword membership — add new keywords
/// here rather than duplicating lists in parsers or expression handlers.
/// Check if a TokenKind is a keyword that cannot be used as a bare identifier
/// in expression context (statement keywords like `if`/`while` have their own
/// match arms and don't reach this check).
pub fn is_keyword_kind(kind: &TokenKind) -> bool {
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
        TokenKind::Nothing |
        TokenKind::Loop |
        TokenKind::True | TokenKind::False | TokenKind::Unit
    )
}

pub fn keyword_or_ident(name: &str) -> TokenKind {
    match name {
        "module" => TokenKind::Module,
        "type" => TokenKind::Type,
        "func" => TokenKind::Func,
        "fn" => TokenKind::Fn,
        "actor" => TokenKind::Actor,
        "newtype" => TokenKind::Newtype,
        "let" => TokenKind::Let,
        "mut" => TokenKind::Mut,
        "ref" => TokenKind::Ref,
        "shared" => TokenKind::Shared,
        "local_shared" => TokenKind::LocalShared,
        "weak" => TokenKind::Weak,
        "weak_local" => TokenKind::WeakLocal,
        "c_shared" => TokenKind::CShared,
        "c_borrow" => TokenKind::CBorrow,
        "c_borrow_mut" => TokenKind::CBorrowMut,
        "raw_string" => TokenKind::RawString,
        "arena" => TokenKind::Arena,
        "alloc" => TokenKind::Alloc,
        "cap" => TokenKind::Cap,
        "trait" => TokenKind::Trait,
        "impl" => TokenKind::Impl,
        "dyn" => TokenKind::Dyn,
        "where" => TokenKind::Where,
        "extern" => TokenKind::Extern,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "for" => TokenKind::For,
        "in" => TokenKind::In,
        "while" => TokenKind::While,
        "return" => TokenKind::Return,
        "break" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "match" => TokenKind::Match,
        "use" => TokenKind::Use,
        "pub" => TokenKind::Pub,
        "drop" => TokenKind::Drop,
        "await" => TokenKind::Await,
        "async" => TokenKind::Async,
        "unsafe" => TokenKind::Unsafe,
        "spawn" => TokenKind::Spawn,
        "steps" => TokenKind::Steps,
        "parasteps" => TokenKind::Parasteps,
        "quote" => TokenKind::Quote,
        "comptime" => TokenKind::Comptime,
        "failure" => TokenKind::Failure,
        "requires" => TokenKind::Requires,
        "ensures" => TokenKind::Ensures,
        "invariant" => TokenKind::Invariant,
        "math" => TokenKind::Math,
        "desc" => TokenKind::Desc,
        "rule" => TokenKind::Rule,
        "old" => TokenKind::Old,
        "mms" => TokenKind::Mms,
        "with" => TokenKind::With,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "loop" => TokenKind::Loop,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "unit" => TokenKind::Unit,
        "i32" | "i64" | "f64" | "bool" | "string" => TokenKind::Ident(name.into()),
        "nothing" => TokenKind::Nothing,
        _ => TokenKind::Ident(name.into()),
    }
}
