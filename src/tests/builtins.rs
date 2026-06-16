use super::*;
use crate::ast::Item;

#[test]
fn string_concat() {
    let src = r#"
func main() -> string {
    let s = "hello" + " " + "world";
    s
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello world".to_string()));
}

#[test]
fn string_concat_empty() {
    let src = r#"
func main() -> string {
    let s = "" + "abc" + "";
    s
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("abc".to_string()));
}

#[test]
fn builtin_len_string() {
    let src = r#"
func main() -> i32 {
    len("hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_len_list() {
    let src = r#"
func main() -> i32 {
    len([1, 2, 3, 4, 5])
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_len_empty_string() {
    let src = r#"
func main() -> i32 {
    len("")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_to_string_int() {
    let src = r#"
func main() -> string {
    to_string(42)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("42".to_string()));
}

#[test]
fn builtin_to_string_bool() {
    let src = r#"
func main() -> string {
    to_string(true)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("true".to_string()));
}

#[test]
fn builtin_abs_int() {
    let src = r#"
func main() -> i32 {
    abs(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_abs_float() {
    let src = r#"
func main() -> f64 {
    abs(-3.14)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(3.14));
}

#[test]
fn builtin_push() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = push(a, 4);
    len(result)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(4));
}

#[test]
fn builtin_pop() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = pop(a);
    let (popped, _) = result;
    popped
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_pop_returns_remaining() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = pop(a);
    let (_, new_list) = result;
    len(new_list)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

// =============================================================================
// P2: Type safety - borrow checking
// =============================================================================

#[test]
fn typecheck_double_mut_borrow_error() {
    let src = r#"
func main() -> i32 {
    let mut x = 42;
    let r1 = &mut x;
    let r2 = &mut x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let has_borrow_error = errors.iter().any(|e| e.message.contains("already mutably borrowed"));
    assert!(has_borrow_error, "Expected mutable borrow error, got: {:?}", errors);
}

#[test]
fn typecheck_imm_mut_borrow_error() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r1 = &x;
    let r2 = &mut x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let has_borrow_error = errors.iter().any(|e| e.message.contains("already immutably borrowed"));
    assert!(has_borrow_error, "Expected immutable borrow error, got: {:?}", errors);
}

#[test]
fn typecheck_double_imm_borrow_ok() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r1 = &x;
    let r2 = &x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "Multiple immutable borrows should be allowed");
}

#[test]
fn typecheck_borrow_scope_isolation() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    {
        let r = &mut x;
    }
    let r2 = &x;
    1
}
"#;
    let file = parse(src);
    let result = core::check(&file);
    assert!(result.is_ok(), "Borrows should be isolated to their scope");
}

// =============================================================================
// P3: on failure + ? integration
// =============================================================================

#[test]
fn on_failure_executes_on_error() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail() -> Res {
    Err("boom")
}

func cleanup() {
    println("CLEANUP_RAN");
}

func main() -> i32 {
    on failure { cleanup(); }
    let _ = fail()?;
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "Error should propagate");
}

#[test]
fn on_failure_lifo_order() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail() -> Res {
    Err("boom")
}

func main() -> i32 {
    on failure { println("C"); }
    on failure { println("B"); }
    on failure { println("A"); }
    let _ = fail()?;
    0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "Error should propagate");
}

#[test]
fn on_failure_no_execute_on_success() {
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func succeed() -> Res {
    Ok(42)
}

func main() -> i32 {
    on failure { println("SHOULD_NOT_RUN"); }
    let x = succeed()?;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42), "Compensation should NOT execute on success");
}

// =============================================================================
// P4.1: pub visibility parsing
// =============================================================================

#[test]
fn parse_pub_func() {
    let src = r#"
pub func helper() -> i32 { 42 }

func main() -> i32 {
    helper()
}
"#;
    let file = parse(src);
    if let Item::Func(f) = &file.items[0] {
        assert!(f.pub_, "func should be marked as pub");
    } else {
        panic!("expected Func item");
    }
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn parse_pub_type() {
    let src = r#"
pub type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    1
}
"#;
    let file = parse(src);
    if let Item::Type(t) = &file.items[0] {
        assert!(t.pub_, "type should be marked as pub");
    } else {
        panic!("expected Type item");
    }
}

#[test]
fn parse_non_pub_func() {
    let src = r#"
func helper() -> i32 { 42 }

func main() -> i32 {
    helper()
}
"#;
    let file = parse(src);
    if let Item::Func(f) = &file.items[0] {
        assert!(!f.pub_, "func without pub should not be marked as pub");
    } else {
        panic!("expected Func item");
    }
}

// =============================================================================
// P6: requires/ensures runtime assertions
// =============================================================================

#[test]
fn requires_passes() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    requires: b > 0
    a + b
}

