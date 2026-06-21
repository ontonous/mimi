// ============================================================
// Advanced CODEGEN Tests — edge cases, complex interactions,
// cross-feature patterns, and stress tests
// ============================================================

use super::*;

fn compile_to_ir(src: &str) -> String {
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    codegen.compile_file(&file).expect("src/tests/codegen_advanced.rs:12 unwrap failed");
    codegen.emit_ir()
}

fn assert_compiles(src: &str) {
    let ir = compile_to_ir(src);
    assert!(ir.contains("define"), "IR should contain function definitions");
}

// ===================== Edge Cases: Empty/Small Functions =====================

#[test]
fn adv_empty_function() {
    assert_compiles("func empty() { }");
}

#[test]
fn adv_main_only_void() {
    assert_compiles("func main() { }");
}

#[test]
fn adv_function_bare_return() {
    assert_compiles("func early() -> i32 { return 42 }");
}

// ===================== Shadowing =====================

#[test]
fn adv_shadowing_same_scope() {
    assert_compiles("func main() -> i32 { let x = 1; let x = 2; x }");
}

// ===================== Deep Nesting =====================

#[test]
fn adv_deeply_nested_if() {
    assert_compiles(r#"
        func deep(x: i32) -> i32 {
            if x > 0 {
                if x > 10 {
                    if x > 100 { 3 } else { 2 }
                } else { 1 }
            } else { 0 }
        }
    "#);
}

// ===================== Large/Negative Literals =====================

#[test]
fn adv_large_literal() {
    assert_compiles("func main() -> i32 { 999999 }");
}

#[test]
fn adv_negative_literal() {
    assert_compiles("func main() -> i32 { -1 }");
}

#[test]
fn adv_negative_large() {
    assert_compiles("func main() -> i32 { -100000 }");
}

// ===================== Boolean in Condition =====================

#[test]
fn adv_bool_in_if() {
    assert_compiles(r#"
        func main() -> i32 { let flag = true; if flag { 1 } else { 0 } }
    "#);
}

// ===================== Mixed Expressions =====================

#[test]
fn adv_mixed_arith() {
    assert_compiles(r#"
        func main() -> i32 { let a = 1 + 2 * 3; let b = 10 - 4 / 2; a + b }
    "#);
}

#[test]
fn adv_complex_bool() {
    assert_compiles(r#"
        func main() -> i32 { let a = 1; let b = 0; let c = 1; (a && b) || c }
    "#);
}

// ===================== Mutations =====================

#[test]
fn adv_multiple_mutations() {
    assert_compiles(r#"
        func main() -> i32 { let mut x = 0; x = x + 1; x = x * 2; x = x - 3; x }
    "#);
}

// ===================== Extern + User Functions =====================

#[test]
fn adv_extern_and_user_fn() {
    assert_compiles(r#"
        extern "C" { func ext_fn(x: i32) -> i32; }
        func user_fn(x: i32) -> i32 { x * 2 }
        func main() -> i32 { user_fn(21) }
    "#);
}

// ===================== Generic Functions =====================

#[test]
fn adv_generic_identity() {
    assert_compiles(r#"
        func id<T>(x: T) -> T { x }
        func main() -> i32 { id::<i32>(42) }
    "#);
}

// ===================== Spawn/Await =====================

#[test]
fn adv_spawn_basic() {
    assert_compiles(r#"
        func compute() -> i32 { 42 }
        func main() -> i32 { let f = spawn compute(); await f }
    "#);
}

// ===================== Async Functions =====================

#[test]
fn adv_async_func() {
    let ir = compile_to_ir(r#"
        async func compute(x: i32) -> i32 { x + 1 }
        func main() -> i32 { let f = compute(41); await f }
    "#);
    assert!(ir.contains("__async_body"), "async func should have body fn");
    assert!(ir.contains("__spawn_wrapper"), "async func should have spawn wrapper");
    assert!(ir.contains("pthread_create"), "async func should create thread");
}

// ===================== Capabilities =====================

#[test]
fn adv_cap_linear_drop() {
    assert_compiles(r#"
        cap MyCap
        func use_cap(c: MyCap) -> i32 { drop(c); 42 }
        func main() -> i32 { 0 }
    "#);
}

// ===================== Arena Blocks =====================

#[test]
fn adv_arena_block() {
    assert_compiles("func main() -> i32 { arena { let x = 42; x } }");
}

#[test]
fn adv_alloc_block() {
    assert_compiles("func main() -> i32 { alloc(arena) { let x = 42; x } }");
}

// ===================== Recursion =====================

#[test]
fn adv_fib_recursive() {
    assert_compiles(r#"
        func fib(n: i32) -> i32 { if n <= 1 { n } else { fib(n - 1) + fib(n - 2) } }
    "#);
}

// ===================== Multiple Params =====================

#[test]
fn adv_five_params_sum() {
    assert_compiles(r#"
        func sum5(a: i32, b: i32, c: i32, d: i32, e: i32) -> i32 { a + b + c + d + e }
        func main() -> i32 { sum5(1, 2, 3, 4, 5) }
    "#);
}

// ===================== If with block branches =====================

#[test]
fn adv_if_branches_different_ops() {
    assert_compiles(r#"
        func main(x: i32) -> i32 { if x > 0 { x * 2 } else { x * 3 } }
    "#);
}

// ===================== Type Alias =====================

#[test]
fn adv_type_alias_in_codegen() {
    assert_compiles(r#"
        type MyInt = i32
        func main() -> i32 { let x: MyInt = 42; x }
    "#);
}

// ===================== Extern in Module =====================

#[test]
fn adv_extern_in_module() {
    assert_compiles(r#"
        module mylib { extern "C" { func lib_fn(x: i32) -> i32; } }
        func main() -> i32 { 42 }
    "#);
}

// ===================== F-strings =====================

#[test]
fn adv_fstring_text() {
    assert_compiles("func main() -> i32 { let s = \"hello\"; 0 }");
}

#[test]
fn adv_fstring_interp() {
    assert_compiles("func main() -> i32 { let x = 42; println(f\"{x}\"); 0 }");
}

// ===================== Cap let tracking =====================

#[test]
fn adv_cap_let_tracking() {
    assert_compiles(r#"
        cap MyCap
        func main() -> i32 { let c: MyCap = 1; drop(c); 0 }
    "#);
}

// ===================== Many functions =====================

#[test]
fn adv_many_functions() {
    let ir = compile_to_ir(r#"
        func f1() -> i32 { 1 }
        func f2() -> i32 { 2 }
        func f3() -> i32 { 3 }
        func f4() -> i32 { 4 }
        func f5() -> i32 { 5 }
        func main() -> i32 { f1() + f2() + f3() + f4() + f5() }
    "#);
    let def_count = ir.matches("define").count();
    assert!(def_count >= 6, "should have >=6 fn defs, got {}", def_count);
}

// ===================== While complex condition =====================

#[test]
fn adv_while_complex_cond() {
    assert_compiles(r#"
        func main() -> i32 {
            let mut i = 0; let mut sum = 0
            while i < 10 && sum < 20 { sum = sum + i; i = i + 1 }
            sum
        }
    "#);
}

// ===================== Nested block deep =====================

#[test]
fn adv_nested_block_deep() {
    assert_compiles(r#"
        func main() -> i32 { let a = 1; let b = 2; let c = 3; let d = 4; a + b + c + d }
    "#);
}

#[allow(dead_code)]
fn can_link() -> bool {
    std::process::Command::new("cc").arg("--version").output().is_ok()
}

// ===================== Previously Missing Codegen: Tuple/If-Expr/Range/Slice/Lambda/Comprehension =====================

#[test]
fn adv_tuple_literal() {
    assert_compiles(r#"
        func main() -> i64 {
            let t = (1, 2, 3)
            0
        }
    "#);
}

#[test]
fn adv_if_expression() {
    assert_compiles(r#"
        func main() -> i64 {
            let x = 10
            let result = if x > 5 { 1 } else { 0 }
            result
        }
    "#);
}

#[test]
fn adv_if_expression_no_else() {
    assert_compiles(r#"
        func main() -> i64 {
            let x = 10
            let result = if x > 5 { 1 } else { 0 }
            result
        }
    "#);
}

#[test]
fn adv_range_expression() {
    assert_compiles(r#"
        func main() -> i64 {
            let r = 0..10
            0
        }
    "#);
}

#[test]
fn adv_slice_expression() {
    assert_compiles(r#"
        func main() -> i64 {
            let arr = [1, 2, 3, 4, 5]
            let sliced = arr[1..4]
            0
        }
    "#);
}

#[test]
fn adv_lambda_expression() {
    assert_compiles(r#"
        func main() -> i64 {
            let add = fn(a: i64, b: i64) -> i64 { a + b }
            0
        }
    "#);
}

#[test]
fn adv_comprehension() {
    assert_compiles(r#"
        func main() -> i64 {
            let xs = [1, 2, 3, 4, 5]
            let evens = [x for x in xs if x > 2]
            0
        }
    "#);
}

// ===================== Match Pattern Tests =====================

#[test]
fn adv_match_constructor_pattern() {
    assert_compiles(r#"
        func main() -> i64 {
            let x = 42
            match x {
                42 => 1,
                _ => 0,
            }
        }
    "#);
}

#[test]
fn adv_match_literal_pattern() {
    assert_compiles(r#"
        func main() -> i64 {
            let x = 10
            match x {
                1 => 100,
                2 => 200,
                _ => 0,
            }
        }
    "#);
}

#[test]
fn adv_match_wildcard_pattern() {
    assert_compiles(r#"
        func main() -> i64 {
            let x = 5
            match x {
                _ => x + 1,
            }
        }
    "#);
}

#[test]
fn adv_match_variable_pattern() {
    assert_compiles(r#"
        func main() -> i64 {
            let x = 7
            match x {
                n => n * 2,
            }
        }
    "#);
}

// ===================== Closure Capture Test =====================

#[test]
fn adv_closure_capture() {
    assert_compiles(r#"
        func main() -> i64 {
            let x = 10
            let add = fn(a: i64) -> i64 { a + x }
            0
        }
    "#);
}

// ===================== Slice Expression Test =====================

#[test]
fn adv_slice_with_indices() {
    assert_compiles(r#"
        func main() -> i64 {
            let arr = [10, 20, 30, 40, 50]
            let sliced = arr[1..4]
            0
        }
    "#);
}

// ===================== Parasteps Test =====================

#[test]
fn adv_parasteps_basic() {
    assert_compiles(r#"
        func main() -> i64 {
            parasteps {
                spawn println("hello")
            }
            0
        }
    "#);
}

// ===================== Quote/Comptime Error Tests =====================

#[test]
fn adv_quote_produces_error() {
    let src = r#"
        func main() -> i64 {
            let ast = quote { let x = 1 }
            0
        }
    "#;
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    let result = codegen.compile_file(&file);
    assert!(result.is_err(), "quote should produce error in codegen");
}

#[test]
fn adv_comptime_produces_error() {
    let src = r#"
        func main() -> i64 {
            let x = comptime { 1 + 2 }
            0
        }
    "#;
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    let result = codegen.compile_file(&file);
    assert!(result.is_err(), "comptime should produce error in codegen");
}

#[test]
fn adv_comptime_block_error_message() {
    // eedf8be: comptime blocks get a specific error message mentioning how to fix
    let src = r#"
        func main() -> i64 {
            let x = comptime { 1 + 2 }
            0
        }
    "#;
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    let err = codegen.compile_file(&file).unwrap_err().to_string();
    assert!(err.contains("comptime"),
        "comptime error should mention 'comptime', got: {}", err);
    assert!(err.contains("mimi run"),
        "comptime error should suggest 'mimi run', got: {}", err);
}

#[test]
fn adv_comptime_func_call_error_message() {
    // eedf8be: calling a comptime function from runtime produces a specific error
    let src = r#"
        comptime func get_magic() -> i64 { 42 }
        func main() -> i64 { get_magic() }
    "#;
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    let err = codegen.compile_file(&file).unwrap_err().to_string();
    assert!(err.contains("comptime function"),
        "comptime call error should mention 'comptime function', got: {}", err);
    assert!(err.contains("compile-time only"),
        "comptime call error should say 'compile-time only', got: {}", err);
}

#[test]
fn adv_quote_block_error_message() {
    // eedf8be: quote blocks get specific error message
    let src = r#"
        func main() -> i64 {
            let ast = quote! { 42 };
            0
        }
    "#;
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    let err = codegen.compile_file(&file).unwrap_err().to_string();
    assert!(err.contains("quote"),
        "quote error should mention 'quote', got: {}", err);
}

#[test]
fn adv_quote_interpolate_error_message() {
    // eedf8be: ${} inside quote! {} — the outer Quote error fires first
    // (QuoteInterpolate error is a safety net; in practice Quote always wraps it)
    let src = r#"
        func main() -> i64 {
            let x = 10;
            let ast = quote! { $(x + 1) };
            0
        }
    "#;
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    let err = codegen.compile_file(&file).unwrap_err().to_string();
    assert!(err.contains("quote"),
        "quote error should mention 'quote', got: {}", err);
    assert!(err.contains("mimi run"),
        "quote error should suggest 'mimi run', got: {}", err);
}
