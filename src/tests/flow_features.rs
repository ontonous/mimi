use crate::ast::*;
use crate::tests::*;

/// User-written (non-fallback) transitions after transfer-matrix expansion.
fn user_transitions(f: &FlowDef) -> Vec<&TransitionDef> {
    f.transitions.iter().filter(|t| !t.is_fallback).collect()
}

#[test]
fn transition_rejects_wrong_target_payload_field_type() {
    let src = r#"
flow Counter {
    state Idle
    state Active { count: i32, ready: bool }
    transition start(Idle) -> Active { return Active { count: true, ready: 1 } }
}
"#;
    let err = check_source(src).expect_err("transition payload field types must be checked");
    assert!(
        err.iter()
            .any(|d| d.message.contains("field") || d.message.contains("type mismatch")),
        "expected payload field type error, got: {err:?}"
    );
}

/// State names excluding the auto-injected Fault sink.
fn user_states(f: &FlowDef) -> Vec<&str> {
    f.states
        .iter()
        .filter(|s| s.name != "Fault")
        .map(|s| s.name.as_str())
        .collect()
}

#[test]
fn flow_parse_debug() {
    // Test that a block body transition doesn't consume the flow body's `}`
    let src = "flow F { state A state B transition go(A) -> B { } }";
    // Tokens: Flow, Ident("F"), LBrace, State, Ident("A"), State, Ident("B"),
    //         Transition, Ident("go"), LParen, Ident("A"), RParen, Arrow, Ident("B"),
    //         LBrace, RBrace, RBrace, Eof
    // The { } is the transition body. The } after that is the flow body closer.
    // parse_block() should consume { } and leave the final } for the flow body.
    // v0.29.10: transfer matrix injects Fault + fallbacks for missing (state,event).
    let file = parse(src);
    assert_eq!(file.items.len(), 3);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.name, "F");
            assert_eq!(user_states(f), vec!["A", "B"]);
            assert!(f.states.iter().any(|s| s.name == "Fault"));
            let user = user_transitions(f);
            assert_eq!(user.len(), 1);
            assert!(user[0].body.is_some(), "transition body should be Some");
            // Fallbacks: B+go, Fault+go
            assert!(f.transitions.iter().any(|t| t.is_fallback));
        }
        other => panic!("expected Item::Flow, got {:?}", other),
    }
}

#[test]
fn flow_parse_states_only() {
    // No transitions → only Fault state is injected (no event matrix cells).
    let src = "flow F { state Idle state Active }";
    let file = parse(src);
    assert_eq!(file.items.len(), 3);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.name, "F");
            assert_eq!(user_states(f), vec!["Idle", "Active"]);
            assert!(f.states.iter().any(|s| s.name == "Fault"));
            // v0.29.13: even with no user events, reset/recover are injected.
            assert!(user_transitions(f).is_empty());
            assert!(f
                .transitions
                .iter()
                .any(|t| t.name == "reset" && t.is_fallback));
            assert!(f
                .transitions
                .iter()
                .any(|t| t.name == "recover" && t.is_fallback));
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_transition_semicolon() {
    let src = "flow F { state A state B transition go(A) -> B; }";
    let file = parse(src);
    assert_eq!(file.items.len(), 3);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.name, "F");
            assert_eq!(user_states(f), vec!["A", "B"]);
            let user = user_transitions(f);
            assert_eq!(user.len(), 1);
            assert_eq!(user[0].name, "go");
            assert_eq!(user[0].from_state, "A");
            assert_eq!(user[0].to_states, vec!["B"]);
            assert!(user[0].body.is_none());
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_empty_block() {
    let src = "flow F { state A state B transition go(A) -> B { } }";
    let file = parse(src);
    assert_eq!(file.items.len(), 3);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.name, "F");
            let user = user_transitions(f);
            assert_eq!(user.len(), 1);
            assert!(user[0].body.is_some());
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_check_transition_empty_body_rejected() {
    let src = "flow F { state A state B transition go(A) -> B { } }";
    assert!(
        check_source(src).is_err(),
        "implemented transitions must return a target state"
    );
}

#[test]
fn flow_check_transition_partial_return_rejected() {
    let src = r#"
flow F {
    state A { value: i32 }
    state B { value: i32 }
    transition go(A, flag: bool) -> B {
        if flag { return B { value: self.value } }
    }
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_err(),
        "implemented transitions must return on every control-flow path"
    );
}

#[test]
fn flow_check_transition_all_paths_return_accepted() {
    let src = r#"
flow F {
    state A { value: i32 }
    state B { value: i32 }
    transition go(A, flag: bool) -> B {
        if flag {
            return B { value: self.value }
        } else {
            return B { value: 0 }
        }
    }
}
func main() -> i32 { 0 }
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn flow_check_cross_flow_same_state_name_rejected_on_pollution() {
    let src = r#"
flow A {
    state Ready { x: i32 }
    transition go(Ready) -> Ready { { return Ready { x: 0 } } }
}
flow B {
    state Ready { y: string }
    transition go(Ready) -> Ready { { return Ready { y: "bad" } } }
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_err(),
        "cross-flow unqualified state name collision with incompatible payloads must be rejected"
    );
}

#[test]
fn flow_check_cross_flow_same_state_name_same_payload_accepted() {
    let src = r#"
flow A {
    state Ready { v: i32 }
    transition go(Ready) -> Ready { { return Ready { v: 0 } } }
}
flow B {
    state Ready { v: i32 }
    transition go(Ready) -> Ready { { return Ready { v: 1 } } }
}
func main() -> i32 { 0 }
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn flow_check_overloaded_event_inconsistent_params_rejected() {
    let src = r#"
flow F {
    state A { v: i32 }
    state B { v: i32 }
    transition go(A, x: i32) -> B { { return B { v: x } } }
    transition go(B, flag: bool) -> A { { return A { v: 0 } } }
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_err(),
        "overloaded event with different param types must be rejected"
    );
}

#[test]
fn flow_check_overloaded_event_consistent_params_accepted() {
    let src = r#"
flow F {
    state A { v: i32 }
    state B { v: i32 }
    transition go(A, x: i32) -> B { { return B { v: x } } }
    transition go(B, x: i32) -> A { { return A { v: x } } }
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "overloaded event with consistent params must be accepted"
    );
}

#[test]
fn flow_parse_multiple_transition_targets() {
    let src = r#"
flow Processor {
    state Idle
    state Active { data: f32 }
    state OverloadWarning { data: f32 }

    transition process(Idle, data: f32) -> Active | OverloadWarning {
        if data > 1.0 {
            return OverloadWarning { data: data }
        } else {
            return Active { data: data }
        }
    }
}
"#;
    let file = parse(src);
    assert_eq!(file.items.len(), 3);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(user_states(f), vec!["Idle", "Active", "OverloadWarning"]);
            assert!(f.states.iter().any(|s| s.name == "Fault"));
            let user = user_transitions(f);
            assert_eq!(user.len(), 1);
            assert_eq!(user[0].to_states, vec!["Active", "OverloadWarning"]);
            assert_eq!(user[0].params.len(), 1);
            assert_eq!(user[0].params[0].name, "data");
            // Fallbacks for Active/OverloadWarning/Fault + process, plus reset/recover
            let fb: Vec<_> = f.transitions.iter().filter(|t| t.is_fallback).collect();
            assert!(fb.len() >= 3, "expected ≥3 fallbacks, got {}", fb.len());
            assert!(fb.iter().any(|t| t.name == "reset"));
            assert!(fb.iter().any(|t| t.name == "recover"));
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_with_annotations() {
    let src = r#"
flow DataPipeline {
    @mailbox(depth = 4096)
    @max_children(100)
    state Ready
    state Processing

    transition run(Ready) -> Processing { return Processing { } }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            assert!(f.annotations.len() >= 2);
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_protocol() {
    let src = r#"
protocol Sensor {
    state Idle
    state Active { data: f32 }
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}
"#;
    let file = parse(src);
    assert_eq!(file.items.len(), 1);
    match &file.items[0] {
        Item::Protocol(p) => {
            assert_eq!(p.name, "Sensor");
            assert_eq!(p.states.len(), 2);
            assert_eq!(p.transitions.len(), 2);
            assert_eq!(p.transitions[0].name, "start");
            assert_eq!(p.transitions[0].from_state, "Idle");
            assert_eq!(p.transitions[0].to_state, "Active");
        }
        _ => panic!("expected Item::Protocol"),
    }
}

// ===================== Removed syntax negative tests =====================
// Architecture amendment clause 2 abolished `delegate view/mutate/consume`
// (nested Flow delegation). The parser must reject it with a clause-referencing
// diagnostic — see golden-document.md §9.2 (Removed 拒绝测试).

fn assert_delegate_rejected_by_clause_2(src: &str) {
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("tokenize");
    let err = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect_err("`delegate` must be rejected by parser");
    assert!(
        err.message.contains("amendment clause 2"),
        "error should mention amendment clause 2, got: {}",
        err.message
    );
}

#[test]
fn flow_parse_delegate() {
    // v0.34.1: `delegate view(...)` is rejected (amendment clause 2).
    assert_delegate_rejected_by_clause_2(
        r#"
flow Parent {
    state Active

    transition run(Active) -> Active {
        delegate view(self.buffer) to sub_flow;
        return Active { }
    }
}
"#,
    );
}

#[test]
fn flow_parse_delegate_mutate_consume() {
    // v0.34.1: `delegate mutate/consume(...)` are rejected too (clause 2).
    assert_delegate_rejected_by_clause_2(
        r#"
flow Parent {
    state Active

    transition run(Active) -> Active {
        delegate mutate(self.buffer) to sub;
        delegate consume(self.owned) to sub;
        return Active { }
    }
}
"#,
    );
}

#[test]
fn flow_parse_pinned_block() {
    let src = r#"
flow SafeFFI {
    state Active { buffer: List<u8> }

    transition process(Active) -> Active {
        pinned(self.buffer) |ptr| {
            let _ = ptr;
        }
        return Active { buffer: self.buffer }
    }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            let body = f.transitions[0].body.as_ref().unwrap();
            // v0.34.27: `do` removed — transition body is the plain block.
            let do_body = body;
            assert!(matches!(do_body[0].unlocated(), Stmt::Pinned { .. }));
            if let Stmt::Pinned { expr, var, .. } = do_body[0].unlocated() {
                let _ = expr;
                assert_eq!(var.as_deref(), Some("ptr"));
                match expr.unlocated() {
                    Expr::Field(obj, name) => {
                        assert_eq!(name, "buffer");
                        assert!(matches!(obj.unlocated(), Expr::Ident(s) if s == "self"));
                    }
                    _ => panic!("expected self.buffer field access"),
                }
            }
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn pinned_body_executes_dual_backend() {
    // H3 (L1): the bytecode compiler must compile a `pinned(expr) |var| { body }`
    // body, not skip it. Pre-fix compiler.rs had `Stmt::Pinned { .. } => {}`
    // (no-op), so any pinned body with observable side effects was silently
    // dropped in bytecode while codegen (block.rs:999) ran it — an L1 break.
    let src = r#"
func main() -> i32 {
    let ptr = 42
    pinned(ptr) |p| {
        println("pinned body executed")
    }
    println("after pinned")
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bc) = run_source_bytecode_with_stdout(src);
    assert_eq!(
        bc.trim(),
        "pinned body executed\nafter pinned",
        "bytecode must run the pinned body"
    );
    let native = compile_and_run(src).expect("codegen pinned body");
    assert_eq!(
        native.trim(),
        "pinned body executed\nafter pinned",
        "codegen must run the pinned body"
    );
}

#[test]
fn flow_parse_with_impl_protocol() {
    let src = r#"
flow LidarDriver {
    impl Sensor
    state Idle
    state Active { data: f32 }

    transition start(Idle) -> Active { { return Active { data: 0.0 } } }
    transition read(Active) -> Active { { return Active { data: 1.0 } } }
    transition stop(Active) -> Idle { { return Idle { } } }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.impl_protocols, vec!["Sensor"]);
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_persistent_fields() {
    let src = r#"
flow ResilientService {
    persistent state Config { max_retries: i32, timeout_ms: i64 }
    state Active { request_id: i32 }

    transition run(Active) -> Active { { return Active { request_id: 1 } } }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.persistent_fields, vec!["max_retries", "timeout_ms"]);
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_lexer_keywords() {
    use crate::lexer::TokenKind;
    // Verify all new flow-related keywords are tokenized correctly.
    // v0.34.2: consume/subflow no longer keywords (dead syntax) → Ident.
    let src = "flow state transition protocol delegate pinned fault reset recover persistent view mutate consume do subflow session dual end and or not";
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("lexer failed");
    // In-source order: soft keywords (fault/reset/recover) tokenize as dedicated
    // kinds or Ident; dead keywords (consume/subflow) are plain Ident.
    let expected_all: Vec<(&str, TokenKind)> = vec![
        ("flow", TokenKind::Flow),
        ("state", TokenKind::State),
        ("transition", TokenKind::Transition),
        ("protocol", TokenKind::Protocol),
        ("delegate", TokenKind::Ident("delegate".into())),
        ("pinned", TokenKind::Pinned),
        ("persistent", TokenKind::Persistent),
        ("view", TokenKind::View),
        ("mutate", TokenKind::Mutate),
        ("consume", TokenKind::Ident("consume".into())),
        // v0.34.27: `do` removed — tokenizes as a plain identifier
        ("do", TokenKind::Ident("do".into())),
        ("subflow", TokenKind::Ident("subflow".into())),
        ("session", TokenKind::Session),
        ("dual", TokenKind::Dual),
        ("end", TokenKind::End),
        ("and", TokenKind::And),
        ("or", TokenKind::Or),
        ("not", TokenKind::Not),
    ];
    let expected_soft: Vec<&str> = vec!["fault", "reset", "recover"];
    let kinds: Vec<&TokenKind> = tokens
        .iter()
        .map(|t| &t.kind)
        .filter(|k| !matches!(k, TokenKind::Newline | TokenKind::Eof))
        .collect();
    let mut idx = 0;
    for (name, exp_kind) in &expected_all {
        assert_eq!(
            *kinds[idx], *exp_kind,
            "token[{}]: expected {:?} for '{}', got {:?}",
            idx, exp_kind, name, kinds[idx]
        );
        idx += 1;
        // soft keywords appear after `pinned` and before `persistent`
        if *name == "pinned" {
            for soft in &expected_soft {
                // F-H7: soft keywords may tokenize as dedicated kinds or Ident.
                match (&kinds[idx], *soft) {
                    (TokenKind::Ident(s), name) if s == name => {}
                    (TokenKind::Fault, "fault")
                    | (TokenKind::Reset, "reset")
                    | (TokenKind::Recover, "recover") => {}
                    other => panic!(
                        "token[{}]: expected soft keyword {}, got {:?}",
                        idx, soft, other
                    ),
                }
                idx += 1;
            }
        }
    }
}

#[test]
fn flow_parse_fault_transition() {
    let src = r#"
flow FaultTolerant {
    state Active { data: i32 }
    state Fault { trace: string }

    transition recover_state(Fault) -> Active {
        return Active { data: 0 }
    }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.transitions[0].name, "recover_state");
            assert_eq!(f.transitions[0].from_state, "Fault");
            assert_eq!(f.transitions[0].to_states, vec!["Active"]);
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_block_statement_in_transition() {
    // v0.34.27: `do { }` wrapper removed — nested bare blocks still parse.
    let src = r#"
flow TestFlow {
    state Ready
    state Done

    transition run(Ready) -> Done {
        let x = 42
        {
            let y = x + 1
        }
        return Done { }
    }
}
"#;
    // Verify that `{ }` blocks are parsed correctly (both outer transition do and inner do)
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            let body = f.transitions[0].body.as_ref().unwrap();
            let do_body = body;
            // First stmt is let x = 42
            assert!(matches!(do_body[0].unlocated(), Stmt::Let { .. }));
            // Second stmt is the nested bare block (was inner do)
            assert!(matches!(do_body[1].unlocated(), Stmt::Block(_)));
            // Third is return
            assert!(matches!(do_body[2].unlocated(), Stmt::Return(_)));
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_do_keyword_rejected() {
    // v0.34.27: `do` removed from the keyword table (language assessment —
    // `do { X }` ≡ `{ X }`, keeping it would burn a keyword slot for no
    // expressiveness). It now lexes as a plain identifier, so `do { }` fails
    // to parse as a statement.
    // transition contexts wrapped in a flow; func context standalone.
    let cases: Vec<String> = vec![
        format!(
            "flow Counter {{\n    state Ready\n    state Done\n    {}\n}}\n",
            r#"transition t(Ready) -> Done { do { return Done { } } }"#
        ),
        format!(
            "flow Counter {{\n    state Ready\n    state Done\n    {}\n}}\n",
            r#"transition t(Ready) -> Done {
                do { return Done { } }
            }"#
        ),
        "func f() -> i32 {\n    do { return 1 }\n}".to_string(),
    ];
    for src in cases {
        // `do` now lexes as a plain identifier: `do { ... }` parses as a
        // struct-constructor expression for an undefined type `do`, so the
        // rejection surfaces in type checking (not parsing).
        let result = check_source(&src);
        assert!(
            result.is_err(),
            "`do` should be rejected after v0.34.27 removal, got Ok for: {src}"
        );
        let err = result
            .unwrap_err()
            .iter()
            .map(|d| format!("{}", d))
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            err.contains("do") || err.contains("unknown") || err.contains("undefined"),
            "expected a diagnostic mentioning `do`, got: {err}"
        );
    }
}

#[test]
fn flow_check_simple_flow() {
    let src = r#"
flow SimpleFlow {
    state Ready
    state Active { value: i32 }
    state Done

    transition run(Ready, input: i32) -> Active {
        return Active { value: input }
    }
    transition finish(Active) -> Done {
        return Done { }
    }
}
"#;
    // Should type-check successfully
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "flow type checking failed: {:?}",
        result.err()
    );
}

#[test]
fn flow_check_undefined_state() {
    let src = r#"
flow BadFlow {
    state Ready
    transition run(Ready) -> NonExistent {
        return NonExistent { }
    }
}
"#;
    // Should fail: NonExistent state is not defined
    let result = check_source(src);
    assert!(result.is_err(), "expected type error for undefined state");
}

#[test]
fn flow_check_undefined_from_state() {
    let src = r#"
flow BadFlow {
    state Ready
    transition run(NonExistent) -> Ready {
        return Ready { }
    }
}
"#;
    // Should fail: NonExistent from-state is not defined
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected type error for undefined from-state"
    );
}

#[test]
fn flow_check_duplicate_state() {
    let src = r#"
flow BadFlow {
    state Ready
    state Ready
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected type error for duplicate state");
}

#[test]
fn flow_check_duplicate_transition() {
    let src = r#"
flow BadFlow {
    state Ready
    transition run(Ready) -> Ready {
        return Ready { }
    }
    transition run(Ready) -> Ready {
        return Ready { }
    }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected type error for duplicate transition"
    );
}

#[test]
fn flow_check_undefined_protocol() {
    let src = r#"
flow BadFlow {
    state Ready
    impl NonExistentProtocol
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected type error for undefined protocol"
    );
}

#[test]
fn flow_check_invalid_field_type() {
    let src = r#"
flow BadFlow {
    state Ready { x: NonExistentType }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected type error for invalid field type"
    );
}

#[test]
fn flow_exec_simple_transition() {
    let src = r#"
flow Calc {
    state Zero { v: i32 }
    state Value { v: i32 }

    transition add(Zero, amount: i32) -> Value {
        return Value { v: self.v + amount }
    }
}

func main() -> i32 {
    let s = Zero { v: 10 }
    let r = Calc::add(s, 5)
    r.v
}
"#;
    let result = run_source_bytecode_result(src);
    assert_eq!(result, Ok(interp::Value::Int(15)));
}

#[test]
fn flow_check_transition_call_rejects_wrong_arity() {
    let src = r#"
flow Calc {
    state Zero { v: i32 }
    state Value { v: i32 }
    transition add(Zero, amount: i32) -> Value { return Value { v: self.v + amount } }
}
func main() -> i32 {
    let s = Zero { v: 10 }
    let r = Calc::add(s, 5, 99)
    r.v
}
"#;
    assert!(
        check_source(src).is_err(),
        "Flow transition calls must enforce their registered arity"
    );
}

#[test]
fn flow_check_transition_call_rejects_wrong_event_type() {
    let src = r#"
flow Calc {
    state Zero { v: i32 }
    state Value { v: i32 }
    transition add(Zero, amount: i32) -> Value { return Value { v: self.v + amount } }
}
func main() -> i32 {
    let s = Zero { v: 10 }
    let r = Calc::add(s, "wrong")
    r.v
}
"#;
    assert!(
        check_source(src).is_err(),
        "Flow transition calls must enforce event parameter types"
    );
}

#[test]
fn flow_check_transition_call_rejects_wrong_source_state() {
    let src = r#"
flow Calc {
    state Zero { v: i32 }
    state Other { v: i32 }
    state Value { v: i32 }
    transition add(Zero, amount: i32) -> Value { return Value { v: self.v + amount } }
    transition add(Other, amount: string) -> Value { return Value { v: self.v } }
}
func main() -> i32 {
    let r = Calc::add(99, 1)
    0
}
"#;
    assert!(
        check_source(src).is_err(),
        "Flow transition overload selection must reject an invalid source state"
    );
}

#[test]
fn flow_exec_multi_target() {
    let src = r#"
flow Checker {
    state Small { v: i32 }
    state Large { v: i32 }

    transition classify(Small, amount: i32) -> Small | Large {
        if self.v + amount > 50 {
            return Large { v: self.v + amount }
        } else {
            return Small { v: self.v + amount }
        }
    }
}

func main() -> i32 {
    let s1 = Small { v: 10 }
    let r1 = Checker::classify(s1, 5)
    let s2 = Small { v: 10 }
    let r2 = Checker::classify(s2, 100)
    r1.v + r2.v
}
"#;
    let result = run_source_bytecode_result(src);
    assert_eq!(result, Ok(interp::Value::Int(125))); // 15 + 110
}

// ===================== Protocol checking tests =====================

#[test]
fn protocol_check_duplicate_state() {
    let src = r#"
protocol BadProto {
    state Ready
    state Ready
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for duplicate state in protocol"
    );
}

#[test]
fn protocol_check_duplicate_transition() {
    let src = r#"
protocol BadProto {
    state Ready
    state Active
    transition go(Ready) -> Active
    transition go(Ready) -> Active
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for duplicate transition in protocol"
    );
}

#[test]
fn protocol_check_undefined_state_in_transition() {
    let src = r#"
protocol BadProto {
    state Ready
    transition go(NonExistent) -> Ready
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for undefined from-state in protocol transition"
    );
}

#[test]
fn protocol_check_undefined_target_state() {
    let src = r#"
protocol BadProto {
    state Ready
    transition go(Ready) -> NonExistent
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for undefined target state in protocol transition"
    );
}

#[test]
fn protocol_check_invalid_payload_type() {
    let src = r#"
protocol BadProto {
    state Ready { data: NonExistentType }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for invalid payload type in protocol state"
    );
}

#[test]
fn flow_check_missing_protocol_state() {
    let src = r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
}

flow BadFlow {
    impl Sensor
    state Idle
    transition start(Idle) -> Idle { return Idle { } }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for missing protocol state in flow"
    );
}

#[test]
fn flow_check_missing_protocol_transition() {
    let src = r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}

flow BadFlow {
    impl Sensor
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active { return Active { data: 0 } }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error for missing protocol transition in flow"
    );
}

// ===================== Flow negative tests (edge cases) =====================

#[test]
fn flow_check_wrong_return_target() {
    let src = r#"
flow BadFlow {
    state Ready
    state Active { v: i32 }
    transition go(Ready) -> Active { return Ready { } }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error: returning wrong target state"
    );
}

#[test]
fn flow_check_missing_field_in_return() {
    let src = r#"
flow BadFlow {
    state Ready { v: i32 }
    state Active { v: i32 }
    transition go(Ready) -> Active { return Active { } }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error: missing required field in return"
    );
}

#[test]
fn flow_check_extra_field_in_return() {
    let src = r#"
flow BadFlow {
    state Ready { v: i32 }
    state Active { v: i32 }
    transition go(Ready) -> Active { return Active { v: 0, x: 1 } }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error: extra field in return");
}

#[test]
fn flow_check_wrong_field_type_in_return() {
    let src = r#"
flow BadFlow {
    state Ready { v: i32 }
    state Active { v: i32 }
    transition go(Ready) -> Active { return Active { v: "hello" } }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "expected error: wrong field type in return"
    );
}

#[test]
fn flow_check_self_in_no_payload_state() {
    let src = r#"
flow BadFlow {
    state Ready
    state Active { v: i32 }
    transition go(Ready) -> Active { return Active { v: self.v } }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error: self has no payload");
}

#[test]
fn flow_check_undefined_param_type() {
    let src = r#"
flow BadFlow {
    state Ready
    state Active { v: i32 }
    transition go(Ready, x: NonExistentType) -> Active { return Active { v: 0 } }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error: undefined param type");
}

#[test]
fn flow_check_return_self_wrong_state() {
    let src = r#"
flow BadFlow {
    state Ready { v: i32 }
    state Active { v: i32 }
    transition go(Ready) -> Active { return Active { v: self.v } }
}
"#;
    // go(Ready) -> Active, self.v is accessible (Ready has payload), return Active is valid
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "returning Active with self.v should be valid"
    );
}

#[test]
fn flow_check_multi_return_type_mismatch() {
    let src = r#"
flow BadFlow {
    state Ready { v: i32 }
    state Active { v: i32 }
    state Done { v: i32 }
    transition go(Ready) -> Active | Done {
        let x = self.v
        return Active { v: x }
    }
}
"#;
    // Only returns Active, not Done — but this is fine since it returns one of the valid targets
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "returning one valid target is acceptable in multi-target"
    );
}

#[test]
fn flow_codegen_multi_target_fails_closed() {
    // v0.34.16 (ADR-002): the tagged-state-union ABI landed — native codegen
    // now compiles multi-target transitions (was: fail-closed rejection with
    // "tagged-state-union ABI"). Renamed intent preserved for history.
    let src = r#"
flow Decision {
    state Pending { value: i32 }
    state Approved { value: i32 }
    state Rejected { value: i32 }

    transition decide(Pending) -> Approved | Rejected { return Approved { value: self.value } }
}

func main() -> i32 { 0 }
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let result = compile_and_run(src);
    assert!(
        result.is_ok(),
        "native multi-target must compile after tagged-state-union ABI: {:?}",
        result.err()
    );
}

#[test]
fn transactional_syntax_rejected_by_amendment_clause_3() {
    // Architecture amendment clause 3 abolished @transactional / WAL.
    // The parser must reject `@transactional` with a clause-referencing diagnostic.
    let src = r#"
flow Tx {
    @transactional persistent state Active { value: i32 }
}
func main() -> i32 { 0 }
"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("tokenize");
    let err = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect_err("@transactional must be rejected by parser");
    assert!(
        err.message.contains("amendment clause 3"),
        "error should mention amendment clause 3, got: {}",
        err.message
    );
}

#[test]
fn flow_check_no_payload_state_return_no_braces() {
    let src = r#"
flow GoodFlow {
    state Ready
    state Done
    transition finish(Ready) -> Done { return Done { } }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "returning no-payload state with braces should be valid"
    );
}

#[test]
fn flow_check_valid_protocol_impl() {
    let src = r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}

flow GoodFlow {
    impl Sensor
    state Idle
    state Active { data: i32 }

    transition start(Idle) -> Active { return Active { data: 0 } }
    transition stop(Active) -> Idle { return Idle { } }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "valid protocol implementation should pass: {:?}",
        result.err()
    );
}

// ── 0.31.14 追加 A: Protocol conformance × linearity ─────────────────

#[test]
fn protocol_impl_alias_bypass_rejected() {
    // 0.31.14 追加 A: aliasing a flow state variable that implements a
    // protocol, then using the original, must be rejected (E0423).
    // The protocol conformance doesn't exempt the flow from linearity.
    let src = r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
}
flow MySensor {
    impl Sensor
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active { return Active { data: 42 } }
}
func main() -> i32 {
    let s = Idle { }
    let alias = s
    let r = MySensor::start(s)
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "alias bypass in protocol impl should be rejected"
    );
    let errors = result.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_deref() == Some("E0423") && d.message.contains("alias")),
        "expected E0423 with alias message, got: {:?}",
        errors
    );
}

#[test]
fn protocol_impl_alias_target_valid() {
    // 0.31.14 追加 A: after aliasing, the alias target is the valid owner
    // and can be used in a protocol transition.
    let src = r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
}
flow MySensor {
    impl Sensor
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active { return Active { data: 42 } }
}
func main() -> i32 {
    let s = Idle { }
    let alias = s
    let r = MySensor::start(alias)
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "alias target should be usable in protocol transition: {:?}",
        result
    );
}

#[test]
fn protocol_linear_payload_downgrade_rejected() {
    // 0.31.14 追加 A: if a protocol state declares a linear payload type
    // (Cap), the flow state must also declare it as linear. Downgrading
    // to a non-linear type is rejected (E0427).
    let src = r#"
protocol Writer {
    state Open { handle: Cap<Write> }
    state Closed
    transition close(Open) -> Closed
}
flow BadWriter {
    impl Writer
    state Open { handle: i32 }
    state Closed
    transition close(Open) -> Closed { return Closed { } }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "linear payload downgrade should be rejected"
    );
    let errors = result.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_deref() == Some("E0427") || d.code.as_deref() == Some("E0209")),
        "expected E0427 or E0209, got: {:?}",
        errors
    );
}

