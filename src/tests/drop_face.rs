//! 0.39.56 — Drop 面正负集（Phase C 前置规范）
//!
//! Phase C「换黑盒」的头号前置是**整体 drop 的可表达性**（见
//! `devdocs/v0.39/phase-c-plan.md §2`）。`linear T` transfer-only 禁 drop T
//! （T 可能 Session）；但 drop-tolerant 泛型面（`sink_g<T>{ drop(x) }`）在
//! 循环/if-let/match 内消费线性元素是 22 个 `dual_generic_linear_*` 测试承载的
//! 真实模式。本文件把该面显式固化为正集（全部应绿，双后端），作为 0.39.57
//! 裁决 (a) `linear drop T` vs (b) 精简 drop-only 泛型面的基线。

use super::*;

fn has_code(errors: &[crate::diagnostic::Diagnostic], code: &str) -> bool {
    errors.iter().any(|d| d.code.as_deref() == Some(code))
}
use super::*;

/// 正集 1：泛型体整体 drop 线性实参（简单调用）。
#[test]
fn drop_face_simple_generic_drop_cap() {
    check_source(
        r#"
cap FileReadCap;

func sink_g<linear drop T> (x: T) -> i32 { drop(x); 1 }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    sink_g(c)
    0
}
"#,
    )
    .expect("generic whole-drop of a cap must stay legal (drop face)");
}

/// 正集 2：泛型循环消费线性元素（for x in List<cap> { sink_g(x) }）。
#[test]
fn drop_face_generic_loop_element_consumption_dual() {
    let src = r#"
cap FileReadCap;

func sink_g<linear drop T> (x: T) -> i32 { drop(x); 1 }
func count<linear drop T> (v: List<T>) -> i32 {
    let mut n = 0
    for x in v { n = n + sink_g(x) }
    n
}
func main() -> i32 {
    let l: List<cap FileReadCap> = [FileReadCap, FileReadCap]
    println(count(l))
    0
}
"#;
    check_source(src).expect("generic loop element consumption must check");
    if !can_link() {
        return;
    }
    let expected = "2";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 正集 3：if-let / match 内 drop 消费线性元素。
#[test]
fn drop_face_iflet_option_element_drop_dual() {
    let src = r#"
cap FileReadCap;

func sink_g<linear drop T> (x: T) -> i32 { drop(x); 1 }
func f<linear drop T> (o: Option<T>) -> i32 {
    let mut n = 0
    if let Some(x) = o { n = n + sink_g(x) } else { n = n + 0 }
    n
}
func main() -> i32 {
    let o: Option<cap FileReadCap> = Some(FileReadCap)
    println(f(o))
    0
}
"#;
    check_source(src).expect("if-let drop consumption must check");
    if !can_link() {
        return;
    }
    let expected = "1";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 正集 4：match 通配臂（Some(x) 消费、_ 臂放行）。
#[test]
fn drop_face_match_wildcard_cap_dual() {
    let src = r#"
cap FileReadCap;

func sink_g<linear drop T> (x: T) -> i32 { drop(x); 1 }
func f<linear drop T> (o: Option<T>) -> i32 {
    match o { Some(x) => sink_g(x), _ => 0 }
}
func main() -> i32 {
    let o: Option<cap FileReadCap> = Some(FileReadCap)
    println(f(o))
    0
}
"#;
    check_source(src).expect("match wildcard drop consumption must check");
    if !can_link() {
        return;
    }
    let expected = "1";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 正集 5：容器整体转移过泛型 + 具体面消费元素（drop face 的转移侧）。
#[test]
fn drop_face_container_whole_transfer_dual() {
    let src = r#"
cap FileReadCap;

func id_list<linear T> (v: List<T>) -> List<T> { v }
func sink(c: cap FileReadCap) -> i32 { drop(c); 5 }
func main() -> i32 {
    let l = [FileReadCap, FileReadCap]
    let l2 = id_list(l)
    let mut t = 0
    for c in l2 { t = t + sink(c) }
    println(t)
    0
}
"#;
    check_source(src).expect("container whole-transfer must check");
    if !can_link() {
        return;
    }
    let expected = "10";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 正集 6：let 绑定 sink（drop 面经中间绑定）。
#[test]
fn drop_face_let_sink_cap_dual() {
    let src = r#"
cap FileReadCap;

func take_g<linear drop T> (x: T) -> i32 { drop(x); 1 }
func f<linear drop T> (v: List<T>) -> i32 {
    let mut n = 0
    for x in v { let k = take_g(x); n = n + k }
    n
}
func main() -> i32 {
    let l: List<cap FileReadCap> = [FileReadCap]
    println(f(l))
    0
}
"#;
    check_source(src).expect("let-sink drop consumption must check");
    if !can_link() {
        return;
    }
    let expected = "1";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 负集 1：SessionChan 经 drop-tolerant 泛型体 drop → E0432（transfer-only，
/// T 可能 Session）。`channel_new()` 返回可 drop 的 `Channel<i64>`（非 Session），
/// 故必须用 `session_pair` 造真实 SessionChan 才触发。
#[test]
fn drop_face_sessionchan_drop_rejected() {
    let errs = check_source(
        r#"
session S = !i32 . ?i32 . end
func dropit<T>(x: T) -> i32 { drop(x); 42 }
func main() -> i32 {
    let (ch0, ch1) = session_pair::<S>()
    let r = dropit(ch0)
    let n = session_recv(ch1)
    session_send(ch1, n + 1)
    session_close(ch1)
    println(r)
    0
}
"#,
    )
    .expect_err("SessionChan drop inside drop-tolerant generic must be rejected");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "SessionChan + dropit<T>{{drop}} must be E0432 (transfer-only), got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 负集 2：`linear drop T` 仅单路径 drop（另一路径弃置）→ E0841（定义时：
/// 每路径必须消费 T——transfer 或 drop）。
#[test]
fn drop_face_partial_path_drop_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;
func f<linear drop T> (b: bool, x: T) -> i32 { if b { drop(x); 0 } else { 0 } }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    f(true, c)
    0
}
"#,
    )
    .expect_err("single-path drop of a linear drop T must be rejected");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0841),
        "partial-path drop of linear drop T must be E0841 (not whole-consumed), got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}
