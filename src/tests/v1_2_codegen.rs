use super::*;

fn compile_to_ir(src: &str) -> String {
    compile_checked_to_ir(src)
}

fn compile_checked_to_ir(src: &str) -> String {
    let file = parse(src);
    let checked = crate::core::check_program(&file).expect("scalar source should check");
    crate::core::CheckedProgram::reset_test_legacy_codegen_function_calls();

    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "checked_scalar_test");
    codegen
        .compile_checked(&checked)
        .expect("checked scalar source should compile");

    assert!(
        crate::core::CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "canonical scalar codegen must not invoke the legacy function emitter"
    );
    codegen.emit_ir()
}

fn assert_compiles(src: &str) {
    let ir = compile_to_ir(src);
    assert!(
        ir.contains("define"),
        "IR should contain function definitions"
    );
}

fn assert_checked_compiles(src: &str) {
    let ir = compile_checked_to_ir(src);
    assert!(
        ir.contains("define"),
        "IR should contain function definitions"
    );
}

fn assert_ir_contains(src: &str, pattern: &str) {
    let ir = compile_to_ir(src);
    assert!(
        ir.contains(pattern),
        "IR should contain '{}': {}",
        pattern,
        &ir[..300.min(ir.len())]
    );
}

#[test]
fn codegen_simple_return() {
    assert_checked_compiles("func main() -> i32 { 42 }");
}

#[test]
fn codegen_return_zero() {
    assert_checked_compiles("func main() -> i32 { 0 }");
}

#[test]
fn codegen_add() {
    assert_checked_compiles("func main() -> i32 { 1 + 2 }");
}

#[test]
fn codegen_sub() {
    assert_checked_compiles("func main() -> i32 { 10 - 3 }");
}

#[test]
fn codegen_mul() {
    assert_checked_compiles("func main() -> i32 { 6 * 7 }");
}

#[test]
fn codegen_div() {
    assert_checked_compiles("func main() -> i32 { 12 / 4 }");
}

#[test]
fn codegen_mod() {
    assert_checked_compiles("func main() -> i32 { 10 % 3 }");
}

#[test]
fn codegen_negation() {
    assert_checked_compiles("func main() -> i32 { -5 }");
}

#[test]
fn codegen_not() {
    assert_compiles("func main() -> i32 { not(0) }");
}

#[test]
fn codegen_eq_cmp() {
    assert_compiles("func main() -> i32 { 1 == 1 }");
}

#[test]
fn codegen_ne_cmp() {
    assert_compiles("func main() -> i32 { 1 != 2 }");
}

#[test]
fn codegen_lt() {
    assert_compiles("func main() -> i32 { 1 < 2 }");
}

#[test]
fn codegen_gt() {
    assert_compiles("func main() -> i32 { 2 > 1 }");
}

#[test]
fn codegen_le() {
    assert_compiles("func main() -> i32 { 1 <= 1 }");
}

#[test]
fn codegen_ge() {
    assert_compiles("func main() -> i32 { 2 >= 1 }");
}

#[test]
fn codegen_and() {
    assert_compiles("func main() -> i32 { let a = 1; let b = 1; a && b }");
}

#[test]
fn codegen_or() {
    assert_compiles("func main() -> i32 { let a = 1; let b = 0; a || b }");
}

#[test]
fn codegen_function_param() {
    assert_compiles("func double(x: i32) -> i32 { x + x }");
}

#[test]
fn codegen_two_params() {
    assert_compiles("func add(x: i32, y: i32) -> i32 { x + y }");
}

#[test]
fn codegen_let_binding() {
    assert_checked_compiles("func main() -> i32 { let x = 10; x }");
}

#[test]
fn codegen_assign() {
    assert_checked_compiles("func main() -> i32 { let mut x = 0; x = 5; x }");
}

#[test]
fn codegen_multiple_stmts() {
    assert_checked_compiles("func main() -> i32 { let x = 1; let y = 2; x + y }");
}

#[test]
fn codegen_bool_return() {
    assert_compiles("func main() -> i32 { true }");
}

#[test]
fn codegen_string_literal() {
    assert_compiles("func main() -> string { \"hello\" }");
}

#[test]
fn codegen_float_literal() {
    assert_compiles("func main() -> f64 { 3.14 }");
}

#[test]
fn codegen_nested_expr() {
    assert_checked_compiles("func main() -> i32 { (1 + 2) * 3 }");
}

#[test]
fn codegen_function_call() {
    assert_checked_compiles(
        r#"
func inc(x: i32) -> i32 { x + 1 }
func main() -> i32 { inc(41) }
"#,
    );
}

#[test]
fn codegen_chained_calls() {
    assert_checked_compiles(
        r#"
func inc(x: i32) -> i32 { x + 1 }
func main() -> i32 { inc(inc(40)) }
"#,
    );
}

