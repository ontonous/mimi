//! Canonical scalar literal-switch differential tests (R6-1051).
//!
//! `match` over a Copy-scalar scrutinee (bool, signed i32/i64) lowers as a
//! `Switch` whose arms carry `MirSwitchCase::Literal` cases.  R6-1051 admits
//! this face through one shared catalog contract
//! (`MirTypeCatalog::validate_scalar_switch`) that the verifier capability
//! gate, the native structural validator, and the bytecode consumer all pass
//! through before their mechanical lowerings.  The matrix pins
//! three-consumer equivalence (reference interpreter, AST-free bytecode VM,
//! native emitter on one shared `MirProgram`).  Floating-scrutinee dispatch
//! and non-exhaustive integer matches stay fail-closed upstream (parser /
//! E0215) and are pinned at those boundaries.

use super::*;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter};
use crate::core::mir::{MirBlockId, MirEdgeId, MirSwitchArm, MirSwitchCase};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn materialize_switch(source: &str, label: &str) -> MirProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|diags| panic!("check {label}: {diags:?}"));
    MirProgram::from_checked_program(&checked)
        .unwrap_or_else(|error| panic!("materialize {label}: {error:?}"))
}

// Generative scalar-switch matrix: every case dispatches at least one literal
// switch (bool and integer scrutinees, expression and let-bound shapes) plus
// non-switch effects for shape mixing; the expected stdout is computed from
// the case table and every case shares one `MirProgram` across the three
// consumers.
#[test]
fn scalar_literal_switch_matrix_agrees_across_consumers() {
    struct ScalarSwitchCase {
        name: &'static str,
        source: &'static str,
        expected_stdout: &'static str,
    }
    const CASES: &[ScalarSwitchCase] = &[
        // The R6-1050 probe shape: a bool match in a helper called from main.
        ScalarSwitchCase {
            name: "bool_match_helper",
            source: r#"
                func label(flag: bool) -> i32 {
                    match flag {
                        true => 41,
                        false => 7,
                    }
                }
                func main() -> i32 {
                    println(label(true))
                    println(label(false))
                    0
                }
            "#,
            expected_stdout: "41\n7\n",
        },
        // Integer scrutinee with a default arm: the default path exercises
        // the fall-through edges of the chain lowering in every consumer.
        // (Pattern literals type as i32, so the scrutinee is i32.)
        ScalarSwitchCase {
            name: "i32_match_default",
            source: r#"
                func classify(v: i32) -> i32 {
                    match v {
                        0 => 1,
                        10 => 2,
                        _ => 3,
                    }
                }
                func main() -> i32 {
                    println(classify(0))
                    println(classify(10))
                    println(classify(99))
                    0
                }
            "#,
            expected_stdout: "1\n2\n3\n",
        },
        // The match-expression face bound by `let`: arm bodies travel to the
        // join block as edge arguments.
        ScalarSwitchCase {
            name: "bool_match_in_let",
            source: r#"
                func main() -> i32 {
                    let flag = true
                    let tag = match flag { true => 5, false => 6 }
                    println(tag)
                    0
                }
            "#,
            expected_stdout: "5\n",
        },
        // Both scrutinee kinds in one function to prove the two chain shapes
        // coexist without block-name or register collisions.
        ScalarSwitchCase {
            name: "mixed_scrutinee_switches",
            source: r#"
                func main() -> i32 {
                    let b = match true { true => 1, false => 2 }
                    let i = match 7 { 5 => 50, 7 => 70, _ => 90 }
                    println(b)
                    println(i)
                    0
                }
            "#,
            expected_stdout: "1\n70\n",
        },
    ];
    for case in CASES {
        let label = format!("scalar switch case {}", case.name);
        let mir = materialize_switch(case.source, &label);
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
            crate::codegen::CodeGenerator::new(&context, &format!("mir_switch_{}", case.name));
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

// The Option<string> assign-face fixture (the R6-1049/R6-1050 boundary
// program) is the motivating combination for this slice: an Option<string>
// unwrap in main plus a bool match in a helper plus an owned string print.
// After the scalar literal-switch face opened, the whole graph passes the
// capability gate and all three consumers byte-for-byte, while the default
// entry keeps its compatibility disposition (no option-island switch
// candidacy exists in main itself).
#[test]
fn option_string_assign_face_runs_canonical_after_scalar_switch() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mir_option_string_assign_face.mimi"),
    )
    .expect("fixture");
    let label = "option string assign face";
    let mir = materialize_switch(&source, label);
    crate::verifier::validate_mir_capabilities(&mir)
        .unwrap_or_else(|errors| panic!("{label} capability gate: {errors:?}"));

    let expected_stdout = "yes\n2\n";
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
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_switch_assign_face");
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
}

