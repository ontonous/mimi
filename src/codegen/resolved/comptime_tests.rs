use inkwell::context::Context;

use crate::codegen::CodeGenerator;

fn checked_program(
    source: &str,
) -> Result<crate::core::CheckedProgram, Vec<crate::diagnostic::Diagnostic>> {
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("comptime fixture lexes");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("comptime fixture parses");
    crate::core::check_program(&file)
}

fn compile(source: &str) -> Result<String, Vec<crate::diagnostic::Diagnostic>> {
    let program = checked_program(source)?;
    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, "typed_comptime_fixture");
    codegen.compile_checked(&program)?;
    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "typed comptime folding must not invoke the legacy emitter"
    );
    Ok(codegen.module.print_to_string().to_string())
}

fn comptime_item_error(source: &str) -> String {
    let program = checked_program(source).expect("comptime contract fixture typechecks");
    let (_, error) = super::comptime::fold_comptime_items(&program);
    error.expect("the top-level comptime item should fail its contract")
}

#[test]
fn typed_comptime_item_and_constant_initializer_fold_in_production_codegen() {
    let ir = compile(
        r#"
const BASE: i32 = 42
comptime func answer() -> i32 { BASE }
func main() -> i32 { comptime { answer() } }
"#,
    )
    .expect("typed comptime item should compile");

    assert!(
        ir.contains("ret i32 42"),
        "expected folded result in LLVM IR: {ir}"
    );
}

#[test]
fn typed_comptime_expression_folds_its_own_local_bindings() {
    let ir = compile(
        r#"
func main() -> i32 {
    comptime {
        let base: i32 = 40
        base + 2
    }
}
"#,
    )
    .expect("typed comptime expression should compile");

    assert!(
        ir.contains("ret i32 42"),
        "expected folded result in LLVM IR: {ir}"
    );
}

#[test]
fn typed_comptime_expression_preserves_recursive_function_evaluation() {
    let ir = compile(
        r#"
comptime func factorial(n: i64) -> i64 {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}
func main() -> i64 { comptime { factorial(5) } }
"#,
    )
    .expect("recursive comptime function should fold");

    assert!(
        ir.contains("ret i64 120"),
        "expected the recursively folded result in LLVM IR: {ir}"
    );
}

#[test]
fn typed_comptime_return_escapes_nested_if_block() {
    let ir = compile(
        r#"
comptime func choose(flag: bool) -> i32 {
    if flag { return 7 }
    2
}
func main() -> i32 { comptime { choose(true) } }
"#,
    )
    .expect("a return from a nested comptime block should end the function");

    assert!(
        ir.contains("ret i32 7"),
        "the later expression must not replace an early return: {ir}"
    );
}

#[test]
fn inline_comptime_keeps_contract_checks_disabled() {
    let ir = compile(
        r#"
comptime func rejected_precondition() -> i32 {
    requires: false
    8
}
comptime func rejected_postcondition() -> i32 {
    ensures: false
    9
}
func main() -> i32 {
    comptime {
        rejected_precondition()
        rejected_postcondition()
    }
}
"#,
    )
    .expect("inline comptime evaluation historically disables contracts");

    assert!(
        ir.contains("ret i32 9"),
        "inline comptime should evaluate the function bodies without contract guards: {ir}"
    );
}

#[test]
fn top_level_comptime_items_keep_requires_contract_errors() {
    let error = comptime_item_error(
        r#"
comptime func rejected_precondition() -> i32 {
    requires: false
    8
}
func main() -> i32 { 0 }
"#,
    );

    assert_eq!(
        error,
        "[E0808] requires condition failed for 'rejected_precondition': false"
    );
}

#[test]
fn top_level_comptime_items_keep_ensures_contract_errors() {
    let error = comptime_item_error(
        r#"
comptime func rejected_postcondition() -> i32 {
    ensures: false
    9
}
func main() -> i32 { 0 }
"#,
    );

    assert_eq!(
        error,
        "[E0808] ensures condition failed for 'rejected_postcondition': false"
    );
}

#[test]
fn typed_comptime_expression_preserves_integer_cast_truncation() {
    let ir = compile(
        r#"
func main() -> i32 { comptime { 3000000000 as i32 } }
"#,
    )
    .expect("typed comptime cast should compile");

    assert!(
        ir.contains("ret i32 -1294967296"),
        "expected the folded i32 wrap result in LLVM IR: {ir}"
    );
}

#[test]
fn typed_comptime_expression_rounds_integer_casts_to_f32_width() {
    let ir = compile(
        r#"
func main() -> i32 { comptime { 16777217 as f32 as i32 } }
"#,
    )
    .expect("typed comptime casts should round to IEEE binary32");

    assert!(
        ir.contains("ret i32 16777216"),
        "16777217 is rounded to 16777216 by an f32 cast: {ir}"
    );
}

#[test]
fn typed_comptime_f64_f32_f64_casts_preserve_binary32_rounding() {
    let ir = compile(
        r#"
func main() -> i32 {
    comptime {
        if (16777217.0 as f32 as f64) == 16777216.0 { 1 } else { 0 }
    }
}
"#,
    )
    .expect("f64-to-f32 must narrow and f32-to-f64 must preserve the rounded value");

    assert!(
        ir.contains("ret i32 1"),
        "the f32 round trip must retain binary32 precision: {ir}"
    );
}