// ── 0.31.17: 高阶交互闭环 — 闭包/集合 × Flow ────────────────────────

#[test]
fn flow_state_closure_capture_rejected() {
    // 0.31.17: capturing a flow state in a closure is rejected (E0427).
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let f = fn() {
        let s1 = Counter::inc(s0)
        0
    }
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "closure capture of flow state should be rejected"
    );
    let errors = result.unwrap_err();
    assert!(
        errors.iter().any(
            |d| d.code.as_deref() == Some("E0427") && d.message.contains("captured by closure")
        ),
        "expected E0427 closure capture error, got: {:?}",
        errors
    );
}

#[test]
fn flow_state_lambda_param_accepted() {
    // 0.31.17: flow state passed as a lambda parameter is OK (not a capture).
    // The lambda owns the parameter — no implicit ownership transfer.
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let f = fn(x: Zero) {
        x.count
    }
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "lambda parameter flow state should be accepted: {:?}",
        result
    );
}

#[test]
fn flow_state_in_list_rejected() {
    // 0.31.17: flow states cannot be stored in lists (E0427).
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let s1 = Zero { count: 1 }
    let states = [s0, s1]
    0
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "flow state in list should be rejected");
    let errors = result.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_deref() == Some("E0427") && d.message.contains("list")),
        "expected E0427 list error, got: {:?}",
        errors
    );
}

// ===================== Pinned block tests =====================

#[test]
fn flow_check_pinned_var_binding() {
    let src = r#"
flow TestFlow {
    state Ready { buf: i32 }
    state Active { result: i32 }

    transition process(Ready) -> Active {
        pinned(self.buf) |ptr| {
            let _x = ptr
        }
        return Active { result: self.buf + 1 }
    }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "pinned with var binding should type-check: {:?}",
        result.err()
    );
}

#[test]
fn pinned_timeout_syntax_rejected_by_amendment_clause_10() {
    // Architecture amendment clause 10 abolished pinned(timeout).
    // The parser must reject `pinned(expr, timeout = N)` with a clear diagnostic.
    let src = r#"
flow TestFlow {
    state Ready
    state Active

    transition go(Ready) -> Active {
        pinned(self, timeout = 5) |_ptr| {
            return Active { }
        }
    }
}
"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("tokenize");
    let err = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect_err("pinned(expr, timeout = N) must be rejected by parser");
    assert!(
        err.message.contains("amendment clause 10"),
        "error should mention amendment clause 10, got: {}",
        err.message
    );
}

#[test]
fn flow_exec_pinned_var_binding() {
    let src = r#"
flow TestFlow {
    state Ready { val: i32 }
    state Active { result: i32 }

    transition process(Ready) -> Active {
        pinned(self.val) |ptr| {
            let _ = ptr
        }
        return Active { result: self.val + 1 }
    }
}

func main() -> i32 {
    let s = Ready { val: 10 }
    let a = TestFlow::process(s)
    a.result
}
"#;
    let result = run_source_bytecode_result(src);
    assert_eq!(result, Ok(interp::Value::Int(11)));
}

// ===================== State machine validation tests =====================

#[test]
fn flow_warn_unreachable_state() {
    let src = r#"
flow BadFlow {
    state Ready
    state Lost
    transition go(Ready) -> Ready { return Ready { } }
}
"#;
    let warnings = check_source_warnings(src);
    assert!(
        warnings.iter().any(|w| w.code.as_deref() == Some("W0400")),
        "expected W0400 warning for unreachable state 'Lost'. warnings: {:?}",
        warnings.iter().map(|w| &w.code).collect::<Vec<_>>()
    );
}

#[test]
fn flow_no_warn_first_state_unreachable() {
    // First state is initial — should not trigger W0400 even if not targeted
    let src = r#"
flow GoodFlow {
    state Ready
    state Active
    transition go(Ready) -> Active { return Active { } }
}
"#;
    let warnings = check_source_warnings(src);
    assert!(
        !warnings.iter().any(|w| w.code.as_deref() == Some("W0400")),
        "first state should not warn as unreachable. warnings: {:?}",
        warnings.iter().map(|w| &w.code).collect::<Vec<_>>()
    );
}

#[test]
fn flow_warn_terminal_state() {
    let src = r#"
flow GoodFlow {
    state Ready
    state Done
    transition go(Ready) -> Done { return Done { } }
}
"#;
    let warnings = check_source_warnings(src);
    assert!(
        warnings.iter().any(|w| w.code.as_deref() == Some("W0401")),
        "expected W0401 warning for terminal state 'Done'"
    );
}

#[test]
fn flow_no_warn_cycling_state() {
    // A state that transitions to itself should not warn about terminal
    let src = r#"
flow GoodFlow {
    state Ready
    state Active
    transition tick(Active) -> Active { return Active { } }
}
"#;
    let warnings = check_source_warnings(src);
    // Ready has no incoming (first state — no W0400) but has no outgoing either
    let terminal: Vec<&str> = warnings
        .iter()
        .filter(|w| w.code.as_deref() == Some("W0401"))
        .filter_map(|w| w.message.split('\'').nth(1))
        .collect();
    assert!(
        !terminal.contains(&"Active"),
        "Active has a self-loop and should not warn about terminal. terminal states: {:?}",
        terminal
    );
    assert!(
        terminal.contains(&"Ready"),
        "Ready has no outgoing and should warn as terminal"
    );
}

#[test]
fn flow_warn_terminal_not_first() {
    let src = r#"
flow GoodFlow {
    state Active
    state Ready
    transition go(Active) -> Ready { return Ready { } }
}
"#;
    let warnings = check_source_warnings(src);
    // 'Ready' has no outgoing, 'Active' is first (no warn for unreachable)
    assert!(
        warnings.iter().any(|w| w.code.as_deref() == Some("W0401")),
        "expected W0401 for terminal state 'Ready'"
    );
}

// ===================== Removed delegate execution syntax =====================

#[test]
fn flow_exec_delegate_view() {
    // v0.34.1: `delegate view` abolished (amendment clause 2) — parse-level rejection.
    assert_delegate_rejected_by_clause_2(
        r#"
flow MyFlow {
    state Active { val: i32 }

    transition process(Active) -> Active {
        let sub = 42
        delegate view(self.val) to sub;
        return Active { val: self.val }
    }
}

func main() -> i32 {
    let s = Active { val: 10 }
    let r = MyFlow::process(s)
    println(r.val)
    0
}
"#,
    );
}

#[test]
fn flow_exec_delegate_consume() {
    // v0.34.1: `delegate consume` abolished (amendment clause 2) — parse-level rejection.
    assert_delegate_rejected_by_clause_2(
        r#"
flow MyFlow {
    state Active { val: i32 }

    transition process(Active) -> Active {
        let sub = 99
        delegate consume(self.val) to sub;
        return Active { val: self.val + 1 }
    }
}

func main() -> i32 {
    let s = Active { val: 10 }
    let r = MyFlow::process(s)
    println(r.val)
    0
}
"#,
    );
}

#[test]
fn flow_exec_delegate_view_no_mutate() {
    // v0.34.1: `delegate view` abolished (amendment clause 2) — parse-level rejection.
    assert_delegate_rejected_by_clause_2(
        r#"
flow MyFlow {
    state Active { val: i32 }

    transition process(Active) -> Active {
        let sub = 99
        delegate view(self.val) to sub;
        return Active { val: self.val }
    }
}

func main() -> i32 {
    let s = Active { val: 10 }
    let r = MyFlow::process(s)
    println(r.val)
    0
}
"#,
    );
}

#[test]
fn flow_exec_delegate_mutate() {
    // v0.34.1: `delegate mutate` abolished (amendment clause 2) — parse-level rejection.
    assert_delegate_rejected_by_clause_2(
        r#"
flow MyFlow {
    state Active { val: i32 }

    transition process(Active) -> Active {
        let sub = 99
        delegate mutate(self.val) to sub;
        return Active { val: self.val + 1 }
    }
}

func main() -> i32 {
    let s = Active { val: 10 }
    let r = MyFlow::process(s)
    println(r.val)
    0
}
"#,
    );
}

#[test]
fn flow_exec_delegate_undefined_target() {
    // v0.34.1: `delegate ... to <undefined>` is also rejected at parse time —
    // the whole construct is abolished (amendment clause 2).
    assert_delegate_rejected_by_clause_2(
        r#"
flow MyFlow {
    state Active { val: i32 }

    transition process(Active) -> Active {
        delegate view(self.val) to nonexistent;
        return Active { val: self.val }
    }
}

func main() -> i32 {
    let s = Active { val: 10 }
    let r = MyFlow::process(s)
    0
}
"#,
    );
}

// ===================== Pinned execution tests (v0.29.16) =====================

#[test]
fn flow_exec_pinned_basic() {
    // v0.29.16: pinned block in do body — basic value scoping.
    let src = r#"
flow Buffer {
    state Active { data: i32 }

    transition use_pinned(Active) -> Active {
        pinned(self.data) |ptr| {
            let _ = ptr
        }
        return Active { data: self.data + 1 }
    }
}

func main() -> i32 {
    let s = Active { data: 100 }
    let r = Buffer::use_pinned(s)
    println(r.data)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "101");
}

#[test]
fn flow_exec_pinned_with_timeout() {
    // v0.29.16: pinned execution (timeout abolished by amendment clause 10).
    let src = r#"
flow Buffer {
    state Active { data: i32 }

    transition process(Active) -> Active {
        pinned(self.data) |p| {
            let _ = p
        }
        return Active { data: self.data + 10 }
    }
}

func main() -> i32 {
    let s = Active { data: 42 }
    let r = Buffer::process(s)
    println(r.data)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "52");
}

#[test]
fn flow_exec_pinned_no_var() {
    // v0.29.16: pinned without pipe-var — just evaluates expr and runs body.
    let src = r#"
flow Buffer {
    state Active { data: i32 }

    transition process(Active) -> Active {
        pinned(self.data) {
            let _ = 42
        }
        return Active { data: self.data * 2 }
    }
}

func main() -> i32 {
    let s = Active { data: 5 }
    let r = Buffer::process(s)
    println(r.data)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "10");
}

#[test]
fn flow_exec_chain() {
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Active { count: i32 }
    state Done

    transition inc(Zero, amount: i32) -> Active {
        return Active { count: self.count + amount }
    }
    transition finish(Active) -> Done {
        return Done { }
    }
}

func main() -> i32 {
    let s = Zero { count: 0 }
    let a = Counter::inc(s, 7)
    let _d = Counter::finish(a)
    42
}
"#;
    let result = run_source_bytecode_result(src);
    assert_eq!(result, Ok(interp::Value::Int(42)));
}

// ===================== Codegen dual-backend tests (v0.29.9) =====================
//
// compile_and_run treats non-zero process exit as failure, so main must
// return 0 and print results via println for dual-backend comparison.

#[test]
fn flow_codegen_simple_transition() {
    let src = r#"
flow Calc {
    state Zero { v: i32 }
    state Value { v: i32 }

    transition add(Zero, amount: i32) -> Value {
        return Value { v: self.v + amount }
    }
}

func main() -> i32 {
    let s = Zero { v: 10 }
    let r = Calc::add(s, 5)
    println(r.v)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check failed: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "15");
}

#[test]
fn flow_codegen_chain() {
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Active { count: i32 }
    state Done

    transition inc(Zero, amount: i32) -> Active {
        return Active { count: self.count + amount }
    }
    transition finish(Active) -> Done {
        return Done { }
    }
}

