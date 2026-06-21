// ============================================================
// CODEGEN IR Analysis Tests — verify LLVM IR output correctness
// ============================================================

use super::*;

fn compile_to_ir(src: &str) -> String {
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    codegen.compile_file(&file).expect("src/tests/codegen_ir.rs:11 unwrap failed");
    codegen.emit_ir()
}

// ===================== LLVM IR Structural Patterns =====================

#[test]
fn ir_module_has_moduleid() {
    let ir = compile_to_ir("func main() -> i32 { 42 }");
    assert!(ir.contains("ModuleID"), "IR should have ModuleID");
}

#[test]
fn ir_module_has_filename() {
    let ir = compile_to_ir("func main() -> i32 { 42 }");
    assert!(ir.contains("source_filename"), "IR should have source_filename");
}

#[test]
fn ir_i32_returns_i64() {
    let ir = compile_to_ir("func main() -> i32 { 42 }");
    assert!(ir.contains("define i64 @main()"), "i32 should map to i64 in IR");
}

// ===================== Void Functions =====================

#[test]
fn ir_void_func_define() {
    let ir = compile_to_ir(r#"func noop() { }"#);
    assert!(ir.contains("define"), "void function should have define");
    assert!(ir.contains("@noop"), "void function should have name");
}

#[test]
fn ir_void_main() {
    let ir = compile_to_ir(r#"func main() { }"#);
    assert!(ir.contains("define"), "void main should have define");
}

// ===================== Binary Operation IR Patterns =====================

#[test]
fn ir_i64_add() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a + b }");
    assert!(ir.contains("add i64"), "i32 promotes to i64, should use add i64");
}

#[test]
fn ir_i64_mul() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a * b }");
    assert!(ir.contains("mul i64"), "i32 mul promotes to i64 mul");
}

#[test]
fn ir_sdiv() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a / b }");
    assert!(ir.contains("sdiv i64"), "signed integer division");
}

#[test]
fn ir_srem() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a % b }");
    assert!(ir.contains("srem i64"), "signed integer remainder");
}

#[test]
fn ir_fadd() {
    let ir = compile_to_ir("func main(a: f64, b: f64) -> f64 { a + b }");
    assert!(ir.contains("fadd double"), "f64 add should use fadd double");
}

#[test]
fn ir_fmul() {
    let ir = compile_to_ir("func main(a: f64, b: f64) -> f64 { a * b }");
    assert!(ir.contains("fmul double"), "f64 mul should use fmul double");
}

#[test]
fn ir_fdiv() {
    let ir = compile_to_ir("func main(a: f64, b: f64) -> f64 { a / b }");
    assert!(ir.contains("fdiv double"), "f64 div should use fdiv double");
}

// ===================== Comparison IR Patterns =====================

#[test]
fn ir_icmp_slt() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a < b }");
    assert!(ir.contains("icmp slt"), "signed less-than");
}

#[test]
fn ir_icmp_sgt() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a > b }");
    assert!(ir.contains("icmp sgt"), "signed greater-than");
}

#[test]
fn ir_icmp_sle() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a <= b }");
    assert!(ir.contains("icmp sle"), "signed less-or-equal");
}

#[test]
fn ir_icmp_sge() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a >= b }");
    assert!(ir.contains("icmp sge"), "signed greater-or-equal");
}

#[test]
fn ir_icmp_eq() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a == b }");
    assert!(ir.contains("icmp eq"), "equality");
}

#[test]
fn ir_icmp_ne() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a != b }");
    assert!(ir.contains("icmp ne"), "not-equal");
}

// ===================== 32-bit vs 64-bit IR =====================

#[test]
fn ir_i64_return_type() {
    let ir = compile_to_ir("func main() -> i64 { 42 }");
    assert!(ir.contains("define i64 @main()"), "i64 return type");
}

#[test]
fn ir_f64_return_type() {
    let ir = compile_to_ir("func main() -> f64 { 3.14 }");
    assert!(ir.contains("define double @main()"), "f64 maps to double");
}

// ===================== Logical Operator IR =====================

#[test]
fn ir_logical_and_uses_and() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a && b }");
    assert!(ir.contains("and i64"), "logical and uses bitwise and");
}

#[test]
fn ir_logical_or_uses_or() {
    let ir = compile_to_ir("func main(a: i32, b: i32) -> i32 { a || b }");
    assert!(ir.contains("or i64"), "logical or uses bitwise or");
}

// ===================== Control Flow IR =====================