#[test]
fn typed_comptime_f32_narrowing_handles_underflow_and_overflow() {
    let ir = compile(
        r#"
func main() -> i32 {
    comptime {
        let mut underflow = 1.0
        let mut overflow = 0.0
        let mut infinity = 0.0
        ieee_float {
            underflow = 1e-50 as f32 as f64
            overflow = 3.4028236e38 as f32 as f64
            infinity = 1.0 / 0.0
        }
        if underflow == 0.0 && overflow == infinity { 1 } else { 0 }
    }
}
"#,
    )
    .expect("binary32 narrowing should preserve IEEE underflow and overflow");

    assert!(
        ir.contains("ret i32 1"),
        "f32 underflow should become zero and overflow should become infinity: {ir}"
    );
}

#[test]
fn typed_comptime_float_to_integer_casts_saturate_nan_and_infinities() {
    let cases = [
        ("i32", "0.0 / 0.0", "ret i32 0"),
        ("i32", "1.0 / 0.0", "ret i32 2147483647"),
        ("i32", "-1.0 / 0.0", "ret i32 -2147483648"),
        ("i64", "0.0 / 0.0", "ret i64 0"),
        ("i64", "1.0 / 0.0", "ret i64 9223372036854775807"),
        ("i64", "-1.0 / 0.0", "ret i64 -9223372036854775808"),
    ];

    for (target, value, expected) in cases {
        let source = format!(
            "func main() -> {target} {{ comptime {{ ieee_float {{ ({value}) as {target} }} }} }}"
        );
        let ir = compile(&source).expect("float-to-integer saturation should fold");
        assert!(
            ir.contains(expected),
            "expected {expected} for {value} as {target}, got: {ir}"
        );
    }
}

#[test]
fn typed_comptime_expression_preserves_i32_shift_masking_and_power_wrap() {
    let shifted = compile(
        r#"
func main() -> i32 {
    comptime {
        let value: i32 = 1024
        value >> 40
    }
}
"#,
    )
    .expect("typed comptime i32 shift should mask the count modulo 32");
    assert!(
        shifted.contains("ret i32 4"),
        "expected i32 shift-count masking in LLVM IR: {shifted}"
    );

    let powered = compile(
        r#"
func main() -> i32 {
    comptime {
        let base: i32 = 2
        base ** 31
    }
}
"#,
    )
    .expect("typed comptime i32 power should wrap at the i32 width");
    assert!(
        powered.contains("ret i32 -2147483648"),
        "expected the existing i32 power-wrap rule in LLVM IR: {powered}"
    );
}

#[test]
fn typed_comptime_expression_keeps_ieee_scope_and_float_to_i32_saturation() {
    let ir = compile(
        r#"
func main() -> i32 {
    comptime {
        ieee_float {
            (1.0 / 0.0) as i32
        }
    }
}
"#,
    )
    .expect("IEEE float scope should allow infinity and VM-compatible i32 casts");

    assert!(
        ir.contains("ret i32 2147483647"),
        "Rust/VM float-to-i32 conversion saturates positive infinity: {ir}"
    );
}

#[test]
fn typed_comptime_expression_keeps_i32_arithmetic_overflow_error() {
    let diagnostics = compile(
        r#"
func main() -> i32 {
    comptime {
        let maximum: i32 = 2147483647
        maximum + 1
    }
}
"#,
    )
    .expect_err("typed comptime i32 addition must keep the established overflow trap");

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("E0802")),
        "expected E0802 for i32 addition overflow, got {diagnostics:?}"
    );
}

#[test]
fn failed_unused_comptime_item_keeps_the_existing_nonfatal_rule() {
    let ir = compile(
        r#"
comptime func broken() -> i64 {
    4611686018427387904 + 4611686018427387904
}
func main() -> i32 { 0 }
"#,
    )
    .expect("an unused comptime item failure is warning-only");

    assert!(ir.contains("ret i32 0"), "main should still compile: {ir}");
}

#[test]
fn referenced_comptime_item_failure_is_a_compile_error() {
    let diagnostics = compile(
        r#"
comptime func broken() -> i64 {
    4611686018427387904 + 4611686018427387904
}
func main() -> i64 { broken() }
"#,
    )
    .expect_err("a failed comptime item cannot be called at runtime");

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("comptime function")),
        "expected a source-level comptime diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn runtime_dependent_comptime_expression_is_rejected() {
    let diagnostics = compile(
        r#"
func main() -> i32 {
    let runtime_value: i32 = 41
    comptime { runtime_value + 1 }
}
"#,
    )
    .expect_err("comptime expressions cannot capture a runtime local");

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("comptime block fold failed")),
        "expected an explicit comptime-fold diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn typed_comptime_expression_reports_integer_overflow() {
    let diagnostics = compile(
        r#"
func main() -> i64 {
    comptime { 4611686018427387904 + 4611686018427387904 }
}
"#,
    )
    .expect_err("compile-time arithmetic overflow must remain an error");

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("E0802")),
        "expected the established comptime overflow diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn typed_comptime_expression_keeps_float_divide_by_zero_error_outside_ieee_scope() {
    let diagnostics = compile(
        r#"
func divide_by_zero() -> f64 { comptime { 1.0 / 0.0 } }
func main() -> i32 { 0 }
"#,
    )
    .expect_err("float divide-by-zero remains an error outside ieee_float");

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("E0801")),
        "expected the established comptime division error, got {diagnostics:?}"
    );
}

#[test]
fn duplicate_comptime_definitions_keep_the_checker_duplicate_diagnostic() {
    let diagnostics = checked_program(
        r#"
comptime func same() -> i32 { 1 }
comptime func same() -> i32 { 2 }
func main() -> i32 { 0 }
"#,
    )
    .expect_err("duplicate comptime functions are rejected by the checker");

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0402")),
        "expected the existing duplicate-definition diagnostic E0402, got {diagnostics:?}"
    );
}