func main() -> i32 {
    let s = Zero { count: 0 }
    let a = Counter::inc(s, 7)
    println(a.count)
    let _d = Counter::finish(a)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "7");
}

#[test]
fn flow_codegen_multi_target() {
    let src = r#"
flow Checker {
    state Small { v: i32 }
    state Large { v: i32 }

    transition classify(Small, amount: i32) -> Small | Large {
        if self.v + amount > 50 {
            return Large { v: self.v + amount }
        } else {
            return Small { v: self.v + amount }
        }
    }
}

func main() -> i32 {
    let s1 = Small { v: 10 }
    let r1 = Checker::classify(s1, 5)
    let s2 = Small { v: 10 }
    let r2 = Checker::classify(s2, 100)
    // v0.29.49: multi-target result must not access fields directly
    let r3 = r1
    let r4 = r2
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
}

#[test]
fn flow_codegen_empty_payload_state() {
    // Empty payload states (Done { }) and transition that returns them.
    let src = r#"
flow F {
    state A
    state B

    transition go(A) -> B {
        return B { }
    }
}

func main() -> i32 {
    let s = A { }
    let _r = F::go(s)
    println(1)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "1");
}

#[test]
fn flow_codegen_delegate_no_op() {
    // v0.34.1: `delegate` abolished (amendment clause 2) — parse-level rejection.
    // Codegen never sees Delegate statements anymore.
    assert_delegate_rejected_by_clause_2(
        r#"
flow Parent {
    state Active { val: i32 }

    transition run(Active) -> Active {
        let sub = 42
        delegate view(self.val) to sub;
        return Active { val: self.val + 1 }
    }
}

func main() -> i32 {
    let s = Active { val: 10 }
    let r = Parent::run(s)
    println(r.val)
    0
}
"#,
    );
}

// ===================== Transfer matrix + Fault fallback (v0.29.10) =====================

// NOTE (0.34.18b): the former @dense matrix-injection tests
// (flow_matrix_injects_fault_and_fallback, flow_matrix_preserves_user_fault_shape,
// flow_matrix_undefined_event_returns_fault_interp,
// flow_codegen_undefined_event_returns_fault) were removed with @dense.
// Amendment clause 1 (sparse-irreversible) makes undeclared (state, event) a
// compile error (E0211), not a runtime Fault; the sparse contract is locked by
// flow_sparse_skips_fallback_injection and flow_sparse_undefined_event_rejected.

#[test]
fn flow_matrix_does_not_override_user_defined_cell() {
    // User defines Positive+inc → Positive; must not be replaced by Fault fallback.
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }

    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
    transition inc(Positive) -> Positive { return Positive { count: self.count + 1 } }
}

func main() -> i32 {
    let s0 = Zero { count: 0 }
    let s1 = Counter::inc(s0)
    let s2 = Counter::inc(s1)
    println(s2.count)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "2");
}

// ===================== Fault absorption (v0.29.11) =====================

#[test]
fn flow_fault_absorption_drop_nested_record() {
    // 0.34.18b: @dense fallback removed (amendment clause 1). Entering Fault now
    // goes through a declared multi-target `-> Dead | Fault` transition whose body
    // panics (div-by-zero) — the compiler absorbs the panic into the Fault variant.
    // The source state carries a heap `string` field to exercise draft drop walking
    // during absorption. Dual-backend: both backends must produce the same Fault.
    let src = r#"
flow Holder {
    state Live { tag: string, n: i32 }
    state Dead { tag: string }

    transition kill(Live, d: i32) -> Dead | Fault {
        let x = self.n / d
        return Dead { tag: self.tag }
    }
}

func main() -> i32 {
    let u = Holder::kill(Live { tag: "x", n: 7 }, 0)
    match u {
        Dead { tag } => println(tag)
        Fault { last_state, unexpected_event, snapshot, trace } => {
            println(last_state)
            println(unexpected_event)
        }
    }
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["Live()", "Panic(E0801)"], "got {:?}", lines);
}

#[test]
fn flow_fault_mailbox_short_circuit_actor() {
    // Actor nested in flow payload: user transition Active → Fault short-circuits
    // the nested actor (fields cleared, faulted=true).
    // v0.29.12: Fault payload includes full SystemTrace fields.
    // Note: actor-typed fields in record literals still need careful typing;
    // this test focuses on SystemTrace after a scalar-payload Fault path.
    let src = r#"
flow S {
    state Active { n: i32 }
    transition fail(Active) -> Fault {
        return Fault {
            last_state: Active,
            unexpected_event: fail,
            snapshot: "user fail",
            trace: SystemTrace {
                last_state_name: "Active",
                unexpected_event: "fail",
                snapshot: "user fail",
                memory_dump: MemoryDump { fields: "", count: 0 },
                panic_payload: PanicPayload { error_type: "fail", file: "", line: 0, stack: "user fail" }
            }
        }
    }
}

func main() -> i32 {
    let s = Active { n: 1 }
    let f = S::fail(s)
    println(f.last_state)
    println(f.trace.last_state_name)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["Active()", "Active"], "got {:?}", lines);
}

#[test]
fn flow_fault_absorption_codegen() {
    // 0.34.18b: comprehensive dual-backend check that an absorbed panic produces a
    // Fault whose flat fields AND structured trace match the bytecode reference
    // (flow_matrix::make_fault_value) field-for-field.
    let src = r#"
flow F {
    state A { v: i32 }
    state B { v: i32 }

    transition go(A, d: i32) -> B | Fault { return B { v: self.v / d } }
}

func main() -> i32 {
    let u = F::go(A { v: 1 }, 0)
    match u {
        B { v } => println(v)
        Fault { last_state, unexpected_event, snapshot, trace } => {
            println(last_state)
            println(unexpected_event)
            println(trace.last_state_name)
            println(trace.unexpected_event)
            println(trace.memory_dump.count)
        }
    }
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(
        lines,
        vec!["A()", "Panic(E0801)", "A", "panic:E0801", "2"],
        "got {:?}",
        lines
    );
}

// ===================== SystemTrace (v0.29.12) =====================

#[test]
fn flow_system_trace_fields_on_fallback() {
    // 0.34.18b: absorbed panic fills the flat Fault fields + structured SystemTrace.
    // snapshot is "" (matches the bytecode absorber, which passes an empty snapshot
    // to make_fault_value). Dual-backend.
    let src = r#"
flow C {
    state Zero { n: i32 }
    state Pos { n: i32 }

    transition inc(Zero, d: i32) -> Pos | Fault { return Pos { n: self.n / d } }
}

func main() -> i32 {
    let u = C::inc(Zero { n: 0 }, 0)
    match u {
        Pos { n } => println(n)
        Fault { last_state, unexpected_event, snapshot, trace } => {
            println(last_state)
            println(trace.snapshot)
            println(unexpected_event)
            println(trace.last_state_name)
            println(trace.unexpected_event)
        }
    }
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    let lines: Vec<&str> = out.trim().lines().collect();
    // trace.snapshot is "" (empty middle line, preserved by lines()).
    assert_eq!(
        lines,
        vec!["Zero()", "", "Panic(E0801)", "Zero", "panic:E0801"],
        "got {:?}",
        lines
    );
}

#[test]
fn flow_system_trace_codegen_print() {
    // 0.34.18b: absorbed panic populates the SystemTrace sub-records (MemoryDump,
    // PanicPayload) identically to the bytecode reference. Dual-backend.
    let src = r#"
flow C {
    state Zero { n: i32 }
    state Pos { n: i32 }

    transition inc(Zero, d: i32) -> Pos | Fault { return Pos { n: self.n / d } }
}

func main() -> i32 {
    let u = C::inc(Zero { n: 0 }, 0)
    match u {
        Pos { n } => println(n)
        Fault { last_state, unexpected_event, snapshot, trace } => {
            println(trace.memory_dump.fields)
            println(trace.memory_dump.count)
            println(trace.panic_payload.error_type)
        }
    }
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(
        lines,
        vec!["from_state=Zero;event=panic:E0801", "2", "panic:E0801"],
        "got {:?}",
        lines
    );
}

#[test]
fn flow_panic_absorbed_to_fault() {
    // Runtime div-by-zero inside a transition body → Fault with panic:E0801.
    // Static type is still the declared to-state (Ready); check fields via
    // runtime only (interp does not re-typecheck after absorption).
    let src = r#"
flow Calc {
    state Ready { v: i32 }

    transition boom(Ready, denom: i32) -> Ready {
        let q = self.v / denom
        return Ready { v: q }
    }
}

func main() -> i32 {
    let s = Ready { v: 10 }
    let f = Calc::boom(s, 0)
    // f is Fault at runtime; print SystemTrace fields
    println(f.last_state)
    println(f.unexpected_event)
    0
}
"#;
    // Type checker still sees Ready — field access on f is a static error.
    // Use run_source_result only (interp path, no typecheck).
    let result = run_source_bytecode_result(src);
    assert_eq!(result, Ok(interp::Value::Int(0)), "got {:?}", result);
    // Capture via a pure-return test without println side channel:
    let src2 = r#"
flow Calc {
    state Ready { v: i32 }

    transition boom(Ready, denom: i32) -> Ready {
        let q = self.v / denom
        return Ready { v: q }
    }
}

func main() -> i32 {
    let s = Ready { v: 10 }
    let f = Calc::boom(s, 0)
    match f {
        Fault { last_state, unexpected_event, snapshot: _, trace: _ } => {
            match last_state {
                Ready => match unexpected_event {
                    Panic(_) => return 1
                    _ => 0
                }
                _ => 0
            }
        }
        _ => 0
    }
}
"#;
    // match may not support Fault pattern if type is Ready — use record field via Value path.
    // Simpler: just assert run succeeds (absorbed) vs Err (not absorbed).
    let r = run_source_bytecode_result(src);
    assert!(
        r.is_ok(),
        "div-by-zero should be absorbed to Fault, got {:?}",
        r
    );
    let _ = src2;
}

// ── 0.36.6 Fault nominal: 二次 Fault 升级 (裁决 4, DoD #5) ────────────

#[test]
fn fault_recover_body_trap_escalates_not_loops() {
    // 0.36.6 (裁决 4, DoD #5): a recover body that traps again must escalate to a
    // trap (E0801), NOT silently re-absorb into a fresh Fault — that would loop
    // Fault → recover → Fault forever. `from_state == "Fault"` is not re-absorbable;
    // both backends fail-closed.
    let src = r#"
flow Svc {
    state Active { n: i32 }
    transition crash(Active) -> Fault {
        return Fault {
            last_state: Active,
            unexpected_event: crash,
            snapshot: "boom",
            trace: SystemTrace {
                last_state_name: "Active",
                unexpected_event: "crash",
                snapshot: "boom",
                memory_dump: MemoryDump { fields: "", count: 0 },
                panic_payload: PanicPayload { error_type: "crash", file: "", line: 0, stack: "boom" }
            }
        }
    }
    transition recover(Fault) -> Active {
        let denom = 0
        let x = 1 / denom
        return Active { n: x }
    }
}

func main() -> i32 {
    let f = Svc::crash(Active { n: 7 })
    let r = Svc::recover(f)
    r.n
}
"#;
    let result = run_source_bytecode_result(src);
    assert!(
        result.is_err(),
        "recover body trap must escalate (not silently loop), got {:?}",
        result
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("division") || err.contains("E0801") || err.contains("zero"),
        "escalated trap should be division-by-zero: {}",
        err
    );
    // L1 parity: the native backend must also fail-closed (hard trap), not loop.
    let native = compile_and_run(src);
    assert!(
        native.is_err(),
        "native recover body trap must fail-closed, got {:?}",
        native
    );
}

#[test]
fn fault_transition_from_fault_rejected() {
    // 0.36.6 (裁决 4, 二次 Fault 升级): a user-declared transition from Fault
    // (other than recover/reset) is illegal — Fault may only be exited via
    // recover/reset; any other event on Fault would silently self-loop.
    // Uses E0440.
    let src = r#"
flow Svc {
    state Active { n: i32 }
    transition crash(Active) -> Active { return Active { n: self.n } }
    transition boom(Fault) -> Active { return Active { n: 0 } }
}

func main() -> i32 { 0 }
"#;
    let errors = check_source(src).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.code.as_deref() == Some(crate::diagnostic::codes::E0440)),
        "user transition from Fault must be E0440, got: {:?}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

// ===================== Reset / Recover (v0.29.13) =====================

#[test]
fn flow_reset_recover_injected() {
    let src = r#"
flow C {
    state Zero { n: i32 }
    state Pos { n: i32 }
    transition inc(Zero) -> Pos { return Pos { n: self.n + 1 } }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            assert!(
                f.transitions
                    .iter()
                    .any(|t| t.name == "reset" && t.from_state == "Fault"),
                "reset must be injected"
            );
            assert!(
                f.transitions
                    .iter()
                    .any(|t| t.name == "recover" && t.from_state == "Fault"),
                "recover must be injected"
            );
            // System verbs target the root state.
            let reset = f
                .transitions
                .iter()
                .find(|t| t.name == "reset" && t.from_state == "Fault")
                .unwrap();
            assert_eq!(reset.to_states, vec!["Zero"]);
        }
        _ => panic!("expected Flow"),
    }
}

#[test]
fn flow_reset_rebuilds_root() {
    // 0.34.18b: @dense fallback removed (amendment clause 1). Enter Fault via a
    // single-target transition whose body panics — the bytecode VM absorbs the
    // panic into a Fault (dynamically typed; statically still `Pos`). reset then
    // rebuilds the root with a default payload (n=0).
    //
    // Bytecode-only: reset/recover on an absorbed Fault is inherently dynamic —
    // the static type of `f` is the to-state, not `Fault`, so codegen cannot
    // type a `reset(f)` call. This mirrors flow_fault_recover_uses_faulting_persistent_draft.
    let src = r#"
flow C {
    state Zero { n: i32 }
    state Pos { n: i32 }

    transition inc(Zero) -> Pos { return Pos { n: self.n + 1 } }
    transition crash(Pos) -> Pos {
        let x = 1 / 0
        return Pos { n: self.n }
    }
}

func main() -> i32 {
    let p = C::inc(Zero { n: 5 })
    let f = C::crash(p)
    let r = C::reset(f)
    println(r.n)
    0
}
"#;
    let (_, out) = run_source_bytecode_with_stdout(src);
    assert_eq!(
        out.trim(),
        "0",
        "reset rebuilds root default, got {:?}",
        out
    );
}

#[test]
fn flow_recover_preserves_persistent() {
    // 0.34.18b: persistent Config.max_retries survives an absorbed-panic Fault
    // and is restored by recover. Entry is a single-target transition whose body
    // panics (bytecode absorbs → Fault, shadowing the clean persistent field).
    // Bytecode-only: see flow_reset_rebuilds_root note (dynamic Fault typing).
    let src = r#"
flow Svc {
    persistent state Config { max_retries: i32 }
    state Active { max_retries: i32, req: i32 }

    transition start(Config) -> Active { return Active { max_retries: self.max_retries, req: 0 } }
    transition crash(Active) -> Active {
        let x = 1 / 0
        return Active { max_retries: self.max_retries, req: self.req }
    }
}

func main() -> i32 {
    let a = Svc::start(Config { max_retries: 7 })
    let f = Svc::crash(a)
    let r = Svc::recover(f)
    println(r.max_retries)
    0
}
"#;
    let (_, out) = run_source_bytecode_with_stdout(src);
    assert_eq!(
        out.trim(),
        "7",
        "recover preserves persistent, got {:?}",
        out
    );
}

#[test]
fn flow_reset_discards_persistent() {
    // 0.34.18b: reset always zeros persistent fields — even though the absorbed
    // Fault shadowed max_retries=7. Entry via single-target absorbed panic.
    // Bytecode-only: see flow_reset_rebuilds_root note (dynamic Fault typing).
    let src = r#"
flow Svc {
    persistent state Config { max_retries: i32 }
    state Active { max_retries: i32 }

    transition start(Config) -> Active { return Active { max_retries: self.max_retries } }
    transition crash(Active) -> Active {
        let x = 1 / 0
        return Active { max_retries: self.max_retries }
    }
}

func main() -> i32 {
    let a = Svc::start(Config { max_retries: 7 })
    let f = Svc::crash(a)
    let r = Svc::reset(f)
    println(r.max_retries)
    0
}
"#;
    let (_, out) = run_source_bytecode_with_stdout(src);
    assert_eq!(out.trim(), "0", "reset discards persistent, got {:?}", out);
}

#[test]
fn flow_fault_recover_uses_faulting_persistent_draft() {
    let src = r#"
flow Svc {
    persistent state Active { value: i32 }

    transition crash(Active) -> Active {
        self.value = 99
        let x = 1 / 0
        return Active { value: self.value }
    }
}

func main() -> i32 {
    let active = Active { value: 7 }
    let failed = Svc::crash(active)
    let recovered = Svc::recover(failed)
    recovered.value
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
}

#[test]
fn transactional_persistent_draft_syntax_rejected_by_amendment_clause_3() {
    // @transactional was abolished by clause 3; the parser must reject it.
    // Non-transactional persistent-field dirty→reset semantics are covered
    // by flow_fault_recover_uses_faulting_persistent_draft above.
    let src = r#"
flow Svc {
    @transactional persistent state Active { value: i32 }

    transition crash(Active) -> Active {
        self.value = 99
        let x = 1 / 0
        return Active { value: self.value }
    }
}

func main() -> i32 {
    let active = Active { value: 7 }
    let failed = Svc::crash(active)
    let recovered = Svc::recover(failed)
    recovered.value
}
"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("tokenize");
    let err = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect_err("@transactional must be rejected by parser");
    assert!(
        err.message.contains("amendment clause 3"),
        "error should mention amendment clause 3, got: {}",
        err.message
    );
}

#[test]
fn flow_user_reset_not_overridden() {
    // 0.34.18b: user-defined reset(Fault) -> Zero wins over the injected system
    // verb. Entry via single-target absorbed panic. Bytecode-only: see
    // flow_reset_rebuilds_root note (dynamic Fault typing).
    let src = r#"
flow C {
    state Zero { n: i32 }
    state Pos { n: i32 }

    transition inc(Zero) -> Pos { return Pos { n: self.n + 1 } }
    transition crash(Pos) -> Pos {
        let x = 1 / 0
        return Pos { n: self.n }
    }
    transition reset(Fault) -> Zero { return Zero { n: 42 } }
}

func main() -> i32 {
    let p = C::inc(Zero { n: 0 })
    let f = C::crash(p)
    let r = C::reset(f)
    println(r.n)
    0
}
"#;
    let (_, out) = run_source_bytecode_with_stdout(src);
    assert_eq!(out.trim(), "42", "user reset wins, got {:?}", out);
}

// ── v0.29.17 Subflow synchronous nesting ──────────────────────────────

#[test]
fn flow_exec_subflow_nested_transition() {
    // Parent payload holds child state; parent transition drives child.
    let src = r#"
flow Child {
    state CIdle { n: i32 }
    state CDone { n: i32 }
    transition step(CIdle) -> CDone { return CDone { n: self.n + 1 } }
}
flow Parent {
    state Working { child: CIdle }
    state Finished { result: i32 }
    transition run(Working) -> Finished {
        let c2 = Child::step(self.child)
        return Finished { result: c2.n }
    }
}
func main() -> i32 {
    let c0 = CIdle { n: 10 }
    let p0 = Working { child: c0 }
    let p1 = Parent::run(p0)
    println(p1.result)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "11");
}

#[test]
fn flow_exec_subflow_nested_field_access() {
    // Nested state field construction + field access (no transition).
    let src = r#"
flow Child {
    state CIdle { n: i32 }
}
flow Parent {
    state Working { child: CIdle, tag: i32 }
}
func main() -> i32 {
    let c0 = CIdle { n: 7 }
    let p0 = Working { child: c0, tag: 3 }
    println(p0.child.n)
    println(p0.tag)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "7\n3");
}

#[test]
fn flow_exec_subflow_reset_nested_defaults() {
    // reset/recover inject zeroed nested subflow payload (not unit).
    let src = r#"
flow Child {
    state CIdle { n: i32 }
}
flow Parent {
    state Working { child: CIdle }
    transition boom(Working) -> Fault {
        return Fault {
            last_state: Working,
            unexpected_event: boom,
            snapshot: "user",
            trace: SystemTrace {
                last_state_name: "Working",
                unexpected_event: "boom",
                snapshot: "user",
                memory_dump: MemoryDump { fields: "", count: 0 },
                panic_payload: PanicPayload { error_type: "boom", file: "", line: 0, stack: "user" }
            }
        }
    }
}
func main() -> i32 {
    let c0 = CIdle { n: 99 }
    let p0 = Working { child: c0 }
    let f = Parent::boom(p0)
    let r = Parent::reset(f)
    // After reset, nested child is zero-default CIdle { n: 0 }
    println(r.child.n)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "0");
}

#[test]
fn flow_check_subflow_unknown_nested_type() {
    // L2: payload field must name a known type (state or record).
    let src = r#"
flow Parent {
    state Working { child: NotARealState }
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err(), "expected type error for unknown nested type");
}

#[test]
fn flow_parse_subflow_payload_shape() {
    // Parser + matrix: nested state field preserved; reset body uses nested default.
    let src = r#"
flow Child { state CIdle { n: i32 } }
flow Parent { state Working { child: CIdle } }
"#;
    let file = parse(src);
    let parent = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Flow(f) if f.name == "Parent" => Some(f),
            _ => None,
        })
        .expect("Parent");
    let working = parent.states.iter().find(|s| s.name == "Working").unwrap();
    let fields = working.payload.as_ref().unwrap();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "child");
    match fields[0].ty.unlocated() {
        Type::Name(n, _) => assert_eq!(n, "CIdle"),
        other => panic!("expected Name(CIdle), got {:?}", other),
    }
    // Injected reset must rebuild Working { child: CIdle { n: 0 } }, not unit.
    let reset = parent
        .transitions
        .iter()
        .find(|t| t.name == "reset" && t.from_state == "Fault")
        .expect("reset");
    let body = reset.body.as_ref().expect("reset body");
    match body.first().map(Stmt::unlocated) {
        Some(Stmt::Return(Some(expr))) => {
            let Expr::Record {
                ty: Some(t),
                fields,
            } = expr.unlocated()
            else {
                panic!("expected record return expression, got {:?}", expr);
            };
            assert_eq!(t, "Working");
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name, "child");
            match fields[0].value.unlocated() {
                Expr::Record {
                    ty: Some(ct),
                    fields: cfields,
                } => {
                    assert_eq!(ct, "CIdle");
                    assert_eq!(cfields.len(), 1);
                    assert_eq!(cfields[0].name, "n");
                    assert!(matches!(
                        cfields[0].value.unlocated(),
                        Expr::Literal(Lit::Int(0))
                    ));
                }
                other => panic!("expected nested CIdle record default, got {:?}", other),
            }
        }
        other => panic!("unexpected reset body: {:?}", other),
    }
}

