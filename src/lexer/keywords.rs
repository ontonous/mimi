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
            | TokenKind::Fails
            | TokenKind::Actor
            | TokenKind::Newtype
            | TokenKind::Let
            | TokenKind::Const
            | TokenKind::Mut
            | TokenKind::Ref
            | TokenKind::Shared
            | TokenKind::Weak
            | TokenKind::Arena
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
            | TokenKind::Defer
            | TokenKind::Failure
            | TokenKind::Requires
            | TokenKind::Ensures
            | TokenKind::Invariant
            | TokenKind::Math
            | TokenKind::Old
            | TokenKind::Comptime
            | TokenKind::Spawn
            | TokenKind::Await
            | TokenKind::Loop
            | TokenKind::As
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Unit
            | TokenKind::Flow
            | TokenKind::State
            | TokenKind::Transition
            | TokenKind::Pinned
            | TokenKind::Persistent
            | TokenKind::View
            | TokenKind::Mutate
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
        "weak" => TokenKind::Weak,
        "arena" => TokenKind::Arena,
        "cap" => TokenKind::Cap,
        "trait" => TokenKind::Trait,
        "impl" => TokenKind::Impl,
        "dyn" => TokenKind::Dyn,
        "where" => TokenKind::Where,
        "extern" => TokenKind::Extern,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "for" => TokenKind::For,
        "fails" => TokenKind::Fails,
        "in" => TokenKind::In,
        "while" => TokenKind::While,
        "return" => TokenKind::Return,
        "break" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "match" => TokenKind::Match,
        "use" => TokenKind::Use,
        "pub" => TokenKind::Pub,
        "drop" => TokenKind::Drop,
        "defer" => TokenKind::Defer,
        "await" => TokenKind::Await,
        "unsafe" => TokenKind::Unsafe,
        "spawn" => TokenKind::Spawn,
        "comptime" => TokenKind::Comptime,
        "failure" => TokenKind::Failure,
        "requires" => TokenKind::Requires,
        "ensures" => TokenKind::Ensures,
        "invariant" => TokenKind::Invariant,
        "math" => TokenKind::Math,
        "old" => TokenKind::Old,
        "flow" => TokenKind::Flow,
        "state" => TokenKind::State,
        "transition" => TokenKind::Transition,
        "pinned" => TokenKind::Pinned,
        "persistent" => TokenKind::Persistent,
        "view" => TokenKind::View,
        "mutate" => TokenKind::Mutate,
        "session" => TokenKind::Session,
        "dual" => TokenKind::Dual,
        "end" => TokenKind::End,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "loop" => TokenKind::Loop,
        "as" => TokenKind::As,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "unit" => TokenKind::Unit,
        "i32" | "i64" | "f64" | "bool" | "string" => TokenKind::Ident(name.into()),
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
    fn keyword_table_count_is_63_hard_is_60() {
        // 0.36.51+ (Phase D soft-keyword policy lock): the keyword table is
        // deliberately small. This test keeps the count and the hard/soft split
        // from drifting silently. `parasteps`, `fault`, `reset`, and
        // `recover` are no longer in the table.
        let all = [
            "module",
            "type",
            "func",
            "fn",
            "actor",
            "newtype",
            "let",
            "const",
            "mut",
            "ref",
            "shared",
            "weak",
            "arena",
            "cap",
            "trait",
            "impl",
            "dyn",
            "where",
            "extern",
            "if",
            "else",
            "for",
            "fails",
            "in",
            "while",
            "return",
            "break",
            "continue",
            "match",
            "use",
            "pub",
            "drop",
            "defer",
            "await",
            "unsafe",
            "spawn",
            "comptime",
            "failure",
            "requires",
            "ensures",
            "invariant",
            "math",
            "old",
            "flow",
            "state",
            "transition",
            "pinned",
            "persistent",
            "view",
            "mutate",
            "session",
            "dual",
            "end",
            "and",
            "or",
            "not",
            "loop",
            "as",
            "true",
            "false",
            "unit",
        ];
        assert_eq!(all.len(), 61, "keyword table must be exactly 61 entries");
        let soft_operators = ["and", "or", "not"];
        for name in all {
            let kind = keyword_or_ident(name);
            assert!(
                !matches!(kind, TokenKind::Ident(_)),
                "keyword table entry {name:?} must map to a keyword token kind"
            );
            let hard = !soft_operators.contains(&name);
            assert_eq!(
                is_keyword_kind(&kind),
                hard,
                "{name:?}: hard/soft classification mismatch"
            );
        }
        assert!(matches!(keyword_or_ident("quote"), TokenKind::Ident(_)));
        assert!(!is_keyword_kind(&TokenKind::Quote));
        assert!(matches!(keyword_or_ident("parasteps"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("fault"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("reset"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("recover"), TokenKind::Ident(_)));
    }

    #[test]
    fn keyword_or_ident_round_trip() {
        // Spot-check that the lookup table is symmetric with is_keyword_kind
        // for the keys we know must round-trip.
        assert_eq!(keyword_or_ident("if"), TokenKind::If);
        assert_eq!(keyword_or_ident("else"), TokenKind::Else);
        assert_eq!(keyword_or_ident("old"), TokenKind::Old);
        assert_eq!(keyword_or_ident("invariant"), TokenKind::Invariant);
        // Removed zombie keywords tokenize as identifiers again.
        assert!(matches!(keyword_or_ident("nothing"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("c_shared"), TokenKind::Ident(_)));
        assert!(matches!(
            keyword_or_ident("local_shared"),
            TokenKind::Ident(_)
        ));
        assert!(matches!(
            keyword_or_ident("weak_local"),
            TokenKind::Ident(_)
        ));
        assert!(matches!(
            keyword_or_ident("raw_string"),
            TokenKind::Ident(_)
        ));
        // 0.36.50 (Phase D pre-roll): `parasteps` demoted from a hard keyword
        // to a contextual identifier. The lexer no longer reserves it; the
        // parser recognizes `parasteps { ... }` in statement position.
        assert!(matches!(keyword_or_ident("parasteps"), TokenKind::Ident(_)));
        assert!(!is_keyword_kind(&TokenKind::Parasteps));
        assert!(matches!(keyword_or_ident("alloc"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("async"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("with"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("desc"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("rule"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("mms"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("c_borrow"), TokenKind::Ident(_)));
        assert!(matches!(
            keyword_or_ident("c_borrow_mut"),
            TokenKind::Ident(_)
        ));
        // 0.36.52 (Phase D soft-keyword policy): reset/recover are system
        // transition names, not syntax keywords; they tokenize as identifiers.
        assert!(matches!(keyword_or_ident("reset"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("recover"), TokenKind::Ident(_)));
        // Type names remain identifiers (they're not reserved at lex time).
        assert_eq!(keyword_or_ident("i32"), TokenKind::Ident("i32".into()));
    }

    #[test]
    fn flow_words_tokenize_as_identifiers() {
        // F-H7: `fault`/`reset`/`recover` are flow/system names, not global
        // keywords. They tokenize as ordinary identifiers; the parser promotes
        // `fault` to the internal flow declaration token inside a flow body.
        assert!(matches!(keyword_or_ident("fault"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("reset"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("recover"), TokenKind::Ident(_)));
        assert!(!is_keyword_kind(&TokenKind::Fault));
        assert!(!is_keyword_kind(&TokenKind::Reset));
        assert!(!is_keyword_kind(&TokenKind::Recover));
    }

    #[test]
    fn dead_keywords_tokenize_as_identifiers() {
        // v0.34.2: subflow/steps/consume are no longer keywords.
        // - subflow: abolished (amendment clause 2, no nested Flow delegation)
        // - steps: MimiSpec-only, no parser arm
        // - consume: only used by the removed `delegate` construct
        // v0.34 (golden §1.1/§1.4): delegate also softened (89→80 keyword diet);
        // rejected in statement position (parse_stmt.rs) but tokenizes as Ident.
        assert!(matches!(keyword_or_ident("subflow"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("steps"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("consume"), TokenKind::Ident(_)));
        assert!(matches!(keyword_or_ident("delegate"), TokenKind::Ident(_)));
    }

    #[test]
    fn and_or_not_are_soft_keywords() {
        // v0.34.2: and/or/not still tokenize as operator kinds (expression
        // position), but are NOT hard keywords — binding position may use
        // them as identifiers.
        assert!(matches!(keyword_or_ident("and"), TokenKind::And));
        assert!(matches!(keyword_or_ident("or"), TokenKind::Or));
        assert!(matches!(keyword_or_ident("not"), TokenKind::Not));
        assert!(!is_keyword_kind(&TokenKind::And));
        assert!(!is_keyword_kind(&TokenKind::Or));
        assert!(!is_keyword_kind(&TokenKind::Not));
    }
}
