//! Canonical owned-String bind differential tests (R6-1055; R6-1060
//! widens the face with second-hand bind roots).
//!
//! A String literal bound to a local and read by `println` lowers as
//! `Const(String)` → `Move` into the slot → `Clone` slot read →
//! `PrintlnString` — the same shape the float bind face (R6-1054) opens for
//! f64, with the owned StringHandle ABI in place of the Copy float ABI.  The
//! face opens only inside a function that carries the string print contract
//! — the same per-function shape the island gate re-proves on the
//! materialized graph — and the differential matrix pins three-consumer
//! equivalence (reference interpreter, AST-free bytecode VM, native emitter
//! on one shared `MirProgram`).  R6-1060 admits a second-hand bind root
//! (`let t = s` with a plain local read as initializer): inside a concrete
//! island an owned-String local can only originate in an admitted literal
//! bind, so the read adds no unclassified provenance.  Call-result binds,
//! String assigns, and dead binds in non-printing functions keep their
//! explicit mixed floors.

use super::*;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn materialize_string_bind(source: &str, label: &str) -> MirProgram {
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

// Generative owned-String bind matrix: every case binds at least one String
// local from a literal and prints it; the combo and split-function cases
// cover shape parity with the integer print face and the per-function
// scoping of the print contract.  Expected stdout comes from the case table,
// and every case shares one `MirProgram` across the three consumers.
#[test]
fn string_bind_matrix_agrees_across_consumers() {
    struct StringBindCase {
        name: &'static str,
        source: &'static str,
        expected_stdout: &'static str,
    }
    const CASES: &[StringBindCase] = &[
        StringBindCase {
            name: "string_literal_bind_print",
            source: r#"
                func main() -> i32 {
                    let s = "hi"
                    println(s)
                    0
                }
            "#,
            expected_stdout: "hi\n",
        },
        // The print face consumes a fresh clone of the owned slot, so
        // printing the same local twice is legal.
        StringBindCase {
            name: "string_bind_double_print",
            source: r#"
                func main() -> i32 {
                    let s = "hi"
                    println(s)
                    println(s)
                    0
                }
            "#,
            expected_stdout: "hi\nhi\n",
        },
        StringBindCase {
            name: "string_bind_two_locals",
            source: r#"
                func main() -> i32 {
                    let s = "hi"
                    let t = "yo"
                    println(s)
                    println(t)
                    0
                }
            "#,
            expected_stdout: "hi\nyo\n",
        },
        // An empty StringHandle still prints its terminating newline on
        // every consumer.
        StringBindCase {
            name: "string_bind_empty_literal",
            source: r#"
                func main() -> i32 {
                    let s = ""
                    println(s)
                    0
                }
            "#,
            expected_stdout: "\n",
        },
        StringBindCase {
            name: "string_bind_integer_print_combo",
            source: r#"
                func main() -> i32 {
                    let s = "hi"
                    println(7)
                    println(s)
                    0
                }
            "#,
            expected_stdout: "7\nhi\n",
        },
        // R6-1060: a second-hand bind root (`let t = s`) joins the same
        // face under the same per-function contract — the read of a
        // transitively literal-origin owned slot adds no unclassified
        // provenance.  Rebind chains stay admitted for the same reason.
        StringBindCase {
            name: "string_rebind_second_hand",
            source: r#"
                func main() -> i32 {
                    let s = "hi"
                    let t = s
                    let u = t
                    println(u)
                    0
                }
            "#,
            expected_stdout: "hi\n",
        },
        // The print contract is per-function: the helper owns its bind and
        // its println, so the caller stays a plain integer graph.
        StringBindCase {
            name: "string_bind_split_function",
            source: r#"
                func shout() -> i32 {
                    let s = "remote"
                    println(s)
                    0
                }
                func main() -> i32 {
                    println(shout())
                    0
                }
            "#,
            expected_stdout: "remote\n0\n",
        },
    ];
    for case in CASES {
        let label = format!("string bind case {}", case.name);
        let checked = checked_program_of(case.source);
        assert!(
            matches!(
                crate::core::mir::classify_scalar_collection_admission(&checked),
                crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
            ),
            "{label} must classify complete"
        );
        let mir = materialize_string_bind(case.source, &label);
        crate::core::mir::validate_scalar_collection_island(&mir)
            .unwrap_or_else(|errors| panic!("{label} island gate: {errors:?}"));
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
            crate::codegen::CodeGenerator::new(&context, &format!("mir_stringbind_{}", case.name));
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

// The bind face graph carries no String operation beyond the owned print
// receipt, so the MIR verifier — which admits `PrintlnString` through the
// checker-owned TypeDesc owned-String validation — still proves the contract
// obligations end to end.
#[test]
fn string_bind_ensures_contract_verifies_on_mir() {
    let source = r#"
        func main() -> i32 {
            ensures: result == 0
            let s = "hi"
            println(s)
            0
        }
    "#;
    let label = "string bind ensures";
    let mir = materialize_string_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "string-bind-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// R6-1060: the second-hand bind graph (Const → Move → Clone read → Move
// bind → Clone read → PrintlnString) proves the contract obligations on
// the MIR path too — the same proof the default `mimi verify` entry
// reaches once the face classifies complete.
#[test]
fn string_second_hand_bind_ensures_verifies_on_mir() {
    let source = r#"
        func main() -> i32 {
            ensures: result == 0
            let s = "hi"
            let t = s
            println(t)
            0
        }
    "#;
    let label = "string second-hand bind ensures";
    let mir = materialize_string_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "string-second-hand-bind-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// The bind face is print-face-scoped: call-result binds, String assigns,
// and dead binds in non-printing functions all keep the graph on the
// explicit mixed compatibility route.  (R6-1060 restated the former
// second-hand bind mixed case: it migrated into the matrix above.)
#[test]
fn string_bind_faces_stay_mixed() {
    struct MixedCase {
        name: &'static str,
        source: &'static str,
    }
    const CASES: &[MixedCase] = &[
        // A call-result root is never a print-face literal or a plain
        // local read of one, so the bind pattern floors on its String
        // type.
        MixedCase {
            name: "string_bind_from_call",
            source: r#"
                func label(flag: bool) -> string {
                    if flag { "yes" } else { "no" }
                }
                func main() -> i32 {
                    let s = label(true)
                    println(s)
                    0
                }
            "#,
        },
        // String assign targets are outside the MIR Phase 0 scalar-assign
        // face; the classifier floors the graph so the canonical route never
        // lures a construction failure.
        MixedCase {
            name: "string_assign",
            source: r#"
                func main() -> i32 {
                    let mut s = "a"
                    s = "b"
                    println(s)
                    0
                }
            "#,
        },
        // The per-function flag parity: a String bind in a function without
        // a String println keeps that function mixed, so the island gate's
        // per-function envelope can never be wider than the classification.
        MixedCase {
            name: "dead_string_bind_without_print",
            source: r#"
                func helper() -> i32 {
                    let s = "dead"
                    0
                }
                func main() -> i32 {
                    println(helper())
                    0
                }
            "#,
        },
        // Cross-face parity with R6-1054: a String bind inside a
        // float-printing function is not covered by the float print
        // contract — each face opens only under its own contract.
        MixedCase {
            name: "string_bind_in_float_print_function",
            source: r#"
                func main() -> i32 {
                    let s = "hi"
                    println(0.5)
                    0
                }
            "#,
        },
    ];
    for case in CASES {
        let label = format!("string bind mixed case {}", case.name);
        let checked = checked_program_of(case.source);
        assert!(
            matches!(
                crate::core::mir::classify_scalar_collection_admission(&checked),
                crate::core::mir::ScalarCollectionAdmission::MixedCoverage
            ),
            "{label} must classify mixed"
        );
    }
}
