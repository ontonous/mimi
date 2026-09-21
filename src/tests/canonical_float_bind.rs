//! Canonical float-bind differential tests (R6-1054; R6-1057 widens the
//! face with float assigns; R6-1059 widens it with second-hand assign
//! roots; R6-1060 widens it with second-hand bind roots; R6-1061 widens it
//! with finite-only f64 Add/Subtract arithmetic).
//!
//! A float literal bound to a local and read by `println` lowers as
//! `Const(FloatBits)` → `Move` into the slot → `Clone` slot read →
//! `PrintlnFloat`.  The face opens only inside a function that carries the
//! float print contract — the same per-function shape the island gate
//! re-proves on the materialized graph — and the differential matrix pins
//! three-consumer equivalence (reference interpreter, AST-free bytecode VM,
//! native emitter on one shared `MirProgram`).  R6-1057 admits float
//! reassignment into the same face (Copy f64 targets need no drop glue, so
//! the Move replacement cannot leak).  R6-1059 admits a second-hand assign
//! root (`x = y` with a plain local read as RHS) and R6-1060 the mirror
//! second-hand bind root (`let y = x`).  R6-1061 admits Add/Subtract over
//! float-symbolic roots: the verifier models the operation in an IEEE
//! symbolic Float domain (round-nearest-ties-even, the exact `fadd`/`fsub`
//! semantics) with an E0813 finiteness definedness obligation mirroring the
//! runtime operand/result traps, the classification tracks float-symbolic
//! locals with branch-generation stamps, and the island gate's binary
//! matrix admits f64 Add/Subtract beside its integer rows.  Call-result
//! binds, Multiply/Divide, arithmetic on opaque-widen locals and uses that
//! cross a branch boundary keep their explicit mixed floors until their
//! contracts are independently materialized.
//! R6-1062 admits the integer-literal widen assign (`x = 2` into an F64
//! target): its `assign_numeric_convert` sources the literal const
//! directly, so the verifier widens the known constant exactly and the
//! target keeps its symbolic Float identity.
//! R6-1063 opens Face B on the contract side: f64 entry values (parameters,
//! extern results) become symbolic IEEE doubles with the E0813 finiteness
//! obligation at introduction, so contract ordering/equality comparisons
//! over f64 verify on the MIR entry (`result >= x` proven, `result > x`
//! disproven).  Contract float arithmetic and float contract literals stay
//! outside the canonical contract.

