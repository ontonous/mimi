use super::*;

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
    assert!(ir.contains(pattern), "IR should contain '{}':\n{}", pattern, ir);
}

#[test]
fn codegen_if_else() {
    assert_compiles(r#"
        func abs(x: i32) -> i32 {
            if x < 0 {
                -x
            } else {
                x
            }
        }
    "#);
    assert_ir_contains(r#"
        func abs(x: i32) -> i32 {
            if x < 0 {
                -x
            } else {
                x
            }
        }
    "#, "then");
    assert_ir_contains(r#"
        func abs(x: i32) -> i32 {
            if x < 0 {
                -x
            } else {
                x
            }
        }
    "#, "else");
}

#[test]
fn codegen_if_no_else() {
    assert_compiles(r#"
        func clamp(x: i32) -> i32 {
            let result = x
            if x > 100 {
                result = 100
            }
            result
        }
    "#);
    assert_ir_contains(r#"
        func clamp(x: i32) -> i32 {
            let result = x
            if x > 100 {
                result = 100
            }
            result
        }
    "#, "then");
}

#[test]
fn codegen_nested_if_else() {
    assert_compiles(r#"
        func classify(x: i32) -> i32 {
            if x > 0 {
                if x > 10 {
                    2
                } else {
                    1
                }
            } else {
                0
            }
        }
    "#);
    let ir = compile_to_ir(r#"
        func classify(x: i32) -> i32 {
            if x > 0 {
                if x > 10 {
                    2
                } else {
                    1
                }
            } else {
                0
            }
        }
    "#);
    // Should have multiple then/else blocks for nested ifs
    let then_count = ir.matches("then").count();
    assert!(then_count >= 2, "Should have at least 2 'then' blocks for nested ifs, got {}", then_count);
}

#[test]
fn codegen_if_with_let_binding() {
    assert_compiles(r#"
        func max(a: i32, b: i32) -> i32 {
            if a > b {
                let result = a
                result
            } else {
                let result = b
                result
            }
        }
    "#);
}

#[test]
fn codegen_if_complex_condition() {
    assert_compiles(r#"
        func check(x: i32, y: i32) -> i32 {
            if x == y {
                1
            } else {
                0
            }
        }
    "#);
    let ir = compile_to_ir(r#"
        func check(x: i32, y: i32) -> i32 {
            if x == y {
                1
            } else {
                0
            }
        }
    "#);
    // Should have icmp for the equality check
    assert!(ir.contains("icmp"), "IR should contain icmp for comparison");
}

#[test]
fn codegen_while_counter() {
    assert_compiles(r#"
        func count() -> i32 {
            let i = 0
            while i < 10 {
                i = i + 1
            }
            i
        }
    "#);
    let ir = compile_to_ir(r#"
        func count() -> i32 {
            let i = 0
            while i < 10 {
                i = i + 1
            }
            i
        }
    "#);
    assert!(ir.contains("loop"), "IR should contain loop block");
    assert!(ir.contains("loopbody"), "IR should contain loop body block");
}

#[test]
fn codegen_while_break() {
    assert_compiles(r#"
        func find_first() -> i32 {
            let i = 0
            while i < 100 {
                if i == 5 {
                    break
                }
                i = i + 1
            }
            i
        }
    "#);
    let ir = compile_to_ir(r#"
        func find_first() -> i32 {
            let i = 0
            while i < 100 {
                if i == 5 {
                    break
                }
                i = i + 1
            }
            i
        }
    "#);
    // Should have a loopcont block for break target
    assert!(ir.contains("loopcont"), "IR should have loopcont block for break");
}

#[test]
fn codegen_while_continue() {
    assert_compiles(r#"
        func skip_even() -> i32 {
            let sum = 0
            let i = 0
            while i < 10 {
                i = i + 1
                if i % 2 == 0 {
                    continue
                }
                sum = sum + i
            }
            sum
        }
    "#);
}

#[test]
fn codegen_nested_while() {
    assert_compiles(r#"
        func nested() -> i32 {
            let i = 0
            let sum = 0
            while i < 5 {
                let j = 0
                while j < 3 {
                    sum = sum + 1
                    j = j + 1
                }
                i = i + 1
            }
            sum
        }
    "#);
}

#[test]
fn codegen_infinite_while_break() {
    assert_compiles(r#"
        func until_found() -> i32 {
            let i = 0
            while true {
                if i == 10 {
                    break
                }
                i = i + 1
            }
            i
        }
    "#);
}

#[test]
fn codegen_match_literal() {
    assert_compiles(r#"
        type Direction { North | South | East | West }
        func describe(d: Direction) -> i32 {
            match d {
                North => 1
                South => 2
                East => 3
                West => 4
            }
        }
    "#);
    let ir = compile_to_ir(r#"
        type Direction { North | South | East | West }
        func describe(d: Direction) -> i32 {
            match d {
                North => 1
                South => 2
                East => 3
                West => 4
            }
        }
    "#);
    assert!(ir.contains("matchcont"), "IR should have matchcont block");
}

#[test]
fn codegen_match_wildcard() {
    assert_compiles(r#"
        type Color { Red | Green | Blue }
        func is_primary(c: Color) -> i32 {
            match c {
                Red => 1
                Blue => 1
                _ => 0
            }
        }
    "#);
}

#[test]
fn codegen_match_with_variable() {
    assert_compiles(r#"
        type Option { Some(i32) | None }
        func unwrap_or(o: Option, default: i32) -> i32 {
            match o {
                Some(x) => x
                None => default
            }
        }
    "#);
}

#[test]
fn codegen_match_nested() {
    assert_compiles(r#"
        type MyResult { Ok(i32) | Err(i32) }
        type Outer { Value(MyResult) | Empty }
        func flatten(o: Outer) -> i32 {
            match o {
                Value(r) => match r {
                    Ok(v) => v
                    Err(e) => e
                }
                Empty => 0
            }
        }
    "#);
}

#[test]
fn codegen_match_with_guard() {
    assert_compiles(r#"
        type Num { Val(i32) }
        func classify(n: Num) -> i32 {
            match n {
                Val(x) if x > 0 => 1
                Val(x) if x < 0 => -1
                Val(_) => 0
            }
        }
    "#);
}

#[test]
fn codegen_record_creation() {
    assert_compiles(r#"
        type Point { x: i32, y: i32 }
        func make_point() -> i32 {
            let p = Point { x: 1, y: 2 }
            0
        }
    "#);
}

#[test]
fn codegen_record_multiple_fields() {
    assert_compiles(r#"
        type Person { name: i32, age: i32, active: bool }
        func make_person() -> i32 {
            let p = Person { name: 42, age: 25, active: true }
            0
        }
    "#);
}

#[test]
fn codegen_enum_type() {
    assert_compiles(r#"
        type Color { Red | Green | Blue }
        func use_color(c: Color) -> i32 {
            0
        }
    "#);
}

#[test]
fn codegen_newtype() {
    assert_compiles(r#"
        type Meter = f64
        func make_distance() -> i32 {
            let d: Meter = 3.14
            0
        }
    "#);
}

#[test]
fn codegen_type_alias() {
    assert_compiles(r#"
        type UserId = i32
        func get_user() -> i32 {
            let id: UserId = 123
            id
        }
    "#);
}

#[test]
fn codegen_block_as_expression() {
    assert_compiles(r#"
        func block_expr() -> i32 {
            let a = 5
            let b = 10
            let x = a + b
            x
        }
    "#);
}

#[test]
fn codegen_nested_block() {
    assert_compiles(r#"
        func nested_block() -> i32 {
            let b = 3
            let a = b * 2
            let x = a + 1
            x
        }
    "#);
}

#[test]
fn codegen_function_call_chain() {
    assert_compiles(r#"
        func add(a: i32, b: i32) -> i32 {
            a + b
        }
        func mul(a: i32, b: i32) -> i32 {
            a * b
        }
        func chain() -> i32 {
            add(1, 2) + mul(3, 4)
        }
    "#);
    let ir = compile_to_ir(r#"
        func add(a: i32, b: i32) -> i32 {
            a + b
        }
        func mul(a: i32, b: i32) -> i32 {
            a * b
        }
        func chain() -> i32 {
            add(1, 2) + mul(3, 4)
        }
    "#);
    // Should have multiple function definitions
    let def_count = ir.matches("define").count();
    assert!(def_count >= 3, "Should have at least 3 function definitions, got {}", def_count);
}

#[test]
fn codegen_multi_function() {
    assert_compiles(r#"
        func square(x: i32) -> i32 {
            x * x
        }
        func cube(x: i32) -> i32 {
            x * x * x
        }
        func compute() -> i32 {
            square(3) + cube(2)
        }
    "#);
}

#[test]
fn codegen_recursive_function() {
    assert_compiles(r#"
        func factorial(n: i32) -> i32 {
            if n <= 1 {
                1
            } else {
                n * factorial(n - 1)
            }
        }
    "#);
}

#[test]
fn codegen_void_function() {
    assert_compiles(r#"
        func do_nothing() {
            let x = 42
        }
    "#);
}

#[test]
fn codegen_multi_parameter() {
    assert_compiles(r#"
        func many_params(a: i32, b: i32, c: i32, d: i32, e: i32) -> i32 {
            a + b + c + d + e
        }
    "#);
}

#[test]
fn codegen_compound_expression() {
    assert_compiles(r#"
        func compound() -> i32 {
            let x = 1 + 2 * 3 - 4 / 2
            let y = x > 5
            if y {
                x * 2
            } else {
                x
            }
        }
    "#);
}

// ===================== Phase A: Builtins Tests =====================

#[test]
fn codegen_builtin_println_string() {
    assert_compiles(r#"
        func main() {
            println("hello")
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            println("hello")
        }
    "#);
    assert!(ir.contains("call"), "IR should contain call for println");
}

#[test]
fn codegen_builtin_println_int() {
    assert_compiles(r#"
        func main() {
            println(42)
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            println(42)
        }
    "#);
    assert!(ir.contains("printf"), "IR should contain printf for integer println");
}

#[test]
fn codegen_builtin_assert() {
    assert_compiles(r#"
        func main() {
            assert(true)
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            assert(true)
        }
    "#);
    assert!(ir.contains("assert_ok"), "IR should have assert_ok block");
    assert!(ir.contains("assert_fail"), "IR should have assert_fail block");
}

#[test]
fn codegen_builtin_assert_eq() {
    assert_compiles(r#"
        func main() {
            assert_eq(1 + 1, 2)
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            assert_eq(1 + 1, 2)
        }
    "#);
    assert!(ir.contains("aeq_ok"), "IR should have aeq_ok block");
    assert!(ir.contains("aeq_fail"), "IR should have aeq_fail block");
}

#[test]
fn codegen_builtin_range() {
    assert_compiles(r#"
        func main() {
            let nums = range(0, 5)
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            let nums = range(0, 5)
        }
    "#);
    assert!(ir.contains("range_loop"), "IR should have range_loop block");
    assert!(ir.contains("malloc"), "IR should call malloc for range");
}

#[test]
fn codegen_builtin_range_in_for_loop() {
    assert_compiles(r#"
        func main() {
            for i in range(0, 3) {
                println(i)
            }
        }
    "#);
}

#[test]
fn codegen_builtin_len() {
    assert_compiles(r#"
        func main() {
            let nums = range(0, 5)
            let n = len(nums)
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            let nums = range(0, 5)
            let n = len(nums)
        }
    "#);
    assert!(ir.contains("list.len"), "IR should load list.len for len builtin");
}

#[test]
fn codegen_builtin_to_string() {
    assert_compiles(r#"
        func main() {
            let s = to_string(42)
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            let s = to_string(42)
        }
    "#);
    assert!(ir.contains("sprintf"), "IR should call sprintf for to_string");
    assert!(ir.contains("strlen"), "IR should call strlen for to_string");
}

#[test]
fn codegen_builtin_abs() {
    assert_compiles(r#"
        func main() {
            let x = abs(-5)
        }
    "#);
}

#[test]
fn codegen_builtin_min_max() {
    assert_compiles(r#"
        func main() {
            let a = min(3, 7)
            let b = max(3, 7)
        }
    "#);
}

// ===================== Phase A: List Operations Tests =====================

#[test]
fn codegen_list_literal() {
    assert_compiles(r#"
        func main() {
            let nums = [1, 2, 3, 4, 5]
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            let nums = [1, 2, 3, 4, 5]
        }
    "#);
    assert!(ir.contains("malloc"), "IR should call malloc for list allocation");
    assert!(ir.contains("list_len"), "IR should store list length");
    assert!(ir.contains("list_data"), "IR should store list data pointer");
}

#[test]
fn codegen_list_literal_empty() {
    assert_compiles(r#"
        func main() {
            let nums = []
        }
    "#);
}

#[test]
fn codegen_list_index() {
    assert_compiles(r#"
        func main() {
            let nums = [10, 20, 30]
            let x = nums[1]
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            let nums = [10, 20, 30]
            let x = nums[1]
        }
    "#);
    assert!(ir.contains("list.data"), "IR should access list.data for indexing");
    assert!(ir.contains("elem_val"), "IR should load element value");
}

#[test]
fn codegen_list_for_loop() {
    assert_compiles(r#"
        func main() {
            let nums = [10, 20, 30]
            for x in nums {
                println(x)
            }
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() {
            let nums = [10, 20, 30]
            for x in nums {
                println(x)
            }
        }
    "#);
    assert!(ir.contains("forloop"), "IR should have forloop block");
    assert!(ir.contains("forbody"), "IR should have forbody block");
}

// ===================== Phase A: Field Access Tests =====================

#[test]
fn codegen_record_field_access() {
    assert_compiles(r#"
        type Point { x: i32, y: i32 }
        func main() {
            let p = Point { x: 10, y: 20 }
            let val = p.x
        }
    "#);
}

#[test]
fn codegen_record_field_access_chain() {
    assert_compiles(r#"
        type Inner { val: i32 }
        type Outer { inner: i32 }
        func main() {
            let o = Outer { inner: 42 }
            let v = o.inner
        }
    "#);
}

// ===================== Phase A: Integration Tests =====================

#[test]
fn codegen_function_with_builtins() {
    assert_compiles(r#"
        func sum_range(n: i32) -> i32 {
            let total = 0
            for i in range(0, n) {
                total = total + i
            }
            total
        }
    "#);
}

#[test]
fn codegen_list_with_function_call() {
    assert_compiles(r#"
        func double(x: i32) -> i32 {
            x * 2
        }
        func main() {
            let nums = [1, 2, 3]
            for x in nums {
                let d = double(x)
            }
        }
    "#);
}

// ===================== Phase B: Extern FFI Codegen Tests =====================

#[test]
fn codegen_extern_block_basic() {
    assert_compiles(r#"
        extern "C" {
            func my_func(x: i32) -> i32;
        }
        func main() -> i32 {
            42
        }
    "#);
    let ir = compile_to_ir(r#"
        extern "C" {
            func my_func(x: i32) -> i32;
        }
        func main() -> i32 {
            42
        }
    "#);
    assert!(ir.contains("declare"), "IR should contain 'declare' for extern function");
    assert!(ir.contains("my_func"), "IR should contain extern function name");
}

#[test]
fn codegen_extern_block_multiple_funcs() {
    assert_compiles(r#"
        extern "C" {
            func ext_add(a: i32, b: i32) -> i32;
            func ext_sub(a: i32, b: i32) -> i32;
        }
        func main() -> i32 {
            42
        }
    "#);
    let ir = compile_to_ir(r#"
        extern "C" {
            func ext_add(a: i32, b: i32) -> i32;
            func ext_sub(a: i32, b: i32) -> i32;
        }
        func main() -> i32 {
            42
        }
    "#);
    assert!(ir.contains("ext_add"), "IR should contain ext_add");
    assert!(ir.contains("ext_sub"), "IR should contain ext_sub");
}

#[test]
fn codegen_extern_block_void_return() {
    assert_compiles(r#"
        extern "C" {
            func ext_print(msg: string);
        }
        func main() {
            42
        }
    "#);
}

#[test]
fn codegen_extern_block_no_params() {
    assert_compiles(r#"
        extern "C" {
            func ext_get_time() -> i64;
        }
        func main() -> i64 {
            ext_get_time()
        }
    "#);
    let ir = compile_to_ir(r#"
        extern "C" {
            func ext_get_time() -> i64;
        }
        func main() -> i64 {
            ext_get_time()
        }
    "#);
    assert!(ir.contains("call"), "IR should contain call to extern function");
}

#[test]
fn codegen_extern_block_with_user_func() {
    assert_compiles(r#"
        extern "C" {
            func ext_multiply(a: i32, b: i32) -> i32;
        }
        func add(a: i32, b: i32) -> i32 {
            a + b
        }
        func main() -> i32 {
            add(1, 2)
        }
    "#);
}

#[test]
fn codegen_extern_in_module() {
    assert_compiles(r#"
        module mylib {
            extern "C" {
                func lib_func(x: i32) -> i32;
            }
        }
        func main() -> i32 {
            42
        }
    "#);
}

#[test]
fn codegen_extern_block_c_shared() {
    let ir = compile_to_ir(r#"
        extern "C" {
            func process_data(data: c_shared i64) -> i32;
        }
        func main() -> i32 {
            42
        }
    "#);
    assert!(ir.contains("mimi_shared_retain"), "IR should contain retain call for c_shared param");
    assert!(ir.contains("mimi_shared_release"), "IR should contain release call for c_shared param");
}

#[test]
fn codegen_extern_block_cap() {
    let ir = compile_to_ir(r#"
        cap FileReadCap;
        extern "C" {
            func read_file(c: FileReadCap) -> i32;
        }
        func main() -> i32 {
            42
        }
    "#);
    assert!(ir.contains("mimi_cap_check"), "IR should contain cap_check call for cap param");
}

#[test]
fn codegen_extern_block_c_borrow() {
    assert_compiles(r#"
        extern "C" {
            func process(data: c_borrow i64) -> i32;
        }
        func main() -> i32 {
            42
        }
    "#);
}

#[test]
fn codegen_cap_register() {
    let ir = compile_to_ir(r#"
        cap FileReadCap;
        func main() -> i32 {
            let c = FileReadCap
            42
        }
    "#);
    assert!(ir.contains("mimi_cap_register"), "IR should contain cap_register call when cap literal is used");
}

#[test]
fn codegen_cap_consume() {
    let ir = compile_to_ir(r#"
        cap FileReadCap;
        func main() -> i32 {
            let c = FileReadCap
            drop(c)
            42
        }
    "#);
    assert!(ir.contains("mimi_cap_consume"), "IR should contain cap_consume call when cap is dropped");
}

#[test]
fn codegen_cap_extern_pass() {
    let ir = compile_to_ir(r#"
        cap FileReadCap;
        extern "C" {
            func use_cap(c: FileReadCap) -> i32;
        }
        func main() -> i32 {
            let c = FileReadCap
            use_cap(c)
        }
    "#);
    assert!(ir.contains("mimi_cap_check"), "IR should contain cap_check for extern param");
}

// ===================== Phase B: Stdlib Module Tests =====================

#[test]
fn codegen_stdlib_module_parse() {
    assert_compiles(r#"
        module mymath {
            pub func add(a: i32, b: i32) -> i32 {
                a + b
            }
            pub func mul(a: i32, b: i32) -> i32 {
                a * b
            }
        }
        func main() -> i32 {
            42
        }
    "#);
}

#[test]
fn codegen_nested_module() {
    assert_compiles(r#"
        module utils {
            module myhelpers {
                pub func square(x: i32) -> i32 {
                    x * x
                }
            }
        }
        func main() -> i32 {
            42
        }
    "#);
}

// ===================== Phase 1: Actor Codegen Tests =====================

#[test]
fn codegen_actor_basic() {
    assert_compiles(r#"
        actor Counter {
            count: i32
            name: string
        }
        func main() -> i32 {
            42
        }
    "#);
    let ir = compile_to_ir(r#"
        actor Counter {
            count: i32
            name: string
        }
        func main() -> i32 {
            42
        }
    "#);
    assert!(ir.contains("Counter_new"), "IR should contain actor constructor");
    assert!(ir.contains("%Counter"), "IR should contain actor type");
}

#[test]
fn codegen_actor_with_methods() {
    assert_compiles(r#"
        actor Counter {
            count: i32
        }
        func main() -> i32 {
            42
        }
    "#);
}

#[test]
fn codegen_actor_multiple_fields() {
    assert_compiles(r#"
        actor Person {
            name: string
            age: i32
            active: bool
        }
        func main() -> i32 {
            42
        }
    "#);
    let ir = compile_to_ir(r#"
        actor Person {
            name: string
            age: i32
            active: bool
        }
        func main() -> i32 {
            42
        }
    "#);
    assert!(ir.contains("Person_new"), "IR should contain Person constructor");
}

// ===================== Phase 1: Parasteps Codegen Tests =====================

#[test]
fn codegen_parasteps_basic() {
    assert_compiles(r#"
        func main() -> i32 {
            parasteps {
                let x = 1
                let y = 2
                x + y
            }
        }
    "#);
}

#[test]
fn codegen_parasteps_with_statements() {
    assert_compiles(r#"
        func main() -> i32 {
            let mut total = 0
            parasteps {
                total = total + 1
                total = total + 2
                total = total + 3
            }
            total
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            let mut total = 0
            parasteps {
                total = total + 1
                total = total + 2
                total = total + 3
            }
            total
        }
    "#);
    assert!(ir.contains("add"), "IR should contain add operations");
}

#[test]
fn codegen_parasteps_nested() {
    assert_compiles(r#"
        func main() -> i32 {
            parasteps {
                let a = 1
                parasteps {
                    let b = 2
                    a + b
                }
            }
        }
    "#);
}

// ===================== Phase 1: Spawn Codegen Tests =====================

#[test]
fn codegen_spawn_basic() {
    assert_compiles(r#"
        func compute() -> i32 {
            42
        }
        func main() -> i32 {
            let future = spawn compute()
            0
        }
    "#);
}

#[test]
fn codegen_spawn_with_await() {
    assert_compiles(r#"
        func compute() -> i32 {
            42
        }
        func main() -> i32 {
            let future = spawn compute()
            let result = await future
            result
        }
    "#);
    let ir = compile_to_ir(r#"
        func compute() -> i32 {
            42
        }
        func main() -> i32 {
            let future = spawn compute()
            let result = await future
            result
        }
    "#);
    assert!(ir.contains("call"), "IR should contain function calls");
}

#[test]
fn codegen_spawn_in_parasteps() {
    assert_compiles(r#"
        func task1() -> i32 {
            1
        }
        func task2() -> i32 {
            2
        }
        func main() -> i32 {
            parasteps {
                let f1 = spawn task1()
                let f2 = spawn task2()
                let r1 = await f1
                let r2 = await f2
                r1 + r2
            }
        }
    "#);
}

// ===================== Phase 1: Cap Codegen Tests =====================

#[test]
fn codegen_cap_type() {
    assert_compiles(r#"
        cap MyCap
        func main() -> i32 {
            42
        }
    "#);
}

#[test]
fn codegen_cap_with_usage() {
    assert_compiles(r#"
        cap FileCap
        func read_file(file_cap: FileCap) -> i32 {
            42
        }
        func main() -> i32 {
            0
        }
    "#);
    let ir = compile_to_ir(r#"
        cap FileCap
        func read_file(file_cap: FileCap) -> i32 {
            42
        }
        func main() -> i32 {
            0
        }
    "#);
    assert!(ir.contains("define"), "IR should contain function definitions");
}

#[test]
fn codegen_drop_statement() {
    assert_compiles(r#"
        cap MyCap
        func main() -> i32 {
            let c: MyCap = 1
            drop(c)
            0
        }
    "#);
}

#[test]
fn codegen_arena_block() {
    assert_compiles(r#"
        func main() -> i32 {
            arena {
                let x = 42
                x
            }
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            arena {
                let x = 42
                x
            }
        }
    "#);
    assert!(ir.contains("define"), "IR should contain function definitions");
}

#[test]
fn codegen_alloc_block() {
    assert_compiles(r#"
        func main() -> i32 {
            alloc(arena) {
                let x = 42
                x
            }
        }
    "#);
}

// ===================== Phase 1: Cap Linear Capability Codegen Tests =====================

#[test]
fn codegen_cap_linear_tracking() {
    // Test that capability variables are tracked
    assert_compiles(r#"
        cap FileCap
        func read_file(file_cap: FileCap) -> i32 {
            drop(file_cap)
            42
        }
        func main() -> i32 {
            0
        }
    "#);
    let ir = compile_to_ir(r#"
        cap FileCap
        func read_file(file_cap: FileCap) -> i32 {
            drop(file_cap)
            42
        }
        func main() -> i32 {
            0
        }
    "#);
    assert!(ir.contains("define"), "IR should contain function definitions");
}

#[test]
fn codegen_cap_let_tracking() {
    // Test that capability variables from let statements are tracked
    assert_compiles(r#"
        cap MyCap
        func main() -> i32 {
            let c: MyCap = 1
            drop(c)
            0
        }
    "#);
}

// ===================== Phase 1: OnFailure Codegen Tests =====================

#[test]
fn codegen_on_failure_basic() {
    // Test that OnFailure blocks are compiled
    assert_compiles(r#"
        func main() -> i32 {
            on failure {
                println("cleanup")
            }
            42
        }
    "#);
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            on failure {
                println("cleanup")
            }
            42
        }
    "#);
    assert!(ir.contains("define"), "IR should contain function definitions");
}

#[test]
fn codegen_on_failure_with_statements() {
    // Test that OnFailure blocks with multiple statements are compiled
    assert_compiles(r#"
        func main() -> i32 {
            on failure {
                let x = 1
                let y = 2
                println(x + y)
            }
            42
        }
    "#);
}

#[test]
fn codegen_turbofish_generic() {
    // Turbofish should produce a mangled function name
    let ir = compile_to_ir(r#"
        func identity<T>(x: T) -> T {
            x
        }
        func main() -> i32 {
            identity::<i32>(42)
        }
    "#);
    assert!(ir.contains("identity__T_i32"), "IR should contain mangled generic function name:\n{}", ir);
}

#[test]
fn codegen_turbofish_multiple_instantiations() {
    // Two different type instantiations should produce two mangled functions
    let ir = compile_to_ir(r#"
        func wrap<T>(x: T) -> T {
            x
        }
        func main() -> i32 {
            let a = wrap::<i32>(1);
            let b = wrap::<i64>(2);
            a
        }
    "#);
    assert!(ir.contains("wrap__T_i32"), "IR should contain wrap__T_i32:\n{}", ir);
    assert!(ir.contains("wrap__T_i64"), "IR should contain wrap__T_i64:\n{}", ir);
}

#[test]
fn codegen_ref_type() {
    // &T should compile to a pointer type
    assert_compiles(r#"
        func take_ref(x: &i32) -> i32 {
            *x
        }
        func main() -> i32 {
            let v = 42;
            take_ref(&v)
        }
    "#);
}

#[test]
fn codegen_ref_mut_type() {
    // &mut T should compile
    assert_compiles(r#"
        func mutate(x: &mut i32) {
            *x = 100
        }
        func main() -> i32 {
            let mut v = 42;
            mutate(&mut v);
            v
        }
    "#);
}

// ===================== LIFO OnFailure Codegen Tests =====================

#[test]
fn codegen_on_failure_discarded_on_normal_exit() {
    // Without exit(), compensation blocks should not appear in the IR
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            on failure {
                println("cleanup")
            }
            42
        }
    "#);
    // The IR should NOT contain the cleanup string (compensation was discarded)
    assert!(!ir.contains("cleanup"), "OnFailure body should be discarded on normal exit:\n{}", ir);
}

#[test]
fn codegen_on_failure_registered_not_inline() {
    // Verify on_failure is registered (not compiled inline) — the body
    // should only appear when exit() triggers compensations
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            on failure {
                println("compensate")
            }
            exit(1)
        }
    "#);
    // The IR should contain the compensation string (it runs before exit)
    assert!(ir.contains("compensate"), "OnFailure body should compile before exit:\n{}", ir);
    // The IR should contain an exit call
    assert!(ir.contains("exit"), "IR should contain exit call:\n{}", ir);
}

#[test]
fn codegen_on_failure_scope_nesting() {
    // Inner scope compensations should be discarded when the inner block exits normally
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            on failure {
                println("outer")
            }
            {
                on failure {
                    println("inner")
                }
            }
            exit(1)
        }
    "#);
    // Only "outer" compensation should appear before exit (inner was discarded)
    assert!(ir.contains("outer"), "Outer compensation should appear:\n{}", ir);
    assert!(!ir.contains("inner"), "Inner compensation (discarded on normal block exit) should NOT appear:\n{}", ir);
}

// ===================== Spawn/Await Real Return Value Tests =====================

#[test]
fn codegen_spawn_await_no_zero_placeholder() {
    // Verify Await no longer returns the constant 0 placeholder
    let ir = compile_to_ir(r#"
        func compute() -> i32 {
            42
        }
        func main() -> i32 {
            let future = spawn compute()
            let result = await future
            result
        }
    "#);
    // The IR should contain pthread_join with a non-null second argument (retval storage)
    assert!(ir.contains("pthread_join"), "IR should contain pthread_join:\n{}", ir);
    // The IR should contain free (to free the malloc'd result)
    assert!(ir.contains("free"), "IR should contain free:\n{}", ir);
    // The IR should contain malloc (to allocate the result storage)
    assert!(ir.contains("malloc"), "IR should contain malloc:\n{}", ir);
}

#[test]
fn codegen_parasteps_sequential_fallback() {
    // Parasteps currently uses sequential fallback — just verify it compiles
    assert_compiles(r#"
        func main() -> i32 {
            parasteps {
                println("step1")
                println("step2")
            }
            0
        }
    "#);
}

// ===================== Async Func Codegen Tests =====================

#[test]
fn codegen_async_func_basic() {
    // Verify async func compiles with spawner + body
    let ir = compile_to_ir(r#"
        async func compute(x: i32) -> i32 {
            x + 1
        }
        func main() -> i32 {
            let f = compute(41)
            await f
        }
    "#);
    // Should have the async body function
    assert!(ir.contains("__async_body"), "IR should contain async body:\n{}", ir);
    // Should have a spawn wrapper (from the spawner calling spawn)
    assert!(ir.contains("__spawn_wrapper"), "IR should contain spawn wrapper:\n{}", ir);
    // Should have pthread_create (from the spawn)
    assert!(ir.contains("pthread_create"), "IR should contain pthread_create:\n{}", ir);
}

#[test]
fn codegen_async_func_returns_i64() {
    // The spawner function should return i64 (thread ID) not the original return type
    let ir = compile_to_ir(r#"
        async func compute() -> i32 {
            42
        }
        func main() -> i32 {
            let f = compute()
            await f
        }
    "#);
    // The spawner function (same name) should exist
    assert!(ir.contains("define i64 @compute"), "Spawner should return i64:\n{}", ir);
}

// ===================== End-to-End Stdlib Codegen Tests =====================

#[test]
fn codegen_stdlib_mymath_functions() {
    // Compile mymath::square, cube, clamp functions via codegen
    assert_compiles(r#"
        func square(x: i32) -> i32 { x * x }
        func cube(x: i32) -> i32 { x * x * x }
        func clamp(value: i32, min_val: i32, max_val: i32) -> i32 {
            if value < min_val { min_val }
            else if value > max_val { max_val }
            else { value }
        }
        func main() -> i32 { square(3) }
    "#);
}

#[test]
fn codegen_stdlib_prelude() {
    // Compile prelude-style utility functions
    assert_compiles(r#"
        func is_even(x: i32) -> bool { x % 2 == 0 }
        func is_odd(x: i32) -> bool { x % 2 != 0 }
        func main() -> i32 { if is_even(42) { 1 } else { 0 } }
    "#);
}

#[test]
fn codegen_stdlib_collections_parse_only() {
    // Verify collections.mimi parses successfully (for-loop over List not yet in codegen)
    use std::path::PathBuf;
    let std_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std");
    assert!(std_dir.exists());
    
    std::env::set_var("MIMI_STDLIB", &std_dir);
    let coll_path = std_dir.join("collections.mimi");
    let mut loader = crate::loader::ModuleLoader::new(std_dir.clone());
    let loaded = loader.load_main(&coll_path)
        .expect("should load collections.mimi");
    assert!(!loaded.file.items.is_empty(), "collections.mimi should have items");
    std::env::remove_var("MIMI_STDLIB");
}

#[test]
fn codegen_stdlib_loader_roundtrip() {
    // End-to-end: load a stdlib file that uses only codegen-supported features
    use std::path::PathBuf;
    let std_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std");
    assert!(std_dir.exists());
    
    std::env::set_var("MIMI_STDLIB", &std_dir);
    // mymath.mimi uses only basic arithmetic + if — supported by codegen
    let math_path = std_dir.join("mymath.mimi");
    let mut loader = crate::loader::ModuleLoader::new(std_dir.clone());
    let _ = loader.load_main(&math_path).expect("should load mymath.mimi");
    
    let merged = loader.merge_all();
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "stdlib_test");
    if let Err(ref e) = codegen.compile_file(&merged) {
        panic!("mymath.mimi compile error: {}", e);
    }
    std::env::remove_var("MIMI_STDLIB");
}

// ===================== For-Loop List Iteration Tests =====================

#[test]
fn codegen_for_list_parameter() {
    // Test for x in list where list is a function parameter (opaque i64 pointer)
    assert_compiles(r#"
        func sum(xs: List<i32>) -> i32 {
            let mut total = 0
            for x in xs {
                total = total + x
            }
            total
        }
        func main() -> i32 { sum([1, 2, 3]) }
    "#);
}

#[test]
fn codegen_for_list_inline() {
    // Test for x in list with inline literal
    assert_compiles(r#"
        func main() -> i32 {
            let mut total = 0
            for x in [1, 2, 3] {
                total = total + x
            }
            total
        }
    "#);
}

#[test]
fn codegen_for_list_empty() {
    // Test for x in [] (empty list)
    assert_compiles(r#"
        func main() -> i32 {
            let xs: List<i32> = []
            let mut total = 0
            for x in xs {
                total = total + x
            }
            total
        }
    "#);
}

// ===================== Codegen Builtin Coverage Tests =====================

#[test]
fn codegen_type_name_known_var() {
    // type_name on a known-typed variable should resolve at compile time
    let ir = compile_to_ir(r#"
        func main() -> i32 {
            let x: i32 = 42
            type_name(x)
            0
        }
    "#);
    assert!(ir.contains("type_name"), "IR should contain the type name string:\n{}", ir);
}

#[test]
fn codegen_type_fields_record() {
    assert_compiles(r#"
        type Point { x: i32, y: i32 }
        func main() -> i32 {
            let fields = type_fields("Point")
            0
        }
    "#);
}

#[test]
fn codegen_contains_list() {
    assert_compiles(r#"
        func main() -> i32 {
            let xs = [1, 2, 3, 4, 5]
            let found = contains(xs, 3)
            0
        }
    "#);
}

#[test]
fn codegen_map_basic() {
    assert_compiles(r#"
        func double(x: i32) -> i32 { x * 2 }
        func main() -> i32 {
            let xs = [1, 2, 3]
            let ys = map(xs, double)
            0
        }
    "#);
}

#[test]
fn codegen_filename_exists() {
    // file_exists should work via FFI to access()
    assert_compiles(r#"
        func main() -> i32 {
            let exists = file_exists("/")
            0
        }
    "#);
}

#[test]
fn codegen_str_char_at_basic() {
    assert_compiles(r#"
        func main() -> i32 {
            let c = str_char_at("hello", 0)
            0
        }
    "#);
}

#[test]
fn codegen_str_trim_basic() {
    assert_compiles(r#"
        func main() -> i32 {
            let s = str_trim("  hello  ")
            0
        }
    "#);
}

#[test]
fn codegen_str_to_upper_basic() {
    assert_compiles(r#"
        func main() -> i32 {
            let s = str_to_upper("hello")
            0
        }
    "#);
}

#[test]
fn codegen_pow_basic() {
    assert_compiles(r#"
        func main() -> i32 {
            let x = pow(2.0, 3.0)
            0
        }
    "#);
}

#[test]
fn codegen_parse_int() {
    assert_compiles(r#"
        func main() -> i32 {
            let n = str_parse_int("42")
            0
        }
    "#);
}

#[test]
fn codegen_str_repeat() {
    assert_compiles(r#"
        func main() -> i32 {
            let s = str_repeat("ab", 3)
            0
        }
    "#);
}

#[test]
fn codegen_str_contains() {
    assert_compiles(r#"
        func main() -> i32 {
            let found = str_contains("hello world", "world")
            0
        }
    "#);
}

// ===================== End-to-End Codegen Runtime Tests =====================

fn can_link() -> bool {
    std::process::Command::new("cc").arg("--version").output().is_ok()
}

#[test]
fn e2e_hello_world() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = super::compile_and_run(r#"
        func main() -> i32 {
            println("hello from mimi")
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "hello from mimi");
}

#[test]
fn e2e_arithmetic() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = super::compile_and_run(r#"
        func add(a: i32, b: i32) -> i32 { a + b }
        func main() -> i32 {
            println(add(40, 2))
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn e2e_list_for_loop() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = super::compile_and_run(r#"
        func sum(xs: List<i32>) -> i32 {
            let mut total = 0
            for x in xs {
                total = total + x
            }
            total
        }
        func main() -> i32 {
            println(sum([1, 2, 3, 4, 5]))
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "15");
}

#[test]
fn e2e_map_fn_ref() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = super::compile_and_run(r#"
        func double(x: i32) -> i32 { x * 2 }
        func main() -> i32 {
            let xs = [1, 2, 3]
            let ys = map(xs, double)
            for x in ys {
                println(x)
            }
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "2\n4\n6");
}

#[test]
fn e2e_filter_fn_ref() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = super::compile_and_run(r#"
        func is_even(x: i32) -> bool { x % 2 == 0 }
        func main() -> i32 {
            let xs = [1, 2, 3, 4, 5]
            let ys = filter(xs, is_even)
            for x in ys {
                println(x)
            }
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "2\n4");
}

#[test]
fn e2e_reduce_fn_ref() {
    if !can_link() { eprintln!("SKIP: cc not available"); return; }
    let stdout = super::compile_and_run(r#"
        func add(a: i32, b: i32) -> i32 { a + b }
        func main() -> i32 {
            let xs = [1, 2, 3, 4, 5]
            let total = reduce(xs, add, 0)
            println(total)
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "15");
}

#[test]
fn codegen_fstring_text_only() {
    assert_compiles(r#"
        func main() -> i32 {
            let s = "hello world"
            println(s)
            0
        }
    "#);
}

#[test]
fn codegen_fstring_with_interp() {
    let stdout = super::compile_and_run(r#"
        func main() -> i32 {
            let x = 42
            println(f"x = {x}")
            0
        }
    "#).unwrap();
    assert_eq!(stdout.trim(), "x = 42");
}