#[test]
fn codegen_with_return() {
    assert_compiles("func main() -> i32 { return 42; }");
}

#[test]
fn codegen_multiple_functions() {
    assert_checked_compiles(
        r#"
func a() -> i32 { 1 }
func b() -> i32 { 2 }
func main() -> i32 { a() + b() }
"#,
    );
}

#[test]
fn codegen_ir_has_main() {
    let ir = compile_checked_to_ir("func main() -> i32 { 42 }");
    assert!(
        ir.contains("define i32 @main(i32"),
        "i32 returns i32 (A1 restoration); main carries the C (argc, argv) pair since 0.35.23"
    );
}

#[test]
fn codegen_ir_has_add() {
    let ir = compile_to_ir("func add(x: i32, y: i32) -> i32 { x + y }");
    assert!(
        ir.contains("define i32 @add(i32"),
        "i32 params are i32 (A1 restoration)"
    );
}

#[test]
fn codegen_emit_ir() {
    let ir = compile_checked_to_ir("func main() -> i32 { 42 }");
    assert!(ir.starts_with("; ModuleID"));
}

#[test]
fn codegen_entry_block() {
    let ir = compile_checked_to_ir("func main() -> i32 { 42 }");
    assert!(ir.contains("entry:"));
}

#[test]
fn codegen_alloca() {
    let ir = compile_checked_to_ir("func main() -> i32 { let x = 10; x }");
    assert!(ir.contains("alloca"));
}

#[test]
fn codegen_store_load() {
    let ir = compile_checked_to_ir("func main() -> i32 { let x = 10; x }");
    assert!(ir.contains("store"));
    assert!(ir.contains("load"));
}

#[test]
fn codegen_add_instruction() {
    assert_ir_contains("func main(a: i32, b: i32) -> i32 { a + b }", "add");
}

#[test]
fn codegen_mul_instruction() {
    assert_ir_contains("func main(a: i32, b: i32) -> i32 { a * b }", "mul");
}

#[test]
fn codegen_div_instruction() {
    assert_ir_contains("func main(a: i32, b: i32) -> i32 { a / b }", "sdiv");
}

#[test]
fn codegen_rem_instruction() {
    assert_ir_contains("func main(a: i32, b: i32) -> i32 { a % b }", "srem");
}

#[test]
fn codegen_neg_instruction() {
    assert_ir_contains("func main(x: i32) -> i32 { -x }", "sub");
}

#[test]
fn codegen_icmp() {
    assert_ir_contains("func main(a: i32, b: i32) -> i32 { a < b }", "icmp");
}

#[test]
fn codegen_and_instruction() {
    // Wave-2 2026-08-05 (full audit §7, wave1-review §2-A): the old assertion
    // pinned the EAGER lowering (`and` over both operands). Eager evaluation
    // was exactly the bug the audit fixed (codegen/expr/operator.rs
    // compile_short_circuit_expr): the bytecode VM short-circuits, so the
    // eager lowering trapped on effects the VM never reaches and ran side
    // effects on the skipped branch. `&&` now lowers to short-circuit control
    // flow. For the i32 operands here the LHS is coerced to a truthiness
    // condition (`icmp ne 0`, mirroring the VM's is_truthy) and the result is
    // merged by an i32 phi over the RHS-evaluated arm (sc_rhs) and the
    // constant-0 arm (sc_const).
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a && b }");
    assert!(
        ir.contains("icmp ne i32"),
        "and: non-bool LHS is coerced to a truthiness condition (icmp ne 0): {}",
        ir
    );
    assert!(
        ir.contains("br i1")
            && ir.contains("label %sc_rhs, label %sc_const")
            && ir.contains("phi i32"),
        "and must lower to short-circuit control flow (cond_br to sc_rhs/sc_const + merge phi), \
         not an eager bitwise and: {}",
        ir
    );
    assert!(
        !ir.contains("and i32"),
        "no eager bitwise and over the operands: {}",
        ir
    );
}

#[test]
fn codegen_or_instruction() {
    assert_ir_contains("func main(a: i32, b: i32) -> i32 { a || b }", "or");
}

#[test]
fn codegen_ret_instruction() {
    let ir = compile_checked_to_ir("func main() -> i32 { 42 }");
    assert!(ir.contains("ret"));
}

#[test]
fn codegen_void_return() {
    assert_compiles("func noop() { }");
}

#[test]
fn codegen_large_literal() {
    assert_checked_compiles("func main() -> i32 { 999999 }");
}

#[test]
fn codegen_complex_expr() {
    assert_checked_compiles("func main() -> i32 { 1 + 2 * 3 - 4 / 2 }");
}