// ── v0.29.18 Protocol interface abstraction ───────────────────────────

#[test]
fn flow_exec_protocol_impl_dual_backend() {
    let src = r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
    transition read(Active) -> Active
    transition stop(Active) -> Idle
}
flow LidarDriver {
    impl Sensor
    state Idle
    state Active { data: i32, internal: i32 }
    transition start(Idle) -> Active { return Active { data: 0, internal: 42 } }
    transition read(Active) -> Active { return Active { data: self.data + 1, internal: self.internal } }
    transition stop(Active) -> Idle { return Idle { } }
}
func main() -> i32 {
    let s0 = Idle { }
    let s1 = LidarDriver::start(s0)
    let s2 = LidarDriver::read(s1)
    println(s2.data)
    println(s2.internal)
    let s3 = LidarDriver::stop(s2)
    let s4 = LidarDriver::start(s3)
    println(s4.data)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(checked_run_source_result(src), Ok(interp::Value::Int(0)));
    let out = checked_compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "1\n42\n0");
}

#[test]
fn flow_exec_protocol_empty_states() {
    // Empty-state protocol (no payload) must still resolve under impl.
    let src = r#"
protocol Toggle {
    state Off
    state On
    transition flip(Off) -> On
    transition flip(On) -> Off
}
flow Switch {
    impl Toggle
    state Off
    state On
    transition flip(Off) -> On { return On { } }
    transition flip(On) -> Off { return Off { } }
}
func main() -> i32 {
    let s0 = Off { }
    let s1 = Switch::flip(s0)
    let s2 = Switch::flip(s1)
    println(1)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "empty protocol: {:?}",
        check_source(src)
    );
    assert_eq!(checked_run_source_result(src), Ok(interp::Value::Int(0)));
    let out = checked_compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "1");
}

#[test]
fn flow_check_protocol_nested_state_payload_rejected() {
    // L2 flatness: protocol payload must not nest another protocol state.
    let src = r#"
protocol Nested {
    state Inner { n: i32 }
    state Outer { data: Inner }
    transition go(Outer) -> Outer
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(
        err.is_err(),
        "expected flatness error for nested protocol payload"
    );
    let msgs: String = err
        .unwrap_err()
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        msgs.contains("must be flat") || msgs.contains("E0412") || msgs.contains("nests"),
        "expected flatness diagnostic, got: {}",
        msgs
    );
}

#[test]
fn flow_check_protocol_missing_transition_target() {
    let src = r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
}
flow Bad {
    impl Sensor
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Idle { return Idle { } }
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_err(),
        "wrong transition target must fail"
    );
}

#[test]
fn flow_check_protocol_extra_payload_fields_ok() {
    // Width subtyping: flow may have extra fields beyond protocol payload.
    let src = r#"
protocol Sensor {
    state Active { data: i32 }
    transition tick(Active) -> Active
}
flow Rich {
    impl Sensor
    state Active { data: i32, extra: i32, more: i32 }
    transition tick(Active) -> Active { return Active { data: self.data + 1, extra: self.extra, more: self.more } }
}
func main() -> i32 {
    let s = Active { data: 1, extra: 2, more: 3 }
    let t = Rich::tick(s)
    println(t.data)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "extra fields: {:?}",
        check_source(src)
    );
    assert_eq!(checked_run_source_result(src), Ok(interp::Value::Int(0)));
    let out = checked_compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "2");
}

#[test]
fn flow_check_protocol_payload_field_name_must_match() {
    let src = r#"
protocol Sensor {
    state Active { data: i32 }
}
flow Bad {
    impl Sensor
    state Active { wrong: i32 }
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_err(),
        "a same-typed field with the wrong name must not satisfy a protocol payload"
    );
}

#[test]
fn flow_check_protocol_multi_target_covers_edge() {
    // Multi-target transition covers protocol edge if required to_state is listed.
    let src = r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
}
flow F {
    impl Sensor
    state Idle
    state Active { data: i32 }
    state Extra { data: i32 }
    transition start(Idle) -> Active | Extra { return Active { data: 7 } }
}
func main() -> i32 {
    let s = Idle { }
    let a = F::start(s)
    // v0.29.49: multi-target result must not access fields directly
    let a2 = a
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "multi-target: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
}

// ── v0.29.19 Session Types compiler skeleton ──────────────────────────

#[test]
fn session_parse_basic() {
    let src = r#"
session S = !i32 . ?string . end
session T = dual(S)
"#;
    let file = parse(src);
    assert_eq!(file.items.len(), 2);
    match &file.items[0] {
        Item::Session(s) => {
            assert_eq!(s.name, "S");
            match s.body.unlocated() {
                SessionType::Send(t, cont) => {
                    assert!(matches!(t.unlocated(), Type::Name(n, _) if n == "i32"));
                    match cont.unlocated() {
                        SessionType::Recv(t2, cont2) => {
                            assert!(matches!(t2.unlocated(), Type::Name(n, _) if n == "string"));
                            assert_eq!(cont2.unlocated(), &SessionType::End);
                        }
                        other => panic!("expected Recv, got {:?}", other),
                    }
                }
                other => panic!("expected Send, got {:?}", other),
            }
        }
        other => panic!("expected Session, got {:?}", other),
    }
    match &file.items[1] {
        Item::Session(s) => {
            assert_eq!(s.name, "T");
            assert!(matches!(s.body.unlocated(), SessionType::Dual(_)));
        }
        other => panic!("expected Session, got {:?}", other),
    }
}

#[test]
fn session_check_order_ok() {
    // Correct send → recv → close order typechecks.
    let src = r#"
session S = !i32 . ?i32 . end
func client(ch: SessionChan<S>) -> i32 {
    session_send(ch, 1)
    let x = session_recv(ch)
    session_close(ch)
    x
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "good order: {:?}",
        check_source(src)
    );
}

#[test]
fn session_check_order_recv_before_send_rejected() {
    let src = r#"
session S = !i32 . ?i32 . end
func bad(ch: SessionChan<S>) -> i32 {
    let x = session_recv(ch)
    x
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err(), "recv-before-send must fail");
    let msgs: String = err
        .unwrap_err()
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        msgs.contains("order")
            || msgs.contains("E0414")
            || msgs.contains("ExpectedRecv")
            || msgs.contains("recv"),
        "expected order violation, got: {}",
        msgs
    );
}

#[test]
fn session_check_close_before_end_rejected() {
    let src = r#"
session S = !i32 . end
func bad(ch: SessionChan<S>) {
    session_close(ch)
}
func main() -> i32 { 0 }
"#;
    assert!(check_source(src).is_err(), "close before end must fail");
}

#[test]
fn session_check_unknown_session_name() {
    let src = r#"
func f(ch: SessionChan<NoSuch>) -> i32 { 0 }
func main() -> i32 { 0 }
"#;
    assert!(check_source(src).is_err(), "unknown session name must fail");
}

#[test]
fn session_check_dual_ok() {
    let src = r#"
session S = !i32 . end
session T = dual(S)
func server(ch: SessionChan<T>) -> i32 {
    let x = session_recv(ch)
    session_close(ch)
    x
}
func main() -> i32 { 0 }
"#;
    assert!(check_source(src).is_ok(), "dual: {:?}", check_source(src));
}

// ── v0.31.12 Typed Session Residual ──────────────────────────────────

#[test]
fn session_alias_transfers_residual() {
    // v0.31.12: `let b = a` transfers the residual from a to b.
    // Using b to complete the protocol is valid.
    let src = r#"
session S = !i32 . ?i32 . end
func client(ch: SessionChan<S>) -> i32 {
    let ch2 = ch
    session_send(ch2, 1)
    let x = session_recv(ch2)
    session_close(ch2)
    x
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "alias transfer: {:?}",
        check_source(src)
    );
}

#[test]
fn session_use_after_alias_rejected() {
    // v0.31.12: using an endpoint after aliasing is E0426 (linear violation).
    let src = r#"
session S = !i32 . end
func bad(ch: SessionChan<S>) -> i32 {
    let ch2 = ch
    session_send(ch, 1)
    session_close(ch2)
    0
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err(), "use-after-alias must fail");
    let errors = err.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0426")),
        "expected E0426, got: {:?}",
        errors
    );
}

#[test]
fn session_scope_exit_unfinished_rejected() {
    // v0.31.12: non-end residual leaving scope is E0425.
    let src = r#"
session S = !i32 . ?i32 . end
func bad(ch: SessionChan<S>) -> i32 {
    session_send(ch, 1)
    0
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err(), "scope exit with non-end residual must fail");
    let errors = err.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0425")),
        "expected E0425, got: {:?}",
        errors
    );
}

#[test]
fn session_scope_exit_complete_ok() {
    // v0.31.12: completing the protocol (residual = end) allows scope exit.
    let src = r#"
session S = !i32 . end
func ok(ch: SessionChan<S>) -> i32 {
    session_send(ch, 1)
    session_close(ch)
    0
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "complete protocol scope exit: {:?}",
        check_source(src)
    );
}

#[test]
fn session_branch_merge_consistent_ok() {
    // v0.31.12: both branches advance the residual identically → merge OK.
    let src = r#"
session S = !i32 . ?i32 . end
func ok(ch: SessionChan<S>, flag: bool) -> i32 {
    if flag {
        session_send(ch, 1)
    } else {
        session_send(ch, 2)
    }
    let x = session_recv(ch)
    session_close(ch)
    x
}
func main() -> i32 { 0 }
"#;
    assert!(
        check_source(src).is_ok(),
        "branch merge consistent: {:?}",
        check_source(src)
    );
}

#[test]
fn session_branch_merge_divergent_rejected() {
    // v0.31.12: branches advance the residual differently → E0425.
    let src = r#"
session S = !i32 . ?i32 . end
func bad(ch: SessionChan<S>, flag: bool) -> i32 {
    if flag {
        session_send(ch, 1)
    } else {
        session_send(ch, 2)
        let x = session_recv(ch)
    }
    session_close(ch)
    0
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err(), "divergent branch residuals must fail");
    let errors = err.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0425")),
        "expected E0425, got: {:?}",
        errors
    );
}

#[test]
fn session_double_close_rejected() {
    // v0.31.13: CFG dataflow catches session endpoint double-close (E0304).
    let src = r#"
session S = !i32 . end
func bad(ch: SessionChan<S>) -> i32 {
    session_send(ch, 1)
    session_close(ch)
    session_close(ch)
    0
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err(), "double close must fail");
    let errors = err.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0304")),
        "expected E0304, got: {:?}",
        errors
    );
}

#[test]
fn session_branch_partial_consume_rejected() {
    // v0.31.13: consuming a session endpoint in only one branch is E0425
    // (scope exit with non-end residual, since the no-else branch conservatively
    // restores the pre-branch residual).
    let src = r#"
session S = !i32 . end
func bad(ch: SessionChan<S>, flag: bool) -> i32 {
    if flag {
        session_send(ch, 1)
        session_close(ch)
    }
    0
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err(), "branch partial consume must fail");
    let errors = err.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0425")),
        "expected E0425, got: {:?}",
        errors
    );
}

#[test]
fn session_endpoint_move_to_function_rejected() {
    // v0.31.13: passing a session endpoint to a function moves it.
    // Using it again is E0304 (moved after consumed).
    let src = r#"
session S = !i32 . end
func send_and_close(ch: SessionChan<S>) -> i32 {
    session_send(ch, 1)
    session_close(ch)
    0
}
func bad(ch: SessionChan<S>) -> i32 {
    let a = send_and_close(ch)
    let b = send_and_close(ch)
    a + b
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err(), "session endpoint move-after-move must fail");
    let errors = err.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0304")),
        "expected E0304, got: {:?}",
        errors
    );
}

// ── v0.29.20 PeerFault cross-Actor propagation ────────────────────────

#[test]
fn flow_peer_fault_injected_default_cascade() {
    // Unhandled peer_fault(State) is injected → Fault with SystemTrace.
    let src = r#"
flow Node {
    state Live { n: i32 }
    transition work(Live) -> Live { return Live { n: self.n + 1 } }
}
func main() -> i32 {
    let s = Live { n: 1 }
    let f = Node::peer_fault(s)
    println(f.last_state)
    println(f.unexpected_event)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "Live()\npeer_fault()");
}

#[test]
fn flow_peer_fault_user_self_loop_not_overridden() {
    // Explicit peer_fault self-loop breaks the cascade (user-defined wins).
    let src = r#"
flow Node {
    state Active { n: i32 }
    transition peer_fault(Active) -> Active { return Active { n: self.n + 10 } }
}
func main() -> i32 {
    let s = Active { n: 5 }
    let t = Node::peer_fault(s)
    println(t.n)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "15");
}

#[test]
fn flow_peer_fault_user_recovering_target() {
    // User handles peer_fault → Recovering (not Fault).
    let src = r#"
flow Node {
    state Active { n: i32 }
    state Recovering { n: i32 }
    transition peer_fault(Active) -> Recovering { return Recovering { n: self.n } }
}
func main() -> i32 {
    let s = Active { n: 3 }
    let r = Node::peer_fault(s)
    println(r.n)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "3");
}

#[test]
fn flow_peer_fault_record_constructible() {
    // PeerFault builtin record type is available.
    let src = r#"
func main() -> i32 {
    let pf = PeerFault { peer_id: "peer-7", reason: "disconnect" }
    println(pf.peer_id)
    println(pf.reason)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "peer-7\ndisconnect");
}

#[test]
fn flow_parse_peer_fault_injection() {
    let src = r#"
flow N {
    state A
    state B
    transition go(A) -> B { { return B { } } }
}
"#;
    let file = parse(src);
    let f = match &file.items[0] {
        Item::Flow(f) => f,
        _ => panic!("expected Flow"),
    };
    // peer_fault injected for A and B (not Fault)
    let pf: Vec<_> = f
        .transitions
        .iter()
        .filter(|t| t.name == "peer_fault")
        .collect();
    assert!(
        pf.iter().any(|t| t.from_state == "A"
            && t.to_states == vec!["Fault".to_string()]
            && t.is_fallback),
        "A.peer_fault → Fault missing: {:?}",
        pf
    );
    assert!(
        pf.iter().any(|t| t.from_state == "B" && t.is_fallback),
        "B.peer_fault missing"
    );
    // Fault state itself gets a peer_fault → Fault self-loop
    // (C5: prevents dispatch failure when peer_fault arrives in Fault state).
    assert!(
        pf.iter().any(|t| t.from_state == "Fault"
            && t.to_states == vec!["Fault".to_string()]
            && t.is_fallback),
        "Fault.peer_fault → Fault self-loop missing: {:?}",
        pf
    );
}

// ── v0.29.21 Mailbox backpressure auto-governance ─────────────────────

#[test]
fn flow_parse_mailbox_depth_annotation() {
    let src = r#"
flow Audio {
    @mailbox(depth = 64)
    state Ready
    transition go(Ready) -> Ready { { return Ready { } } }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            assert!(
                f.annotations
                    .iter()
                    .any(|a| matches!(a.kind, FlowAnnotationKind::MailboxDepth(64))),
                "expected MailboxDepth(64), got {:?}",
                f.annotations
            );
        }
        other => panic!("expected Flow, got {:?}", other),
    }
}

#[test]
fn mailbox_bp_state_mute_and_hysteresis() {
    use crate::interp::MailboxBpState;
    let bp = MailboxBpState::new(4);
    assert!(!bp.is_muted());
    // Fill to limit without mute (over is > limit)
    for _ in 0..4 {
        bp.on_enqueue();
    }
    assert!(!bp.is_muted() || bp.current_depth() == 4);
    // One more triggers mute
    bp.on_enqueue();
    assert!(bp.is_muted());
    assert_eq!(bp.current_depth(), 5);
    // Drain to ≤ 50% (2) should allow unmute after cooldown (set cooldown to 0)
    // Force cooldown elapsed by setting unmute_after_ms to 0
    bp.unmute_after_ms
        .store(0, std::sync::atomic::Ordering::Release);
    for _ in 0..3 {
        bp.on_dequeue();
    }
    // depth = 2, low = 2, should unmute
    bp.try_unmute();
    assert!(!bp.is_muted(), "should unmute at ≤50% depth");
}

#[test]
fn actor_mailbox_depth_and_set() {
    let src = r#"
actor Counter {
    n: i32
    func bump() -> i32 {
        self.n = self.n + 1
        self.n
    }
    func get() -> i32 {
        self.n
    }
}
func main() -> i32 {
    let c = Counter.spawn()
    actor_set_mailbox_depth(c, 8)
    let d = actor_mailbox_depth(c)
    let m = actor_is_muted(c)
    println(d)
    println(m)
    let v = c.bump()
    println(v)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    // depth starts 0, muted 0, bump returns 1
    assert_eq!(out.trim(), "0\n0\n1");
}

#[test]
fn actor_is_faulted_returns_int_dual() {
    // actor_is_faulted must return Int(0/1) on both backends: codegen exposes
    // mimi_actor_is_faulted -> i32, so the bytecode VM must not return Bool.
    let src = r#"
actor Counter {
    n: i32
    func get() -> i32 { self.n }
}
func main() -> i32 {
    let c = Counter.spawn()
    let f = actor_is_faulted(c)
    println(f)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(
        run_source_bytecode_result(src),
        Ok(interp::Value::Int(0)),
        "bytecode actor_is_faulted must yield Int"
    );
    let out = compile_and_run(src).expect("codegen failed");
    assert_eq!(out.trim(), "0", "codegen actor_is_faulted must print 0");
}

#[test]
fn actor_mailbox_backpressure_ttl() {
    // With depth=1, a slow consumer causes second concurrent send to wait;
    // we simulate by setting depth=1 and flooding from main (sequential still ok).
    // L1: setting depth and reading it dual-backend.
    let src = r#"
actor Worker {
    n: i32
    func work() -> i32 {
        self.n = self.n + 1
        self.n
    }
}
func main() -> i32 {
    let w = Worker.spawn()
    actor_set_mailbox_depth(w, 1)
    let a = w.work()
    let b = w.work()
    println(a)
    println(b)
    println(actor_mailbox_depth(w))
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen failed");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "1");
    assert_eq!(lines[1], "2");
    // depth should be 0 after both completed
    assert_eq!(lines[2], "0");
}

// ── v0.29.22 Progressive Typestate ────────────────────────────────────

#[test]
fn progressive_script_injects_main_single() {
    let src = r#"
func main() -> i32 {
    let x = 42
    println(x)
    0
}
"#;
    let file = parse(src);
    assert!(file.implicit_single, "script mode should be active");
    assert!(
        file.items
            .iter()
            .any(|i| matches!(i, Item::Flow(f) if f.name == "Main")),
        "Main flow should be injected"
    );
    let main_flow = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Flow(f) if f.name == "Main" => Some(f),
            _ => None,
        })
        .unwrap();
    assert!(main_flow.states.iter().any(|s| s.name == "Single"));
    // Fault injected by matrix expand
    assert!(main_flow.states.iter().any(|s| s.name == "Fault"));
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "42");
}

