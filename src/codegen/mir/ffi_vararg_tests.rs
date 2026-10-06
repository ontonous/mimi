//! Native ABI probes for checker-receipted C variadic calls.
//!
//! These tests exercise the actual LLVM declaration and C varargs boundary:
//! source values that need default promotion arrive at C as `int`/`double`,
//! and call sites with different tails share only the fixed declaration prefix.

use crate::codegen::CodeGenerator;
use crate::core::mir::reference::MirProgram;
use crate::core::mir::MirFfiCallContract;
use crate::tests::E2EConfig;
use inkwell::context::Context;

const SOURCE: &str = include_str!("../../../tests/fixtures/mir_native_ffi_vararg_l3.mimi");
const C_SOURCE: &str = include_str!("../../../tests/fixtures/mir_native_ffi_vararg_l3.c");

fn canonical_program(source: &str) -> MirProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    MirProgram::from_checked_program(&checked).expect("canonical MIR")
}

fn compile_rejecting_ffi_mutation(
    mut program: MirProgram,
    mutate: impl FnOnce(&mut MirFfiCallContract),
) -> Vec<crate::diagnostic::Diagnostic> {
    let mut receipts = program.ffi_calls().clone();
    let receipt = receipts
        .values_mut()
        .next()
        .expect("source contains at least one FFI call");
    mutate(receipt);
    program.replace_ffi_calls_for_test_only(receipts);

    let context = Context::create();
    let mut generator = CodeGenerator::new(&context, "mir_native_ffi_vararg_reject");
    let diagnostics = generator
        .compile_mir_native(&program)
        .expect_err("forged vararg metadata must be rejected before native emission");
    assert!(
        generator
            .module
            .get_function("mimi_ffi_vararg_probe")
            .is_none(),
        "forged FFI receipt must fail before emitting the foreign declaration"
    );
    diagnostics
}

#[test]
fn native_varargs_promote_per_call_and_declare_only_the_fixed_prefix() {
    let program = canonical_program(SOURCE);
    assert_eq!(program.ffi_calls().len(), 6);
    for receipt in program.ffi_calls().values() {
        assert_eq!(receipt.fixed_argument_count, 1);
        assert!(receipt.variadic);
        assert_eq!(receipt.parameter_types.len(), receipt.arguments.len());
        assert_eq!(
            receipt.variadic_arguments.len(),
            receipt.arguments.len() - receipt.fixed_argument_count
        );
    }

    let context = Context::create();
    let mut generator = CodeGenerator::new(&context, "mir_native_ffi_vararg_l3");
    generator
        .compile_mir_native(&program)
        .expect("native emitter consumes the typed variadic receipts");
    generator
        .module
        .verify()
        .expect("variadic native LLVM module verifies");
    let ir = generator.emit_ir();
    assert!(
        ir.contains("declare i32 @mimi_ffi_vararg_probe(i32, ...)")
            || ir.contains("declare i32 (i32, ...) @mimi_ffi_vararg_probe"),
        "the C declaration must contain only its fixed i32 prefix and vararg marker: {ir}"
    );

    let native = crate::tests::link_and_observe_canonical_mir_with_config(
        &generator,
        &E2EConfig {
            extra_c_src: Some(C_SOURCE.to_owned()),
            ..E2EConfig::default()
        },
    )
    .expect("link and run native C varargs observer");
    assert_eq!(native.exit_code, Some(0), "{native:?}");
}

#[test]
fn native_varargs_reject_wrong_promotion_receipt() {
    let source = r#"
extern "C" { func mimi_ffi_vararg_probe(tag: i32 ...) -> i32; }
func main() -> i32 { mimi_ffi_vararg_probe(2, 1.5 as f32) }
"#;
    let diagnostics = compile_rejecting_ffi_mutation(canonical_program(source), |receipt| {
        receipt.variadic_arguments[0].promotion =
            crate::core::mir::MirFfiDefaultPromotion::Identity;
    });
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message.to_ascii_lowercase().contains("ffi")
            || diagnostic.message.to_ascii_lowercase().contains("variadic")
    }));
}

#[test]
fn native_varargs_reject_a_tail_attached_to_nonvariadic_shape() {
    let source = r#"
extern "C" { func mimi_ffi_vararg_probe(tag: i32 ...) -> i32; }
func main() -> i32 { mimi_ffi_vararg_probe(1, true) }
"#;
    let diagnostics = compile_rejecting_ffi_mutation(canonical_program(source), |receipt| {
        receipt.variadic = false;
    });
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message.to_ascii_lowercase().contains("ffi")
            || diagnostic.message.to_ascii_lowercase().contains("variadic")
    }));
}

#[test]
fn native_varargs_reject_physical_tail_type_that_disagrees_with_promotion() {
    let source = r#"
extern "C" { func mimi_ffi_vararg_probe(tag: i32 ...) -> i32; }
func main() -> i32 { mimi_ffi_vararg_probe(2, 1.5 as f32) }
"#;
    let program = canonical_program(source);
    let diagnostics = compile_rejecting_ffi_mutation(program, |receipt| {
        receipt.parameter_types[receipt.fixed_argument_count] =
            receipt.variadic_arguments[0].source_type.clone();
    });
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.message.to_ascii_lowercase().contains("ffi")
            || diagnostic.message.to_ascii_lowercase().contains("variadic")
    }));
}

#[test]
fn native_varargs_reject_a_missing_per_argument_tail_receipt() {
    let source = r#"
extern "C" { func mimi_ffi_vararg_probe(tag: i32 ...) -> i32; }
func main() -> i32 { mimi_ffi_vararg_probe(1, true) }
"#;
    let diagnostics = compile_rejecting_ffi_mutation(canonical_program(source), |receipt| {
        receipt.variadic_arguments.clear();
    });
    assert!(!diagnostics.is_empty());
}

#[test]
fn native_varargs_reject_a_receipt_bound_to_different_actual_arguments() {
    let source = r#"
extern "C" { func mimi_ffi_vararg_probe(tag: i32 ...) -> i32; }
func main() -> i32 { mimi_ffi_vararg_probe(1, true) }
"#;
    let diagnostics = compile_rejecting_ffi_mutation(canonical_program(source), |receipt| {
        receipt.arguments[1] = receipt.arguments[0].clone();
    });
    assert!(!diagnostics.is_empty());
}

#[test]
fn native_varargs_reject_a_forged_source_node_identity() {
    let source = r#"
extern "C" { func mimi_ffi_vararg_probe(tag: i32 ...) -> i32; }
func main() -> i32 { mimi_ffi_vararg_probe(1, true) }
"#;
    let diagnostics = compile_rejecting_ffi_mutation(canonical_program(source), |receipt| {
        receipt.variadic_arguments[0].source_node = crate::core::NodeId("forged-node".into());
    });
    assert!(!diagnostics.is_empty());
}
