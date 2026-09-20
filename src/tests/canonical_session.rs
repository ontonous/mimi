//! Canonical session-channel differential tests (R6-1046).
//!
//! Session programs are not yet in the migrated default-route profile (the
//! route layer keeps them on the compatibility route with
//! `outside-migrated-profile`), but the MIR-level consumers already execute
//! the integer-payload session face.  These pins lock the three-consumer
//! differential (reference interpreter, AST-free bytecode VM, native emitter
//! on one shared `MirProgram`) so a later default-route migration starts from
//! proven equivalence instead of assumption.  The faces outside the profile —
//! non-integer session payloads (checker E0444), actor handles without
//! canonical glue (structural validation), and assign statements outside MIR
//! Phase 0 — are pinned as fail-closed rejections with their owning layer
//! named.

use super::*;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn materialize_session(source: &str, label: &str) -> MirProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|diags| panic!("check {label}: {diags:?}"));
    MirProgram::from_checked_program(&checked)
        .unwrap_or_else(|error| panic!("materialize {label}: {error:?}"))
}

// Generative roundtrip matrix: a client sends A over the lo endpoint, the
// server receives and prints it, sends A+k back, the client receives and
// prints the reply, and both endpoints close.  The expected stdout is
// computed at generation time from the case parameters (never by executing a
// backend), and every case shares one `MirProgram` across the three
// consumers.
#[test]
fn session_scalar_roundtrip_matrix_agrees_across_consumers() {
    struct SessionRoundtripCase {
        name: &'static str,
        payload_ty: &'static str,
        send_a: &'static str,
        k_text: &'static str,
        expected_first: i64,
        expected_second: i64,
    }
    const CASES: &[SessionRoundtripCase] = &[
        SessionRoundtripCase {
            name: "i32_anchor",
            payload_ty: "i32",
            send_a: "10",
            k_text: "1",
            expected_first: 10,
            expected_second: 11,
        },
        SessionRoundtripCase {
            name: "i32_zero",
            payload_ty: "i32",
            send_a: "0",
            k_text: "0",
            expected_first: 0,
            expected_second: 0,
        },
        SessionRoundtripCase {
            name: "i32_negative",
            payload_ty: "i32",
            send_a: "-7",
            k_text: "3",
            expected_first: -7,
            expected_second: -4,
        },
        SessionRoundtripCase {
            name: "i64_wide",
            payload_ty: "i64",
            send_a: "7 as i64",
            k_text: "5 as i64",
            expected_first: 7,
            expected_second: 12,
        },
        SessionRoundtripCase {
            name: "i64_large",
            payload_ty: "i64",
            send_a: "3037000499 as i64",
            k_text: "1 as i64",
            expected_first: 3037000499,
            expected_second: 3037000500,
        },
    ];
    for case in CASES {
        let source = format!(
            r#"
session Proto = !{ty} . ?{ty} . end

func main() -> i64 {{
    let (tx, rx) = session_pair::<Proto>()
    session_send(tx, {a})
    let first = session_recv(rx)
    session_send(rx, first + {k})
    let second = session_recv(tx)
    session_close(rx)
    session_close(tx)
    println(first)
    println(second)
    0
}}
"#,
            ty = case.payload_ty,
            a = case.send_a,
            k = case.k_text,
        );
        let label = format!("session roundtrip case {}", case.name);
        let mir = materialize_session(&source, &label);
        assert!(
            crate::verifier::validate_mir_capabilities(&mir).is_ok(),
            "{label} must pass the capability gate"
        );
        let digest = mir.canonical_digest();

        let expected_stdout = format!("{}\n{}\n", case.expected_first, case.expected_second);
        let reference = MirReferenceInterpreter::new(&mir)
            .execute_with_output(&NodeId("function:main".into()), &[])
            .unwrap_or_else(|error| panic!("{label} reference execution failed: {error}"));
        assert_eq!(reference.output, expected_stdout, "{label} reference");

        let bytecode = compile_mir_program(&mir)
            .unwrap_or_else(|error| panic!("{label} bytecode compilation failed: {error:?}"));
        assert!(bytecode.ast.is_none(), "{label} bytecode must be AST-free");
        let mut vm = BytecodeVM::new(bytecode);
        assert!(vm.run_value().is_ok(), "{label} bytecode runs");
        assert_eq!(vm.stdout(), expected_stdout, "{label} bytecode");

        if !can_link() {
            continue;
        }
        let context = inkwell::context::Context::create();
        let mut generator =
            crate::codegen::CodeGenerator::new(&context, &format!("mir_session_{}", case.name));
        generator
            .compile_mir_native(&mir)
            .unwrap_or_else(|error| panic!("{label} native emission failed: {error:?}"));
        generator
            .module
            .verify()
            .unwrap_or_else(|error| panic!("{label} native module verifies: {error}"));
        let native = link_and_observe_canonical_mir(&generator)
            .unwrap_or_else(|error| panic!("{label} native execution failed: {error}"));
        assert_eq!(native.exit_code, Some(0), "{label} native exit");
        assert_eq!(native.stdout, expected_stdout, "{label} native stdout");
        assert_eq!(native.stderr, "", "{label} native stderr");
        assert_eq!(mir.canonical_digest(), digest, "{label} digest stability");
    }
}

