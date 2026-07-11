use crate::ast::*;
use crate::tests::*;

#[test]
fn flow_parse_debug() {
    // Test that a block body transition doesn't consume the flow body's `}`
    let src = "flow F { state A state B transition go(A) -> B { } }";
    // Tokens: Flow, Ident("F"), LBrace, State, Ident("A"), State, Ident("B"),
    //         Transition, Ident("go"), LParen, Ident("A"), RParen, Arrow, Ident("B"),
    //         LBrace, RBrace, RBrace, Eof
    // The { } is the transition body. The } after that is the flow body closer.
    // parse_block() should consume { } and leave the final } for the flow body.
    let file = parse(src);
    assert_eq!(file.items.len(), 1);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.name, "F");
            assert_eq!(f.states.len(), 2);
            assert_eq!(f.transitions.len(), 1);
            assert!(
                f.transitions[0].body.is_some(),
                "transition body should be Some"
            );
        }
        other => panic!("expected Item::Flow, got {:?}", other),
    }
}

#[test]
fn flow_parse_states_only() {
    let src = "flow F { state Idle state Active }";
    let file = parse(src);
    assert_eq!(file.items.len(), 1);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.name, "F");
            assert_eq!(f.states.len(), 2);
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_transition_semicolon() {
    let src = "flow F { state A state B transition go(A) -> B; }";
    let file = parse(src);
    assert_eq!(file.items.len(), 1);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.name, "F");
            assert_eq!(f.states.len(), 2);
            assert_eq!(f.transitions.len(), 1);
            assert_eq!(f.transitions[0].name, "go");
            assert_eq!(f.transitions[0].from_state, "A");
            assert_eq!(f.transitions[0].to_states, vec!["B"]);
            assert!(f.transitions[0].body.is_none());
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_empty_block() {
    let src = "flow F { state A state B transition go(A) -> B { } }";
    let file = parse(src);
    assert_eq!(file.items.len(), 1);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.name, "F");
            assert_eq!(f.transitions.len(), 1);
            assert!(f.transitions[0].body.is_some());
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_multiple_transition_targets() {
    let src = r#"
flow Processor {
    state Idle
    state Active { data: f32 }
    state OverloadWarning { data: f32 }

    transition process(Idle, data: f32) -> Active | OverloadWarning {
        do {
            if data > 1.0 {
                return OverloadWarning { data: data }
            } else {
                return Active { data: data }
            }
        }
    }
}
"#;
    let file = parse(src);
    assert_eq!(file.items.len(), 1);
    match &file.items[0] {
        Item::Flow(f) => {
            assert_eq!(f.states.len(), 3);
            assert_eq!(f.transitions.len(), 1);
            assert_eq!(
                f.transitions[0].to_states,
                vec!["Active", "OverloadWarning"]
            );
            assert_eq!(f.transitions[0].params.len(), 1);
            assert_eq!(f.transitions[0].params[0].name, "data");
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

    transition run(Ready) -> Processing {
        do { return Processing { } }
    }
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

#[test]
fn flow_parse_delegate() {
    let src = r#"
flow Parent {
    state Active

    transition run(Active) -> Active {
        do {
            delegate view(self.buffer) to sub_flow;
            return Active { }
        }
    }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            let body = f.transitions[0].body.as_ref().unwrap();
            let do_body = match &body[0] {
                Stmt::Do(b) => b,
                _ => body,
            };
            assert!(matches!(
                do_body[0],
                Stmt::Delegate {
                    kind: DelegateKind::View,
                    ..
                }
            ));
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_delegate_mutate_consume() {
    let src = r#"
flow Parent {
    state Active

    transition run(Active) -> Active {
        do {
            delegate mutate(self.buffer) to sub;
            delegate consume(self.owned) to sub;
            return Active { }
        }
    }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            let body = f.transitions[0].body.as_ref().unwrap();
            let do_body = match &body[0] {
                Stmt::Do(b) => b,
                _ => body,
            };
            assert!(matches!(
                do_body[0],
                Stmt::Delegate {
                    kind: DelegateKind::Mutate,
                    ..
                }
            ));
            assert!(matches!(
                do_body[1],
                Stmt::Delegate {
                    kind: DelegateKind::Consume,
                    ..
                }
            ));
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_pinned_block() {
    let src = r#"
flow SafeFFI {
    state Active { buffer: List<u8> }

    transition process(Active) -> Active {
        do {
            pinned(self.buffer, timeout = 5) |ptr| {
                let _ = ptr;
            }
            return Active { buffer: self.buffer }
        }
    }
}
"#;
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            let body = f.transitions[0].body.as_ref().unwrap();
            let do_body = match &body[0] {
                Stmt::Do(b) => b,
                _ => body,
            };
            assert!(matches!(do_body[0], Stmt::Pinned { .. }));
            if let Stmt::Pinned {
                expr, timeout, var, ..
            } = &do_body[0]
            {
                assert!(timeout.is_some());
                assert_eq!(var.as_deref(), Some("ptr"));
                match expr {
                    Expr::Field(obj, name) => {
                        assert_eq!(name, "buffer");
                        assert!(matches!(obj.as_ref(), Expr::Ident(s) if s == "self"));
                    }
                    _ => panic!("expected self.buffer field access"),
                }
            }
        }
        _ => panic!("expected Item::Flow"),
    }
}

#[test]
fn flow_parse_with_impl_protocol() {
    let src = r#"
flow LidarDriver {
    impl Sensor
    state Idle
    state Active { data: f32 }

    transition start(Idle) -> Active { do { return Active { data: 0.0 } } }
    transition read(Active) -> Active { do { return Active { data: 1.0 } } }
    transition stop(Active) -> Idle { do { return Idle { } } }
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

    transition run(Active) -> Active { do { return Active { request_id: 1 } } }
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
    // Verify all new flow-related keywords are tokenized correctly
    let src = "flow state transition protocol delegate pinned fault reset recover persistent view mutate consume do subflow";
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("lexer failed");
    let expected_all: Vec<(&str, TokenKind)> = vec![
        ("flow", TokenKind::Flow),
        ("state", TokenKind::State),
        ("transition", TokenKind::Transition),
        ("protocol", TokenKind::Protocol),
        ("delegate", TokenKind::Delegate),
        ("pinned", TokenKind::Pinned),
        ("persistent", TokenKind::Persistent),
        ("view", TokenKind::View),
        ("mutate", TokenKind::Mutate),
        ("consume", TokenKind::Consume),
        ("do", TokenKind::Do),
        ("subflow", TokenKind::Subflow),
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
                match &kinds[idx] {
                    TokenKind::Ident(s) => assert_eq!(
                        s, soft,
                        "token[{}]: expected Ident({}), got Ident({})",
                        idx, soft, s
                    ),
                    other => panic!(
                        "token[{}]: expected Ident for soft keyword {}, got {:?}",
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
        do {
            return Active { data: 0 }
        }
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
fn flow_do_block_statement() {
    let src = r#"
flow TestFlow {
    state Ready
    state Done

    transition run(Ready) -> Done {
        do {
            let x = 42
            do {
                let y = x + 1
            }
            return Done { }
        }
    }
}
"#;
    // Verify that `do { }` blocks are parsed correctly (both outer transition do and inner do)
    let file = parse(src);
    match &file.items[0] {
        Item::Flow(f) => {
            let body = f.transitions[0].body.as_ref().unwrap();
            let do_body = match &body[0] {
                Stmt::Do(b) => b,
                _ => body,
            };
            // First stmt is let x = 42
            assert!(matches!(do_body[0], Stmt::Let { .. }));
            // Second stmt is the inner do block
            assert!(matches!(do_body[1], Stmt::Do(_)));
            // Third is return
            assert!(matches!(do_body[2], Stmt::Return(_)));
        }
        _ => panic!("expected Item::Flow"),
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
        do {
            return Active { value: input }
        }
    }
    transition finish(Active) -> Done {
        do {
            return Done { }
        }
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
        do {
            return NonExistent { }
        }
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
        do {
            return Ready { }
        }
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
        do {
            return Ready { }
        }
    }
    transition run(Ready) -> Ready {
        do {
            return Ready { }
        }
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
        do {
            return Value { v: self.v + amount }
        }
    }
}

func main() -> i32 {
    let s = Zero { v: 10 }
    let r = Calc::add(s, 5)
    r.v
}
"#;
    let result = run_source_result(src);
    assert_eq!(result, Ok(interp::Value::Int(15)));
}

#[test]
fn flow_exec_multi_target() {
    let src = r#"
flow Checker {
    state Small { v: i32 }
    state Large { v: i32 }

    transition classify(Small, amount: i32) -> Small | Large {
        do {
            if self.v + amount > 50 {
                return Large { v: self.v + amount }
            } else {
                return Small { v: self.v + amount }
            }
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
    let result = run_source_result(src);
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
    assert!(result.is_err(), "expected error for duplicate state in protocol");
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
    assert!(result.is_err(), "expected error for duplicate transition in protocol");
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
    assert!(result.is_err(), "expected error for undefined from-state in protocol transition");
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
    assert!(result.is_err(), "expected error for undefined target state in protocol transition");
}

#[test]
fn protocol_check_invalid_payload_type() {
    let src = r#"
protocol BadProto {
    state Ready { data: NonExistentType }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error for invalid payload type in protocol state");
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
    transition start(Idle) -> Idle {
        do { return Idle { } }
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error for missing protocol state in flow");
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
    transition start(Idle) -> Active {
        do { return Active { data: 0 } }
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error for missing protocol transition in flow");
}

// ===================== Flow negative tests (edge cases) =====================

#[test]
fn flow_check_wrong_return_target() {
    let src = r#"
flow BadFlow {
    state Ready
    state Active { v: i32 }
    transition go(Ready) -> Active {
        do { return Ready { } }
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error: returning wrong target state");
}

#[test]
fn flow_check_missing_field_in_return() {
    let src = r#"
flow BadFlow {
    state Ready { v: i32 }
    state Active { v: i32 }
    transition go(Ready) -> Active {
        do { return Active { } }
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error: missing required field in return");
}

#[test]
fn flow_check_extra_field_in_return() {
    let src = r#"
flow BadFlow {
    state Ready { v: i32 }
    state Active { v: i32 }
    transition go(Ready) -> Active {
        do { return Active { v: 0, x: 1 } }
    }
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
    transition go(Ready) -> Active {
        do { return Active { v: "hello" } }
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "expected error: wrong field type in return");
}

#[test]
fn flow_check_self_in_no_payload_state() {
    let src = r#"
flow BadFlow {
    state Ready
    state Active { v: i32 }
    transition go(Ready) -> Active {
        do { return Active { v: self.v } }
    }
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
    transition go(Ready, x: NonExistentType) -> Active {
        do { return Active { v: 0 } }
    }
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
    transition go(Ready) -> Active {
        do { return Active { v: self.v } }
    }
}
"#;
    // go(Ready) -> Active, self.v is accessible (Ready has payload), return Active is valid
    let result = check_source(src);
    assert!(result.is_ok(), "returning Active with self.v should be valid");
}

#[test]
fn flow_check_multi_return_type_mismatch() {
    let src = r#"
flow BadFlow {
    state Ready { v: i32 }
    state Active { v: i32 }
    state Done
    transition go(Ready) -> Active | Done {
        do {
            let x = self.v
            return Active { v: x }
        }
    }
}
"#;
    // Only returns Active, not Done — but this is fine since it returns one of the valid targets
    let result = check_source(src);
    assert!(result.is_ok(), "returning one valid target is acceptable in multi-target");
}

#[test]
fn flow_check_no_payload_state_return_no_braces() {
    let src = r#"
flow GoodFlow {
    state Ready
    state Done
    transition finish(Ready) -> Done {
        do { return Done { } }
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "returning no-payload state with braces should be valid");
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

    transition start(Idle) -> Active {
        do { return Active { data: 0 } }
    }
    transition stop(Active) -> Idle {
        do { return Idle { } }
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "valid protocol implementation should pass: {:?}", result.err());
}

// ===================== Pinned block tests =====================

#[test]
fn flow_check_pinned_var_binding() {
    let src = r#"
flow TestFlow {
    state Ready { buf: i32 }
    state Active { result: i32 }

    transition process(Ready) -> Active {
        do {
            pinned(self.buf) |ptr| {
                let _x = ptr
            }
            return Active { result: self.buf + 1 }
        }
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "pinned with var binding should type-check: {:?}", result.err());
}

#[test]
fn flow_check_pinned_timeout_non_int() {
    let src = r#"
flow TestFlow {
    state Ready
    state Active

    transition go(Ready) -> Active {
        do {
            pinned(self, timeout = "hello") |_ptr| {
                return Active { }
            }
        }
    }
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "pinned with non-int timeout should error");
}

#[test]
fn flow_exec_pinned_var_binding() {
    let src = r#"
flow TestFlow {
    state Ready { val: i32 }
    state Active { result: i32 }

    transition process(Ready) -> Active {
        do {
            pinned(self.val) |ptr| {
                let _ = ptr
            }
            return Active { result: self.val + 1 }
        }
    }
}

func main() -> i32 {
    let s = Ready { val: 10 }
    let a = TestFlow::process(s)
    a.result
}
"#;
    let result = run_source_result(src);
    assert_eq!(result, Ok(interp::Value::Int(11)));
}

#[test]
fn flow_exec_chain() {
    let src = r#"
flow Counter {
    state Zero { count: i32 }
    state Active { count: i32 }
    state Done

    transition inc(Zero, amount: i32) -> Active {
        do {
            return Active { count: self.count + amount }
        }
    }
    transition finish(Active) -> Done {
        do {
            return Done { }
        }
    }
}

func main() -> i32 {
    let s = Zero { count: 0 }
    let a = Counter::inc(s, 7)
    let _d = Counter::finish(a)
    42
}
"#;
    let result = run_source_result(src);
    assert_eq!(result, Ok(interp::Value::Int(42)));
}