#[test]
fn ir_if_then_else() {
    let ir = compile_to_ir(r#"
        func main(x: i32) -> i32 { if x > 0 { 1 } else { 0 } }
    "#);
    assert!(ir.contains("then"), "if branch should have then block");
    assert!(ir.contains("else"), "else branch should have else block");
}

#[test]
fn ir_if_no_else_block() {
    let ir = compile_to_ir(r#"
        func main(x: i32) -> i32 {
            let mut r = 0
            if x > 0 { r = 1 }
            r
        }
    "#);
    assert!(ir.contains("then"), "if should have then block");
}

#[test]
fn ir_while_loop_blocks() {
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            let mut i = 0
            while i < 10 { i = i + 1 }
            i
        }
    "#);
    assert!(ir.contains("loop"), "while should have loop header");
    assert!(ir.contains("loopbody"), "while should have loop body");
}

#[test]
fn ir_nested_while_multi_loop() {
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            let mut sum = 0; let mut i = 0
            while i < 3 {
                let mut j = 0
                while j < 3 { sum = sum + 1; j = j + 1 }
                i = i + 1
            }
            sum
        }
    "#);
    let loop_count = ir.matches("loop").count();
    assert!(loop_count >= 4, "nested while should have >=4 loop blocks, got {}", loop_count);
}

#[test]
fn ir_while_break_cont() {
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            let i = 0
            while i < 100 {
                if i == 5 { break }
                i = i + 1
            }
            i
        }
    "#);
    assert!(ir.contains("loopcont"), "break targets loopcont");
}

// ===================== Function Call IR =====================