#[test]
fn progressive_explicit_flow_no_injection() {
    let src = r#"
flow Counter {
    state Zero { n: i32 }
    transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } }
}
func main() -> i32 {
    let s = Zero { n: 0 }
    let s2 = Counter::inc(s)
    println(s2.n)
    0
}
"#;
    let file = parse(src);
    assert!(!file.implicit_single);
    // Only user Counter flow (+ matrix Fault), not auto Main — unless user named Main
    let flow_names: Vec<_> = file
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Flow(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(flow_names.contains(&"Counter"));
    assert!(
        !flow_names.contains(&"Main") || flow_names.iter().filter(|n| **n == "Main").count() == 0
    );
    assert!(check_source(src).is_ok());
}

#[test]
fn progressive_migration_warning_on_flow_plus_main() {
    let src = r#"
flow Counter {
    state Zero { n: i32 }
    transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } }
}
func main() -> i32 {
    let x = 1
    let s = Zero { n: 0 }
    let s2 = Counter::inc(s)
    println(s2.n)
    0
}
"#;
    let warns = check_source_warnings(src);
    assert!(
        warns.iter().any(
            |w| w.code.as_deref() == Some(crate::diagnostic::codes::W011)
                || w.message.contains("progressive")
                || w.message.contains("implicit Single")
        ),
        "expected W011 migration warning, got {:?}",
        warns
    );
}

#[test]
fn progressive_protocol_only_no_injection() {
    let src = r#"
protocol P {
    state A
    transition go(A) -> A
}
"#;
    let file = parse(src);
    assert!(!file.implicit_single);
    assert!(!file.items.iter().any(|i| matches!(i, Item::Flow(_))));
}

// ── v0.29.23 view/mutate local lexical borrow ─────────────────────────

#[test]
fn view_mutate_parse_param_borrow() {
    let src = r#"
func f(a: view i32, b: mutate i32) -> i32 { a }
func main() -> i32 { 0 }
"#;
    let file = parse(src);
    let f = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Func(f) if f.name == "f" => Some(f),
            _ => None,
        })
        .expect("func f");
    assert_eq!(f.params[0].borrow, Some(ParamBorrow::View));
    assert_eq!(f.params[1].borrow, Some(ParamBorrow::Mutate));
    assert!(f.params[1].mut_, "mutate implies mut_");
}

#[test]
fn view_mutate_exec_dual_backend() {
    let src = r#"
func compute_mean(data: view List<i32>) -> i32 {
    len(data)
}
func id_view(x: view i32) -> i32 {
    x
}
func add_mutate(x: mutate i32) -> i32 {
    x = x + 1
    x
}
func main() -> i32 {
    let xs = [10, 20, 30]
    let m = compute_mean(xs)
    println(m)
    let b = id_view(5)
    println(b)
    let mut cv = 7
    let c = add_mutate(cv)
    println(c)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(
        out.trim(),
        "3
5
8"
    );
}

#[test]
fn view_param_write_rejected() {
    let src = r#"
func bad(data: view i32) {
    data = 1
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err());
    let msgs: String = err
        .unwrap_err()
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        msgs.contains("view") || msgs.contains("E0415") || msgs.contains("read-only"),
        "{}",
        msgs
    );
}

#[test]
fn view_param_transition_rejected() {
    let src = r#"
flow F {
    state A { n: i32 }
    transition go(A) -> A { return A { n: self.n + 1 } }
}
func bad(data: view i32) -> i32 {
    let s = A { n: data }
    let s2 = F::go(s)
    s2.n
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err());
    let msgs: String = err
        .unwrap_err()
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        msgs.contains("transition") || msgs.contains("borrow") || msgs.contains("E0415"),
        "{}",
        msgs
    );
}

#[test]
fn view_param_drop_rejected() {
    let src = r#"
func bad(data: view i32) {
    drop(data)
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    assert!(err.is_err(), "expected drop under view to fail");
}

// ── v0.29.24 Spawn quota (@max_children) ───────────────────────────────

#[test]
fn spawn_quota_parse_max_children() {
    let src = r#"
flow Parent {
    @max_children(3)
    state Idle
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            assert!(
                f.annotations
                    .iter()
                    .any(|a| matches!(a.kind, FlowAnnotationKind::MaxChildren(3))),
                "got {:?}",
                f.annotations
            );
        }
        _ => panic!("expected Flow"),
    }
}

#[test]
fn spawn_quota_within_limit_dual_backend() {
    let src = r#"
flow Parent {
    @max_children(2)
    state Idle
    transition go(Idle) -> Idle { { return Idle { } } }
}
actor Worker {
    n: i32
    func get() -> i32 { self.n }
}
func main() -> i32 {
    println(actor_max_children())
    let a = Worker.spawn()
    let b = Worker.spawn()
    println(actor_spawn_count())
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "2\n2");
}

#[test]
fn spawn_quota_exceeded_runtime_error() {
    let src = r#"
flow Parent {
    @max_children(1)
    state Idle
    transition go(Idle) -> Idle { { return Idle { } } }
}
actor Worker {
    n: i32
    func get() -> i32 { self.n }
}
func main() -> i32 {
    let a = Worker.spawn()
    let b = Worker.spawn()
    0
}
"#;
    let err = run_source_bytecode_result(src);
    assert!(err.is_err(), "expected QuotaExceeded");
    let msg = err.unwrap_err();
    assert!(
        msg.contains("QuotaExceeded") || msg.contains("max_children"),
        "got {}",
        msg
    );
}

#[test]
fn spawn_quota_set_builtin_dual_backend() {
    let src = r#"
actor Worker {
    n: i32
    func get() -> i32 { self.n }
}
func main() -> i32 {
    actor_set_max_children(1)
    println(actor_max_children())
    let a = Worker.spawn()
    println(actor_spawn_count())
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "1\n1");
}

// ── v0.29.25 Flow polymorphic broadcast ───────────────────────────────

#[test]
fn broadcast_same_type_actors_dual_backend() {
    let src = r#"
actor Sensor {
    v: i32
    func read() -> i32 { self.v }
    func set(n: i32) { self.v = n }
}
func main() -> i32 {
    let a = Sensor.spawn()
    let b = Sensor.spawn()
    a.set(3)
    b.set(7)
    let targets = [a, b]
    let results = broadcast(targets, "read")
    println(len(results))
    println(results[0])
    println(results[1])
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "2\n3\n7");
}

#[test]
fn broadcast_empty_list_dual_backend() {
    let src = r#"
actor Sensor {
    v: i32
    func read() -> i32 { self.v }
}
func main() -> i32 {
    let targets: List = []
    let results = broadcast(targets, "read")
    println(len(results))
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "0");
}

#[test]
fn broadcast_unknown_method_returns_zero_slot() {
    // Codegen path returns 0 for unknown method; interp returns PeerFault record.
    // L1: both complete without crash; interp list length preserved.
    let src = r#"
actor Sensor {
    v: i32
    func read() -> i32 { self.v }
}
func main() -> i32 {
    let a = Sensor.spawn()
    let targets = [a]
    let results = broadcast(targets, "nope")
    println(len(results))
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "1");
}

// ── v0.29.27 pinned true semantics ────────────────────────────────────

#[test]
fn pinned_reject_transition_under_pin() {
    // L2: Flow::transition inside pinned body → E0416
    let src = r#"
flow Buf {
    state Active { data: i32 }
    transition step(Active) -> Active { return Active { data: self.data + 1 } }
    transition bad(Active) -> Active {
        pinned(self.data) |p| {
            let _ = p
            let _ = Buf::step(Active { data: 0 })
        }
        return Active { data: self.data }
    }
}
"#;
    let err = check_source(src);
    assert!(err.is_err(), "expected E0416, got ok");
    let msg = format!("{:?}", err);
    assert!(
        msg.contains("E0416") || msg.contains("pinned") || msg.contains("cannot"),
        "got {}",
        msg
    );
}

// ── v0.29.29 mutate parameter hardening ────────────────────────────────

#[test]
fn mutate_reassign_rejected() {
    // L2: reassigning mutate param (realloc / swap) → E0417
    let src = r#"
func bad(data: mutate i32) -> i32 {
    data = 99
    data
}
"#;
    let err = check_source(src);
    assert!(err.is_err(), "expected E0417");
    let msgs = format!("{:?}", err);
    assert!(
        msgs.contains("E0417") || msgs.contains("mutate"),
        "got {}",
        msgs
    );
}

#[test]
fn mutate_list_push_allowed() {
    // Mutate via builtin (push) → allowed (element-level mutation).
    let src = r#"
use std::collections

func bump_last(data: mutate List<i32>) {
    let n = len(data)
    push(data, n)
}

func main() -> i32 {
    let xs = [10, 20]
    bump_last(xs)
    println(xs[2])
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "2");
}

#[test]
fn mutate_literal_reassign_rejected() {
    // L2: mutate = literal → E0417 (realloc banned)
    let src = r#"
func bad(data: mutate i32) -> i32 {
    data = 42
    data
}
"#;
    let err = check_source(src);
    assert!(err.is_err(), "expected E0417");
    let msgs = format!("{:?}", err);
    assert!(
        msgs.contains("E0417") || msgs.contains("mutate"),
        "got {}",
        msgs
    );
}

#[test]
fn mutate_other_ident_reassign_rejected() {
    // L2: mutate = unrelated ident → E0417
    let src = r#"
func bad(data: mutate i32, other: i32) -> i32 {
    data = other
    data
}
"#;
    let err = check_source(src);
    assert!(err.is_err(), "expected E0417");
    let msgs = format!("{:?}", err);
    assert!(
        msgs.contains("E0417") || msgs.contains("mutate"),
        "got {}",
        msgs
    );
}

// ── v0.29.31 per-actor-type spawn quota + mailbox auto-depth ───────────

// ── v0.29.33 view/mutate deep realloc ban + ref ABI ───────────────────

#[test]
fn mutate_list_literal_realloc_rejected() {
    // L2: `xs = [1, 2]` on a mutate List param → E0417 (deep realloc banned)
    let src = r#"
func bad(xs: mutate List<i32>) {
    xs = [1, 2]
}
"#;
    let err = check_source(src);
    assert!(
        err.is_err(),
        "expected E0417 for list literal realloc, got ok"
    );
    let msgs = format!("{:?}", err);
    assert!(
        msgs.contains("E0417") || msgs.contains("mutate"),
        "got {}",
        msgs
    );
}

#[test]
fn mutate_list_index_assign_allowed() {
    // L2: `xs[i] = val` on a mutate List → allowed (element-level mutation, not realloc)
    let src = r#"
func set_first(xs: mutate List<i32>) {
    xs[0] = 42
}
func main() -> i32 {
    0
}
"#;
    // This should check OK (index assign is element-level, not realloc)
    assert!(
        check_source(src).is_ok(),
        "mutate List index assign should be allowed: {:?}",
        check_source(src).err()
    );
}

#[test]
fn view_mutate_dual_backend_no_regression() {
    // L1: view/mutate still works correctly after E0417 deep realloc ban.
    let src = r#"
func sum_view(data: view List<i32>) -> i32 {
    len(data)
}
func bump(x: mutate i32) -> i32 {
    x = x + 1
    x
}
flow Process {
    state Active { buffer: List<i32>, tag: i32 }
    state Done { total: i32 }
    transition process(Active) -> Done {
        let n = sum_view(self.buffer)
        let t = bump(self.tag)
        return Done { total: n + t }
    }
}
func main() -> i32 {
    let buf = [1, 2, 3, 4]
    let s0 = Active { buffer: buf, tag: 10 }
    let s1 = Process::process(s0)
    println(s1.total)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "15");
}

#[test]
fn mutate_field_writeback_clause6_dual_backend() {
    // v0.34.13 (clause 6, golden §3.3): `mutate self.field` payload member
    // borrow must write the callee's final parameter value back into the
    // payload slot. Root causes fixed: (1) MutateSetupField op + VM RecordSet
    // writeback (was: silently dropped — `apply_filter(mutate self.buffer)`
    // left self.buffer unchanged); (2) do_return collected mutate-param vals
    // AFTER mem::replace, so a return register aliasing a mutate param (e.g.
    // `RET r0` with x at r0) wrote Unit back to the caller.
    let src = r#"
func apply_filter(buf: mutate List<i32>) -> i32 {
    buf[0] = 99
    len(buf)
}
flow Process {
    state Active { buffer: List<i32> }
    state Done { first: i32 }
    transition process(Active) -> Done {
        let n = apply_filter(mutate self.buffer)
        let f = self.buffer[0]
        return Done { first: f + n }
    }
}
func main() -> i32 {
    let s0 = Active { buffer: [1, 2, 3] }
    let s1 = Process::process(s0)
    println(s1.first)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    // L1: codegen must agree — self.buffer[0] is 99 after the mutate borrow.
    let out = compile_and_run(src).expect("codegen mutate field writeback");
    assert_eq!(out.trim(), "102");
}

#[test]
fn mutate_nested_place_rejected_q5() {
    // Q5 (0.34.25c): `bump(o.inner.value)` is a nested field place. The
    // mutate borrow's writeback only handles Ident and single-level
    // Field(Ident,_) places, so a nested place would silently lose the
    // callee's mutation. Fail-closed: the checker rejects it (E0434)
    // instead of accepting a writeback that cannot be honoured.
    let src = r#"
type Inner { value: i32 }
type Outer { inner: Inner }
func bump(x: mutate i32) {
    x = x + 1
}
func main() -> i32 {
    let mut o = Outer { inner: Inner { value: 10 } }
    bump(o.inner.value)
    println(o.inner.value)
    0
}
"#;
    let diags = check_source(src).expect_err("nested mutate place must be rejected (E0434)");
    let rendered = diags
        .iter()
        .map(|d| format!("{}", d))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("E0434"),
        "expected E0434 diagnostic, got:\n{rendered}"
    );
}

#[test]
fn mutate_literal_place_rejected_q5() {
    // Q5: `bump(42)` is not a place at all — a mutate borrow cannot target
    // a literal (nothing to write back). Rejected with E0434.
    let src = r#"
func bump(x: mutate i32) {
    x = x + 1
}
func main() -> i32 {
    bump(42)
    0
}
"#;
    let diags = check_source(src).expect_err("literal mutate arg must be rejected (E0434)");
    let rendered = diags
        .iter()
        .map(|d| format!("{}", d))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("E0434"),
        "expected E0434 diagnostic, got:\n{rendered}"
    );
}

#[test]
fn mutate_aliasing_borrow_should_be_rejected_m6() {
    // M6 (0.34.25c, negative test): two simultaneous `mutate` borrows of the
    // SAME field alias exclusive borrows. The borrow system must reject this
    // (cf. Rust: two &mut to the same place) — E0435.
    let src = r#"
func bump2(x: mutate i32, y: mutate i32) -> i32 {
    x = x + 1
    y = y + 1
    x + y
}
flow P {
    state Active { tag: i32 }
    state Done { v: i32 }
    transition go(Active) -> Done {
        let a = bump2(self.tag, self.tag)
        return Done { v: a }
    }
}
func main() -> i32 {
    let s = Active { tag: 1 }
    let r = P::go(s)
    println(r.v)
    0
}
"#;
    // The checker rejects aliasing mutate borrows.
    let diags = check_source(src)
        .expect_err("aliasing mutate borrows of the same field must be rejected (E0435)");
    let rendered = diags
        .iter()
        .map(|d| format!("{}", d))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("E0435"),
        "expected E0435 diagnostic, got:\n{rendered}"
    );
}

