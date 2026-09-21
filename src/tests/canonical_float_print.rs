//! Canonical float-print differential tests (R6-1053).
//!
//! `println` over an f64 literal lowers as `PrintlnFloat`.  The output text
//! is produced by the one shared shortest round-trip formatter: the reference
//! executor uses Rust's `Display` on `f64`, and the native emitter calls the
//! `mimi_to_string_f64` runtime helper whose body is the same `Display` call,
//! so reference and native stdout are byte-identical by construction.  The
//! differential matrix pins three-consumer equivalence (reference
//! interpreter, AST-free bytecode VM, native emitter on one shared
//! `MirProgram`) plus the Z3 symbolic contract path.  The face was
//! deliberately literal-only at R6-1053; R6-1054/R6-1061 migrated float
//! bindings and Add/Subtract arithmetic to `canonical_float_bind.rs`, and
//! what stays mixed here (multiply) is pinned by the closing test.

use super::*;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn materialize_float_print(source: &str, label: &str) -> MirProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|diags| panic!("check {label}: {diags:?}"));
    MirProgram::from_checked_program(&checked)
        .unwrap_or_else(|error| panic!("materialize {label}: {error:?}"))
}

// Generative float-print matrix: every case prints at least one f64 literal
// beside scalar stdout shapes, the integral-display case pins the exact
// Rust `Display` contract (no `.0` suffix), and the collection case is the
// R6-1053 route face (a Set operation beside the float print).  Expected
// stdout comes from the case table; every case shares one `MirProgram`
// across the three consumers.
#[test]
fn float_print_matrix_agrees_across_consumers() {
    struct FloatPrintCase {
        name: &'static str,
        source: &'static str,
        expected_stdout: &'static str,
    }
    const CASES: &[FloatPrintCase] = &[
        FloatPrintCase {
            name: "float_literal_print",
            source: r#"
                func main() -> i32 {
                    println(0.5)
                    0
                }
            "#,
            expected_stdout: "0.5\n",
        },
        FloatPrintCase {
            name: "float_integral_display",
            source: r#"
                func main() -> i32 {
                    println(2.0)
                    0
                }
            "#,
            expected_stdout: "2\n",
        },
        FloatPrintCase {
            name: "float_negative_and_precision",
            source: r#"
                func main() -> i32 {
                    println(-0.25)
                    println(3.14159)
                    println(7)
                    0
                }
            "#,
            expected_stdout: "-0.25\n3.14159\n7\n",
        },
        // The collection-island combination (R6-1053 route face): a Set
        // operation materializes the copy-scalar-collection candidate while
        // an f64 literal prints in the same function.
        FloatPrintCase {
            name: "collection_graph_float_print",
            source: r#"
                func main() -> i32 {
                    let values: Set<i32> = {4, 1}
                    println(contains(values, 1))
                    println(0.5)
                    drop(values)
                    0
                }
            "#,
            expected_stdout: "true\n0.5\n",
        },
    ];
    for case in CASES {
        let label = format!("float print case {}", case.name);
        let mir = materialize_float_print(case.source, &label);
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
            crate::codegen::CodeGenerator::new(&context, &format!("mir_fprint_{}", case.name));
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

// The Z3 symbolic path treats the float print as an opaque side effect whose
// argument must satisfy the checker-owned Copy f64 ABI; the ensures contract
// over the integer result must still verify through the graph.
#[test]
fn float_print_ensures_contract_verifies_on_mir() {
    let source = r#"
        func main() -> i32 {
            ensures: result == 0
            println(0.5)
            0
        }
    "#;
    let label = "float print ensures";
    let mir = materialize_float_print(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "float-print-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// R6-1054 restatement: the float BIND face this pin used to hold closed now
// routes canonical (see `canonical_float_bind.rs`).  R6-1061 restatement:
// float ADD/SUBTRACT joined it too.  R6-1067 restatement: the island
// triangle (reference executor, bytecode VM, native emitter, verifier IEEE
// domain) grew Multiply/Divide, and the surface cannot even express float
// remainder (E0202).  R6-1071 restatement: arithmetic over a call-sourced
// float whose callee sits on the one-edge print closure routes canonical
// too — the verifier symbolically executes the callee body, so the binding
// is exactly modeled.  The mixed floor that stays here is arithmetic over a
// TWO-EDGE call-sourced float: `mid` sits on main's print closure, but its
// own body calls `inner`, which the closure does not reach — mid's root
// call-result floors, and the composition stays mixed.
#[test]
fn float_binding_and_arithmetic_stay_mixed() {
    let source = r#"
        func inner() -> f64 {
            0.5
        }
        func mid() -> f64 {
            inner()
        }
        func main() -> i32 {
            let x = mid()
            println(x * 2.0)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&checked),
        crate::core::mir::ScalarCollectionAdmission::MixedCoverage,
        "arithmetic over a two-edge call-sourced float must stay on the mixed compatibility route"
    );
}