// The shared scalar-switch contract, probed directly: fabricated arms against
// a materialized catalog pin the positive shapes and every fail-closed
// negative (missing default, duplicate case, non-scalar scrutinee ABI).
#[test]
fn scalar_switch_catalog_contract_pins() {
    let source = r#"
        func label(flag: bool) -> i32 {
            match flag { true => 41, false => 7 }
        }
        func main() -> i32 {
            let probe_float = 0.5
            drop(probe_float)
            let probe_int = match 3 { 1 => 10, _ => 20 }
            println(label(true))
            println(probe_int)
            0
        }
    "#;
    let mir = materialize_switch(source, "scalar switch catalog pins");
    let function = mir
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main function");

    let mut bool_ty = None;
    let mut int_ty = None;
    let mut float_ty = None;
    for info in function.values.values() {
        let Some(desc) = mir.type_catalog().get(&info.ty) else {
            continue;
        };
        match desc.abi {
            crate::core::mir::types::MirAbiClass::Bool => bool_ty = Some(info.ty.clone()),
            crate::core::mir::types::MirAbiClass::Integer {
                bits: 32,
                signed: true,
            } => int_ty = Some(info.ty.clone()),
            crate::core::mir::types::MirAbiClass::Float { bits: 64 } => {
                float_ty = Some(info.ty.clone())
            }
            _ => {}
        }
    }
    let bool_ty = bool_ty.expect("catalog must contain the bool scrutinee");
    let int_ty = int_ty.expect("catalog must contain the i32 scrutinee");
    let float_ty = float_ty.expect("catalog must contain an f64 TypeDesc");

    let catalog = mir.type_catalog();
    let arm = |case| MirSwitchArm {
        edge: MirEdgeId::new("probe:edge").expect("edge id"),
        target: MirBlockId::new("probe:target").expect("block id"),
        arguments: Vec::new(),
        bindings: Vec::new(),
        case,
    };
    use crate::core::ResolvedLiteral;

    // Positive shapes.
    catalog
        .validate_scalar_switch(
            &bool_ty,
            &[
                arm(MirSwitchCase::Literal(ResolvedLiteral::Bool(true))),
                arm(MirSwitchCase::Literal(ResolvedLiteral::Bool(false))),
            ],
        )
        .expect("complete bool coverage is admitted");
    catalog
        .validate_scalar_switch(
            &bool_ty,
            &[
                arm(MirSwitchCase::Literal(ResolvedLiteral::Bool(false))),
                arm(MirSwitchCase::Default),
            ],
        )
        .expect("bool literal plus default is admitted");
    catalog
        .validate_scalar_switch(
            &int_ty,
            &[
                arm(MirSwitchCase::Literal(ResolvedLiteral::Int(3))),
                arm(MirSwitchCase::Default),
            ],
        )
        .expect("integer literal plus default is admitted");

    // Fail-closed negatives.
    let missing_default = catalog.validate_scalar_switch(
        &bool_ty,
        &[arm(MirSwitchCase::Literal(ResolvedLiteral::Bool(true)))],
    );
    assert!(
        missing_default
            .unwrap_err()
            .contains("must cover true and false"),
        "single-bool-arm switch must be rejected as non-exhaustive"
    );
    let duplicate = catalog.validate_scalar_switch(
        &bool_ty,
        &[
            arm(MirSwitchCase::Literal(ResolvedLiteral::Bool(true))),
            arm(MirSwitchCase::Literal(ResolvedLiteral::Bool(true))),
        ],
    );
    assert!(
        duplicate.unwrap_err().contains("repeated"),
        "duplicate bool case must be rejected"
    );
    let int_without_default = catalog.validate_scalar_switch(
        &int_ty,
        &[arm(MirSwitchCase::Literal(ResolvedLiteral::Int(3)))],
    );
    assert!(
        int_without_default
            .unwrap_err()
            .contains("must carry a default arm"),
        "integer switch without default must be rejected"
    );
    let float_scrutinee = catalog.validate_scalar_switch(&float_ty, &[arm(MirSwitchCase::Default)]);
    assert!(
        float_scrutinee
            .unwrap_err()
            .contains("outside the scalar switch contract"),
        "float scrutinee must stay outside the scalar switch contract"
    );
}

// Upstream fail-closed pins: floating match patterns are a parse error and a
// non-exhaustive integer match is checker E0215, so the backend contract
// never observes those shapes from source.
#[test]
fn scalar_switch_negatives_fail_closed_upstream() {
    let float_source = r#"
        func main() -> i32 {
            let x = match 0.5 { 0.5 => 1, _ => 2 }
            println(x)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(float_source)
        .tokenize()
        .expect("lex");
    let parsed = crate::parser::Parser::new(tokens).parse_file();
    assert!(
        parsed.is_err(),
        "float match patterns must stay a parse error"
    );

    let exhaustiveness_source = r#"
        func main() -> i32 {
            let x = match 3 { 1 => 10, 2 => 20 }
            println(x)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(exhaustiveness_source)
        .tokenize()
        .expect("lex");
    let parsed = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let diagnostics = crate::core::check_program(&parsed)
        .expect_err("non-exhaustive integer match must fail checking");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0215")),
        "the exhaustiveness error must surface: {diagnostics:?}"
    );
}