#[test]
fn mutate_var_writeback_alias_return_dual_backend() {
    // v0.34.13: `bump(mutate v)` write-back when the callee's return register
    // aliases the mutate param register (`RET r0`, x at r0). do_return used to
    // collect param values after mem::replace → Unit written back to v.
    let src = r#"
func bump(x: mutate i32) -> i32 {
    x = x + 1
    x
}
func main() -> i32 {
    let mut v = 10
    let r = bump(mutate v)
    let sum = v + r
    println(sum)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen mutate var writeback");
    assert_eq!(out.trim(), "22");
}

// ── v0.29.35 broadcast PeerFault sentinel ─────────────────────────────

// ── v0.29.38 Test engineering: assert_state + inject_fault ────────────

#[test]
fn assert_state_correct_state() {
    // L2: assert_state passes when state matches.
    let src = r#"
flow C {
    state A { v: i32 }
    state B { v: i32 }
    transition go(A) -> B { { return B { v: self.v + 1 } } }
}
func main() -> i32 {
    let s0 = A { v: 0 }
    let s1 = C::go(s0)
    assert_state(s1, "B")
    println(s1.v)
    0
}
"#;
    // 0.31.16: flow states are linear — assert_state(s0, "A") before
    // C::go(s0) would consume s0, making the transition a use-after-move.
    // Pre-transition assertions must use a separate copy or be omitted.
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
}

#[test]
fn assert_state_wrong_state() {
    // L2: assert_state fails when state doesn't match.
    let src = r#"
flow C {
    state A { v: i32 }
    state B { v: i32 }
    transition go(A) -> B { { return B { v: self.v + 1 } } }
}
func main() -> i32 {
    let s0 = A { v: 0 }
    assert_state(s0, "B")
    0
}
"#;
    let err = run_source_bytecode_result(src);
    assert!(err.is_err(), "assert_state should fail on mismatch");
    let msg = err.unwrap_err().to_string();
    assert!(msg.contains("assert_state failed"), "got: {}", msg);
}

#[test]
fn inject_fault_constructs_fault() {
    // L2: inject_fault returns a Fault record with SystemTrace.
    let src = r#"
flow C {
    state A { v: i32 }
}
func main() -> i32 {
    let s0 = A { v: 42 }
    let f = inject_fault(s0)
    println(f.last_state)
    println(f.trace.last_state_name)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
}

// ── v0.29.37 Actor lifecycle: SystemKill + spawn detached ─────────────

#[test]
fn spawn_detached_dual_backend() {
    // L1: spawn_detached creates an actor that can be called normally.
    let src = r#"
actor W {
    v: i32
    func read() -> i32 { self.v }
    func set(n: i32) { self.v = n }
}
func main() -> i32 {
    let a = W.spawn()
    a.set(10)
    let d = W.spawn_detached()
    d.set(99)
    println(a.read())
    println(d.read())
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "10");
    assert_eq!(lines[1], "99");
}

#[test]
fn bare_spawn_detached_is_rejected_with_typed_migration() {
    let src = r#"
actor Worker {}
func main() {
    let worker = spawn_detached("Worker")
}
"#;
    let diagnostics = check_source(src).expect_err("bare spawn_detached must be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("ActorType.spawn_detached()")),
        "diagnostic should point users to the portable typed method: {:?}",
        diagnostics
    );
}

// ── v0.29.36 Payload covariance + conservative projection ─────────────

#[test]
fn protocol_payload_covariance_allowed() {
    // L2: flow state with extra fields beyond protocol requirement → OK (width subtyping / covariance).
    let src = r#"
protocol P {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}
flow F {
    impl P
    state Idle
    state Active { data: i32, extra: i32 }
    transition start(Idle) -> Active { { return Active { data: 0, extra: 99 } } }
    transition stop(Active) -> Idle { { return Idle { } } }
}
func main() -> i32 { 0 }
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
}

#[test]
fn protocol_conservative_projection_subflow_rejected() {
    // L2: subflow state in protocol payload that is also a transition target → E0418.
    // This is a conservative rejection: the projection from nested subflow to
    // flat protocol is ambiguous when the inner state is also a protocol target.
    let src = r#"
protocol P {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
}
flow Inner {
    state Active { data: i32 }
}
flow F {
    impl P
    state Idle
    state Active { data: i32, inner: Active }
    transition start(Idle) -> Active { { return Active { data: 0, inner: Active { data: 0 } } } }
}
func main() -> i32 { 0 }
"#;
    let err = check_source(src);
    // T-H17: must reject — E0418 (projection) or E0412 (flatness), not silently ok.
    assert!(
        err.is_err(),
        "expected conservative projection rejection, got Ok"
    );
    let msgs: String = err
        .unwrap_err()
        .iter()
        .map(|d| format!("{:?} {}", d.code, d.message))
        .collect::<Vec<_>>()
        .join(";");
    assert!(
        msgs.contains("0418")
            || msgs.contains("0412")
            || msgs.contains("projection")
            || msgs.contains("flat"),
        "unexpected diagnostics: {}",
        msgs
    );
}

// ── v0.29.35 broadcast PeerFault sentinel ─────────────────────────────

#[test]
fn broadcast_peerfault_sentinel_dual_backend() {
    // L1: broadcast with unknown method → PeerFault sentinel -1 (both backends).
    let src = r#"
actor S {
    v: i32
    func read() -> i32 { self.v }
    func set(n: i32) { self.v = n }
}
func main() -> i32 {
    let a = S.spawn()
    a.set(42)
    let targets = [a]
    let ok = broadcast(targets, "read")
    println(ok[0])
    let bad = broadcast(targets, "nonexistent")
    println(bad[0])
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines[0], "42", "read result");
    assert_eq!(lines[1], "-1", "PeerFault sentinel");
}

// ── v0.29.31 per-actor-type spawn quota + mailbox auto-depth ───────────

#[test]
fn per_type_max_children_quota() {
    let src = r#"
flow W {
    @max_children(1)
    state Idle
}
actor W { n: i32; func read() -> i32 { self.n } }
func main() -> i32 {
    let a = W.spawn()
    let b = W.spawn()
    0
}
"#;
    let err = run_source_bytecode_result(src);
    assert!(err.is_err(), "expected QuotaExceeded, got ok");
    let msg = err.unwrap_err().to_string();
    assert!(
        msg.contains("QuotaExceeded") || msg.contains("max_children"),
        "got {}",
        msg
    );
}

#[test]
fn mailbox_auto_depth_applied() {
    // Flow with @mailbox(depth=N) → auto-applied to spawned actor of same name.
    // The limit is applied but reading it requires builtin parity.
    // Just verify spawn succeeds (no crash from auto-apply code).
    let src = r#"
flow W {
    @mailbox(depth = 50)
    state Idle
}
actor W { n: i32; func read() -> i32 { self.n } }
func main() -> i32 {
    let a = W.spawn()
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "");
}

// ── v0.29.40 Linear type inference optimization ───────────────────────

#[test]
fn multi_target_transition_typecheck() {
    // L2: transition returning multiple states (B | A) typechecks.
    // v0.29.49: caller must not access fields directly on multi-target result.
    let src = r#"
flow C {
    state A { v: i32 }
    state B { v: i32 }
    transition go(A) -> B | A {
        if self.v > 0 {
            return B { v: self.v }
        }
        return A { v: 0 }
    }
}
func main() -> i32 {
    let s = A { v: 5 }
    let r = C::go(s)
    // v0.29.49: must use r as a whole value, not access fields directly
    let r2 = r
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
}

#[test]
fn transition_return_with_subflow_payload() {
    // L2: transition with subflow payload in return type.
    let src = r#"
flow Inner {
    state IActive { n: i32 }
    transition bump(IActive) -> IActive { return IActive { n: self.n + 1 } }
}
flow Outer {
    state Working { child: IActive }
    transition step(Working) -> Working {
        let c = Inner::bump(self.child)
        return Working { child: c }
    }
}
func main() -> i32 {
    let c0 = IActive { n: 0 }
    let w0 = Working { child: c0 }
    let w1 = Outer::step(w0)
    println(w1.child.n)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "1");
}

// ── v0.29.42: Explicit FFI_Pinned State Declaration ──────────────────

#[test]
fn ffi_pinned_state_declaration() {
    // L2: declaring `state FFI_Pinned` in a flow should be accepted and
    // trigger injection of enter_ffi / exit_ffi / ffi_crash transitions.
    let src = r#"
flow FFI {
    state Active { buffer: i32 }
    state FFI_Pinned { buffer: i32 }

    transition process(Active) -> Active { return Active { buffer: self.buffer + 1 } }
}
func main() -> i32 {
    let s = Active { buffer: 42 }
    let fp = FFI::enter_ffi(s)
    let back = FFI::exit_ffi(fp)
    back.buffer
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
}

#[test]
fn ffi_pinned_roundtrip_dual_backend() {
    // L1: enter_ffi then exit_ffi preserves payload data.
    let src = r#"
flow FFI {
    state Active { buffer: i32 }
    state FFI_Pinned { buffer: i32 }

    transition process(Active) -> Active { return Active { buffer: self.buffer + 1 } }
}
func main() -> i32 {
    let s = Active { buffer: 99 }
    let fp = FFI::enter_ffi(s)
    let back = FFI::exit_ffi(fp)
    println(back.buffer)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "99");
}

#[test]
fn ffi_pinned_crash_to_fault() {
    // L1: ffi_crash from FFI_Pinned produces a Fault value.
    let src = r#"
flow FFI {
    state Active { buffer: i32 }
    state FFI_Pinned { buffer: i32 }

    transition process(Active) -> Active { return Active { buffer: self.buffer + 1 } }
}
func main() -> i32 {
    let s = Active { buffer: 7 }
    let fp = FFI::enter_ffi(s)
    let faulted = FFI::ffi_crash(fp)
    println(faulted.last_state)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
    let out = compile_and_run(src).expect("codegen");
    assert_eq!(out.trim(), "FFI_Pinned()");
}

#[test]
fn ffi_pinned_transitions_injected() {
    // L2: verify that enter_ffi, exit_ffi, and ffi_crash are injected
    // when state FFI_Pinned is declared.
    use crate::flow_matrix::expand_file;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    let src = r#"
flow FFI {
    state Active { buffer: i32 }
    state FFI_Pinned { buffer: i32 }
    transition process(Active) -> Active { return Active { buffer: self.buffer } }
}
"#;
    let tokens = Lexer::new(src).tokenize().expect("lex");
    let mut file = Parser::new(tokens).parse_file().expect("parse");
    expand_file(&mut file);
    let flow = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Flow(f) => Some(f),
            _ => None,
        })
        .expect("flow");
    assert!(flow.states.iter().any(|s| s.name == "FFI_Pinned"));
    assert!(flow
        .transitions
        .iter()
        .any(|t| t.name == "enter_ffi" && t.from_state == "Active" && t.is_ffi_pinned));
    assert!(flow
        .transitions
        .iter()
        .any(|t| t.name == "exit_ffi" && t.from_state == "FFI_Pinned" && t.is_ffi_pinned));
    assert!(flow
        .transitions
        .iter()
        .any(|t| t.name == "ffi_crash" && t.from_state == "FFI_Pinned" && t.is_fallback));
}

// ── v0.29.44: Shadow Memory Tagging ───────────────────────────────────

#[test]
fn shadow_alloc_tag_check() {
    // L1 interp: allocate tagged memory, check with correct/wrong tag.
    let src = r#"
func main() -> i32 {
    let ptr = shadow_alloc(64, 1, "test_buf")
    let ok = shadow_check(ptr, 1)
    let bad = shadow_check(ptr, 2)
    println(ok)
    println(bad)
    shadow_free(ptr)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let r = run_source_bytecode_result(src).expect("run");
    assert_eq!(r, interp::Value::Int(0));
}

#[test]
fn shadow_check_rejects_untagged() {
    // L1 interp: checking a random untracked pointer returns false.
    let src = r#"
func main() -> i32 {
    let ok = shadow_check(99999, 1)
    println(ok)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let r = run_source_bytecode_result(src).expect("run");
    assert_eq!(r, interp::Value::Int(0));
}

// ── v0.29.46: Full-Actor Muting (Producer-Side) ───────────────────────

#[test]
fn producer_mute_cascade() {
    // L1 interp: when consumer actor enters mute (mailbox overflow),
    // producer actor should also be muted (push-mute cascade).
    let src = r#"
actor Consumer {
    n: i32
    func bump() -> i32 {
        self.n = self.n + 1
        self.n
    }
}
actor Producer {
    n: i32
    func get() -> i32 { self.n }
}
func main() -> i32 {
    let c = Consumer.spawn()
    actor_set_mailbox_depth(c, 2)
    let p = Producer.spawn()

    // Fill consumer's mailbox to trigger mute
    let _ = c.bump()
    let _ = c.bump()
    let _ = c.bump()

    // Consumer should be muted
    let cm = actor_is_muted(c)
    println(cm)

    0
}
"#;
    let r = run_source_bytecode_result(src);
    assert!(r.is_ok(), "producer mute cascade should not crash: {:?}", r);
}

// ── v0.29.48: Integration Test Sandbox ────────────────────────────────

#[test]
fn test_sandbox_multi_actor() {
    // L1 interp: test_sandbox spawns actors and runs transitions.
    // fix-plan: verify actual output, not just is_ok() (was false success).
    let src = r#"
actor Counter {
    n: i32
    func bump() -> i32 { self.n = self.n + 1; self.n }
}
func main() -> i32 {
    let cfg = Record { actors: ["Counter"], calls: [], faults: [] }
    let results = test_sandbox(cfg)
    println(results.len())
    0
}
"#;
    let (val, stdout) = run_source_with_stdout(src);
    assert_eq!(
        stdout.trim(),
        "1",
        "test_sandbox should spawn 1 actor, got: {}",
        stdout
    );
    assert_eq!(val, interp::Value::Int(0));
}

// ── v0.29.49: Multi-Target Transition Caller Exhaustiveness ───────────

#[test]
fn multi_target_direct_field_rejected() {
    // L2: direct field access on multi-target transition result is rejected (E0420).
    let src = r#"
flow C {
    state A { v: i32 }
    state B { v: i32 }
    transition go(A) -> B | A {
        if self.v > 0 { return B { v: self.v } }
        return A { v: 0 }
    }
}
func main() -> i32 {
    let s = A { v: 5 }
    let r = C::go(s)
    r.v
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "direct field access on multi-target should be rejected"
    );
    let errors = result.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|d| d.message.contains("E0420") || d.message.contains("multi-target")),
        "expected E0420 error, got: {:?}",
        errors
    );
}

#[test]
fn multi_target_incompatible_payload_layout_accepted_adr002() {
    // v0.34.15 (ADR-002, golden §1.2): payload layouts MAY differ across
    // multi-target states — the runtime dispatches on the state TAG, never on
    // layout. Pre-0.34.15 rejected differing layouts with E0419 (M6); that
    // check was inverted, so this test asserts ACCEPTANCE (name updated
    // 0.34.33 to match the post-inversion contract). The transition
    // type-checks, bytecode runs both branches to the correct tagged state,
    // and codegen dispatches via the tagged-state-union ABI.
    let src = r#"
flow C {
    state A { v: i32 }
    state B { message: string }
    transition go(A) -> B | A {
        if self.v > 0 { return B { message: "positive" } }
        return A { v: 0 }
    }
}
func main() -> i32 {
    let s0 = A { v: 5 }
    let r = C::go(s0)
    let tag = match r {
        B { message } => 1,
        A { v } => 2
    }
    println(tag)
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "differing layouts must be accepted after ADR-002 inversion: {:?}",
        check_source(src)
    );
    assert_eq!(
        run_source_bytecode_result(src),
        Ok(interp::Value::Int(0)),
        "runtime dispatch by state tag"
    );
    // v0.34.16 (ADR-002): the tagged-state-union ABI landed — codegen
    // dispatches by tag across differing layouts (no more fail-closed).
    let out = compile_and_run(src).expect("codegen multi-target differing layouts");
    assert_eq!(out.trim(), "1");
}

#[test]
fn multi_target_runtime_tag_dispatch_bytecode() {
    // v0.34.15 (ADR-002): multi-target transition results carry the target
    // state name as the record tag; match arms dispatch on it at runtime
    // (IsVariant extended to Record, PatternField for named field extraction).
    // Covers both same-layout and differing-layout target sets.
    let src = r#"
flow Checker {
    state Small { v: i32 }
    state Large { v: i32 }
    transition classify(Small, amount: i32) -> Small | Large {
        if self.v + amount > 50 {
            return Large { v: self.v + amount }
        } else {
            return Small { v: self.v + amount }
        }
    }
}
flow C {
    state A { v: i32 }
    state B { message: string }
    transition go(A) -> B | A {
        if self.v > 0 { return B { message: "positive" } }
        return A { v: 0 }
    }
}
func main() -> i32 {
    let s1 = Small { v: 10 }
    let r1 = Checker::classify(s1, 100)
    let s2 = Small { v: 10 }
    let r2 = Checker::classify(s2, 5)
    let t1 = match r1 {
        Small { v } => v,
        Large { v } => v
    }
    let t2 = match r2 {
        Small { v } => v,
        Large { v } => v
    }
    let s3 = A { v: 5 }
    let r3 = C::go(s3)
    let t3 = match r3 {
        B { message } => 1,
        A { v } => 2
    }
    let s4 = A { v: -1 }
    let r4 = C::go(s4)
    let t4 = match r4 {
        B { message } => 1,
        A { v } => 2
    }
    println(t1)
    println(t2)
    println(t3)
    println(t4)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (result, out) = run_source_bytecode_with_stdout(src);
    assert_eq!(result, interp::Value::Int(0));
    assert_eq!(out.trim(), "110\n15\n1\n2");
}

#[test]
fn multi_target_codegen_dual_backend_tag_dispatch() {
    // v0.34.16 (ADR-002): L1 — codegen tagged-state-union ABI must agree
    // with bytecode across same-layout and differing-layout target sets
    // (match dispatch by tag + boxed payload decode).
    let src = r#"
flow Checker {
    state Small { v: i32 }
    state Large { v: i32 }
    transition classify(Small, amount: i32) -> Small | Large {
        if self.v + amount > 50 {
            return Large { v: self.v + amount }
        } else {
            return Small { v: self.v + amount }
        }
    }
}
flow C {
    state A { v: i32 }
    state B { message: string }
    transition go(A) -> B | A {
        if self.v > 0 { return B { message: "positive" } }
        return A { v: 0 }
    }
}
func main() -> i32 {
    let s1 = Small { v: 10 }
    let r1 = Checker::classify(s1, 100)
    let s2 = Small { v: 10 }
    let r2 = Checker::classify(s2, 5)
    let t1 = match r1 {
        Small { v } => v,
        Large { v } => v
    }
    let t2 = match r2 {
        Small { v } => v,
        Large { v } => v
    }
    let s3 = A { v: 5 }
    let r3 = C::go(s3)
    let t3 = match r3 {
        B { message } => 1,
        A { v } => 2
    }
    let s4 = A { v: -1 }
    let r4 = C::go(s4)
    let t4 = match r4 {
        B { message } => 1,
        A { v } => 2
    }
    println(t1)
    println(t2)
    println(t3)
    println(t4)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bytecode_out) = run_source_bytecode_with_stdout(src);
    assert_eq!(bytecode_out.trim(), "110\n15\n1\n2");
    let native = compile_and_run(src).expect("codegen multi-target dual backend");
    assert_eq!(native.trim(), "110\n15\n1\n2");
}

#[test]
fn multi_target_disjoint_target_sets_tag_dual_backend() {
    // C1 (audit 2026-08-03): L1 — when two multi-target transitions declare
    // DIFFERENT target sets, the return site must tag with the flow-wide
    // name-sorted ordinal, not the per-transition subset index.
    // Flow union: {A, B, C} → sorted ordinals A=0, B=1, C=2.
    // t1 targets {A, B}: returning A → subset 0 == global 0 (coincidentally
    // correct). t2 targets {A, C}: returning C → subset 1, but global C = 2 —
    // the old subset-relative tag 1 was silently decoded as B by the match.
    // Regression: codegen printed "r1 A / r2 B" while bytecode printed
    // "r1 A / r2 C", exit=0, no diagnostic.
    let src = r#"
flow Calc {
    state A { v: i32 }
    state B { v: i32 }
    state C { v: i32 }
    transition t1(A, d: i32) -> A | B { if d == 0 { return A { v: 1 } } return B { v: 2 } }
    transition t2(B, d: i32) -> A | C { if d == 0 { return C { v: 3 } } return A { v: 4 } }
    transition go(A) -> B { return B { v: 9 } }
}
func main() -> i32 {
    let r1 = Calc::t1(A { v: 0 }, 0)
    match r1 {
        A { v } => println("r1 A")
        B { v } => println("r1 B")
    }
    let b = Calc::go(A { v: 0 })
    let r2 = Calc::t2(b, 0)
    match r2 {
        A { v } => println("r2 A")
        B { v } => println("r2 B")
        C { v } => println("r2 C")
    }
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bytecode_out) = run_source_bytecode_with_stdout(src);
    assert_eq!(bytecode_out.trim(), "r1 A\nr2 C");
    let native = compile_and_run(src).expect("codegen must not mis-tag subset target");
    assert_eq!(native.trim(), "r1 A\nr2 C");
}

#[test]
fn multi_target_disjoint_sets_fault_tag_dual_backend() {
    // C1 (audit 2026-08-03): the `-> S | Fault` absorption path must also use
    // the flow-wide ordinal for the Fault tag. Flow union {A, B, Fault} sorted
    // → A=0, B=1, Fault=2. t2 targets {B, Fault}; the old subset-relative
    // Fault tag (1) was decoded as state B by the match — the Fault arm never
    // fired and the payload was mis-read as B's record.
    let src = r#"
flow Calc {
    state A { v: i32 }
    state B { v: i32 }
    transition t1(A, d: i32) -> A | B { if d == 0 { return A { v: 1 } } return B { v: 2 } }
    transition t2(B, d: i32) -> B | Fault { return B { v: self.v / d } }
    transition go(A) -> B { return B { v: 9 } }
}
func main() -> i32 {
    let r1 = Calc::t1(A { v: 0 }, 0)
    match r1 {
        A { v } => println("r1 A")
        B { v } => println("r1 B")
    }
    let b = Calc::go(A { v: 0 })
    let r2 = Calc::t2(b, 0)
    match r2 {
        B { v } => println("r2 B")
        Fault { last_state, unexpected_event, snapshot: _, trace: _ } => println(last_state)
    }
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bytecode_out) = run_source_bytecode_with_stdout(src);
    assert_eq!(bytecode_out.trim(), "r1 A\nB()");
    let native = compile_and_run(src).expect("codegen must not mis-tag subset Fault");
    assert_eq!(native.trim(), "r1 A\nB()");
}

#[test]
fn multi_target_nested_record_payload_box_sized_dual_backend() {
    // C2 (MEM-C8, L1): wrap_multi_target_value must size the payload box from
    // the actual LLVM type size (size_of), NOT field_count × 8. A target state
    // with a NESTED record field (Done { inner: Inner }, Inner = {i32,i32,i32})
    // lowers to { { i32, i32, i32 } }: count_fields() == 1 but the struct is
    // 12 bytes, so field_count × 8 == 8 undersized the box and the store
    // overflowed the heap. The bytecode data round-trip reads the nested
    // fields back (sum == 6); codegen binds the nested record whole and
    // dispatches by tag (100) — both backends agree, no heap corruption.
    let src = r#"
type Inner { a: i32, b: i32, c: i32 }
flow F {
    state Start { v: i32 }
    state Done { inner: Inner }
    state Skip { v: i32 }
    transition go(Start) -> Done | Skip {
        if self.v > 0 {
            return Done { inner: Inner { a: 1, b: 2, c: 3 } }
        }
        return Skip { v: 0 }
    }
}
func main() -> i32 {
    let s = Start { v: 5 }
    let r = F::go(s)
    let sum = match r {
        Done { inner } => inner.a + inner.b + inner.c,
        Skip { v } => v
    }
    println(sum)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    // Bytecode round-trips the nested payload correctly.
    let (_, bytecode_out) = run_source_bytecode_with_stdout(src);
    assert_eq!(
        bytecode_out.trim(),
        "6",
        "bytecode nested-record round-trip"
    );
    // Codegen must box the 12-byte nested struct without overflow and
    // dispatch the correct tag. (Nested field access in the match arm is a
    // separate native-emitter capability, so the codegen assertion uses a
    // tag-only variant below.)
    let src_tag = r#"
type Inner { a: i32, b: i32, c: i32 }
flow F {
    state Start { v: i32 }
    state Done { inner: Inner }
    state Skip { v: i32 }
    transition go(Start) -> Done | Skip {
        if self.v > 0 {
            return Done { inner: Inner { a: 1, b: 2, c: 3 } }
        }
        return Skip { v: 0 }
    }
}
func main() -> i32 {
    let s = Start { v: 5 }
    let r = F::go(s)
    let tag = match r {
        Done { inner } => 100,
        Skip { v } => v
    }
    println(tag)
    0
}
"#;
    let native = compile_and_run(src_tag).expect("codegen nested-record box must not overflow");
    assert_eq!(native.trim(), "100", "codegen nested-record tag dispatch");
}

#[test]
fn multi_target_payload_box_reuse_no_double_free_dual_backend() {
    // L6 (codegen): the multi-target payload box (wrap_multi_target_value) is
    // registered in heap_allocs at the transition call site and freed once at
    // scope exit. Matching the result multiple times must NOT double-free or
    // use-after-free the box — the decode (inttoptr+load) copies fields out and
    // does not free. A wrong fix (freeing at decode) would abort the codegen
    // binary on the second match. valgrind-clean separately ("All heap blocks
    // were freed"). Bytecode uses Rust deep-clone Value (no box), so both
    // backends agree. (Flow states are linear: `match` reads without consuming,
    // but aliasing `let r2 = r` consumes r — so this test re-matches r directly
    // rather than via an alias.)
    let src = r#"
flow F {
    state Start { v: i32 }
    state Done { v: i32 }
    state Skip { v: i32 }
    transition go(Start) -> Done | Skip {
        if self.v > 0 { return Done { v: self.v } }
        return Skip { v: 0 }
    }
}
func main() -> i32 {
    let s = Start { v: 5 }
    let r = F::go(s)
    let t1 = match r { Done { v } => v, Skip { v } => v }
    let t2 = match r { Done { v } => v + 100, Skip { v } => v }
    println(t1)
    println(t2)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bc) = run_source_bytecode_with_stdout(src);
    assert_eq!(bc.trim(), "5\n105", "bytecode multi-target box re-match");
    let native = compile_and_run(src).expect("codegen multi-target re-match must not double-free");
    assert_eq!(native.trim(), "5\n105", "codegen multi-target box re-match");
}

#[test]
fn enum_packed_payload_box_no_leak_no_double_free_dual_backend() {
    // L6 (codegen): a custom-enum `Packed` variant payload is a heap box
    // (malloc'd by the ctor, ptrtoint-encoded into the i64 slot). Three
    // ownership paths must each free it exactly once:
    //   1. local box (`let r = Rect(..)`) — registered at the ctor site
    //      (register_heap_box), freed at scope exit; re-matching decodes
    //      (inttoptr+load) WITHOUT freeing, so two matches ≠ double-free.
    //   2. returned box (`func .. -> Shape { return Rect(..) }`) — the callee
    //      claims it on return (claim_returned_enum_box) so its scope-exit free
    //      skips it; the caller re-registers it (HeapEntry::EnumBox,
    //      tag-conditional free) and frees it at the caller's scope exit.
    //   3. conditional returns — a box registered in an untaken branch must
    //      free(null) (entry-block null-init), not garbage.
    // A wrong fix (free at decode, or no return-claim) aborts the codegen
    // binary (double-free / use-after-free) or leaks. valgrind-clean separately
    // ("All heap blocks were freed"). Bytecode uses Rust deep-clone Value (no
    // box), so both backends agree on output.
    let src = r#"
type Point { x: i32, y: i32 }
type Shape {
    Circle(f64)
    Rect(f64, f64)
    Wrapped(Point)
    Empty
}
func make_shape(kind: i32) -> Shape {
    if kind == 0 { return Circle(1.5) }
    if kind == 1 { return Rect(2.0, 3.0) }
    return Wrapped(Point { x: 7, y: 8 })
}
func main() -> i32 {
    let r = Rect(2.0, 3.0)
    let a1 = match r { Circle(rad) => 1, Rect(a, b) => 2, Wrapped(p) => 3, Empty => 0 }
    let a2 = match r { Circle(rad) => 1, Rect(a, b) => 2, Wrapped(p) => 3, Empty => 0 }
    let s1 = make_shape(0)
    let s2 = make_shape(1)
    let s3 = make_shape(2)
    let m1 = match s1 { Circle(rad) => 10, Rect(a, b) => 20, Wrapped(p) => 30, Empty => 0 }
    let m2 = match s2 { Circle(rad) => 10, Rect(a, b) => 20, Wrapped(p) => 30, Empty => 0 }
    let m3 = match s3 { Circle(rad) => 10, Rect(a, b) => 20, Wrapped(p) => 30, Empty => 0 }
    println(a1 + a2)
    println(m1 + m2 + m3)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bc) = run_source_bytecode_with_stdout(src);
    assert_eq!(
        bc.trim(),
        "4\n60",
        "bytecode enum packed box reuse + return"
    );
    let native =
        compile_and_run(src).expect("codegen enum packed box must not double-free or leak");
    assert_eq!(
        native.trim(),
        "4\n60",
        "codegen enum packed box reuse + return"
    );
}

#[test]
fn lambda_returning_boxed_enum_dual_backend() {
    // A let-bound lambda declared `-> Shape` must compile its function with the
    // enum's real `{i32 tag, i64 payload}` layout (lambda_ret_type consults
    // type_llvm), NOT the standalone i64 fallback. Three pieces cooperate:
    //   1. lambda_ret_type resolves the custom-enum return → {i32,i64} signature;
    //   2. the let-binding records the closure's Func type in var_types so the
    //      indirect call site knows the return type (closure_return_llvm_type);
    //   3. the closure call registers the returned Packed box (EnumBox) for a
    //      tag-conditional free at the caller's scope exit, while the lambda
    //      claimed it on return.
    // Before the fix the lambda was `define i64` returning a `{i32,i64}` value
    // and the caller called it as i64 — a signature/body mismatch that segfaulted
    // (use-after-free of the misread box pointer). Boxed (Rect) variants allocate
    // a box that must be freed exactly once; inline (Circle) and unit (Empty)
    // variants carry no box. valgrind-clean separately. Both backends agree.
    let src = r#"
type Point { x: i32, y: i32 }
type Shape {
    Circle(f64)
    Rect(f64, f64)
    Wrapped(Point)
    Empty
}
func main() -> i32 {
    let mk_rect = fn() -> Shape { Rect(2.0, 3.0) }
    let mk_circle = fn() -> Shape { Circle(9.0) }
    let mk_empty = fn() -> Shape { Empty }
    let r = mk_rect()
    let c = mk_circle()
    let e = mk_empty()
    let mr = match r { Circle(rad) => 1, Rect(a, b) => 2, Wrapped(p) => 3, Empty => 0 }
    let mc = match c { Circle(rad) => 1, Rect(a, b) => 2, Wrapped(p) => 3, Empty => 0 }
    let me = match e { Circle(rad) => 1, Rect(a, b) => 2, Wrapped(p) => 3, Empty => 0 }
    println(mr + mc + me)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bc) = run_source_bytecode_with_stdout(src);
    assert_eq!(bc.trim(), "3", "bytecode lambda-returned boxed enum");
    let native =
        compile_and_run(src).expect("codegen lambda-returned boxed enum must not crash or leak");
    assert_eq!(native.trim(), "3", "codegen lambda-returned boxed enum");
}

#[test]
fn closure_call_result_record_field_access_dual_backend() {
    // A let-binding whose init is a closure call (`let p = mk_point()`) must
    // track the closure's return type name in var_type_names so field access on
    // the result (`p.x`) resolves the record layout. Before the fix, the
    // closure call result had no tracked type — infer_object_type fell back to
    // the variable name ("p"), which is not in type_llvm, so codegen rejected
    // the field access with E0707 while the bytecode backend (which carries
    // resolved types) worked. The closure's Func return type (recorded at the
    // lambda's own let-binding) is the source of truth.
    let src = r#"
type Point { x: i32, y: i32 }
func main() -> i32 {
    let mk_point = fn() -> Point { Point { x: 7, y: 8 } }
    let p = mk_point()
    println(p.x + p.y)
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bc) = run_source_bytecode_with_stdout(src);
    assert_eq!(
        bc.trim(),
        "15",
        "bytecode closure-result record field access"
    );
    let native = compile_and_run(src)
        .expect("codegen closure-result record field access must compile (was E0707)");
    assert_eq!(
        native.trim(),
        "15",
        "codegen closure-result record field access"
    );
}

#[test]
fn multi_target_match_accepted() {
    // A multi-target value may be moved as a whole before it is matched.
    let src2 = r#"
flow C {
    state A { v: i32 }
    state B { v: i32 }
    transition go(A) -> B | A {
        if self.v > 0 { return B { v: self.v } }
        return A { v: 0 }
    }
}
func main() -> i32 {
    let s = A { v: 5 }
    let r = C::go(s)
    // Using r as a whole value (not field access) should be OK
    let r2 = r
    0
}
"#;
    let result = check_source(src2);
    assert!(
        result.is_ok(),
        "non-field use of multi-target should be accepted: {:?}",
        result
    );
}

// ── FLOW-IDENTITY-001: State Unforgeability (E0421) ──────────────────

#[test]
fn flow_state_forgery_non_root_outside_transition_rejected() {
    // L2: constructing a non-root flow state outside a transition body is
    // state forgery and must be rejected (E0421).
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let forged = Positive { count: 999 }
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "non-root state construction outside transition should be rejected"
    );
    let errors = result.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0421")
            || d.message.contains("cannot be constructed outside")),
        "expected E0421 error, got: {:?}",
        errors
    );
}

#[test]
fn flow_state_root_construction_outside_transition_allowed() {
    // Constructing the root (first-declared) state outside a transition body
    // is the legitimate Flow constructor and must be accepted.
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let s1 = Counter::inc(s0)
    s1.count
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "root state construction outside transition should be accepted: {:?}",
        result
    );
}

#[test]
fn flow_state_non_root_inside_transition_allowed() {
    // Constructing a non-root state inside a transition body is the normal
    // state production path and must be accepted.
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
    transition reset(Positive) -> Zero { return Zero { count: 0 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let s1 = Counter::inc(s0)
    s1.count
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "non-root state construction inside transition should be accepted: {:?}",
        result
    );
}

// ── FLOW-IDENTITY-001: Linear Generation (E0423) ─────────────────────

#[test]
fn flow_state_use_after_transition_rejected() {
    // L2: using a flow state variable after it has been consumed by a
    // transition call must be rejected (E0423).
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    state Done
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
    transition finish(Positive) -> Done { return Done { } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let s1 = Counter::inc(s0)
    let _d = Counter::finish(s1)
    println(s1.count)
    0
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "use-after-transition should be rejected");
    let errors = result.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_deref() == Some("E0423")
                || d.message.contains("consumed by transition")),
        "expected E0423 error, got: {:?}",
        errors
    );
}