// Session protocol payloads are checker-constrained to integer scalars: the
// endpoint runtime transports values in i64 handle slots, so a float payload
// must be rejected before MIR exists (E0444), not trapped or coerced later.
#[test]
fn session_float_payload_is_rejected_at_the_checker_with_e0444() {
    let source = r#"
        session FloatProto = !f64 . end

        func main() -> i64 {
            let (tx, rx) = session_pair::<FloatProto>()
            session_send(tx, 2.5)
            let v = session_recv(rx)
            session_close(tx)
            session_close(rx)
            println(v)
            0
        }
    "#;
    let file = parse_prod(source);
    let diagnostics = crate::core::check_program(&file)
        .expect_err("checker must reject float session payloads with E0444");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0444")
                && diagnostic
                    .message
                    .contains("protocol payloads must be integer scalars")),
        "the rejection must carry E0444 naming the payload contract: {diagnostics:?}"
    );
}

// The checker enforces session protocol order: a one-shot `!i32 . end`
// protocol is ENDED after the single send, so a second operation on the same
// endpoint is an E0414 order violation (AlreadyEnded) — the linear session
// state machine, not a runtime surprise.
#[test]
fn session_protocol_order_violation_is_rejected_at_the_checker_with_e0414() {
    let source = r#"
        session OneShot = !i32 . end

        func main() -> i64 {
            let (tx, rx) = session_pair::<OneShot>()
            session_send(tx, 10)
            let v = session_recv(tx)
            session_close(tx)
            session_close(rx)
            println(v)
            0
        }
    "#;
    let file = parse_prod(source);
    let diagnostics = crate::core::check_program(&file)
        .expect_err("checker must reject use-after-end on a session endpoint");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0414")
                && diagnostic.message.contains("AlreadyEnded")),
        "the rejection must carry E0414 naming the ended endpoint: {diagnostics:?}"
    );
}

// Actor handles are outside the canonical ownership contract: lowering
// succeeds, but the structural validator rejects every local and call shape
// touching the handle type because it has no canonical MoveOut/Clone/Drop
// glue.  The rejection must stay fail-closed — never a silent legacy
// fallback or a partial canonical emission.
#[test]
fn actor_handle_without_canonical_glue_fails_closed_at_materialization() {
    let source = r#"
        actor Scale {
            factor: i64 = 3 as i64

            func scale(v: i64) -> i64 { v * 3 as i64 }
        }

        func main() -> i64 {
            let s = Scale.spawn()
            let out = s.scale(14 as i64)
            println(out)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|diags| panic!("check scalar actor: {diags:?}"));
    let error = MirProgram::from_checked_program(&checked)
        .expect_err("actor handles must fail canonical materialization");
    let messages = format!("{error:?}");
    assert!(
        messages.contains("no canonical MoveOut glue"),
        "the rejection must name the missing canonical glue: {messages}"
    );
}

// MIR Phase 0 does not lower assign statements yet — even at function-body
// top level, outside any structured control flow.  The boundary must stay an
// explicit lowering rejection (the compatibility route keeps such programs
// working), never a silently dropped assignment.
#[test]
fn assign_statement_boundary_stays_fail_closed_in_mir_phase_0() {
    let source = r#"
        func reassign() -> i64 {
            let mut total = 40 as i64
            total = total + 2 as i64
            total
        }

        func main() -> i64 {
            let out = reassign()
            println(out)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|diags| panic!("check reassign: {diags:?}"));
    let error = MirProgram::from_checked_program(&checked)
        .expect_err("assign statements must fail MIR Phase 0 lowering explicitly");
    let messages = format!("{error:?}");
    assert!(
        messages.contains("structured control flow is not lowered by MIR Phase 0"),
        "the rejection must name the Phase 0 statement boundary: {messages}"
    );
}
