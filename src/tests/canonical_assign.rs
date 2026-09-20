//! Canonical scalar-assign differential tests (R6-1049).
//!
//! R6-1049 excavates the MIR Phase 0 assign face: a direct local target with
//! no projections, an Identity or NumericWiden conversion into a signed
//! 32/64-bit integer or bool local, and an RHS that the surrounding island
//! face already admits.  The differential matrix below pins three-consumer
//! equivalence (reference interpreter, AST-free bytecode VM, native emitter
//! on one shared `MirProgram`) for that exact face, so the "construction
//! capability widens => classifier parity" rule (the R6-1048 lesson) has a
//! proof to point at.  Faces outside the scalar assign contract — float
//! targets and projected/aggregate targets — stay fail-closed at their
//! owning layer and are pinned too.

use super::*;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter};
use crate::core::NodeId;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};

fn materialize_assign(source: &str, label: &str) -> MirProgram {
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

// Generative scalar-assign matrix: every case declares one `mut` local and
// reassigns it through checker-legal direct-local statements (Identity or
// NumericWiden conversion), printing after each reassignment.  The expected
// stdout is computed at generation time from the case table (never by
// executing a backend), and every case shares one `MirProgram` across the
// three consumers.
#[test]
fn scalar_assign_matrix_agrees_across_consumers() {
    struct ScalarAssignCase {
        name: &'static str,
        variable: &'static str,
        declaration: &'static str,
        // (reassignment statement, printed value) pairs executed in order.
        steps: &'static [(&'static str, &'static str)],
    }
    const CASES: &[ScalarAssignCase] = &[
        // Straight-line i32 reassignment: the value survives the statement
        // boundary and each assign observes the previous one.
        ScalarAssignCase {
            name: "i32_sequential",
            variable: "acc",
            declaration: "let mut acc = 1",
            steps: &[
                ("acc = acc + 1", "2"),
                ("acc = acc * 10", "20"),
                ("acc = 7", "7"),
            ],
        },
        // i64 with a bare-integer-literal RHS: the checker records a
        // NumericWiden conversion receipt, so the assign must materialize the
        // same Convert receipt the call-argument face uses.
        ScalarAssignCase {
            name: "i64_widen_from_literal",
            variable: "acc",
            declaration: "let mut acc = 3037000499 as i64",
            steps: &[("acc = acc + 1", "3037000500"), ("acc = 5", "5")],
        },
        // Bool reassignment pins the non-numeric Copy scalar of the face.
        ScalarAssignCase {
            name: "bool_reassign",
            variable: "flag",
            declaration: "let mut flag = false",
            steps: &[("flag = true", "true"), ("flag = false", "false")],
        },
    ];
    for case in CASES {
        let mut body = format!("    {}\n", case.declaration);
        let mut expected_stdout = String::new();
        for (statement, printed) in case.steps {
            body.push_str(&format!(
                "    {statement}\n    println({})\n",
                case.variable
            ));
            expected_stdout.push_str(printed);
            expected_stdout.push('\n');
        }
        body.push_str("    0\n");
        let source = format!("func main() -> i32 {{\n{body}}}\n",);
        let label = format!("scalar assign case {}", case.name);
        let mir = materialize_assign(&source, &label);
        assert!(
            crate::verifier::validate_mir_capabilities(&mir).is_ok(),
            "{label} must pass the capability gate"
        );
        let digest = mir.canonical_digest();

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
            crate::codegen::CodeGenerator::new(&context, &format!("mir_assign_{}", case.name));
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

// A float local target is deliberately outside the R6-1049 scalar-assign
// contract (no differential proof exists for float reassignment yet); it
// must keep the compatibility route via a construction failure, not enter a
// canonical graph unproven.
#[test]
fn float_assign_target_stays_fail_closed_at_construction() {
    let source = r#"
        func main() -> i32 {
            let mut acc = 0.5
            acc = 1.5
            0
        }
    "#;
    let mir = MirProgram::from_checked_program(&checked_program_of(source));
    let error = mir.expect_err("float assign must stay fail-closed");
    assert!(
        error.to_string().contains("assign"),
        "the construction failure must name the assign boundary: {error:?}"
    );
}
