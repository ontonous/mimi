use crate::lexer::token::TokenKind;

/// Check if a TokenKind represents a language keyword (not an identifier).
/// This is the single source of truth for keyword membership — add new keywords
/// here rather than duplicating lists in parsers or expression handlers.
/// Check if a TokenKind is a keyword that cannot be used as a bare identifier
/// in expression context (statement keywords like `if`/`while` have their own
/// match arms and don't reach this check).
pub fn is_keyword_kind(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Module
            | TokenKind::Type
            | TokenKind::Func
            | TokenKind::Fn
            | TokenKind::Fault
            | TokenKind::Fails
            | TokenKind::Reset
            | TokenKind::Recover
            | TokenKind::Actor
            | TokenKind::Newtype
            | TokenKind::Let
            | TokenKind::Const
            | TokenKind::Mut
            | TokenKind::Ref
            | TokenKind::Shared
            | TokenKind::LocalShared
            | TokenKind::Weak
            | TokenKind::WeakLocal
            | TokenKind::CShared
            | TokenKind::CBorrow
            | TokenKind::CBorrowMut
            | TokenKind::RawString
            | TokenKind::Arena
            | TokenKind::Alloc
            | TokenKind::Cap
            | TokenKind::Trait
            | TokenKind::Impl
            | TokenKind::Dyn
            | TokenKind::Where
            | TokenKind::Extern
            | TokenKind::If
            | TokenKind::Else
            | TokenKind::For
            | TokenKind::In
            | TokenKind::While
            | TokenKind::Return
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Match
            | TokenKind::Unsafe
            | TokenKind::Use
            | TokenKind::Pub
            | TokenKind::Drop
            | TokenKind::Steps
            | TokenKind::Parasteps
            | TokenKind::Failure
            | TokenKind::Requires
            | TokenKind::Ensures
            | TokenKind::Invariant
            | TokenKind::Math
            | TokenKind::Desc
            | TokenKind::Rule
            | TokenKind::Old
            | TokenKind::Mms
            | TokenKind::With
            | TokenKind::And
            | TokenKind::Or
            | TokenKind::Not
            | TokenKind::Async
            | TokenKind::Comptime
            | TokenKind::Spawn
            | TokenKind::Await
            | TokenKind::Quote
            | TokenKind::Nothing
            | TokenKind::Loop
            | TokenKind::As
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Unit
            | TokenKind::Flow
            | TokenKind::State
            | TokenKind::Transition
            | TokenKind::Protocol
            | TokenKind::Delegate
            | TokenKind::Pinned
            | TokenKind::Persistent
            | TokenKind::View
            | TokenKind::Mutate
            | TokenKind::Consume
            | TokenKind::Do
            | TokenKind::Subflow
            | TokenKind::Session
            | TokenKind::Dual
            | TokenKind::End
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
        "const" => TokenKind::Const,
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
        "fault" => TokenKind::Fault,
        "fails" => TokenKind::Fails,
        "in" => TokenKind::In,
        "while" => TokenKind::While,
        "return" => TokenKind::Return,
        "reset" => TokenKind::Reset,
        "recover" => TokenKind::Recover,
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
        "flow" => TokenKind::Flow,
        "state" => TokenKind::State,
        "transition" => TokenKind::Transition,
        "protocol" => TokenKind::Protocol,
        "delegate" => TokenKind::Delegate,
        "pinned" => TokenKind::Pinned,
        "persistent" => TokenKind::Persistent,
        "view" => TokenKind::View,
        "mutate" => TokenKind::Mutate,
        "consume" => TokenKind::Consume,
        "do" => TokenKind::Do,
        "subflow" => TokenKind::Subflow,
        "session" => TokenKind::Session,
        "dual" => TokenKind::Dual,
        "end" => TokenKind::End,
        "with" => TokenKind::With,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "loop" => TokenKind::Loop,
        "as" => TokenKind::As,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "unit" => TokenKind::Unit,
        "i32" | "i64" | "f64" | "bool" | "string" => TokenKind::Ident(name.into()),
        "nothing" => TokenKind::Nothing,
        _ => TokenKind::Ident(name.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_keyword_kind_covers_statement_keywords() {
        // Regression: `is_keyword_kind` previously omitted statement-start
        // keywords (if/else/for/while/return/break/continue/match/old/invariant),
        // so the parser treated them as valid bare identifiers in expression
        // position. (audit LE-MEDIUM: is_keyword_kind 缺失多个关键字)
        for kind in [
            TokenKind::If,
            TokenKind::Else,
            TokenKind::For,
            TokenKind::While,
            TokenKind::Return,
            TokenKind::Break,
            TokenKind::Continue,
            TokenKind::Match,
            TokenKind::Old,
            TokenKind::Invariant,
        ] {
            assert!(is_keyword_kind(&kind), "{kind:?} should be a keyword");
        }
    }

    #[test]
    fn keyword_or_ident_round_trip() {
        // Spot-check that the lookup table is symmetric with is_keyword_kind
        // for the keys we know must round-trip.
        assert_eq!(keyword_or_ident("if"), TokenKind::If);
        assert_eq!(keyword_or_ident("else"), TokenKind::Else);
        assert_eq!(keyword_or_ident("old"), TokenKind::Old);
        assert_eq!(keyword_or_ident("invariant"), TokenKind::Invariant);
        assert_eq!(keyword_or_ident("nothing"), TokenKind::Nothing);
        // Type names remain identifiers (they're not reserved at lex time).
        assert_eq!(keyword_or_ident("i32"), TokenKind::Ident("i32".into()));
    }

    #[test]
    fn fault_reset_recover_are_keywords() {
        // F-H7: soft keywords must tokenize as keyword kinds.
        assert!(matches!(keyword_or_ident("fault"), TokenKind::Fault));
        assert!(matches!(keyword_or_ident("reset"), TokenKind::Reset));
        assert!(matches!(keyword_or_ident("recover"), TokenKind::Recover));
        assert!(is_keyword_kind(&TokenKind::Fault));
        assert!(is_keyword_kind(&TokenKind::Reset));
        assert!(is_keyword_kind(&TokenKind::Recover));
    }
}