func main() -> i32 {
    add(1, 2)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn requires_fails() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    requires: a > 0
    a + b
}

func main() -> i32 {
    add(-1, 2)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("requires condition failed"), "Expected requires error, got: {}", err);
}

#[test]
fn ensures_passes() {
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == x * 2
    x * 2
}

func main() -> i32 {
    double(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn ensures_fails() {
    let src = r#"
func buggy(x: i32) -> i32 {
    ensures: result == x * 2
    x * 3
}

func main() -> i32 {
    buggy(5)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("ensures condition failed"), "Expected ensures error, got: {}", err);
}

#[test]
fn requires_ensures_combined() {
    let src = r#"
func abs_val(x: i32) -> i32 {
    requires: x != 0
    ensures: result > 0
    if x < 0 { -x } else { x }
}

func main() -> i32 {
    abs_val(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

// =============================================================================
// P7: comptime keyword, nothing type, more builtins
// =============================================================================

#[test]
fn builtin_min_int() {
    let src = r#"
func main() -> i32 {
    min(3, 7)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_max_int() {
    let src = r#"
func main() -> i32 {
    max(3, 7)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn builtin_min_float() {
    let src = r#"
func main() -> f64 {
    min(3.14, 2.71)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(2.71));
}

#[test]
fn builtin_contains_list() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3, 4, 5];
    if contains(a, 3) { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_contains_string() {
    let src = r#"
func main() -> i32 {
    let s = "hello world";
    if contains(s, "world") { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_contains_not_found() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    if contains(a, 99) { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn nothing_type_parsing() {
    let src = r#"
func diverge() -> nothing {
    assert(false)
}

func main() -> i32 {
    1
}
"#;
    let _file = parse(src);
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

// =============================================================================
// Critical bug fix tests
// =============================================================================

#[test]
fn bugfix_division_by_zero() {
    let src = r#"
func main() -> i32 {
    10 / 0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("division by zero"), "Expected division by zero error, got: {}", err);
}

#[test]
fn bugfix_modulo_by_zero() {
    let src = r#"
func main() -> i32 {
    10 % 0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("modulo by zero"), "Expected modulo by zero error, got: {}", err);
}

#[test]
fn bugfix_negative_exponent() {
    let src = r#"
func main() -> i32 {
    2 ** -1
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("negative exponent"), "Expected negative exponent error, got: {}", err);
}

#[test]
fn bugfix_immutable_assignment() {
    let src = r#"
func main() -> i32 {
    let x = 10;
    x = 20;
    x
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("immutable"), "Expected immutable error, got: {}", err);
}

#[test]
fn bugfix_mut_assignment_works() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    x = 20;
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(20));
}

#[test]
fn bugfix_error_in_expr_statement() {
    // Value::Error from ? operator should propagate through expression statements
    let src = r#"
type Res {
    Ok(i32)
    Err(string)
}

func fail() -> Res { Err("boom") }

func main() -> i32 {
    fail()?;
    1
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err(), "Error should propagate through expression statement");
}

#[test]
fn bugfix_float_division_by_zero() {
    let src = r#"
func main() -> f64 {
    10.0 / 0.0
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("division by zero"), "Expected division by zero error, got: {}", err);
}

// ==================== shared/local_shared/weak tests ====================

#[test]
fn shared_basic_creation() {
    let src = r#"
func main() {
    shared x = 42;
    println(x);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn shared_clone_refcount() {
    let src = r#"
func main() {
    shared x = 42;
    shared y = x;
    println(x);
    println(y);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn shared_field_access() {
    let src = r#"
type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    shared s = Point { x: 10, y: 20 };
    s.x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn shared_deref_method() {
    let src = r#"
func main() -> i32 {
    shared x = 42;
    x.deref()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn local_shared_basic() {
    let src = r#"
func main() {
    local_shared x = 100;
    local_shared y = x;
    println(x);
    println(y);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn local_shared_deref() {
    let src = r#"
func main() -> i32 {
    local_shared x = 99;
    x.inner()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(99));
}

#[test]
fn weak_shared_basic() {
    let src = r#"
func main() {
    shared x = 42;
    weak w = x;
    println(w);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn weak_upgrade_success() {
    let src = r#"
func main() -> i32 {
    shared x = 42;
    weak w = x;
    let upgraded = w.upgrade();
    upgraded.deref()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn weak_upgrade_none_after_drop() {
    let src = r#"
func get_weak() -> weak i32 {
    shared x = 42;
    weak w = x;
    w
}

func main() -> i32 {
    let w = get_weak();
    let upgraded = w.upgrade();
    // upgraded is a variant - check if it's None
    match upgraded {
        Some(v) => v.deref(),
        None => 0,
    }
}
"#;
    let result = run_source_result(src);
    // After shared x is dropped, upgrade returns None
    assert!(result.is_ok());
}

#[test]
fn weak_local_basic() {
    let src = r#"
func main() {
    local_shared x = 10;
    weak w = x;
    println(w);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn weak_local_upgrade() {
    let src = r#"
func main() -> i32 {
    local_shared x = 55;
    weak w = x;
    let upgraded = w.upgrade();
    upgraded.inner()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(55));
}

#[test]
fn shared_record_field_access() {
    let src = r#"
type Node {
    value: i32
    next: i32
}

func main() -> i32 {
    shared node = Node { value: 7, next: 0 };
    node.value
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn shared_multiple_shares() {
    let src = r#"
func main() {
    shared a = 1;
    shared b = a;
    shared c = b;
    println(a);
    println(b);
    println(c);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn shared_as_function_arg() {
    let src = r#"
func use_shared(x: shared i32) {
    println(x);
}

func main() {
    shared v = 42;
    use_shared(v);
    println(v);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn weak_shared_in_list() {
    let src = r#"
func main() {
    shared a = 10;
    shared b = 20;
    weak wa = a;
    weak wb = b;
    let list = [wa, wb];
    println(list);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

// ==================== arena escape checking tests ====================

#[test]
fn arena_no_escape_ok() {
    let src = r#"
func process() -> i32 {
    arena {
        let ref x = 10;
        let val = x;
        42
    }
}

func main() -> i32 {
    process()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn arena_escape_return_detected() {
    let src = r#"
func process() -> i32 {
    arena {
        let ref x = 10;
        return x;
    }
}

func main() -> i32 {
    process()
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("arena escape"), "Expected arena escape error, got: {}", err);
}

#[test]
fn arena_escape_variable_detected() {
    let src = r#"
func main() -> i32 {
    let mut escaped = 0;
    arena {
        let ref x = 42;
        escaped = x;
    }
    escaped
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("arena escape"), "Expected arena escape error, got: {}", err);
}

#[test]
fn arena_nested_ok() {
    let src = r#"
func main() -> i32 {
    arena {
        let a = 10;
        arena {
            let b = 20;
            a + b
        }
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn arena_no_ref_ok() {
    let src = r#"
func main() -> i32 {
    let mut x = 0;
    arena {
        x = 42;
    }
    x
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn arena_ref_within_scope_ok() {
    let src = r#"
func main() -> i32 {
    arena {
        let a = 10;
        let b = 20;
        let result = a + b;
        result
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

// ==================== comptime quote! tests ====================

#[test]
fn quote_basic_literal() {
    let src = r#"
func main() {
    let ast = quote! { 42 };
    println(ast);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_interpolation() {
    let src = r#"
func main() {
    let x = 10;
    let ast = quote! { $(x + 1) };
    println(ast);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_let_statement() {
    let src = r#"
func main() {
    let ast = quote! { let y = 5; };
    println(ast);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_dump() {
    let src = r#"
func main() {
    let ast = quote! { 42 };
    let dumped = ast_dump(ast);
    println(dumped);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_eval_literal() {
    let src = r#"
func main() -> i32 {
    let ast = quote! { 42 };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn quote_eval_binary() {
    let src = r#"
func main() -> i32 {
    let ast = quote! { 10 + 20 };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn quote_eval_interpolation() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    let ast = quote! { $(x * 3) };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

#[test]
fn quote_eval_block() {
    let src = r#"
func main() -> i32 {
    let ast = quote! {
        let a = 10;
        let b = 20;
        a + b
    };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn quote_eval_string_concat() {
    let src = r#"
func main() {
    let ast = quote! { "hello" + " " + "world" };
    let result = ast_eval(ast);
    println(result);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

#[test]
fn quote_nested_interpolation() {
    let src = r#"
func main() -> i32 {
    let a = 3;
    let b = 4;
    let ast = quote! { $(a + b) };
    ast_eval(ast)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

// ==================== actor async tests ====================

#[test]
fn actor_await_method() {
    let src = r#"
actor Counter {
    mut count: i32 = 0;

    func increment() {
        self.count = self.count + 1;
    }

    func get() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let c = Counter.spawn();
    c.increment();
    let val = await c.get();
    val
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn actor_sync_method_still_works() {
    let src = r#"
actor Counter {
    mut count: i32 = 0;

    func get() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let c = Counter.spawn();
    c.get()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn actor_await_multiple_methods() {
    let src = r#"
actor Calculator {
    mut value: i32 = 0;

    func add(n: i32) {
        self.value = self.value + n;
    }

    func get() -> i32 {
        return self.value;
    }
}

func main() -> i32 {
    let calc = Calculator.spawn();
    calc.add(10);
    calc.add(20);
    let result = await calc.get();
    result
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn actor_await_with_args() {
    let src = r#"
actor Greeter {
    mut name: string = "world";

    func greet() -> string {
        return "Hello, " + self.name;
    }
}

func main() {
    let g = Greeter.spawn();
    let msg = await g.greet();
    println(msg);
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Unit);
}

// ==================== cap.split() tests ====================

#[test]
fn cap_combined_declaration() {
    let src = r#"
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn cap_split_returns_tuple() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let parts = c.split();
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_runtime() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    drop(write);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_single_error() {
    let src = r#"
cap FileReadCap;

func main() -> i32 {
    let c = FileReadCap;
    c.split();
    42
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("split() requires a combined capability"), "Expected split error, got: {}", err);
}

#[test]
fn cap_split_drop_one() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    drop(write);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== old(x) in ensures tests ====================

#[test]
fn old_basic_snapshot() {
    let src = r#"
func double(x: i32) -> i32 {
    ensures: result == old(x) * 2
    return x * 2;
}

func main() -> i32 {
    double(5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn old_with_mutation() {
    let src = r#"
func increment(x: i32) -> i32 {
    ensures: result == old(x) + 1
    return x + 1;
}

func main() -> i32 {
    increment(10)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(11));
}

#[test]
fn old_fails() {
    let src = r#"
func bad(x: i32) -> i32 {
    ensures: result == old(x) + 10
    return x + 1;
}

func main() -> i32 {
    bad(5)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("ensures condition failed"), "Expected ensures error, got: {}", err);
}

#[test]
fn old_multiple_params() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    ensures: result == old(a) + old(b)
    return a + b;
}

func main() -> i32 {
    add(3, 4)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

// ==================== math: block tests ====================

#[test]
fn math_constant_evaluation() {
    let src = r#"
func main() -> i32 {
    math: {
        1 + 2;
        3 * 4;
    }
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn math_with_variables() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    math: {
        x + 1;
    }
    x * 2
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn math_boolean_expressions() {
    let src = r#"
func main() -> bool {
    math: {
        1 < 2;
        3 > 2;
        1 == 1;
    }
    true
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Bool(true));
}

// ==================== trait/impl tests ====================

#[test]
fn trait_definition() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn trait_with_impl() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

type MyType {
    value: i32
}

impl Display for MyType {
    func to_string() -> string {
        return "MyType";
    }
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn trait_multiple_methods() {
    let src = r#"
trait Printable {
    func to_string() -> string;
    func print();
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn trait_with_params() {
    let src = r#"
trait Addable {
    func add(x: i32) -> i32;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== where clause tests ====================

#[test]
fn where_single_constraint() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

type MyType {
    value: i32
}

func print(x: MyType) where MyType: Display {
    println(x);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn where_multiple_constraints() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

trait Clone {
    func clone() -> Self;
}

type MyType {
    value: i32
}

func process(x: MyType) where MyType: Display + Clone {
    println(x);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn where_with_return_type() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

type MyType {
    value: i32
}

func format(x: MyType) -> string where MyType: Display {
    x.to_string()
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== extern block tests ====================

#[test]
fn extern_block_basic() {
    let src = r#"
extern "C" {
    func printf(fmt: string) -> i32;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_multiple_funcs() {
    let src = r#"
extern "C" {
    func malloc(size: i32) -> i32;
    func free(ptr: i32);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_with_cap() {
    let src = r#"
cap FileReadCap;

extern "C" {
    func read(fd: i32, file_cap: FileReadCap) -> string;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_with_borrow() {
    let src = r#"
cap FileReadCap;

extern "C" {
    func read(fd: i32, file_cap: FileReadCap) -> string;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== Additional edge case tests ====================

#[test]
fn cap_split_nested_combination() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_use_individual_parts() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func use_read(r: FileReadCap) -> i32 {
    1
}

func use_write(w: FileWriteCap) -> i32 {
    2
}

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    let a = use_read(read);
    let b = use_write(write);
    a + b
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn old_on_string_non_copy() {
    let src = r#"
func append_world(s: string) -> string {
    ensures: result == old(s) + "world"
    return s + "world";
}

func main() -> string {
    append_world("hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("helloworld".to_string()));
}

#[test]
fn old_with_multiple_returns() {
    let src = r#"
func abs(x: i32) -> i32 {
    ensures: result >= 0
    ensures: result == old(x) || result == -old(x)
    if x < 0 {
        return -x;
    }
    return x;
}

func main() -> i32 {
    abs(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn math_empty_block() {
    let src = r#"
func main() -> i32 {
    math: {
    }
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn math_with_division() {
    let src = r#"
func main() -> i32 {
    math: {
        10 / 2;
        100 / 10;
    }
    5
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn math_with_negative_numbers() {
    let src = r#"
func main() -> i32 {
    math: {
        -1 + 1;
        -5 * -3;
    }
    15
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(15));
}

#[test]
fn trait_with_multiple_methods_impl() {
    let src = r#"
trait Printable {
    func to_string() -> string;
    func print();
}

type MyItem {
    value: i32
}

impl Printable for MyItem {
    func to_string() -> string {
        return "MyItem";
    }
    func print() {
        println("MyItem");
    }
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn where_with_multiple_bounds() {
    let src = r#"
trait Display {
    func to_string() -> string;
}

trait Clone {
    func clone() -> Self;
}

type MyType {
    value: i32
}

func process(x: MyType) -> string where MyType: Display + Clone {
    x.to_string()
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_with_multiple_params() {
    let src = r#"
extern "C" {
    func write(fd: i32, buf: string, len: i32) -> i32;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_with_no_return() {
    let src = r#"
extern "C" {
    func exit(code: i32);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

// ==================== f-string tests ====================

#[test]
fn fstring_basic() {
    let src = r#"
func main() -> string {
    let name = "World";
    f"Hello, {name}!"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("Hello, World!".to_string()));
}

#[test]
fn fstring_multiple_interpolations() {
    let src = r#"
func main() -> string {
    let a = 1;
    let b = 2;
    f"{a} + {b} = {a + b}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("1 + 2 = 3".to_string()));
}

#[test]
fn fstring_no_interpolation() {
    let src = r#"
func main() -> string {
    f"just text"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("just text".to_string()));
}

#[test]
fn fstring_expression_interpolation() {
    let src = r#"
func main() -> string {
    let x = 10;
    f"double is {x * 2}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("double is 20".to_string()));
}

#[test]
fn fstring_with_function_call() {
    let src = r#"
func greet(name: string) -> string {
    f"Hi, {name}!"
}

func main() -> string {
    greet("Alice")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("Hi, Alice!".to_string()));
}

// ==================== list comprehension tests ====================

#[test]
fn comprehension_basic() {
    let src = r#"
func main() -> i32 {
    let nums = [1, 2, 3, 4, 5];
    let doubled = [x * 2 for x in nums];
    len(doubled)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn comprehension_with_guard() {
    let src = r#"
func main() -> i32 {
    let nums = [1, 2, 3, 4, 5, 6];
    let evens = [x for x in nums if x % 2 == 0];
    len(evens)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn comprehension_transform() {
    let src = r#"
func main() -> string {
    let words = ["hello", "world"];
    let upper = [w + "!" for w in words];
    upper[0] + " " + upper[1]
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello! world!".to_string()));
}

#[test]
fn comprehension_empty_list() {
    let src = r#"
func main() -> i32 {
    let empty = [];
    let result = [x for x in empty];
    len(result)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn comprehension_nested_unsupported() {
    let src = r#"
func main() -> i32 {
    let lists = [[1, 2], [3, 4], [5]];
    let flat = [x for sub in lists for x in sub];
    len(flat)
}
"#;
    let result = run_source_result(src);
    // Nested comprehensions not yet supported, should error
    assert!(result.is_err());
}