use super::*;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn materialize_float_bind(source: &str, label: &str) -> MirProgram {
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

// Generative float-bind matrix: every case binds at least one float local
// from a literal and prints it; the combo case mixes an integer print for
// shape coverage.  Expected stdout comes from the case table, and every case
// shares one `MirProgram` across the three consumers.
#[test]
fn float_bind_matrix_agrees_across_consumers() {
    struct FloatBindCase {
        name: &'static str,
        source: &'static str,
        expected_stdout: &'static str,
    }
    const CASES: &[FloatBindCase] = &[
        FloatBindCase {
            name: "float_literal_bind_print",
            source: r#"
                func main() -> i32 {
                    let x = 0.5
                    println(x)
                    0
                }
            "#,
            expected_stdout: "0.5\n",
        },
        // The print face consumes a fresh clone of the slot, so printing the
        // same local twice is legal.
        FloatBindCase {
            name: "float_bind_double_print",
            source: r#"
                func main() -> i32 {
                    let x = 0.5
                    println(x)
                    println(x)
                    0
                }
            "#,
            expected_stdout: "0.5\n0.5\n",
        },
        // The shared shortest round-trip formatter renders integral floats
        // without a trailing fraction.
        FloatBindCase {
            name: "float_bind_integral_display",
            source: r#"
                func main() -> i32 {
                    let x = 2.0
                    println(x)
                    0
                }
            "#,
            expected_stdout: "2\n",
        },
        FloatBindCase {
            name: "float_bind_two_locals",
            source: r#"
                func main() -> i32 {
                    let x = 0.5
                    let y = 1.5
                    println(x)
                    println(y)
                    0
                }
            "#,
            expected_stdout: "0.5\n1.5\n",
        },
        FloatBindCase {
            name: "float_bind_integer_print_combo",
            source: r#"
                func main() -> i32 {
                    let x = 0.25
                    println(7)
                    println(x)
                    0
                }
            "#,
            expected_stdout: "7\n0.25\n",
        },
        // R6-1057: the scalar-assign face admits the Copy f64 target — a
        // reassignment materializes a fresh slot via Move (Copy scalars need
        // no drop glue, so replacing the latest definition cannot leak).
        FloatBindCase {
            name: "float_assign_literal",
            source: r#"
                func main() -> i32 {
                    let mut x = 0.5
                    x = 1.5
                    println(x)
                    0
                }
            "#,
            expected_stdout: "1.5\n",
        },
        // A bare-integer-literal RHS records the same NumericWiden Convert
        // receipt the call-argument face uses; the Convert result is a
        // Copy-role f64 under the print-face admission.
        FloatBindCase {
            name: "float_assign_widen",
            source: r#"
                func main() -> i32 {
                    let mut acc = 0.5
                    acc = 2
                    println(acc)
                    0
                }
            "#,
            expected_stdout: "2\n",
        },
        // R6-1059: a second-hand assign root (RHS is a plain local read,
        // not a literal) joins the same face inside the print-contract
        // envelope — the graph is Const → Move → Clone (the read) → Move
        // (the assign) → PrintlnFloat, all admitted vocabulary.  The
        // routed `mimi verify` for this shape proves the contract on the
        // MIR path (pinned by the ensures test below).
        FloatBindCase {
            name: "float_assign_second_hand",
            source: r#"
                func main() -> i32 {
                    let mut x = 0.5
                    let y = 1.5
                    x = y
                    println(x)
                    0
                }
            "#,
            expected_stdout: "1.5\n",
        },
        // R6-1060: a second-hand bind root (`let y = x`) joins the same
        // face under the same contract — the mirror of the assign root
        // above.  Rebind chains stay admitted because every hop is a local
        // read of a transitively literal-origin value.
        FloatBindCase {
            name: "float_rebind_second_hand",
            source: r#"
                func main() -> i32 {
                    let x = 0.5
                    let y = x
                    let z = y
                    println(z)
                    0
                }
            "#,
            expected_stdout: "0.5\n",
        },
        // R6-1061: finite-only f64 Add/Subtract over float-symbolic roots
        // prints through the same face.  Reference and native share
        // round-nearest-ties-even `fadd`/`fsub` semantics (the verifier's
        // IEEE symbolic domain models exactly that), and the bytecode VM
        // carries the matching float traps.
        FloatBindCase {
            name: "float_arithmetic_add_print",
            source: r#"
                func main() -> i32 {
                    let a = 1.5
                    let b = 2.0
                    println(a + b)
                    0
                }
            "#,
            expected_stdout: "3.5\n",
        },
        FloatBindCase {
            name: "float_arithmetic_subtract_print",
            source: r#"
                func main() -> i32 {
                    let a = 1.5
                    let b = 2.0
                    println(a - b)
                    0
                }
            "#,
            expected_stdout: "-0.5\n",
        },
        // An integer literal operand widens as a known constant before the
        // operation, so it stays inside the symbolic domain (shortest
        // round-trip renders 3.5 without a trailing fraction).
        FloatBindCase {
            name: "float_arithmetic_literal_coerce",
            source: r#"
                func main() -> i32 {
                    println(2 + 1.5)
                    0
                }
            "#,
            expected_stdout: "3.5\n",
        },
        // R6-1062: an integer literal widening into an F64 assign target
        // keeps the target in the symbolic domain — the lowering carries the
        // widen as `assign_numeric_convert` sourced directly from the
        // literal const, which the MIR verifier widens exactly, so the
        // arithmetic that follows stays admitted.
        FloatBindCase {
            name: "float_arithmetic_after_int_literal_widen",
            source: r#"
                func main() -> i32 {
                    let mut x = 0.5
                    x = 2
                    println(x + 1.0)
                    0
                }
            "#,
            expected_stdout: "3\n",
        },
        // Sequential literal widen assigns compose: each replacement is an
        // exactly-modeled constant, so the symbolic fact never goes stale.
        FloatBindCase {
            name: "float_arithmetic_after_sequential_widens",
            source: r#"
                func main() -> i32 {
                    let mut x = 0.0
                    x = 2
                    x = 3
                    println(x + 1.0)
                    0
                }
            "#,
            expected_stdout: "4\n",
        },
        FloatBindCase {
            name: "float_arithmetic_chain",
            source: r#"
                func main() -> i32 {
                    let a = 1.5
                    let b = 2.0
                    println(a + b - 0.5)
                    0
                }
            "#,
            expected_stdout: "3\n",
        },
        // A bind whose initializer is admitted arithmetic produces a new
        // float-symbolic local — the result feeds the print face through
        // the same root vocabulary as any other local read.
        FloatBindCase {
            name: "float_arithmetic_bind_result",
            source: r#"
                func main() -> i32 {
                    let a = 1.5
                    let b = 2.0
                    let z = a + b
                    println(z)
                    0
                }
            "#,
            expected_stdout: "3.5\n",
        },
        // Second-hand chains and nested arithmetic compose: the operands of
        // the outer Add are a second-hand read of an arithmetic bind and a
        // literal-origin local.
        FloatBindCase {
            name: "float_arithmetic_second_hand_operand",
            source: r#"
                func main() -> i32 {
                    let a = 1.5
                    let b = 2.0
                    let c = a + b
                    let d = c
                    println(d + a)
                    0
                }
            "#,
            expected_stdout: "5\n",
        },
        // Arithmetic hosted inside a branch region is visible only within
        // that region (branch-generation stamps), so the per-path state the
        // verifier explores is exactly the state each path establishes.
        FloatBindCase {
            name: "float_arithmetic_in_branch",
            source: r#"
                func main() -> i32 {
                    if 1 > 0 {
                        let a = 1.5
                        let b = 2.0
                        println(a + b)
                    }
                    0
                }
            "#,
            expected_stdout: "3.5\n",
        },
    ];
    for case in CASES {
        let label = format!("float bind case {}", case.name);
        let checked = checked_program_of(case.source);
        assert!(
            matches!(
                crate::core::mir::classify_scalar_collection_admission(&checked),
                crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
            ),
            "{label} must classify complete"
        );
        let mir = materialize_float_bind(case.source, &label);
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
            crate::codegen::CodeGenerator::new(&context, &format!("mir_floatbind_{}", case.name));
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

// The bind face graph (Const → Move → Clone → PrintlnFloat) carries no
// float arithmetic, so the MIR verifier proves the contract obligations
// end to end — since R6-1061 in the IEEE symbolic domain, before that in
// the opaque-value domain.
#[test]
fn float_bind_ensures_contract_verifies_on_mir() {
    let source = r#"
        func main() -> i32 {
            ensures: result == 0
            let x = 0.5
            println(x)
            0
        }
    "#;
    let label = "float bind ensures";
    let mir = materialize_float_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "float-bind-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// R6-1059: the second-hand assign graph (Const → Move → Clone read → Move
// assign → PrintlnFloat) carries no float arithmetic either, so the MIR
// verifier proves the contract obligations on this routed shape too — the
// same proof the default `mimi verify` entry now reaches once the face
// classifies complete (the legacy-path verifier keeps floats
// uninterpreted and failed this exact program before the flip).
#[test]
fn float_second_hand_assign_ensures_verifies_on_mir() {
    let source = r#"
        func main() -> i32 {
            ensures: result == 0
            let mut x = 0.5
            let y = 1.5
            x = y
            println(x)
            0
        }
    "#;
    let label = "float second-hand assign ensures";
    let mir = materialize_float_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "float-second-hand-assign-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// R6-1060: the second-hand bind graph (Const → Move → Clone read → Move
// bind → Clone read → PrintlnFloat) carries no float arithmetic either, so
// the MIR verifier proves the contract obligations on this routed shape
// too — the same proof the default `mimi verify` entry reaches once the
// face classifies complete.
#[test]
fn float_second_hand_bind_ensures_verifies_on_mir() {
    let source = r#"
        func main() -> i32 {
            ensures: result == 0
            let x = 0.5
            let y = x
            println(y)
            0
        }
    "#;
    let label = "float second-hand bind ensures";
    let mir = materialize_float_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "float-second-hand-bind-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// R6-1061: the arithmetic graph carries float Binary operations, so this is
// the proof that the MIR verifier's IEEE symbolic domain closes the face —
// Add/Subtract evaluate as round-nearest-ties-even FP arithmetic with an
// E0813 finiteness definedness obligation (the mirror of the runtime
// operand/result traps), and the contract proves on the routed MIR path
// the default `mimi verify` entry reaches.
#[test]
fn float_arithmetic_ensures_verifies_on_mir() {
    let source = r#"
        func main() -> i32 {
            ensures: result == 0
            let a = 1.5
            let b = 2.0
            println(a - b)
            println(2 + 1.5)
            println(a + b - 0.5)
            let z = a + b
            println(z)
            0
        }
    "#;
    let label = "float arithmetic ensures";
    let mir = materialize_float_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "float-arithmetic-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// R6-1061: arithmetic hosted inside a branch region verifies per path —
// the Branch exploration keeps each arm's symbolic state separate, so the
// float-symbolic facts the branch establishes are exactly the facts its
// path proves.
#[test]
fn float_arithmetic_in_branch_ensures_verifies_on_mir() {
    let source = r#"
        func main() -> i32 {
            ensures: result == 0
            if 1 > 0 {
                let a = 1.5
                let b = 2.0
                println(a + b)
            }
            0
        }
    "#;
    let label = "float arithmetic in branch ensures";
    let mir = materialize_float_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "float-arithmetic-branch-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// R6-1062: the integer-literal widen assign joins the proof surface.  The
// graph carries `assign_numeric_convert` sourced directly from the literal
// const, so the MIR verifier widens the known constant exactly
// (`Float::from_f64`) and the following Add/Subtract obligations prove in
// the IEEE symbolic domain — the same routed path the default `mimi verify`
// entry reaches now that the face classifies complete.
#[test]
fn float_int_literal_widen_ensures_verifies_on_mir() {
    let source = r#"
        func main() -> i32 {
            ensures: result == 0
            let mut x = 0.5
            x = 2
            println(x + 1.0)
            0
        }
    "#;
    let label = "float int literal widen ensures";
    let mir = materialize_float_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "float-int-widen-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// The bind face is print-face-scoped: float operations outside the
// symbolic domain (Multiply), arithmetic over opaque-widen provenance,
// uses that cross a branch boundary, call-result binds and dead float
// binds in non-printing functions all keep the graph on the explicit
// mixed compatibility route.  (R6-1057 restated the former literal
// float-assign mixed case, R6-1059 the second-hand float-assign case and
// R6-1060 the second-hand float-bind case: all migrated into the matrix
// above.  R6-1061 restates the former float-arithmetic case, which is now
// the matrix's add/subtract rows.  R6-1062 restates the former
// known-constant-widen case, now the matrix's int-literal-widen rows.)
#[test]
fn float_bind_faces_stay_mixed() {
    struct MixedCase {
        name: &'static str,
        source: &'static str,
    }
    const CASES: &[MixedCase] = &[
        // Multiply/Divide stay construction-rejected — the island matrix
        // and the verifier fallback both keep them outside the symbolic
        // domain, so the classification floor must hold.
        MixedCase {
            name: "float_multiply_print",
            source: r#"
                func main() -> i32 {
                    let a = 1.5
                    let b = 2.0
                    println(a * b)
                    0
                }
            "#,
        },
        // An int-typed local read is not a float-symbolic operand: its
        // widening is an opaque Convert the verifier cannot model, so the
        // operand floor keeps the shape mixed instead of luring the route
        // into a verifier hard error.
        MixedCase {
            name: "float_arithmetic_int_local_operand",
            source: r#"
                func main() -> i32 {
                    let n = 2
                    let a = 1.5
                    println(n + a)
                    0
                }
            "#,
        },
        // An assign from opaque provenance removes the target from the
        // float-symbolic set in walk order — the stale fact can never
        // outlive the value that replaced it.
        MixedCase {
            name: "float_arithmetic_after_opaque_reassign",
            source: r#"
                func main() -> i32 {
                    let mut x = 0.5
                    let n = 3
                    x = n
                    println(x + 1.0)
                    0
                }
            "#,
        },
        // R6-1062 restatement: the known-constant widen assign migrated into
        // the matrix above (its Convert source is the literal const, which
        // the verifier widens exactly).  An assign hosted inside a branch
        // region keeps the whole composition on the conservative envelope —
        // this predates the widen face (the plain Identity float assign in
        // the same position floors identically), so the widen face must not
        // open a branch-position envelope the established assign face
        // doesn't already have.
        MixedCase {
            name: "float_arithmetic_after_widen_in_branch",
            source: r#"
                func main() -> i32 {
                    let mut x = 0.0
                    if 1 > 0 {
                        x = 2
                        println(x + 1.0)
                    }
                    0
                }
            "#,
        },
        // A use after any branch boundary cannot lean on a binding a
        // different path may not have established (branch-generation
        // stamps), so the use floors and the shape stays mixed.
        MixedCase {
            name: "float_arithmetic_use_after_branch",
            source: r#"
                func main() -> i32 {
                    let a = 1.5
                    if 1 > 0 {
                        println(7)
                    }
                    println(a + 1.0)
                    0
                }
            "#,
        },
        // R6-1060 restatement: the second-hand float bind migrated into
        // the matrix above.  A call-result root is never a plain local
        // read — its provenance is an unmigrated body, so the bind pattern
        // still floors on its f64 type.
        MixedCase {
            name: "float_bind_from_call_is_mixed",
            source: r#"
                func value() -> f64 {
                    0.5
                }
                func main() -> i32 {
                    let x = value()
                    println(x)
                    0
                }
            "#,
        },
        // The per-function flag parity: a float bind in a function without a
        // float println keeps that function mixed, so the island gate's
        // per-function envelope can never be wider than the classification.
        MixedCase {
            name: "dead_float_bind_without_print",
            source: r#"
                func helper() -> i32 {
                    let x = 0.5
                    0
                }
                func main() -> i32 {
                    println(helper())
                    0
                }
            "#,
        },
    ];
    for case in CASES {
        let label = format!("float bind mixed case {}", case.name);
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

// R6-1063 Face B: f64 contract values are in-domain for ordering
// comparisons.  The strict form doubles as the non-vacuity pin — the same
// identity body disproves `result > x` while proving `result >= x`, so the
// comparison predicates demonstrably evaluate instead of trivially holding.
#[test]
fn float_param_result_ordering_strict_inequality_is_disproven_on_mir() {
    let source = r#"
        func same(x: f64) -> f64 {
            ensures: result > x
            x
        }
        func main() -> i32 {
            let y = same(1.5)
            println(y)
            0
        }
    "#;
    let label = "float param strict ordering";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-param-strict-ordering".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    // Only `same` carries a contract; main has none, so exactly one
    // obligation is collected.
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Disproven),
        "{label} must be disproven, got {:?}",
        results[0].status
    );
}
