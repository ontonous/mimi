use super::*;
#[test]
fn fstring_escape_sequences() {
    let src = r#"
func main() -> string {
    "hello\nworld"
}
"#;
    assert_eq!(
        run_source_bytecode_result(src),
        Ok(interp::Value::String(Arc::new("hello\nworld".to_string())))
    );
}

#[test]
fn comprehension_filter_all() {
    let src = r#"
func main() -> i32 {
    let result = [x for x in [1, 2, 3] if false];
    len(result)
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
}

#[test]
fn comprehension_transform_strings() {
    let src = r#"
func main() -> i32 {
    let result = [len(x) for x in ["a", "ab", "abc"]];
    result[2]
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(3)));
}

#[test]
fn tuple_index() {
    let src = r#"
func main() -> i32 {
    let t = (1, 2, 3);
    t.1
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(2)));
}

#[test]
fn match_on_literal() {
    let src = r#"
func main() -> i32 {
    match 42 {
        42 => 100,
        _ => 0,
    }
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(100)));
}

#[test]
fn match_on_string() {
    let src = r#"
func main() -> i32 {
    match "hello" {
        "world" => 0,
        "hello" => 1,
        _ => 2,
    }
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(1)));
}

#[test]
fn nested_if_else() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    if x > 0 {
        if x > 3 {
            10
        } else {
            5
        }
    } else {
        0
    }
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(10)));
}

#[test]
fn while_with_break_equivalent() {
    let src = r#"
func main() -> i32 {
    let mut i = 0;
    while i < 5 {
        i = i + 1;
    }
    i
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(5)));
}

#[test]
fn type_alias_simple() {
    let src = r#"
type Age = i32;

func main() -> i32 {
    let a: Age = 25;
    a
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(25)));
}

#[test]
fn newtype_isolation_runtime() {
    let src = r#"
newtype UserId = i32;

func main() -> i32 {
    let id = UserId(42);
    42
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

#[test]
fn record_field_order_independent() {
    let src = r#"
type Point {
    x: i32,
    y: i32
}

func main() -> i32 {
    let p = Point { y: 10, x: 5 };
    p.x + p.y
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(15)));
}

#[test]
fn closure_capture_and_call() {
    let src = r#"
func main() -> i32 {
    let x = 10;
    let f = fn(y: i32) -> i32 { x + y };
    f(5)
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(15)));
}

#[test]
fn closure_no_params() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let f = fn() -> i32 { x };
    f()
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

#[test]
fn strict_mode_non_locked_ok() {
    let src = r#"
func main() -> i32 {
    42
}
"#;
    let result = check_source_strict(src);
    assert!(
        result.is_ok(),
        "non-locked function should pass strict mode: {:?}",
        result.err()
    );
}

#[test]
fn desc_statement() {
    let src = r#"
func main() -> i32 {
    desc "this is a description";
    42
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

#[test]
fn rule_statement() {
    let src = r#"
func main() -> i32 {
    rule "this is a rule";
    42
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

#[test]
fn on_failure_basic() {
    let src = r#"
func main() -> i32 {
    on failure {
        println("cleanup");
    }
    42
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

#[test]
fn shared_ownership_basic() {
    let src = r#"
func main() -> i32 {
    shared x = 42;
    let y = x;
    42
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "shared ownership should pass: {:?}",
        result.err()
    );
}

#[test]
fn weak_shared_basic() {
    let src = r#"
func main() -> i32 {
    shared x = 42;
    weak w = x;
    42
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "weak from shared should pass: {:?}",
        result.err()
    );
}

#[test]
fn try_operator_option() {
    let src = r#"
type MyOption {
    Some(i32),
    None
}

func safe_div(a: i32, b: i32) -> MyOption {
    if b == 0 {
        None
    } else {
        Some(a / b)
    }
}

func main() -> i32 {
    42
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

// ===== T300: 泛型单态化测试 =====

#[test]
fn ref_basic_creation_and_deref() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r = &x;
    *r
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

#[test]
fn ref_mut_basic() {
    // &mut x creates a mutable reference that holds a copy of the value
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    let r = &mut x;
    *r
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(10)));
}

#[test]
fn ref_does_not_move() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r = &x;
    let y = x;
    y + *r
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(84)));
}

#[test]
fn ref_mut_through_deref_assign() {
    // *r modifies the reference's inner value
    let src = r#"
func main() -> i32 {
    let mut x = 5;
    let r = &mut x;
    *r = *r + 10;
    *r
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(15)));
}

#[test]
fn ref_type_check_basic() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r = &x;
    *r
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn ref_mut_type_check() {
    let src = r#"
func main() -> i32 {
    let mut x = 10;
    let r = &mut x;
    *r = 20;
    x
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn ref_type_check_deref_non_ref_error() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    *x
}
"#;
    let err = check_source(src).unwrap_err();
    assert!(err.iter().any(|d| d.message.contains("cannot dereference")));
}

#[test]
fn ref_mut_assign_through_imm_ref_error() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r = &x;
    *r = 10;
    x
}
"#;
    let err = check_source(src).unwrap_err();
    assert!(err.iter().any(|d| d.message.contains("non-mutable")));
}

#[test]
fn ref_multiple_immut_borrows() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r1 = &x;
    let r2 = &x;
    *r1 + *r2
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(84)));
    assert!(check_source(src).is_ok());
}