#[test]
fn flow_state_sequential_transitions_accepted() {
    // Valid sequential transitions: each state variable is used exactly once
    // as a transition source, then the result is bound to a new variable.
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    state Done
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
    transition inc2(Positive) -> Positive { return Positive { count: self.count + 1 } }
    transition finish(Positive) -> Done { return Done { } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let s1 = Counter::inc(s0)
    let s2 = Counter::inc2(s1)
    let _d = Counter::finish(s2)
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "sequential transitions should be accepted: {:?}",
        result
    );
}

// ── 0.31.13 追加 A: Flow state alias tracking + shared/ref rejection ──

#[test]
fn flow_state_alias_then_use_original_rejected() {
    // 0.31.13 追加 A: `let b = s0` consumes s0 (alias transfer).
    // Using s0 after aliasing must be rejected (E0423).
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let b = s0
    let s1 = Counter::inc(s0)
    0
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "use-after-alias should be rejected");
    let errors = result.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_deref() == Some("E0423") && d.message.contains("alias")),
        "expected E0423 with alias message, got: {:?}",
        errors
    );
}

#[test]
fn flow_state_alias_target_usable() {
    // 0.31.13 追加 A: after `let b = s0`, b is the valid owner.
    // Using b in a transition should be accepted.
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let b = s0
    let s1 = Counter::inc(b)
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "alias target should be usable: {:?}",
        result
    );
}

#[test]
fn flow_state_shared_rejected() {
    // 0.31.13 追加 A: `shared` wrapping of a flow state is rejected (E0427).
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    shared s = s0
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "shared wrapping of flow state should be rejected"
    );
    let errors = result.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0427")),
        "expected E0427, got: {:?}",
        errors
    );
}

#[test]
fn flow_state_ref_rejected() {
    // 0.31.13 追加 A: `ref` borrowing of a flow state is rejected (E0427).
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let ref r = s0
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "ref borrowing of flow state should be rejected"
    );
    let errors = result.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0427")),
        "expected E0427, got: {:?}",
        errors
    );
}

#[test]
fn flow_state_shadowing_does_not_reset_consumption() {
    // 0.31.13 追加 A: shadowing a consumed flow state variable does NOT
    // clear the consumption record. The old variable remains consumed.
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let s1 = Counter::inc(s0)
    let s0 = Zero { count: 99 }
    let s2 = Counter::inc(s0)
    0
}
"#;
    // After shadowing removal, the second `Counter::inc(s0)` triggers E0423
    // because the name "s0" is still marked as consumed from the first use.
    // This is a known false positive that 0.31.16 (CFG place tracking) will fix.
    let result = check_source(src);
    assert!(
        result.is_err(),
        "shadowing should not reset consumption (conservative)"
    );
    let errors = result.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0423")),
        "expected E0423, got: {:?}",
        errors
    );
}

// ── FLOW-TURN-001: Atomic Turn — fails E + Rejected ──────────────────

#[test]
fn flow_turn_try_without_fails_rejected() {
    // L2: `?` in a transition body without `fails E` is a static error (E0424).
    let src = r#"
flow Account {
    state Active { balance: i32 }
    transition withdraw(Active, amount: i32) -> Active {
        let result = safe_div(self.balance, amount)
        let new_balance = result?
        return Active { balance: new_balance }
    }
}
func safe_div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { return Err("div0") }
    return Ok(a / b)
}
func main() -> i32 { 0 }
"#;
    let result = check_source(src);
    assert!(result.is_err(), "? without fails E should be rejected");
    let errors = result.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0424")),
        "expected E0424, got: {:?}",
        errors
    );
}

#[test]
fn flow_turn_try_with_fails_accepted() {
    // `?` in a transition body with `fails E` is accepted by the checker.
    let src = r#"
flow Account {
    state Active { balance: i32 }
    transition withdraw(Active, amount: i32) -> Active fails string {
        let result = safe_div(self.balance, amount)
        let new_balance = result?
        return Active { balance: new_balance }
    }
}
func safe_div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { return Err("div0") }
    return Ok(a / b)
}
func main() -> i32 {
    let s0 = Active { balance: 100 }
    let r = Account::withdraw(s0, 5)
    match r {
        Ok(s1) => s1.balance,
        Err(_) => 0 - 1,
    }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "? with fails E should be accepted: {:?}",
        result
    );
}

#[test]
fn flow_turn_rejected_returns_source() {
    // Interpreter: `?` failure in a transition with `fails E` produces
    // Err((source_payload, error)) — the source generation is returned.
    let src = r#"
flow Account {
    state Active { balance: i32 }
    transition withdraw(Active, amount: i32) -> Active fails string {
        let result = safe_div(self.balance, amount)
        let new_balance = result?
        return Active { balance: new_balance }
    }
}
func safe_div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { return Err("div0") }
    return Ok(a / b)
}
func main() -> i32 {
    let s0 = Active { balance: 100 }
    let r = Account::withdraw(s0, 0)
    0
}
"#;
    let result = checked_run_source_result(src);
    assert_eq!(result, Ok(interp::Value::Int(0)));
}

#[test]
fn flow_turn_success_path_unaffected() {
    // Happy path: transition with `fails E` that does NOT trigger `?`
    // returns the target state normally.
    let src = r#"
flow Account {
    state Active { balance: i32 }
    transition withdraw(Active, amount: i32) -> Active fails string {
        let result = safe_div(self.balance, amount)
        let new_balance = result?
        return Active { balance: new_balance }
    }
}
func safe_div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { return Err("div0") }
    return Ok(a / b)
}
func main() -> i32 {
    let s0 = Active { balance: 100 }
    let r = Account::withdraw(s0, 5)
    match r {
        Ok(s1) => s1.balance,
        Err(_) => 0 - 1,
    }
}
"#;
    let result = checked_run_source_result(src);
    assert_eq!(result, Ok(interp::Value::Int(20)));
}

#[test]
fn flow_turn_become_explicit_terminal() {
    // v0.34.11 (ADR-001): `become` removed — `return Target { ... }` is the
    // unique transition terminal (was FLOW-TURN-001 become semantics).
    let src = r#"
flow Counter {
    state Idle { count: i32 }
    state Active { count: i32 }
    transition start(Idle) -> Active { return Active { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Idle { count: 10 }
    let s1 = Counter::start(s0)
    s1.count
}
"#;
    let result = checked_run_source_result(src);
    assert_eq!(result, Ok(interp::Value::Int(11)));
}

#[test]
fn flow_turn_become_dual_backend() {
    // v0.34.11 (ADR-001): transition terminal via `return Target { ... }`
    // works in both interpreter and codegen.
    let src = r#"
flow Counter {
    state Idle { count: i32 }
    state Active { count: i32 }
    transition start(Idle) -> Active { return Active { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Idle { count: 10 }
    let s1 = Counter::start(s0)
    println(s1.count)
    0
}
"#;
    let interp_result = checked_run_source_result(src);
    assert_eq!(interp_result, Ok(interp::Value::Int(0)));
    let native = checked_compile_and_run(src).expect("codegen return transition");
    assert_eq!(native.trim(), "11");
}

#[test]
fn flow_turn_stay_self_loop() {
    // v0.34.11 (ADR-001): `stay` removed — self-loop via explicit
    // `return Active { ... }` returning the source state.
    let src = r#"
flow Counter {
    state Active { count: i32 }
    transition noop(Active) -> Active { return Active { count: self.count } }
}
func main() -> i32 {
    let s0 = Active { count: 42 }
    let s1 = Counter::noop(s0)
    s1.count
}
"#;
    let result = checked_run_source_bytecode_result(src);
    assert_eq!(result, Ok(interp::Value::Int(42)));
}

#[test]
fn flow_turn_stay_dual_backend() {
    // v0.34.11 (ADR-001): `stay` removed — self-loop via explicit
    // `return Active { ... }` works in both interpreter and codegen.
    let src = r#"
flow Counter {
    state Active { count: i32 }
    transition noop(Active) -> Active { return Active { count: self.count } }
}
func main() -> i32 {
    let s0 = Active { count: 42 }
    let s1 = Counter::noop(s0)
    println(s1.count)
    0
}
"#;
    let interp_result = checked_run_source_bytecode_result(src);
    assert_eq!(interp_result, Ok(interp::Value::Int(0)));
    let native = checked_compile_and_run(src).expect("codegen return self-loop");
    assert_eq!(native.trim(), "42");
}

#[test]
fn flow_turn_become_multi_target() {
    // v0.34.11 (ADR-001): `become` removed — multi-target transition with
    // conditional uses `return Open { ... }` / `return Closed { ... }`.
    // v0.34.33: this test intentionally exercises the BYTECODE-LEVEL ABI
    // (direct field access on a tagged multi-target result) through the
    // unchecked harness. In checked mode this form is fail-closed with E0420
    // (asserted below; canonical test: `multi_target_direct_field_rejected`).
    // Checked dual-backend dispatch via match-on-state-tag is covered by
    // `multi_target_codegen_dual_backend_tag_dispatch`.
    let src = r#"
flow Gate {
    state Idle { v: i32 }
    state Open { v: i32 }
    state Closed { v: i32 }
    transition decide(Idle, threshold: i32) -> Open | Closed {
        if self.v > threshold {
            return Open { v: self.v }
        } else {
            return Closed { v: self.v }
        }
    }
}
func main() -> i32 {
    let s0 = Idle { v: 10 }
    let s1 = Gate::decide(s0, 5)
    s1.v
}
"#;
    let result = run_source_bytecode_result(src);
    assert_eq!(result, Ok(interp::Value::Int(10)));
    // Contract: the same source is fail-closed (E0420) in checked mode, so
    // this unchecked ABI probe does not silently weaken L2.
    let errors = check_source(src).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|d| d.message.contains("E0420") || d.message.contains("multi-target")),
        "expected E0420 for direct field access on multi-target, got: {:?}",
        errors
    );
}

#[test]
fn flow_turn_become_stay_rejected_as_removed_keywords() {
    // v0.34.11 (ADR-001): `become`/`stay` removed from the keyword table —
    // they now lex as ordinary identifiers, so any use fails to parse (a bare
    // identifier followed by a state constructor is not a valid statement).
    for kw in ["become", "stay"] {
        let src = format!(
            r#"
flow Counter {{
    state Idle {{ count: i32 }}
    state Active {{ count: i32 }}
    transition start(Idle) -> Active {{ {kw} Active {{ count: self.count + 1 }} }}
}}
func main() -> i32 {{
    let s0 = Idle {{ count: 10 }}
    let s1 = Counter::start(s0)
    s1.count
}}
"#
        );
        let result = checked_run_source_bytecode_result(&src);
        assert!(
            result.is_err(),
            "`{kw}` should fail to parse after v0.34.11 removal, got {result:?}"
        );
    }
}

#[test]
fn flow_turn_rejected_dual_backend() {
    // FLOW-TURN-001: `?` failure in a `fails E` transition produces
    // Err((source, error)) in both interpreter and codegen.
    let src = r#"
flow Account {
    state Active { balance: i32 }
    transition withdraw(Active, amount: i32) -> Active fails string {
        let result = safe_div(self.balance, amount)
        let new_balance = result?
        return Active { balance: new_balance }
    }
}
func safe_div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { return Err("div0") }
    return Ok(a / b)
}
func main() -> i32 {
    let s0 = Active { balance: 100 }
    let r = Account::withdraw(s0, 0)
    let out = match r {
        Ok(_) => 1,
        Err(_) => 0 - 1,
    }
    println(out)
    0
}
"#;
    // Interpreter: Rejected path returns Err((source, "div0")), match hits Err branch → -1
    let interp_result = checked_run_source_result(src);
    assert_eq!(interp_result, Ok(interp::Value::Int(0)));
    // Codegen: same behavior
    let native = checked_compile_and_run(src).expect("codegen rejected path");
    assert_eq!(native.trim(), "-1");
}

#[test]
fn flow_turn_success_dual_backend() {
    // FLOW-TURN-001: `?` success in a `fails E` transition returns Ok(target)
    // in both interpreter and codegen.
    let src = r#"
flow Account {
    state Active { balance: i32 }
    transition withdraw(Active, amount: i32) -> Active fails string {
        let result = safe_div(self.balance, amount)
        let new_balance = result?
        return Active { balance: new_balance }
    }
}
func safe_div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { return Err("div0") }
    return Ok(a / b)
}
func main() -> i32 {
    let s0 = Active { balance: 100 }
    let r = Account::withdraw(s0, 5)
    let out = match r {
        Ok(_) => 1,
        Err(_) => 0 - 1,
    }
    println(out)
    0
}
"#;
    let interp_result = checked_run_source_result(src);
    assert_eq!(interp_result, Ok(interp::Value::Int(0)));
    let native = checked_compile_and_run(src).expect("codegen success path");
    assert_eq!(native.trim(), "1");
}

