//! Plain-scalar island route candidacy (R6-1052).
//!
//! Before this slice every program's scalar-collection coverage scan was
//! poisoned by the automatically merged prelude (float helpers, capturing
//! lambdas, prelude traits), so a bare `println(41)` program had no route
//! candidate and kept the legacy route.  R6-1052 decouples the prelude from
//! the scan, admits the owned string print face into the stdout receipt, and
//! completes the parity chain so a complete plain-scalar graph routes
//! canonical on the default entries:
//!
//! 1. checker admission (`classify_scalar_collection_admission`) — prelude
//!    callables filtered, string values admitted only through the print face;
//! 2. S11 island gate — scalar literal switches admitted through the shared
//!    `validate_scalar_switch` contract, variant switches fail-closed;
//! 3. the symbolic MIR verifier explores literal switches with equality
//!    guards so contracted match functions verify instead of failing with
//!    the variant-tag machinery.
//!
//! A construction-failure guard keeps statement shapes the MIR lowering
//! cannot express (an assignment nested inside a block) on the working
//! compatibility route instead of turning a previously running program into
//! a hard rejection.

use super::*;
use crate::core::mir::reference::MirReferenceInterpreter;
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

type PreludeExclusions = std::collections::HashSet<crate::span::SourceId>;

// Mirror the CLI loader: the prelude is merged into every program without
// registering a user import, and the route callers pass the prelude source as
// the compatibility exclusion — exactly the graph shape the route dispatch
// observes on the default entries.
fn checked_program_of(source: &str) -> (crate::core::CheckedProgram, PreludeExclusions) {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let mut file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    crate::loader::merge_prelude_into(&mut file);
    let excluded_sources = file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect();
    let checked =
        crate::core::check_program(&file).unwrap_or_else(|diags| panic!("check failed: {diags:?}"));
    (checked, excluded_sources)
}

// The admission matrix pins the exact flip set of the prelude decoupling:
// complete plain-scalar programs (with or without scalar literal matches,
// user helpers, string print literals, root-level scalar assignment) gain
// `CompleteCoverage` for the first time, while every compatibility boundary
// (string values outside the print face, floats) keeps `MixedCoverage`.
// R6-1055 restatement: a String literal bind read by the print face joined
// the complete set (its differential matrix lives in
// canonical_string_bind.rs).  R6-1060 restatement: the second-hand String
// bind joined it too (R6-1060 opened Load-root print-face bind roots), so
// the mixed representative below uses a dead float bind in a
// non-printing function — the per-function contract boundary the bind
// exemption still respects.  R6-1061 restatement: float Add/Subtract over
// float-symbolic roots joined the complete set under the same per-function
// contract (its differential matrix lives in canonical_float_bind.rs);
// the representative here still carries no float println, so the
// per-function envelope keeps it mixed.
#[test]
fn plain_scalar_admission_matrix_pins_the_flip_set() {
    let complete_shapes = [
        ("int_print", "func main() { println(41) }"),
        ("bool_print", "func main() { println(true) }"),
        (
            "string_literal_print",
            r#"func main() { println("hello") }"#,
        ),
        (
            "string_literal_bind_print",
            r#"func main() {
                let s = "hi"
                println(s)
            }"#,
        ),
        (
            "bool_match_helper",
            r#"
                func label(flag: bool) -> i32 {
                    match flag { true => 41, false => 7 }
                }
                func main() { println(label(true)) }
            "#,
        ),
        (
            "int_match_in_main",
            r#"
                func main() {
                    let tag = match 7 { 5 => 50, 7 => 70, _ => 90 }
                    println(tag)
                }
            "#,
        ),
        (
            "root_scalar_assign",
            r#"
                func main() {
                    let mut x = 0
                    x = 5
                    println(x)
                }
            "#,
        ),
    ];
    for (name, source) in complete_shapes {
        let (checked, _) = checked_program_of(source);
        let admission = crate::core::mir::classify_scalar_collection_admission(&checked);
        assert!(
            matches!(
                admission,
                crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
            ),
            "{name} must be a complete plain-scalar admission: {admission:?}"
        );
    }

    let mixed_shapes = [(
        "float_local",
        r#"func main() {
                println(1)
                let probe_float = 0.5
                drop(probe_float)
            }"#,
    )];
    for (name, source) in mixed_shapes {
        let (checked, _) = checked_program_of(source);
        let admission = crate::core::mir::classify_scalar_collection_admission(&checked);
        assert!(
            matches!(
                admission,
                crate::core::mir::ScalarCollectionAdmission::MixedCoverage
            ),
            "{name} must keep the mixed compatibility boundary: {admission:?}"
        );
    }
}