#[test]
fn ref_nested() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let r = &x;
    let r2 = &r;
    *(*r2)
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

// ===== T303: 模块命名空间（0.39.137 重述为 spec §6.14 合同）=====
// 裁决：Mimi 模块系统是文件级 merge 模型——`use` 把 pub 导出以裸名合并进
// 作用域；`::` 保留给 Flow 转移边。旧 T303 用例绕过 checker 直达 VM，锁定的
// "module 前缀调用可行"是任何 CLI 路径都不可达的僵尸行为（checker 一律拒绝），
// 已按真实合同重写为 fail-closed 断言。

#[test]
fn module_inline_block_rejected_e0445_at_parse() {
    // 0.39.139（spec §6.14 选项 C）：`module` 关键字退役，内联 module 块在
    // 解析期以 E0445 定向拒绝（迁移指引：文件模块 + `use` 裸名合并）。
    let src = r#"
module Math {
    func add(a: i32, b: i32) -> i32 {
        a + b
    }
}

func main() -> i32 {
    0
}
"#;
    let err = parse_error(src);
    assert_eq!(err.code.as_deref(), Some("E0445"), "got: {err:?}");
    assert!(
        err.message.contains("own .mimi file"),
        "diagnostic must carry the migration note: {}",
        err.message
    );
}

#[test]
fn module_nested_inline_block_rejected_e0445_at_parse() {
    // 嵌套内联 module：解析器首个错误即终止（fail-fast 单条）。
    let src = r#"
module Outer {
    module Inner {
        func hello() -> i32 { 42 }
    }
}
func main() -> i32 { 0 }
"#;
    let err = parse_error(src);
    assert_eq!(err.code.as_deref(), Some("E0445"), "got: {err:?}");
    assert!(
        err.message.contains("Outer"),
        "diagnostic must name the outermost block: {}",
        err.message
    );
}

#[test]
fn module_is_an_ordinary_identifier_after_retirement() {
    // 关键字退役的正向锁：`module` 可以再次作为普通标识符使用。
    let src = r#"
func main() -> i32 {
    let module = 40;
    let fn_ref = 2;
    module + fn_ref
}
"#;
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 42);
}

#[test]
fn module_file_merge_bare_call_works() {
    // 文件模块 merge 主路径：use 后裸名调用（spec §6.14）。
    // 单测环境无 loader 多文件合并，用文本拼接 std/csv.mimi 模拟 use 合并。
    let csv_src = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std/csv.mimi"),
    )
    .expect("read std/csv.mimi");
    let src = r#"
func main() -> i32 {
    let rows = parse("a,b\nc,d")
    println(cell(rows, 1, 0))
    0
}
"#;
    let merged = format!("{}\n{}", csv_src, src);
    check_source(&merged).expect("merged csv program must check");
    let (_val, out) = run_source_with_stdout(&merged);
    assert_eq!(out.trim(), "c", "merged bare-name call must work");
}

#[test]
fn module_duplicate_export_fails_loud() {
    // maps 与 set 都导出 size：同时导入必须 fail-loud，不静默遮蔽。
    // duplicate 错误产生于 loader 合并层——经 ModuleLoader 走真实多文件路径。
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("target/tmp/dup_export_probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tmp project");
    let entry = dir.join("main.mimi");
    // 0.39.136 stdlib consolidation removed cross-stdlib collisions; the
    // fail-loud contract is now pinned with two user modules.
    std::fs::write(dir.join("helper.mimi"), "pub func size() -> i32 { 1 }")
        .expect("write helper.mimi");
    std::fs::write(dir.join("other.mimi"), "pub func size() -> i32 { 2 }")
        .expect("write other.mimi");
    std::fs::write(
        &entry,
        "use helper\nuse other\nfunc main() -> i32 { println(size()) }\n",
    )
    .expect("write main.mimi");
    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let err = loader
        .load_main(&entry)
        .err()
        .or_else(|| loader.merge_all().err())
        .expect("duplicate export must fail loud at load/merge time");
    assert!(
        err.contains("duplicate item 'size'"),
        "expected duplicate-item error, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ===== T304: extern FFI 测试 =====

#[test]
fn extern_block_parses() {
    let src = r#"
extern "C" {
    func puts(s: string) -> i32
}

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn extern_block_multiple_funcs() {
    let src = r#"
extern "C" {
    func puts(s: string) -> i32
    func strlen(s: string) -> i32
}

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn extern_function_type_check() {
    let src = r#"
extern "C" {
    func add(a: i32, b: i32) -> i32
}

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn extern_function_wrong_arg_type() {
    let src = r#"
extern "C" {
    func add(a: i32, b: i32) -> i32
}

func main() -> i32 {
    add("hello", 1)
}
"#;
    let err = check_source(src).unwrap_err();
    assert!(err
        .iter()
        .any(|d| d.message.contains("expected i32") || d.message.contains("found string")));
}

#[test]
fn extern_with_no_return() {
    let src = r#"
extern "C" {
    func printf(format: string)
}

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

// === T400: Comptime Reflection Tests ===

#[test]
fn user_func_not_shadowed_by_builtin() {
    let src = r#"
func sum(x: i32) -> i32 {
    x + 100
}

func main() -> i32 {
    sum(5)
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(105)));
}

// === T502: Test Framework Tests ===
