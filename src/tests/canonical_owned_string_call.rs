//! R6-1076: the owned-String constant callable admission face.
//!
//! Before this slice, a program whose only String shapes were a constant
//! callable (`func greet() -> string { "hi" }`) and a call-result bind of it
//! stayed on the compatibility route: the checker-side admission scanner
//! floored the callee's String signature result, its root block, the bind
//! pattern and the call expression's own type, and the island gate rejected
//! the StringHandle values the materialized graph carries outside any string
//! print face.  R6-1074 had already opened the verifier half (the capability
//! gate mirrors `eval_direct_owned_string_call`'s routing), so the default
//! verify route proved these contracts through the dual-engine floor.
//!
//! R6-1076 closes the admission half with a shape-derived face: a concrete,
//! effect-free, non-prelude callable whose whole body is a string-literal
//! result joins the owned-String constant set.  Its String result, root
//! block, call-result binds and call expressions skip the profile floor, the
//! construction ledger (`validate_owned_string_return_shape` via the new
//! exactly-one-instruction constant-return candidate) re-proves the
//! materialized `Const "…" → Return` graph, and the island gate's String
//! value/constant arms are now the canonical StringHandle contract itself
//! (per-instruction Move/Clone/Drop/PrintlnString/Call arms keep policing
//! every use).  Off-shape callees — one-edge wrappers, multi-statement
//! bodies, String parameters, concatenation, branches, second-hand rebinds
//! outside a string print face — keep the compatibility floor.

use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn checked_program_of(source: &str) -> crate::core::CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    crate::core::check_program(&file).expect("check")
}

fn assert_admitted(source: &str, label: &str) -> MirProgram {
    let checked = checked_program_of(source);
    assert_eq!(
        crate::core::mir::classify_scalar_collection_admission(&checked),
        crate::core::mir::ScalarCollectionAdmission::CompleteCoverage,
        "{label} must classify complete"
    );
    let mir = MirProgram::from_checked_program(&checked)
        .unwrap_or_else(|error| panic!("{label} materialize: {error:?}"));
    crate::core::mir::validate_scalar_collection_island(&mir)
        .unwrap_or_else(|errors| panic!("{label} island gate: {errors:?}"));
    mir
}

#[test]
fn owned_string_constant_call_bind_and_print_agrees_across_consumers() {
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            let s = greet()
            println(s)
            0
        }
    "#;
    let label = "owned-string constant call bind and print";
    let mir = assert_admitted(source, label);
    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference: {error:?}"));
    assert_eq!(
        reference,
        MirRuntimeValue::Int(0),
        "{label} reference result"
    );
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn owned_string_constant_call_with_integer_stdout_routes_complete() {
    // The R6-1074 CLI contract shape without the extern declaration: the
    // dead call-result bind plus an integer stdout receipt is a complete
    // scalar-island program, and the ensures contract verifies through the
    // canonical route (no dual-engine E0439 floor).
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            ensures: result == 1
            let s = greet()
            println(7)
            1
        }
    "#;
    let label = "owned-string constant call integer stdout";
    let mir = assert_admitted(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "owned-string-call-int-stdout".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify, got {:?}",
        results[0].status
    );
}

#[test]
fn owned_string_constant_call_prints_through_call_argument() {
    // The call result in print-argument position joins the same face: the
    // visit_expr call-result exemption admits the call node and the string
    // print arm polices the PrintlnString contract.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            println(greet())
            0
        }
    "#;
    let label = "owned-string constant call print argument";
    let mir = assert_admitted(source, label);
    let bytecode =
        compile_mir_program(&mir).unwrap_or_else(|error| panic!("{label} bytecode: {error:?}"));
    BytecodeVM::new(bytecode)
        .run()
        .unwrap_or_else(|error| panic!("{label} vm: {error:?}"));
}

#[test]
fn owned_string_constant_callable_is_construction_validated() {
    // The materialized `Const "…" → Return` graph of a constant callable is
    // exactly the new candidate class of the construction ledger: the
    // one-block liveness proof runs on it at construction time.
    let source = r#"
        func greet() -> string {
            "hi"
        }
        func main() -> i32 {
            let s = greet()
            println(7)
            0
        }
    "#;
    let label = "owned-string constant callable construction ledger";
    let mir = assert_admitted(source, label);
    let catalog = mir.type_catalog();
    let greet = mir
        .functions()
        .get(&NodeId("function:greet".into()))
        .unwrap_or_else(|| panic!("{label} greet absent"));
    assert!(
        crate::core::mir::is_owned_string_return_candidate(greet, catalog),
        "{label} greet must be an owned-String return candidate"
    );
    crate::core::mir::validate_owned_string_return_shape(greet, catalog)
        .unwrap_or_else(|message| panic!("{label} greet ledger: {message}"));
}

// The narrowest floors this slice keeps: every callee below returns a
// String but its provenance escapes the closed constant face, so the whole
// program stays on the compatibility route.
#[test]
fn off_shape_string_callables_keep_the_compatibility_floor() {
    struct OffShapeCase {
        name: &'static str,
        source: &'static str,
    }
    const CASES: &[OffShapeCase] = &[
        // A one-edge wrapper's String result has no constant face of its
        // own — the call chain would need its own transitive closure proof.
        OffShapeCase {
            name: "one_edge_wrapper",
            source: r#"
                func greet() -> string {
                    "hi"
                }
                func wrap() -> string {
                    greet()
                }
                func main() -> i32 {
                    let s = wrap()
                    println(7)
                    0
                }
            "#,
        },
        // A multi-statement body materializes Move/Clone/Drop glue the
        // checker-side shape predicate does not cover.
        OffShapeCase {
            name: "multi_statement_body",
            source: r#"
                func greet() -> string {
                    let a = "x"
                    a
                }
                func main() -> i32 {
                    let s = greet()
                    println(7)
                    0
                }
            "#,
        },
        // A String parameter keeps the parameter profile floor.
        OffShapeCase {
            name: "string_parameter",
            source: r#"
                func greet(n: string) -> string {
                    "hi"
                }
                func main() -> i32 {
                    let s = greet("x")
                    println(7)
                    0
                }
            "#,
        },
        // Concatenation is an unmodeled String operation.
        OffShapeCase {
            name: "concat_body",
            source: r#"
                func greet() -> string {
                    "a" + "b"
                }
                func main() -> i32 {
                    let s = greet()
                    println(7)
                    0
                }
            "#,
        },
        // A branchy String return floors through its nested blocks.
        OffShapeCase {
            name: "branchy_body",
            source: r#"
                func greet(c: bool) -> string {
                    if c {
                        "a"
                    } else {
                        "b"
                    }
                }
                func main() -> i32 {
                    let s = greet(true)
                    println(7)
                    0
                }
            "#,
        },
        // A second-hand String rebind outside a string print face has no
        // admitted provenance.
        OffShapeCase {
            name: "second_hand_rebind",
            source: r#"
                func greet() -> string {
                    "hi"
                }
                func main() -> i32 {
                    let s = greet()
                    let t = s
                    println(7)
                    0
                }
            "#,
        },
    ];
    for case in CASES {
        let checked = checked_program_of(case.source);
        assert!(
            matches!(
                crate::core::mir::classify_scalar_collection_admission(&checked),
                crate::core::mir::ScalarCollectionAdmission::MixedCoverage
            ),
            "{} must classify mixed",
            case.name
        );
    }
}