// The flip set runs one shared MirProgram across all three consumers, and the
// route materialization now succeeds with a complete collection admission —
// the default-entry precondition the CLI pins observe end to end.
#[test]
fn plain_scalar_flip_matrix_agrees_across_consumers() {
    struct PlainScalarCase {
        name: &'static str,
        source: &'static str,
        expected_stdout: &'static str,
    }
    const CASES: &[PlainScalarCase] = &[
        PlainScalarCase {
            name: "bool_match_helper",
            source: r#"
                func label(flag: bool) -> i32 {
                    match flag { true => 41, false => 7 }
                }
                func main() -> i32 {
                    println(label(true))
                    println(label(false))
                    0
                }
            "#,
            expected_stdout: "41\n7\n",
        },
        PlainScalarCase {
            name: "i32_match_default",
            source: r#"
                func classify(v: i32) -> i32 {
                    match v { 0 => 1, 10 => 2, _ => 3 }
                }
                func main() -> i32 {
                    println(classify(0))
                    println(classify(99))
                    0
                }
            "#,
            expected_stdout: "1\n3\n",
        },
        PlainScalarCase {
            name: "string_literal_print",
            source: r#"
                func main() -> i32 {
                    println("hello")
                    println("world")
                    0
                }
            "#,
            expected_stdout: "hello\nworld\n",
        },
        PlainScalarCase {
            name: "root_scalar_assign",
            source: r#"
                func main() -> i32 {
                    let mut x = 0
                    x = 5
                    println(x)
                    0
                }
            "#,
            expected_stdout: "5\n",
        },
        // R6-1052 S11 matrix widening: Multiply/Divide/Remainder on signed
        // integers are admitted by the native validator and the verifier
        // capability gate, so the island's intersection matrix admits them
        // too.  probe_p14 (contracts + x*2 + a/b) is the real-world shape.
        PlainScalarCase {
            name: "int_mul_div_rem",
            source: r#"
                func scale(x: i32) -> i32 {
                    x * 2
                }
                func main() -> i32 {
                    println(scale(21))
                    println(100 / 4)
                    println(47 / 5)
                    println(47 % 5)
                    0
                }
            "#,
            expected_stdout: "42\n25\n9\n2\n",
        },
    ];
    for case in CASES {
        let label = format!("plain scalar case {}", case.name);
        let (checked, excluded_sources) = checked_program_of(case.source);
        let route =
            crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
                .unwrap_or_else(|error| panic!("{label} route materialization failed: {error:?}"));
        assert!(
            matches!(
                route.admission.collection,
                crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
            ),
            "{label} must carry a complete collection admission: {:?}",
            route.admission.collection
        );
        assert!(
            route.materialized_collection_candidate,
            "{label} must materialize its stdout receipt"
        );
        let mir = route.program;
        let digest = mir.canonical_digest();
        assert!(
            crate::verifier::validate_mir_capabilities(&mir).is_ok(),
            "{label} must pass the capability gate"
        );

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
            crate::codegen::CodeGenerator::new(&context, &format!("mir_plain_{}", case.name));
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

// S11 island-gate parity: the scalar literal switch is inside the island
// envelope (the whole graph of a match program validates), while a
// variant-payload switch keeps its fail-closed boundary in the same gate.
#[test]
fn scalar_island_gate_admits_literal_switch_keeps_variant_boundary() {
    let literal_source = r#"
        func label(flag: bool) -> i32 {
            match flag { true => 41, false => 7 }
        }
        func main() -> i32 {
            println(label(true))
            0
        }
    "#;
    let (checked, excluded_sources) = checked_program_of(literal_source);
    let route =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .expect("literal-switch graph must materialize");
    crate::core::mir::validate_scalar_collection_island(&route.program).unwrap_or_else(|errors| {
        panic!("literal switch must be inside the scalar island: {errors:?}")
    });

    let variant_source = r#"
        func main() -> i32 {
            let maybe: Option<i32> = Some(7)
            let tag = match maybe { Some(v) => v, None => 0 }
            println(tag)
            0
        }
    "#;
    let (checked, excluded_sources) = checked_program_of(variant_source);
    let route =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .expect("variant-switch graph must materialize its compatibility graph");
    assert!(
        crate::core::mir::validate_scalar_collection_island(&route.program).is_err(),
        "a variant-payload switch must stay outside the scalar island gate"
    );
}

// The symbolic verifier explores literal switches with equality guards: a
// contracted match function verifies end to end through the route receipt.
// The contract is semantic: the default arm returns v + 1, so
// `result >= 1` needs both switch paths to be explored.
#[test]
fn contracted_match_function_verifies_through_literal_switch() {
    let source = r#"
        func pick(v: i32) -> i32 {
            requires: v >= 0
            ensures: result >= 1
            match v { 0 => 1, _ => v + 1 }
        }
        func main() -> i32 {
            println(pick(0))
            println(pick(5))
            0
        }
    "#;
    let (checked, excluded_sources) = checked_program_of(source);
    let route =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .expect("contracted match program must materialize");
    let mir = route.program;
    let receipt = mir.route_receipt("r6-1052-verifier-v1");
    let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let results = crate::verifier::verify_mir_with_route_receipt(&mir, &receipt, source_hash)
        .expect("contracted match program must verify");
    assert!(
        crate::verifier::canonical_execution_route_verifier_ready(&results, false, false),
        "the literal-switch contract pass must be verifier-ready: {results:?}"
    );
}

// Construction ground-truth guard: an assignment nested in a block is
// outside the MIR Phase 0 scalar-assign face.  The admission is complete
// (the coverage scan is a type scan), so without the guard the default route
// would hard-reject a program legacy runs fine; with the guard the
// materialization returns the explicit compatibility error carrying the
// lowering message and the program keeps its working legacy route.
#[test]
fn nested_assign_construction_downgrades_to_compatibility() {
    let source = r#"
        func main() {
            let mut x = 0
            let flag = true
            if flag { x = 1 }
            println(x)
        }
    "#;
    let (checked, excluded_sources) = checked_program_of(source);
    let admission = crate::core::mir::classify_scalar_collection_admission(&checked);
    assert!(
        matches!(
            admission,
            crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
        ),
        "the nested-assign scan must stay complete (construction is the ground truth): {admission:?}"
    );
    let error =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .expect_err("nested-block assign must not route canonical");
    match error {
        crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility {
            message, ..
        } => {
            assert!(
                message.contains("assign inside a nested block"),
                "the compatibility message must carry the lowering error: {message}"
            );
        }
        other => panic!("nested-block assign must downgrade to compatibility, got: {other:?}"),
    }
}

// The prelude-callee boundary: calling an automatically merged prelude
// function keeps the mixed coverage state, so a program that still depends
// on a compatibility body never silently routes canonical.  Under the
// dispatch exclusions the call into excluded prelude code is a missing-target
// compatibility error — never a Complete rejection and never a canonical
// route candidate — which is exactly what keeps the CLI on the legacy route.
#[test]
fn prelude_callee_keeps_mixed_boundary() {
    let source = r#"func main() { println(clamp(50, 1, 10)) }"#;
    let (checked, excluded_sources) = checked_program_of(source);
    let admission = crate::core::mir::classify_scalar_collection_admission(&checked);
    assert!(
        matches!(
            admission,
            crate::core::mir::ScalarCollectionAdmission::MixedCoverage
        ),
        "prelude callee must keep mixed coverage: {admission:?}"
    );
    let error =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .expect_err("a mixed prelude-calling program must not materialize a canonical route");
    match error {
        crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility {
            admission: compatibility_admission,
            message,
        } => {
            assert!(
                matches!(
                    compatibility_admission.collection,
                    crate::core::mir::ScalarCollectionAdmission::MixedCoverage
                ),
                "the compatibility admission must carry the mixed collection boundary: {:?}",
                compatibility_admission.collection
            );
            assert!(
                message.contains("callee 'function:clamp' is absent"),
                "the compatibility message must carry the missing-target boundary: {message}"
            );
        }
        other => panic!("prelude callee must stay a compatibility input, got: {other:?}"),
    }
}

// ── R6-1109: deterministic generative scalar matrix (goal ⑤) ──────────
//
// splitmix64-seeded straight-line i32 programs driven through the full
// three-consumer differential.  The generator simulates each program in
// Rust (Mimi i32 `/` truncates toward zero and `%` follows the dividend's
// sign exactly like Rust), so expected stdout is computed, not hardcoded;
// the consumers must agree with it and with each other.  Overflow is
// excluded by construction here; the dedicated overflow family below
// covers the generative trap side.

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

struct GenerativeProgram {
    source: String,
    expected_stdout: String,
}

fn generate_overflow_free_program(seed: u64) -> GenerativeProgram {
    const STATEMENTS: u64 = 10;
    let mut rng = SplitMix64(seed);
    let mut acc: i32 = rng.below(51) as i32;
    let mut source = format!("func main() -> i32 {{\n    let mut acc = {acc}\n");
    // The initial value is not printed; only the post-statement printlns are.
    let mut expected = String::new();
    let mut multiplications = 0u64;
    for _ in 0..STATEMENTS {
        let op = rng.below(5);
        // Guarded so the simulation can never overflow: `*` appears at most
        // three times and is rerouted to `+` whenever the magnitude could
        // leave comfortable i32 headroom.
        let op = if op == 2 && (multiplications == 3 || acc.abs() > 100_000) {
            0
        } else {
            op
        };
        let (text, next) = match op {
            0 => {
                let c = rng.below(20) + 1;
                (format!("acc = acc + {c}"), acc.wrapping_add(c as i32))
            }
            1 => {
                let c = rng.below(20) + 1;
                (format!("acc = acc - {c}"), acc.wrapping_sub(c as i32))
            }
            2 => {
                let c = rng.below(2) + 2;
                multiplications += 1;
                (format!("acc = acc * {c}"), acc.wrapping_mul(c as i32))
            }
            3 => {
                let c = rng.below(8) + 2;
                (format!("acc = acc / {c}"), acc.wrapping_div(c as i32))
            }
            _ => {
                let c = rng.below(8) + 2;
                (format!("acc = acc % {c}"), acc.wrapping_rem(c as i32))
            }
        };
        acc = next;
        source.push_str(&format!("    {text}\n    println(acc)\n"));
        expected.push_str(&format!("{acc}\n"));
    }
    source.push_str("    0\n}\n");
    GenerativeProgram {
        source,
        expected_stdout: expected,
    }
}

#[test]
fn generative_scalar_programs_agree_across_consumers() {
    const PROGRAM_COUNT: u64 = 48;
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    for seed in 1..=PROGRAM_COUNT {
        let label = format!("generative scalar seed {seed}");
        let program = generate_overflow_free_program(seed);
        let (checked, excluded_sources) = checked_program_of(&program.source);
        let route =
            crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
                .unwrap_or_else(|error| panic!("{label} route materialization failed: {error:?}"));
        assert!(
            matches!(
                route.admission.collection,
                crate::core::mir::ScalarCollectionAdmission::CompleteCoverage
            ),
            "{label} must stay inside the plain-scalar island: {:?}",
            route.admission.collection
        );
        let mir = route.program;
        let digest = mir.canonical_digest();
        crate::verifier::validate_mir_capabilities(&mir)
            .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));

        let reference = MirReferenceInterpreter::new(&mir)
            .execute_with_output(&NodeId("function:main".into()), &[])
            .unwrap_or_else(|error| panic!("{label} reference failed: {error}"));
        assert_eq!(
            reference.output, program.expected_stdout,
            "{label} reference"
        );

        let bytecode = compile_mir_program(&mir)
            .unwrap_or_else(|error| panic!("{label} bytecode compilation failed: {error:?}"));
        assert!(bytecode.ast.is_none(), "{label} bytecode must be AST-free");
        let mut vm = BytecodeVM::new(bytecode);
        vm.run_value()
            .unwrap_or_else(|error| panic!("{label} bytecode failed: {error}"));
        assert_eq!(vm.stdout(), program.expected_stdout, "{label} bytecode");

        // Native compiles are sampled deterministically (every 4th seed) to
        // keep the suite budget bounded; reference/bytecode cover all seeds.
        if seed % 4 == 0 && can_link() {
            let context = inkwell::context::Context::create();
            let mut generator =
                crate::codegen::CodeGenerator::new(&context, &format!("mir_generative_{seed}"));
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
            assert_eq!(
                native.stdout, program.expected_stdout,
                "{label} native stdout"
            );
            assert_eq!(native.stderr, "", "{label} native stderr");
        }
        assert_eq!(mir.canonical_digest(), digest, "{label} digest stability");
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

// Generative negative family: seeded i32 self-multiplication chains that
// deterministically overflow must trap identically in every consumer
// (SD-7 checked arithmetic, E0802-class) instead of wrapping silently.
#[test]
fn generative_scalar_overflow_traps_agree_across_consumers() {
    const TRAP_PROGRAMS: u64 = 8;
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    for seed in 0..TRAP_PROGRAMS {
        let label = format!("generative overflow seed {seed}");
        let mut rng = SplitMix64(0x0F1_2E3 ^ seed);
        let base = 46_342 + rng.below(500_000) as i32;
        let source = format!(
            "func main() -> i32 {{\n    let mut acc = {base}\n    acc = acc * acc\n    acc = acc * acc\n    0\n}}\n"
        );
        let (checked, excluded_sources) = checked_program_of(&source);
        let route =
            crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
                .unwrap_or_else(|error| panic!("{label} route materialization failed: {error:?}"));
        let mir = route.program;

        let reference = MirReferenceInterpreter::new(&mir)
            .execute_with_output(&NodeId("function:main".into()), &[])
            .expect_err("{label} reference must trap on overflow");
        let reference_text = reference.to_string();
        assert!(
            reference_text.contains("E0802") || reference_text.contains("overflow"),
            "{label} reference trap must name the overflow: {reference_text}"
        );

        let bytecode = compile_mir_program(&mir)
            .unwrap_or_else(|error| panic!("{label} bytecode compilation failed: {error:?}"));
        let mut vm = BytecodeVM::new(bytecode);
        let error = vm
            .run_value()
            .expect_err("{label} bytecode must trap on overflow");
        let error_text = error.to_string();
        assert!(
            error_text.contains("E0802") || error_text.contains("overflow"),
            "{label} bytecode trap must name the overflow: {error_text}"
        );

        if can_link() {
            let context = inkwell::context::Context::create();
            let mut generator = crate::codegen::CodeGenerator::new(
                &context,
                &format!("mir_generative_overflow_{seed}"),
            );
            generator
                .compile_mir_native(&mir)
                .unwrap_or_else(|error| panic!("{label} native emission failed: {error:?}"));
            let native = link_and_observe_canonical_mir(&generator)
                .unwrap_or_else(|error| panic!("{label} native execution failed: {error}"));
            assert_ne!(
                native.exit_code,
                Some(0),
                "{label} native must not exit cleanly on overflow: {native:?}"
            );
        }
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}
