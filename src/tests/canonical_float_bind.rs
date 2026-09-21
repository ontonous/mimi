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
//! binds from off-closure callees, arithmetic on opaque-widen locals and
//! uses that cross a branch boundary keep their explicit mixed floors
//! until their contracts are independently materialized.
//! R6-1062 admits the integer-literal widen assign (`x = 2` into an F64
//! target): its `assign_numeric_convert` sources the literal const
//! directly, so the verifier widens the known constant exactly and the
//! target keeps its symbolic Float identity.
//! R6-1063 opens Face B on the contract side: f64 entry values (parameters,
//! extern results) become symbolic IEEE doubles with the E0813 finiteness
//! obligation at introduction, so contract ordering/equality comparisons
//! over f64 verify on the MIR entry (`result >= x` proven, `result > x`
//! disproven).  R6-1065 admits float contract literals as known constants;
//! R6-1066 admits IEEE negate on both sides; R6-1067 admits float contract
//! arithmetic (Add/Subtract/Multiply/Divide under RNE — overflow and
//! divide-by-zero are IEEE-defined non-finite results owned by the runtime
//! E0813 trap, so unbounded multiply bodies honestly reject) and widens the
//! body arithmetic face with Multiply/Divide across all consumers.
//! R6-1070 widens the print envelope itself to the one-edge f64 print
//! closure: a helper that prints its f64 parameter admits the caller's
//! literal, a helper returning an f64 literal (or straight-line symbolic
//! arithmetic over a seeded parameter) admits the caller's println of the
//! call result, and the island gate mirrors the same closure per
//! materialized function.  Beyond one edge, non-face parameters, call-result
//! binds and branchy float returns keep their explicit floors.
//! R6-1071 completes the bind side of the same closure: a call-result bind
//! whose callee sits on the closure joins the face (the verifier
//! symbolically executes the callee body, so the binding is exactly
//! modeled), and the literal/second-hand/arithmetic f64 bind roots widen
//! from the per-function print contract to the closure — a helper on a
//! caller's print closure binds its own f64 literal and arithmetic.  The
//! owned-String bind half stays print-function-scoped.

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
        // R6-1067: Multiply/Divide join the finite-only arithmetic face on
        // the same evidence as Add/Subtract — RNE fpa ops are the exact
        // fmul/fdiv semantics, and the shared E0813 finiteness trap owns
        // overflow / divide-by-zero (both are IEEE-defined non-finite
        // results).
        FloatBindCase {
            name: "float_arithmetic_multiply_print",
            source: r#"
                func main() -> i32 {
                    let a = 1.5
                    let b = 2.0
                    println(a * b)
                    0
                }
            "#,
            expected_stdout: "3\n",
        },
        FloatBindCase {
            name: "float_arithmetic_divide_print",
            source: r#"
                func main() -> i32 {
                    let a = 2.5
                    let b = 2.0
                    println(a / b)
                    0
                }
            "#,
            expected_stdout: "1.25\n",
        },
        // R6-1068: the comparison face joins — plain IEEE ordered
        // predicates with no finiteness trap.  The `x == 0.0` case is the
        // sharpest one: `0.0 - 1.0 * 0.0` produces -0.0, whose runtime bit
        // patterns differ from `0.0`, so every consumer must decode to f64
        // before comparing (a bit-pattern comparison would answer 0).
        // Shortest round-trip renders the printed -0.0 as "0".
        FloatBindCase {
            name: "float_comparison_equal_negzero",
            source: r#"
                func main() -> i32 {
                    let x = 0.0 - 1.0 * 0.0
                    println(x)
                    if x == 0.0 {
                        println(1)
                    } else {
                        println(0)
                    }
                    0
                }
            "#,
            expected_stdout: "0\n1\n",
        },
        // Ordering, not-equal, and a Bool-typed bind of a comparison
        // result feeding a branch — the bind face takes the Bool the
        // compare produces like any other Copy scalar.
        FloatBindCase {
            name: "float_comparison_ordering_ne_bind",
            source: r#"
                func main() -> i32 {
                    let a = 0.5
                    let b = 1.5
                    println(a)
                    let hit = a < b
                    if hit {
                        println(1)
                    } else {
                        println(0)
                    }
                    if a != b {
                        println(1)
                    } else {
                        println(0)
                    }
                    0
                }
            "#,
            expected_stdout: "0.5\n1\n1\n",
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
        // R6-1064: a second-hand int read widening into an F64 assign
        // target — the RHS carries literal provenance (`let n = 2`), the
        // verifier propagates the constant through Load into the Convert
        // widen, so the target is still an exactly-modeled known constant
        // and the arithmetic that follows stays admitted.
        FloatBindCase {
            name: "float_arithmetic_after_int_read_widen",
            source: r#"
                func main() -> i32 {
                    let n = 2
                    let mut x = 0.5
                    x = n
                    println(x + 1.0)
                    0
                }
            "#,
            expected_stdout: "3\n",
        },
        // R6-1064: the int-literal operand floor flips — an int-typed local
        // read whose provenance is a tracked literal join is a
        // verifier-backed widening, so it licenses the admission the
        // opaque-read floor used to deny.
        FloatBindCase {
            name: "float_arithmetic_int_local_operand",
            source: r#"
                func main() -> i32 {
                    let n = 2
                    let a = 1.5
                    println(n + a)
                    0
                }
            "#,
            expected_stdout: "3.5\n",
        },
        // R6-1064: a direct int-literal bind used as a float-arithmetic
        // operand — the bind seeds the provenance set and the binary's
        // operand read of it widens as the known constant 2.
        FloatBindCase {
            name: "float_arithmetic_direct_int_bind_operand",
            source: r#"
                func main() -> i32 {
                    let x = 2
                    println(x + 1.0)
                    0
                }
            "#,
            expected_stdout: "3\n",
        },
        // R6-1064: the widened target itself becomes an arithmetic bind
        // result — the symbolic Float identity the widen assign grants
        // feeds the bind face through the ordinary root vocabulary.
        FloatBindCase {
            name: "float_arithmetic_int_read_widen_rebind",
            source: r#"
                func main() -> i32 {
                    let n = 2
                    let mut x = 0.5
                    x = n
                    let y = x + 1.0
                    println(y)
                    0
                }
            "#,
            expected_stdout: "3\n",
        },
        // R6-1069: the unary negate face joins the float-origin domain —
        // IEEE sign-bit negation is exact for every finite operand and the
        // verifier's (Negate, Float) evaluator adds no obligation, so a
        // literal negate bind, a second-hand negate of a tracked local, and
        // negates inside comparison operands all classify complete.  The
        // `-x == -1.5` case answers false (1.5 != -1.5): the consumer
        // triangle must not lose the sign anywhere along the chain.
        FloatBindCase {
            name: "float_negate_literal_bind_chain",
            source: r#"
                func main() -> i32 {
                    let x = -(1.5)
                    println(x)
                    let y = -x
                    println(y)
                    if -x == -1.5 {
                        println(1)
                    } else {
                        println(0)
                    }
                    0
                }
            "#,
            expected_stdout: "-1.5\n1.5\n0\n",
        },
        // R6-1069: negating +0.0 produces -0.0 — shortest round-trip keeps
        // the sign ("-0") while the IEEE equality still compares equal to
        // +0.0, so the print face and the comparison face disagree on the
        // surface exactly as IEEE dictates, on every consumer.
        FloatBindCase {
            name: "float_negate_negzero_identity",
            source: r#"
                func main() -> i32 {
                    let z = -(0.0)
                    println(z)
                    if z == 0.0 {
                        println(1)
                    } else {
                        println(0)
                    }
                    0
                }
            "#,
            expected_stdout: "-0\n1\n",
        },
        // R6-1071: a call-result bind whose callee sits on the one-edge f64
        // print closure joins the face — the verifier symbolically executes
        // the callee body, so the binding is a plain Move of an exactly
        // modeled f64 and the seeded binding feeds later arithmetic through
        // the ordinary symbolic root vocabulary.
        FloatBindCase {
            name: "float_call_result_bind_and_arithmetic_print",
            source: r#"
                func value() -> f64 {
                    0.5
                }
                func main() -> i32 {
                    let x = value()
                    println(x)
                    println(x * 2.0)
                    0
                }
            "#,
            expected_stdout: "0.5\n1\n",
        },
        // R6-1070: the one-edge f64 print closure — the print face crosses
        // one call edge in both directions.  A helper that prints its own
        // f64 parameter admits the caller's literal argument (Const in the
        // caller, parameter + Clone + PrintlnFloat in the callee); a helper
        // returning an f64 literal admits the caller's PrintlnFloat of the
        // call result; a helper whose f64-parameter arithmetic is the root
        // result admits both the parameter seed and the symbolic root.
        // Beyond one edge, non-face parameters, call-result binds and
        // branchy float returns keep their explicit floors (the negative
        // pins below).
        FloatBindCase {
            name: "float_cross_function_param_print",
            source: r#"
                func showf(v: f64) -> i32 {
                    println(v)
                    0
                }
                func main() -> i32 {
                    showf(2.5)
                }
            "#,
            expected_stdout: "2.5\n",
        },
        FloatBindCase {
            name: "float_cross_function_result_print",
            source: r#"
                func ret() -> f64 {
                    1.5
                }
                func main() -> i32 {
                    println(ret())
                    0
                }
            "#,
            expected_stdout: "1.5\n",
        },
        FloatBindCase {
            name: "float_cross_function_param_arithmetic_result",
            source: r#"
                func twice(v: f64) -> f64 {
                    v * 2.0
                }
                func main() -> i32 {
                    println(twice(1.5))
                    0
                }
            "#,
            expected_stdout: "3\n",
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

// R6-1064: a second-hand int read widening into f64 arithmetic.  The
// verifier propagates the bind's known constant through Load into the
// Convert widen, so `x + n` lands in the Float domain and the ordering
// obligation proves against the symbolic parameter.
#[test]
fn float_int_read_widen_ensures_verifies_on_mir() {
    let source = r#"
        func widen(x: f64) -> f64 {
            ensures: result >= x
            let n = 2
            let y = x + n
            y
        }
        func main() -> i32 {
            let w = widen(1.5)
            println(w)
            0
        }
    "#;
    let label = "float int read widen ensures";
    let mir = materialize_float_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "float-int-read-widen-ensures".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// The non-vacuity pin for the same face: the widened constant genuinely
// participates — `result == x` (true only if n contributed zero) is
// disproven, so the propagated constant is exactly 2, not an opaque value
// silently accepted or a vacuous hold.
#[test]
fn float_int_read_widen_constant_is_nonzero_on_mir() {
    let source = r#"
        func widen(x: f64) -> f64 {
            ensures: result == x
            let n = 2
            let y = x + n
            y
        }
        func main() -> i32 {
            let w = widen(1.5)
            println(w)
            0
        }
    "#;
    let label = "float int read widen nonzero constant";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-int-read-widen-nonzero".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Disproven),
        "{label} must be disproven, got {:?}",
        results[0].status
    );
}

// The bind face is print-face-scoped: float operations outside the
// symbolic domain (Multiply), arithmetic over call-sourced widen
// provenance and uses that cross a branch boundary all keep the graph on
// the explicit mixed compatibility route.  (R6-1057 restated the former
// literal float-assign mixed case, R6-1059 the second-hand float-assign
// case and R6-1060 the second-hand float-bind case: all migrated into the
// matrix above.  R6-1061 restates the former float-arithmetic case, which
// is now the matrix's add/subtract rows.  R6-1062 restates the former
// known-constant-widen case, now the matrix's int-literal-widen rows.
// R6-1064 restates the former int-local-operand and literal-provenance
// widen cases — both verifier-backed now — leaving the call-sourced
// widen as the opaque-provenance floor.  R6-1067 restates the former
// multiply mixed case, now the matrix's multiply/divide rows.
// R6-1071 restates the former call-result bind case — a face-closure
// callee's returned f64 is exactly modeled — leaving the two-edge
// provenance as the off-closure floor.)
#[test]
fn float_bind_faces_stay_mixed() {
    struct MixedCase {
        name: &'static str,
        source: &'static str,
    }
    const CASES: &[MixedCase] = &[
        // R6-1067 restatement: the former multiply mixed case migrated into
        // the matrix above (`float_arithmetic_multiply_print`) — the island
        // matrix, the shared TypeDesc validator, the reference executor,
        // the VM adapter, the native emitter and the verifier's IEEE
        // symbolic domain all admit f64 Multiply/Divide on the same E0813
        // evidence.  A call-sourced int keeps the floor: its provenance is
        // an unmigrated body, so the widening lands opaque and the
        // verifier hard-rejects the operand mix (pinned by the ensures
        // tests below).
        MixedCase {
            name: "float_arithmetic_after_call_sourced_widen",
            source: r#"
                func source() -> i64 {
                    3
                }
                func main() -> i32 {
                    let mut x = 0.5
                    let n = source()
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
        // R6-1071 restatement: the former call-result bind mixed case
        // migrated into the matrix above (`float_call_result_bind_and_
        // arithmetic_print`) — the verifier symbolically executes the callee
        // body, so a face-closure callee's returned f64 is exactly modeled.
        // The floor that stays is the TWO-EDGE provenance: `mid` sits on
        // main's print closure, but its own body calls `inner`, which the
        // closure does not reach — mid's root call-result floors and the
        // bind composition keeps the whole program mixed.
        MixedCase {
            name: "float_two_edge_call_result_bind_is_mixed",
            source: r#"
                func inner() -> f64 {
                    0.5
                }
                func mid() -> f64 {
                    inner()
                }
                func main() -> i32 {
                    let x = mid()
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
        // R6-1070: the closure is one call edge only.  `mid` sits on main's
        // print closure, but `inner` is a helper of a helper — its f64
        // result has no print face one edge away, so its result floor holds.
        MixedCase {
            name: "float_two_edge_helper_chain_is_mixed",
            source: r#"
                func inner() -> f64 {
                    2.5
                }
                func mid() -> f64 {
                    inner()
                }
                func main() -> i32 {
                    println(mid())
                    0
                }
            "#,
        },
        // R6-1070: the cross-function root face is straight-line only — a
        // branchy float return hosts its F64 blocks below the root block,
        // where no exemption exists, so the helper stays mixed.
        MixedCase {
            name: "float_branchy_helper_result_is_mixed",
            source: r#"
                func twice2(v: f64) -> f64 {
                    if v > 1.0 {
                        v * 2.0
                    } else {
                        0.0
                    }
                }
                func main() -> i32 {
                    println(twice2(1.5))
                    0
                }
            "#,
        },
        // R6-1070: an f64 parameter floors unless the enclosing callable is
        // on the FLOAT print closure.  main prints only an integer here, so
        // the float print set is empty and `area`'s parameter keeps the
        // per-function envelope floor even though the program is a migrated
        // candidate overall.
        MixedCase {
            name: "float_param_helper_without_float_print_caller_is_mixed",
            source: r#"
                func area(w: f64) -> f64 {
                    w * 2.0
                }
                func main() -> i32 {
                    println(7)
                    let x = area(2.0)
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

// R6-1070: the island gate mirrors the one-edge f64 print closure on the
// materialized graph.  The two-edge helper chain materializes (the generic
// constructor does not consult the classifier), but the island gate rejects
// it exactly on `inner`'s print-face constant — the f64 literal sits in a
// function the one-edge closure does not reach, so the default route's
// classifier floor is the only thing between this graph and the mixed
// compatibility route.
#[test]
fn float_print_closure_island_rejects_two_edge_helper() {
    let source = r#"
        func inner() -> f64 {
            2.5
        }
        func mid() -> f64 {
            inner()
        }
        func main() -> i32 {
            println(mid())
            0
        }
    "#;
    let label = "float closure two-edge island";
    let mir = materialize_float_bind(source, label);
    let errors = crate::core::mir::validate_scalar_collection_island(&mir)
        .expect_err("{label} must reject the two-edge helper chain");
    assert!(
        errors
            .iter()
            .any(|error| error.contains("FloatBits") || error.contains("function:inner")),
        "{label} must reject the off-closure constant: {errors:?}"
    );
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

// R6-1065: an f64 contract literal lowers to `MirContractExpr::Float(bits)`
// and the verifier evaluates it as a known finite constant.  A
// constant-returning body proves both an equality and an ordering against
// literals — the literal genuinely participates in the predicate.
#[test]
fn float_contract_literal_equality_and_ordering_verify_on_mir() {
    let source = r#"
        func one() -> f64 {
            ensures: result == 1.5
            ensures: result >= 1.0
            1.5
        }
        func main() -> i32 {
            ensures: result == 0
            println(one())
            0
        }
    "#;
    let label = "float contract literal verifies";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-contract-literal".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 2, "{label} obligation count");
    assert!(
        results
            .iter()
            .all(|result| matches!(result.status, crate::verifier::VerifStatus::Verified)),
        "{label} every literal obligation must verify: {results:?}"
    );
}

// The non-vacuity pin for the literal face: `result >= 0.0` against a
// symbolic identity body is disproven (the parameter can be negative), so
// the literal constant demonstrably enters the comparison instead of the
// obligation holding vacuously.
#[test]
fn float_contract_literal_ordering_is_disproven_against_symbolic_identity() {
    let source = r#"
        func identity(x: f64) -> f64 {
            ensures: result >= 0.0
            x
        }
        func main() -> i32 {
            println(identity(-1.5))
            0
        }
    "#;
    let label = "float contract literal non-vacuity";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-contract-literal-nv".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Disproven),
        "{label} must be disproven, got {:?}",
        results[0].status
    );
}

// R6-1066: IEEE negation joins both the runtime symbolic interpreter and
// the contract expression domain — a body negation and a contract
// negation compose into an exact round-trip identity over the symbolic
// parameter.
#[test]
fn float_contract_negate_roundtrip_verifies_on_mir() {
    let source = r#"
        func mirrored(x: f64) -> f64 {
            ensures: result == -(x)
            -x
        }
        func main() -> i32 {
            println(mirrored(1.5))
            0
        }
    "#;
    let label = "float contract negate roundtrip";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-contract-negate".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// The non-vacuity pin: without the body's negation the same contract is
// disproven, so `-(x)` in the predicate demonstrably flips the sign of
// the symbolic operand instead of matching anything.
#[test]
fn float_contract_negate_is_disproven_without_the_flip() {
    let source = r#"
        func mirrored(x: f64) -> f64 {
            ensures: result == -(x)
            x
        }
        func main() -> i32 {
            println(mirrored(1.5))
            0
        }
    "#;
    let label = "float contract negate non-vacuity";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-contract-negate-nv".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Disproven),
        "{label} must be disproven, got {:?}",
        results[0].status
    );
}

// R6-1069: the negate bind chain verifies through the symbolic domain —
// the body stores `-(x)` into a tracked local and re-negates it on the way
// out, so the verifier must model the negate through StoreLocal/Load
// exactly (double negation recovers x, not -x).
#[test]
fn float_body_negate_bind_chain_verifies_on_mir() {
    let source = r#"
        func mirrored(x: f64) -> f64 {
            ensures: result == x
            let y = -x
            -y
        }
        func main() -> i32 {
            println(mirrored(1.5))
            0
        }
    "#;
    let label = "float body negate bind chain";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-body-negate-chain".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify: {results:?}"
    );
}

// The non-vacuity pin: against the identity ensures, the same bind chain
// is disproven — the chain demonstrably produces x, not -x, so the
// verifier models the intermediate store instead of matching anything.
#[test]
fn float_body_negate_bind_chain_is_disproven_against_the_flipped_ensures() {
    let source = r#"
        func mirrored(x: f64) -> f64 {
            ensures: result == -(x)
            let y = -x
            -y
        }
        func main() -> i32 {
            println(mirrored(1.5))
            0
        }
    "#;
    let label = "float body negate bind chain non-vacuity";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-body-negate-chain-nv".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Disproven),
        "{label} must be disproven, got {:?}",
        results[0].status
    );
}

// R6-1069: a negate-sourced zero divisor keeps the E0801 trap contract.
// `-(0.0)` is -0.0, and a ±0.0 divisor is a language-level
// division-definedness violation (small-step §3), not an IEEE ±inf result
// owned by the E0813 finiteness obligation — the R6-1067 trap parity
// extends verbatim to divisors that only become zero through negation.
#[test]
fn float_negate_zero_divisor_traps_on_mir_consumers() {
    let source = r#"
        func main() -> i32 {
            let z = -(0.0)
            println(1.0 / z)
            0
        }
    "#;
    let label = "float negate zero divisor trap";
    let mir = materialize_float_bind(source, label);
    let Err(reference_error) = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
    else {
        panic!("{label} reference must trap on the -0.0 divisor")
    };
    assert!(
        reference_error.message.contains("E0801")
            && reference_error.message.contains("division by zero"),
        "{label} reference trap must be the E0801 division-definedness violation: {reference_error}"
    );

    let bytecode = compile_mir_program(&mir)
        .unwrap_or_else(|error| panic!("{label} bytecode compilation failed: {error:?}"));
    assert!(bytecode.ast.is_none(), "{label} bytecode must be AST-free");
    let mut vm = BytecodeVM::new(bytecode);
    let Err(vm_error) = vm.run_value() else {
        panic!("{label} VM must trap on the -0.0 divisor")
    };
    assert!(
        matches!(
            vm_error,
            crate::interp::error::InterpError::DivisionByZero(_)
        ),
        "{label} VM trap must be the E0801 division-definedness violation: {vm_error:?}"
    );

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_float_negate_zero_div");
    generator
        .compile_mir_native(&mir)
        .unwrap_or_else(|error| panic!("{label} native emission failed: {error:?}"));
    let native = link_and_observe_canonical_mir(&generator)
        .unwrap_or_else(|error| panic!("{label} native execution failed: {error}"));
    assert_eq!(native.exit_code, Some(1), "{label} native exit");
    assert!(
        native.stderr.contains("division by zero"),
        "{label} native trap must report division by zero: {native:?}"
    );
}

// R6-1067: float contract arithmetic joins the R6-1063 comparison face.
// Multiply proves against a requires-bounded parameter (the E0813 result
// obligation needs the bound — unbounded x can overflow); Divide proves
// unbounded because |x/2| stays below |x| for every finite x, so no model
// can reach the trap.
#[test]
fn float_contract_multiply_and_divide_arithmetic_verify_on_mir() {
    let source = r#"
        func doubled(x: f64) -> f64 {
            requires: x >= 0.0
            requires: x <= 1.0
            ensures: result == x * 2.0
            x * 2.0
        }
        func half(x: f64) -> f64 {
            ensures: result == x / 2.0
            x / 2.0
        }
        func main() -> i32 {
            println(doubled(1.25))
            println(half(5.0))
            0
        }
    "#;
    let label = "float contract mul/div arithmetic verifies";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-contract-mul-div".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 2, "{label} obligation count");
    assert!(
        results
            .iter()
            .all(|result| matches!(result.status, crate::verifier::VerifStatus::Verified)),
        "{label} every arithmetic obligation must verify: {results:?}"
    );
}

// The non-vacuity pin: without the body's product the same contract is
// disproven, so `x * 2.0` in the predicate demonstrably constrains the
// symbolic result instead of matching anything.
#[test]
fn float_contract_multiply_is_disproven_without_the_product() {
    let source = r#"
        func doubled(x: f64) -> f64 {
            ensures: result == x * 2.0
            x
        }
        func main() -> i32 {
            println(doubled(1.25))
            0
        }
    "#;
    let label = "float contract multiply non-vacuity";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-contract-mul-nv".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Disproven),
        "{label} must be disproven, got {:?}",
        results[0].status
    );
}

// The E0813 teeth pin: an unbounded multiply body can overflow to infinity
// (x near f64::MAX), so the result-finiteness obligation is honestly
// unreachable — the verifier rejects instead of waving the trap through.
// This is the dimension Multiply adds over the R6-1061 Add/Subtract
// probes, which used literal constants.
#[test]
fn float_contract_unbounded_multiply_trips_the_finiteness_obligation() {
    let source = r#"
        func doubled(x: f64) -> f64 {
            ensures: result == x * 2.0
            x * 2.0
        }
        func main() -> i32 {
            println(doubled(1.25))
            0
        }
    "#;
    let label = "float contract unbounded multiply E0813";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-contract-mul-overflow".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Disproven),
        "{label} must be disproven (E0813 reachable), got {:?}",
        results[0].status
    );
}

// R6-1068: a body-level comparison feeds the branch the verifier walks —
// the Binary(Less) instruction produces the branch's Bool through the same
// fpa predicate the runtime computes, so the bounded parameter forces the
// then-arm and `result == 1` proves.
#[test]
fn float_body_comparison_branch_verifies_on_mir() {
    let source = r#"
        func check(x: f64) -> i32 {
            requires: x >= 0.0
            requires: x <= 1.0
            ensures: result == 1
            if x < 1.5 {
                1
            } else {
                0
            }
        }
        func main() -> i32 {
            println(check(0.5))
            0
        }
    "#;
    let label = "float body comparison branch verifies";
    let mir = materialize_float_bind(source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));
    let results = crate::verifier::verify_mir(&mir, "float-body-compare".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Verified),
        "{label} must verify, got {:?}",
        results[0].status
    );
}

// The non-vacuity pin: against the negated ensures the same body
// disproves, so the modeled comparison demonstrably drives the branch
// instead of both arms matching any contract.
#[test]
fn float_body_comparison_branch_is_disproven_against_the_false_ensures() {
    let source = r#"
        func check(x: f64) -> i32 {
            requires: x >= 0.0
            requires: x <= 1.0
            ensures: result == 0
            if x < 1.5 {
                1
            } else {
                0
            }
        }
        func main() -> i32 {
            println(check(0.5))
            0
        }
    "#;
    let label = "float body comparison branch non-vacuity";
    let mir = materialize_float_bind(source, label);
    let results = crate::verifier::verify_mir(&mir, "float-body-compare-nv".into())
        .unwrap_or_else(|error| panic!("{label} verification failed: {error}"));
    assert_eq!(results.len(), 1, "{label} obligation count");
    assert!(
        matches!(results[0].status, crate::verifier::VerifStatus::Disproven),
        "{label} must be disproven, got {:?}",
        results[0].status
    );
}