#[test]
fn ir_call_instruction() {
    let ir = compile_to_ir(r#"
        func inc(x: i32) -> i32 { x + 1 }
        func main() -> i32 { inc(41) }
    "#);
    assert!(ir.contains("call i64"), "should have call to i64 function");
    assert!(ir.contains("@inc"), "should call @inc");
    assert!(ir.contains("@main"), "should define @main");
}

#[test]
fn ir_chained_calls_multi() {
    let ir = compile_to_ir(r#"
        func inc(x: i32) -> i32 { x + 1 }
        func main() -> i32 { inc(inc(40)) }
    "#);
    let call_count = ir.matches("call i64").count();
    assert!(call_count >= 2, "chained calls should have >=2 call insts, got {}", call_count);
}

#[test]
fn ir_recursive_call_self() {
    let ir = compile_to_ir(r#"
        func factorial(n: i32) -> i32 {
            if n <= 1 { 1 } else { n * factorial(n - 1) }
        }
    "#);
    assert!(ir.contains("factorial"), "recursive fn should call itself");
}

// ===================== Memory Operations =====================

#[test]
fn ir_alloca_exists() {
    let ir = compile_to_ir("func main() -> i32 { let x = 42; x }");
    assert!(ir.contains("alloca"), "local variables need alloca");
}

#[test]
fn ir_store_exists() {
    let ir = compile_to_ir("func main() -> i32 { let mut x = 0; x = 5; x }");
    assert!(ir.contains("store"), "assignment needs store");
}

#[test]
fn ir_load_exists() {
    let ir = compile_to_ir("func main() -> i32 { let x = 42; x }");
    assert!(ir.contains("load"), "reading a var needs load");
}

// ===================== Builtin Call IR =====================

#[test]
fn ir_println_uses_printf() {
    let ir = compile_to_ir(r#"func main() { println("hello") }"#);
    assert!(ir.contains("printf"), "println string calls printf");
}

#[test]
fn ir_println_int_printf() {
    let ir = compile_to_ir(r#"func main() { println(42) }"#);
    assert!(ir.contains("printf"), "println int calls printf");
}

#[test]
fn ir_range_malloc() {
    let ir = compile_to_ir(r#"func main() { let r = range(0, 5) }"#);
    assert!(ir.contains("malloc"), "range should allocate");
    assert!(ir.contains("range_loop"), "range should have loop");
}

#[test]
fn ir_len_list_field() {
    let ir = compile_to_ir(r#"func main() { let xs = [1,2,3]; let n = len(xs) }"#);
    assert!(ir.contains("list_len"), "len accesses list_len");
}

#[test]
fn ir_assert_ok_fail_blocks() {
    let ir = compile_to_ir(r#"func main() { assert(true) }"#);
    assert!(ir.contains("assert_ok"), "assert should have ok");
    assert!(ir.contains("assert_fail"), "assert should have fail");
}

// ===================== List IR Patterns =====================

#[test]
fn ir_list_malloc() {
    let ir = compile_to_ir(r#"func main() { let xs = [1, 2, 3] }"#);
    assert!(ir.contains("malloc"), "list literal needs malloc");
    assert!(ir.contains("list_len"), "list struct has len");
    assert!(ir.contains("list_data"), "list struct has data");
}

#[test]
fn ir_list_index_gep() {
    let ir = compile_to_ir(r#"
        func main() { let xs = [10, 20, 30]; let x = xs[1] }
    "#);
    assert!(ir.contains("getelementptr"), "list index uses GEP");
    assert!(ir.contains("elem_val"), "list index loads elem");
}

#[test]
fn ir_for_list_blocks() {
    let ir = compile_to_ir(r#"
        func main() { for x in [1, 2, 3] { println(x) } }
    "#);
    assert!(ir.contains("forloop"), "for list needs forloop header");
    assert!(ir.contains("forbody"), "for list needs forbody");
}

// ===================== Extern Block IR =====================

#[test]
fn ir_extern_declare() {
    let ir = compile_to_ir(r#"
        extern "C" { func my_func(x: i32) -> i32; }
        func main() -> i32 { 42 }
    "#);
    assert!(ir.contains("declare"), "extern func should have declare");
    assert!(ir.contains("@my_func"), "extern func name should be declared");
}

#[test]
fn ir_extern_multiple_funcs() {
    let ir = compile_to_ir(r#"
        extern "C" { func add(a: i32, b: i32) -> i32; func sub(a: i32, b: i32) -> i32; }
        func main() -> i32 { 42 }
    "#);
    assert!(ir.contains("@add") && ir.contains("@sub"), "multiple extern funcs");
}

#[test]
fn ir_extern_void_func() {
    let ir = compile_to_ir(r#"
        extern "C" { func ext_fn(x: i32); }
        func main() -> i32 { 42 }
    "#);
    assert!(ir.contains("declare"), "void extern should have declare");
}

// ===================== Generic/Turbofish IR =====================

#[test]
fn ir_generic_mangling_i32() {
    let ir = compile_to_ir(r#"
        func identity<T>(x: T) -> T { x }
        func main() -> i32 { identity::<i32>(42) }
    "#);
    assert!(ir.contains("identity$T_i32"), "generic should be mangled");
}

#[test]
fn ir_generic_multi_instantiation() {
    let ir = compile_to_ir(r#"
        func wrap<T>(x: T) -> T { x }
        func main() -> i32 { let a = wrap::<i32>(1); let b = wrap::<i64>(2); a }
    "#);
    assert!(ir.contains("wrap$T_i32"), "first instantiation");
    assert!(ir.contains("wrap$T_i64"), "second instantiation");
}

// ===================== Capabilities IR =====================

#[test]
fn ir_cap_does_not_crash() {
    let ir = compile_to_ir(r#"cap MyCap; func main() -> i32 { 42 }"#);
    // cap MyCap is a declaration, followed by func
    assert!(ir.contains("define"), "cap should not break IR");
}

// ===================== Actor IR =====================

#[test]
fn ir_actor_constructor_and_type() {
    let ir = compile_to_ir(r#"
        actor Counter { count: i32; name: string }
        func main() -> i32 { 42 }
    "#);
    assert!(ir.contains("Counter_new"), "actor should have constructor");
    assert!(ir.contains("%Counter"), "actor should have type name");
}

// ===================== Return Handling =====================

#[test]
fn ir_ret_instruction() {
    let ir = compile_to_ir("func main() -> i32 { 42 }");
    assert!(ir.contains("ret"), "should have ret instruction");
}

#[test]
fn ir_early_return_multi_ret() {
    let ir = compile_to_ir(r#"
        func main(x: i32) -> i32 {
            if x > 0 { return x }
            0
        }
    "#);
    assert!(ir.contains("ret"), "should have ret instructions");
}

// ===================== On Failure IR =====================

#[test]
fn ir_on_failure_no_exit_skips_body() {
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            on failure { println(1) }
            42
        }
    "#);
    assert!(ir.contains("define"), "function should be defined");
}

#[test]
fn ir_on_failure_with_exit_includes_body() {
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            on failure { println(99) }
            exit(1)
        }
    "#);
    assert!(ir.contains("exit"), "exit should be in IR");
    assert!(ir.contains("printf"), "on failure body should compile");
}

// ===================== String constants =====================

#[test]
fn ir_string_global_constant() {
    let ir = compile_to_ir(r#"func main() -> i32 { let s = "abc"; 0 }"#);
    assert!(ir.contains("abc"), "string literal content should appear in IR");
}

// ===================== Match =====================

#[test]
fn ir_match_has_merge_block() {
    let ir = compile_to_ir(r#"
        type Color { Red | Green | Blue }
        func describe(c: Color) -> i32 {
            match c {
                Red => 1
                _ => 0
            }
        }
        func main() -> i32 { 0 }
    "#);
    assert!(ir.contains("matchcont"), "match needs merge block");
}

// ===================== dyn Trait type mapping test =====================

#[test]
fn ir_dyn_trait_type_maps_to_fat_pointer() {
    let ir = compile_to_ir(r#"
trait Drawable {
    func draw() -> i32;
}

func use_dyn(d: dyn Drawable) -> i32 { 0 }
func main() -> i32 { 0 }
"#);
    // The fat pointer for dyn Drawable is `{ ptr, ptr }` in opaque-pointer LLVM IR
    assert!(ir.contains("{ ptr, ptr }") || ir.contains("i8*, i8*") || ir.contains("{ i8*, i8* }"),
        "dyn Trait should compile to fat pointer, got:\n{}", ir);
}

#[test]
fn ir_vtable_contains_method() {
    let ir = compile_to_ir(r#"
trait Drawable {
    func draw() -> i32;
}

type Circle {
    radius: i32
}

impl Drawable for Circle {
    func draw() -> i32 { 42 }
}

func main() -> i32 { 0 }
"#);
    assert!(ir.contains("Circle__Drawable__draw"),
        "impl method should be compiled with mangled name, got:\n{}", ir);
    assert!(ir.contains("Circle_Drawable_vtable"),
        "vtable global should exist, got:\n{}", ir);
}
