//! Canonical string-print differential tests (R6-1050).
//!
//! `println` over a StringHandle lowers as `PrintlnString`.  The owned
//! direct print consumes a fresh clone of the source (the lowerer
//! materializes the Clone, so printing the same local twice is legal and
//! matches the legacy semantics), and the borrowed record-field receipt is
//! the non-consuming observation shape.  The differential matrix pins
//! three-consumer equivalence (reference interpreter, AST-free bytecode VM,
//! native emitter on one shared `MirProgram`) so retiring the historical
//! recoverable-Flow-only stdout gate has a proof to point at.  The
//! Option<string> unwrap face stays fail-closed at the native flat Copy
//! variant contract and is pinned at that boundary, not silently dropped.

use super::*;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn materialize_string_print(source: &str, label: &str) -> MirProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|diags| panic!("check {label}: {diags:?}"));
    MirProgram::from_checked_program(&checked)
        .unwrap_or_else(|error| panic!("materialize {label}: {error:?}"))
}

fn checked_program_of(source: &str) -> crate::core::CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    crate::core::check_program(&file).expect("check")
}

// Generative string-print matrix: every case prints at least one owned
// string (literal, local, call result) plus one scalar for shape mixing; the
// record case exercises the borrowed String-field receipt (`n.label`).  The
// expected stdout is computed from the case table, and every case shares one
// `MirProgram` across the three consumers.
#[test]
fn string_print_matrix_agrees_across_consumers() {
    struct StringPrintCase {
        name: &'static str,
        source: &'static str,
        expected_stdout: &'static str,
    }
    const CASES: &[StringPrintCase] = &[
        StringPrintCase {
            name: "string_literal_print",
            source: r#"
                func main() -> i32 {
                    println("hello canonical")
                    0
                }
            "#,
            expected_stdout: "hello canonical\n",
        },
        // The owned print consumes a fresh clone; the source local must stay
        // live for the second println.
        StringPrintCase {
            name: "string_local_double_print",
            source: r#"
                func main() -> i32 {
                    let s = "twice"
                    println(s)
                    println(s)
                    0
                }
            "#,
            expected_stdout: "twice\ntwice\n",
        },
        StringPrintCase {
            name: "string_return_print",
            source: r#"
                func label(flag: bool) -> string {
                    if flag { "yes" } else { "no" }
                }
                func main() -> i32 {
                    let s = label(true)
                    println(s)
                    println(7)
                    0
                }
            "#,
            expected_stdout: "yes\n7\n",
        },
        // The borrowed record-field receipt (`n.label`) is the
        // non-consuming observation shape and must not move the record.
        StringPrintCase {
            name: "record_string_field_borrow_print",
            source: r#"
                type Named { label: string, size: i64 }
                func main() -> i32 {
                    let n = Named { label: "point", size: 4 }
                    println(n.label)
                    println(n.size)
                    0
                }
            "#,
            expected_stdout: "point\n4\n",
        },
        // The collection-island combination (R6-1050 route face): a List
        // operation materializes the copy-scalar-collection candidate while
        // an owned string literal prints in the same function.  This is the
        // graph shape the S11 capability gate now admits alongside the
        // checker-level print admission.
        StringPrintCase {
            name: "collection_graph_string_print",
            source: r#"
                func main() -> i32 {
                    let mut total = 0
                    total = 5
                    let xs = [10, 20, 30]
                    println(xs.len())
                    println(total)
                    println("tag")
                    0
                }
            "#,
            expected_stdout: "3\n5\ntag\n",
        },
        // The record-island combination: a flat Copy record construction in
        // the same function as an owned string literal print.
        StringPrintCase {
            name: "record_graph_owned_string_print",
            source: r#"
                type Point { x: i64, y: i64 }
                func make_point(a: i64, b: i64) -> Point { Point { x: a, y: b } }
                func main() -> i32 {
                    let flag = true
                    let p = make_point(3, 4)
                    println(p.x)
                    println(flag)
                    println("tag")
                    println(p.y)
                    0
                }
            "#,
            expected_stdout: "3\ntrue\ntag\n4\n",
        },
    ];
    for case in CASES {
        let label = format!("string print case {}", case.name);
        let mir = materialize_string_print(case.source, &label);
        assert!(
            crate::verifier::validate_mir_capabilities(&mir).is_ok(),
            "{label} must pass the capability gate"
        );
        let digest = mir.canonical_digest();

        let reference = MirReferenceInterpreter::new(&mir)
            .execute_with_output(&NodeId("function:main".into()), &[])
            .unwrap_or_else(|error| panic!("{label} reference execution failed: {error}"));
        assert_eq!(reference.output, case.expected_stdout, "{label} reference");

        let bytecode = compile_mir_program(&mir)
            .unwrap_or_else(|error| panic!("{label} bytecode compilation failed: {error:?}"));
        assert!(bytecode.ast.is_none(), "{label} bytecode must be AST-free");
        let mut vm = BytecodeVM::new(bytecode);
        assert!(vm.run_value().is_ok(), "{label} bytecode runs");
        assert_eq!(vm.stdout(), case.expected_stdout, "{label} bytecode");

        if !can_link() {
            continue;
        }
        let context = inkwell::context::Context::create();
        let mut generator =
            crate::codegen::CodeGenerator::new(&context, &format!("mir_strprint_{}", case.name));
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
        assert_eq!(native.stdout, case.expected_stdout, "{label} native stdout");
        assert_eq!(native.stderr, "", "{label} native stderr");
        assert_eq!(mir.canonical_digest(), digest, "{label} digest stability");
    }
}

// The Option<string> unwrap face (the R6-1049 boundary fixture) stays on the
// explicit boundary after the string-print gate retired: the unwrap-style
// consumption is not the option island's switch candidacy, so the program
// keeps the compatibility route, and the `--mir` opt-in must keep failing
// closed at the Copy variant capability (verifier Switch gate, native flat
// Copy variant contract) rather than entering an unproven non-Copy payload
// ABI.  This pin documents the boundary; widening the variant contract to
// StringHandle payloads is its own differential slice, not a silent
// pass-through here.
#[test]
fn option_string_unwrap_stays_outside_profile_and_fail_closed() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mir_option_string_assign_face.mimi"),
    )
    .expect("fixture");
    let label = "option string unwrap boundary";
    let mir = materialize_string_print(&source, label);
    let capability = crate::verifier::validate_mir_capabilities(&mir)
        .expect_err("the Option<string> payload must stay outside the Copy variant capability");
    assert!(
        capability
            .iter()
            .any(|message| message.contains("Copy variant")),
        "the capability failure must name the Copy variant boundary: {capability:?}"
    );

    // Route disposition: no Option<string> switch candidacy exists (the only
    // match scrutinee is bool), so the option island stays OutsideProfile and
    // the default entry keeps the compatibility route instead of a mixed
    // rejection.
    let checked = checked_program_of(&source);
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("the fixture must still materialize its canonical route graph");
    assert!(
        !crate::core::mir::CanonicalMirRouteProfile::NonCopyOptionStringVariant
            .is_admitted(route.admission),
        "unwrap-style Option<string> consumption must stay OutsideProfile: {:?}",
        route.admission
    );
}
