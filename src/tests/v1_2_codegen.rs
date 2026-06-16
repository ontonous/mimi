use super::*;

// ============================================================
// T600: LLVM Codegen
// ============================================================

fn compile_to_ir(src: &str) -> String {
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    codegen.compile_file(&file).unwrap();
    codegen.emit_ir()
}

fn assert_compiles(src: &str) {
    let ir = compile_to_ir(src);
    assert!(ir.contains("define"), "IR should contain function definitions");
}

fn assert_ir_contains(src: &str, pattern: &str) {
    let ir = compile_to_ir(src);
    assert!(ir.contains(pattern), "IR should contain '{}': {}", pattern, &ir[..300.min(ir.len())]);
}

#[test]
fn codegen_simple_return() {
    assert_compiles("func main() -> i32 { 42 }");
}

#[test]
fn codegen_return_zero() {
    assert_compiles("func main() -> i32 { 0 }");
}

#[test]
fn codegen_add() {
    assert_compiles("func main() -> i32 { 1 + 2 }");
}

#[test]
fn codegen_sub() {
    assert_compiles("func main() -> i32 { 10 - 3 }");
}

#[test]
fn codegen_mul() {
    assert_compiles("func main() -> i32 { 6 * 7 }");
}

#[test]
fn codegen_div() {
    assert_compiles("func main() -> i32 { 12 / 4 }");
}

#[test]
fn codegen_mod() {
    assert_compiles("func main() -> i32 { 10 % 3 }");
}

#[test]
fn codegen_negation() {
    assert_compiles("func main() -> i32 { -5 }");
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
    assert_compiles("func main() -> i32 { let x = 10; x }");
}

#[test]
fn codegen_assign() {
    assert_compiles("func main() -> i32 { let mut x = 0; x = 5; x }");
}

#[test]
fn codegen_multiple_stmts() {
    assert_compiles("func main() -> i32 { let x = 1; let y = 2; x + y }");
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
    assert_compiles("func main() -> i32 { (1 + 2) * 3 }");
}

#[test]
fn codegen_function_call() {
    assert_compiles(r#"
func inc(x: i32) -> i32 { x + 1 }
func main() -> i32 { inc(41) }
"#);
}

#[test]
fn codegen_chained_calls() {
    assert_compiles(r#"
func inc(x: i32) -> i32 { x + 1 }
func main() -> i32 { inc(inc(40)) }
"#);
}

#[test]
fn codegen_with_return() {
    assert_compiles("func main() -> i32 { return 42; }");
}

#[test]
fn codegen_multiple_functions() {
    assert_compiles(r#"
func a() -> i32 { 1 }
func b() -> i32 { 2 }
func main() -> i32 { a() + b() }
"#);
}

#[test]
fn codegen_ir_has_main() {
    let ir = compile_to_ir("func main() -> i32 { 42 }");
    assert!(ir.contains("define i64 @main()"));
}

#[test]
fn codegen_ir_has_add() {
    let ir = compile_to_ir("func add(x: i32, y: i32) -> i32 { x + y }");
    assert!(ir.contains("define i64 @add(i64"));
}

#[test]
fn codegen_emit_ir() {
    let ir = compile_to_ir("func main() -> i32 { 42 }");
    assert!(ir.starts_with("; ModuleID"));
}

#[test]
fn codegen_entry_block() {
    let ir = compile_to_ir("func main() -> i32 { 42 }");
    assert!(ir.contains("entry:"));
}

#[test]
fn codegen_alloca() {
    let ir = compile_to_ir("func main() -> i32 { let x = 10; x }");
    assert!(ir.contains("alloca"));
}

#[test]
fn codegen_store_load() {
    let ir = compile_to_ir("func main() -> i32 { let x = 10; x }");
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
    assert_ir_contains("func main(a: i32, b: i32) -> i32 { a && b }", "and");
}

#[test]
fn codegen_or_instruction() {
    assert_ir_contains("func main(a: i32, b: i32) -> i32 { a || b }", "or");
}

#[test]
fn codegen_ret_instruction() {
    let ir = compile_to_ir("func main() -> i32 { 42 }");
    assert!(ir.contains("ret"));
}

#[test]
fn codegen_void_return() {
    assert_compiles("func noop() { }");
}

#[test]
fn codegen_large_literal() {
    assert_compiles("func main() -> i32 { 999999 }");
}

#[test]
fn codegen_complex_expr() {
    assert_compiles("func main() -> i32 { 1 + 2 * 3 - 4 / 2 }");
}