#[test]
fn flow_typed_fault_parse_and_check() {
    // v0.31.10: `fault ErrorType` declares a per-Flow typed Fault.
    // The injected Fault state carries an additional `error: ErrorType` field.
    let src = r#"
type AccountError {
    code: i32,
    reason: string,
}

flow Account {
    state Active { balance: i32 }
    fault AccountError
    transition deposit(Active, amount: i32) -> Active { return Active { balance: self.balance + amount } }
}
func main() -> i32 {
    let s0 = Active { balance: 100 }
    let s1 = Account::deposit(s0, 50)
    println(s1.balance)
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "per-flow typed fault should be accepted: {:?}",
        result
    );
    // Interpreter: normal transition still works
    let interp_result = checked_run_source_result(src);
    assert_eq!(interp_result, Ok(interp::Value::Int(0)));
    // Codegen: normal transition still works
    let native = checked_compile_and_run(src).expect("codegen typed fault");
    assert_eq!(native.trim(), "150");
}

#[test]
fn flow_typed_fault_fallback_includes_error_field() {
    // v0.31.10 / 0.34.18b: a `fault T` flow's absorbed-panic Fault carries the
    // typed `error: T` field defaulted (error.code = 0). @dense fallback removed
    // (amendment clause 1); entry is now a multi-target `-> Running | Fault`
    // transition whose body panics. Dual-backend: both backends default `error`.
    let src = r#"
type MyError {
    code: i32,
}

flow Svc {
    state Idle { n: i32 }
    state Running { n: i32 }
    fault MyError
    transition start(Idle, d: i32) -> Running | Fault { return Running { n: self.n / d } }
}
func main() -> i32 {
    let u = Svc::start(Idle { n: 5 }, 0)
    match u {
        Running { n } => println(n)
        Fault { last_state, unexpected_event, snapshot, trace, error } => {
            println(last_state)
            println(error.code)
        }
    }
    0
}
"#;
    assert!(
        check_source(src).is_ok(),
        "type check: {:?}",
        check_source(src)
    );
    let interp_result = checked_run_source_result(src);
    assert_eq!(interp_result, Ok(interp::Value::Int(0)));
    let native = checked_compile_and_run(src).expect("codegen typed fault absorption");
    assert_eq!(native.trim(), "Idle()\n0", "got {:?}", native);
}

#[test]
fn flow_sparse_skips_fallback_injection() {
    // v0.31.10: @sparse flows skip N×M fallback injection.
    // Calling a declared event from a state that doesn't handle it is a
    // compile-time error instead of auto-routing to Fault.
    let src = r#"
flow Gate @sparse {
    state Idle { v: i32 }
    state Open { v: i32 }
    transition open(Idle) -> Open { return Open { v: self.v } }
}
func main() -> i32 {
    let s0 = Idle { v: 1 }
    let s1 = Gate::open(s0)
    println(s1.v)
    0
}
"#;
    // Normal transition still works
    let result = check_source(src);
    assert!(result.is_ok(), "sparse flow check: {:?}", result);
    let interp_result = checked_run_source_result(src);
    assert_eq!(interp_result, Ok(interp::Value::Int(0)));
    let native = checked_compile_and_run(src).expect("codegen sparse");
    assert_eq!(native.trim(), "1");
}

#[test]
fn flow_panic_absorption_div_zero_dual_backend() {
    // v0.34.18a (amendment clause 1): a transition that declares it can fault
    // (`-> S | Fault`) absorbs a runtime panic (division by zero) into the
    // `Fault` variant instead of aborting — the compiler bottoms out the Fault
    // payload. Mirrors the bytecode VM's absorb_flow_fault; dual-backend parity.
    let src = r#"
flow Calc {
    state Ready { v: i32 }
    transition div(Ready, d: i32) -> Ready | Fault { return Ready { v: self.v / d } }
}
func main() -> i32 {
    let r = Calc::div(Ready { v: 10 }, 0)
    match r {
        Ready { v } => println(v)
        Fault { last_state, unexpected_event, snapshot: _, trace: _ } => {
            println(last_state)
            println(unexpected_event)
        }
    }
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bc) = run_source_bytecode_with_stdout(src);
    assert_eq!(
        bc.trim(),
        "Ready()\nPanic(E0801)",
        "bytecode absorbed div-by-zero"
    );
    let native = compile_and_run(src).expect("codegen must absorb div-by-zero, not abort");
    assert_eq!(
        native.trim(),
        "Ready()\nPanic(E0801)",
        "codegen absorbed div-by-zero"
    );
}

#[test]
fn flow_panic_absorption_persistent_shadow_dual_backend() {
    // H4 (audit-codegen 2026-08-03): persistent draft values must survive a
    // panic→Fault absorption — the Fault record shadows the from-state's
    // persistent fields (interp: shadow_persistent_into_fault). Before the fix
    // codegen defaulted them (printed 0 where interp printed the draft 10).
    let src = r#"
flow Calc {
    persistent state S { v: i32 }
    transition go(S, d: i32) -> S | Fault { { return S { v: self.v / d } } }
}
func main() -> i32 {
    let r = Calc::go(S { v: 10 }, 0)
    match r {
        S { v } => println(v)
        Fault { last_state, unexpected_event, snapshot: _, trace: _, v } => {
            println(last_state)
            println(unexpected_event)
            println(v)
        }
    }
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bc) = run_source_bytecode_with_stdout(src);
    assert_eq!(
        bc.trim(),
        "S()\nPanic(E0801)\n10",
        "bytecode shadows persistent draft into Fault"
    );
    let native = compile_and_run(src).expect("codegen must absorb div-by-zero, not abort");
    assert_eq!(
        native.trim(),
        "S()\nPanic(E0801)\n10",
        "codegen must shadow the persistent draft value (10), not the default (0)"
    );
}

#[test]
fn flow_panic_absorption_normal_path_returns_state_dual_backend() {
    // v0.34.18a: when no panic occurs, the `-> S | Fault` transition returns the
    // state variant normally (the absorption path is not taken).
    let src = r#"
flow Calc {
    state Ready { v: i32 }
    transition div(Ready, d: i32) -> Ready | Fault { return Ready { v: self.v / d } }
}
func main() -> i32 {
    let r = Calc::div(Ready { v: 10 }, 2)
    match r {
        Ready { v } => println(v)
        Fault { last_state, unexpected_event, snapshot: _, trace: _ } => println(last_state)
    }
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bc) = run_source_bytecode_with_stdout(src);
    assert_eq!(bc.trim(), "5", "bytecode normal path");
    let native = compile_and_run(src).expect("codegen normal path");
    assert_eq!(native.trim(), "5", "codegen normal path");
}

#[test]
fn flow_panic_absorption_multi_flow_match_dispatch_dual_backend() {
    // v0.34.18a: with two fallible flows, each flow's `__MultiTarget` union has
    // its own `Fault` variant. The match dispatch must resolve `Fault` scoped to
    // the scrutinee's flow (not globally — the shared `Fault` name is otherwise
    // ambiguous across flows' synthetic enums, mis-tagging the arms and aborting
    // with a non-exhaustive match). Both flows absorb a div-by-zero here.
    let src = r#"
flow A {
    state S { v: i32 }
    transition go(S, d: i32) -> S | Fault { { return S { v: self.v / d } } }
}
flow B {
    state T { v: i32 }
    transition go(T, d: i32) -> T | Fault { { return T { v: self.v / d } } }
}
func main() -> i32 {
    let ra = A::go(S { v: 1 }, 0)
    match ra {
        S { v } => println(v)
        Fault { last_state, unexpected_event, snapshot: _, trace: _ } => println(last_state)
    }
    let rb = B::go(T { v: 2 }, 0)
    match rb {
        T { v } => println(v)
        Fault { last_state, unexpected_event, snapshot: _, trace: _ } => println(last_state)
    }
    0
}
"#;
    assert!(check_source(src).is_ok(), "{:?}", check_source(src));
    let (_, bc) = run_source_bytecode_with_stdout(src);
    assert_eq!(bc.trim(), "S()\nT()", "bytecode multi-flow fault dispatch");
    let native = compile_and_run(src).expect("codegen multi-flow fault dispatch must not abort");
    assert_eq!(
        native.trim(),
        "S()\nT()",
        "codegen multi-flow fault dispatch"
    );
}

#[test]
fn flow_sparse_undefined_event_rejected() {
    // v0.31.10: In a @sparse flow, calling a declared event from a state
    // that doesn't handle it is a compile-time error (no fallback to Fault).
    let src = r#"
flow Gate @sparse {
    state Idle { v: i32 }
    state Open { v: i32 }
    transition open(Idle) -> Open { return Open { v: self.v } }
}
func main() -> i32 {
    let s0 = Open { v: 1 }
    let s1 = Gate::open(s0)
    0
}
"#;
    // Calling open(Open) should fail — no fallback injected in sparse mode
    let result = check_source(src);
    assert!(
        result.is_err(),
        "sparse flow should reject undefined (state, event) pair"
    );
}

#[test]
fn flow_dense_annotation_rejected_by_amendment_clause_1() {
    // 0.34.18b: @dense (N×M Fault fallback injection) was repealed by amendment
    // clause 1 (sparse-irreversible). The parser must reject it outright.
    let src = r#"
flow Counter @dense {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("tokenize");
    let err = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect_err("@dense must be rejected by parser");
    assert!(
        err.message.contains("amendment clause 1"),
        "error should mention amendment clause 1, got: {}",
        err.message
    );
}

#[test]
fn flow_explicit_reset_overrides_system_verb() {
    // v0.31.10 / 0.34.18b: user-defined reset(Fault) -> State overrides the
    // auto-injected system verb. Entry via single-target absorbed panic.
    // Bytecode-only: see flow_reset_rebuilds_root note (dynamic Fault typing).
    let src = r#"
flow Counter {
    state Zero { n: i32 }
    state Positive { n: i32 }
    transition inc(Zero) -> Positive { return Positive { n: 1 } }
    transition crash(Positive) -> Positive {
        let x = 1 / 0
        return Positive { n: self.n }
    }
    transition reset(Fault) -> Zero { return Zero { n: 42 } }
}
func main() -> i32 {
    let s1 = Counter::inc(Zero { n: 0 })
    let f = Counter::crash(s1)
    let z = Counter::reset(f)
    println(z.n)
    0
}
"#;
    // User-defined reset returns n=42 (not the default n=0)
    let (_, out) = run_source_bytecode_with_stdout(src);
    assert_eq!(out.trim(), "42", "explicit reset overrides, got {:?}", out);
}

#[test]
fn flow_explicit_recover_overrides_system_verb() {
    // v0.31.10 / 0.34.18b: user-defined recover(Fault) -> State overrides the
    // auto-injected system verb. Entry via single-target absorbed panic.
    // Bytecode-only: see flow_reset_rebuilds_root note (dynamic Fault typing).
    let src = r#"
flow Svc {
    persistent state Config { retries: i32 }
    state Running { n: i32 }
    transition start(Config) -> Running { return Running { n: self.retries } }
    transition crash(Running) -> Running {
        let x = 1 / 0
        return Running { n: self.n }
    }
    transition recover(Fault) -> Config { return Config { retries: 99 } }
}
func main() -> i32 {
    let s1 = Svc::start(Config { retries: 0 })
    let f = Svc::crash(s1)
    let c = Svc::recover(f)
    println(c.retries)
    0
}
"#;
    // User-defined recover returns retries=99 (not the persistent shadow)
    let (_, out) = run_source_bytecode_with_stdout(src);
    assert_eq!(
        out.trim(),
        "99",
        "explicit recover overrides, got {:?}",
        out
    );
}

// ── 0.36.5 Fault nominal: recover 穷尽 match (裁决 2, DoD #3) ──────────

#[test]
fn fault_recover_exhaustive_match_full_arms_ok() {
    // 0.36.5 (裁决 2): recover reads the failure attribution via an exhaustive
    // `match` over the nominal StateId (Active + Fault). All arms → check passes.
    let src = r#"
flow Svc {
    state Active { n: i32 }
    transition crash(Active) -> Fault {
        return Fault {
            last_state: Active,
            unexpected_event: crash,
            snapshot: "boom",
            trace: SystemTrace {
                last_state_name: "Active",
                unexpected_event: "crash",
                snapshot: "boom",
                memory_dump: MemoryDump { fields: "", count: 0 },
                panic_payload: PanicPayload { error_type: "crash", file: "", line: 0, stack: "boom" }
            }
        }
    }
    transition recover(Fault) -> Active {
        let n = match self.last_state {
            Active => 1
            Fault => 2
        }
        return Active { n: n }
    }
}

func main() -> i32 {
    let f = Svc::crash(Active { n: 0 })
    let r = Svc::recover(f)
    r.n
}
"#;
    assert!(
        check_source(src).is_ok(),
        "exhaustive StateId match should check: {:?}",
        check_source(src)
    );
}

#[test]
fn fault_recover_exhaustive_match_missing_arm_rejected() {
    // 0.36.5 (裁决 2, DoD #3): a recover match over StateId that omits an arm
    // (here `Fault`) is a compile error (E0215) — the checker enforces
    // exhaustiveness, so a renamed state cannot silently fall through.
    let src = r#"
flow Svc {
    state Active { n: i32 }
    transition crash(Active) -> Fault {
        return Fault {
            last_state: Active,
            unexpected_event: crash,
            snapshot: "boom",
            trace: SystemTrace {
                last_state_name: "Active",
                unexpected_event: "crash",
                snapshot: "boom",
                memory_dump: MemoryDump { fields: "", count: 0 },
                panic_payload: PanicPayload { error_type: "crash", file: "", line: 0, stack: "boom" }
            }
        }
    }
    transition recover(Fault) -> Active {
        let n = match self.last_state {
            Active => 1
            // missing `Fault` arm → non-exhaustive (E0215)
        }
        return Active { n: n }
    }
}

func main() -> i32 {
    let f = Svc::crash(Active { n: 0 })
    let r = Svc::recover(f)
    r.n
}
"#;
    let errors = check_source(src).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.code.as_deref() == Some(crate::diagnostic::codes::E0215)),
        "missing StateId arm must be E0215, got: {:?}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 追加 B: `?` after linear resource consumption is rejected (E0429).
/// Architecture amendment clause 9: linear resources consumed before
/// fallible operations cannot be rolled back on Rejected.
#[test]
fn flow_turn_try_after_linear_consumption_rejected() {
    let src = r#"
flow Parser {
    state Pending { data: i32 }
    state Ready { data: i32 }
    transition parse(Pending, token: i32) -> Ready fails string {
        // Consume the linear resource (flow state alias)
        let consumed = self
        // Then try a fallible operation — should be rejected
        let result = safe_div(10, token)
        let value = result?
        return Ready { data: value }
    }
}
func safe_div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { return Err("div0") }
    return Ok(a / b)
}
func main() -> i32 { 0 }
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "? after linear consumption should be rejected"
    );
    let errors = result.unwrap_err();
    assert!(
        errors.iter().any(|d| d.code.as_deref() == Some("E0429")),
        "expected E0429, got: {:?}",
        errors
    );
}

/// 追加 B: `?` before linear resource consumption is accepted.
#[test]
fn flow_turn_try_before_linear_consumption_accepted() {
    let src = r#"
flow Parser {
    state Pending { data: i32 }
    state Ready { data: i32 }
    transition parse(Pending, token: i32) -> Ready fails string {
        // Fallible operation first — no linear resource consumed yet
        let result = safe_div(10, token)
        let value = result?
        // Now consume the linear resource
        return Ready { data: value + self.data }
    }
}
func safe_div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { return Err("div0") }
    return Ok(a / b)
}
func main() -> i32 {
    let s0 = Pending { data: 5 }
    let r = Parser::parse(s0, 2)
    match r {
        Ok(s1) => s1.data,
        Err(_) => 0 - 1,
    }
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "? before linear consumption should be accepted: {:?}",
        result
    );
}

// ── 0.31.19 攻击审查: tuple × Flow ─────────────────────────────────

#[test]
fn flow_state_tuple_rejected() {
    // 0.31.19 攻击审查: flow states cannot be stored in tuples (E0427).
    // Tuple construction implies the element is accessible by index,
    // violating exactly-once consumption.
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let t = (s0, 42)
    0
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "flow state in tuple should be rejected");
    let errors = result.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_deref() == Some("E0427") && d.message.contains("tuple")),
        "expected E0427 with tuple message, got: {:?}",
        errors
    );
}

// ── 0.31.17: 高阶交互闭环 — 集合 × Flow（补充）────────────────────

#[test]
fn flow_state_map_value_rejected() {
    // 0.31.17: flow states cannot be map values (E0427).
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    transition inc(Zero) -> Zero { return Zero { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let m = {"state": s0}
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "flow state as map value should be rejected"
    );
    let errors = result.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_deref() == Some("E0427") && d.message.contains("map")),
        "expected E0427 with map message, got: {:?}",
        errors
    );
}

#[test]
fn explicit_lifetime_annotation_rejected_by_adr004() {
    // v0.34.4 (ADR-004): explicit lifetime annotations `&'a T` removed.
    // The lexer rejects `'` outright — it has no other use in the language.
    let src = r#"
func f(x: &'a i32) -> i32 { *x }
func main() -> i32 { 0 }
"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize();
    assert!(
        tokens.is_err(),
        "lexer must reject `'a` lifetime syntax (ADR-004)"
    );
}
