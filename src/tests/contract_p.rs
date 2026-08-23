//! 0.1.9 Phase 0 — P 合同（整体转移合同）规范正负例。
//!
//! 裁决：`devdocs/kernel-final-verdict-2026-08-18.md` Q2。
//! 迁移期合同 = 一句话：**整体转移才允许未标 kind 的 `T` 接线性实参**。
//!
//! 正例（整体转移，必须过，双后端同跑）：
//! - `identity<T>(x: T) -> T { x }`：直通 `cap` / `List<cap>` / `Option<cap>` /
//!   `List<List<cap>>` 任意嵌套线性容器。
//! - `dropit<T>(x: T) { drop(x) }`：泛型体整体 drop 线性实参。
//!
//! 反例（非整体转移，必须拒 E0432）：
//! - `first<T>(xs: List<T>) -> T { xs[0] }`：元素提取（投影）。
//! - `discard`（`sink<T>(x: T) { 0 }`）：泛型体静默弃置线性实参。
//! - `snd<T>(t: (T, i32)) -> T { t.0 }`：元组投影（不是整体转移）。
//! - `get<T>(o: Option<T>) -> T { o.unwrap() }`：Option 解包（元素提取）。
//!
//! 现状：当前 `linear_blackbox` 已满足全部上述正负例（正例过、反例 E0432 拒），
//! 本模块把它钉死为规范测试；0.1.9 换掉 blackbox 特判网（上 `linear T` 种类、
//! 单态化、drop glue）时，这组必须仍绿。禁止先删 4000 行再猜规则。

use super::*;

fn has_code(errors: &[crate::diagnostic::Diagnostic], code: &str) -> bool {
    errors.iter().any(|e| e.code.as_deref() == Some(code))
}

/// 正例：`identity<T>(x: T) -> T { x }` 以 `cap` 实参调用，整体转移直通。
const IDENTITY_CAP_SRC: &str = r#"
cap FileReadCap;

func identity<T>(x: T) -> T { x }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = identity(c)
    drop(d)
    println("ok")
    0
}
"#;

#[test]
fn p_contract_identity_cap_passes_check() {
    check_source(IDENTITY_CAP_SRC).expect("identity(cap) whole-value pass-through must check");
}

#[test]
fn p_contract_identity_cap_dual_backend_runs() {
    if !can_link() {
        return;
    }
    // Bytecode VM path.
    let (_v, interp_stdout) = checked_run_source_with_stdout(IDENTITY_CAP_SRC);
    assert_eq!(
        interp_stdout.trim(),
        "ok",
        "VM must run identity(cap) whole-value pass-through"
    );
    // Production compile_checked native path.
    let native = checked_codegen_compile_and_run(IDENTITY_CAP_SRC)
        .expect("identity(cap) must compile_checked and run natively");
    assert_eq!(
        native.trim(),
        "ok",
        "native must run identity(cap) whole-value pass-through"
    );
}

/// 反例：`first<T>(xs: List<T>) -> T { xs[0] }` 对 `List<cap>` 做元素提取——
/// 不是整体转移，必须拒。
const FIRST_EXTRACT_SRC: &str = r#"
cap FileReadCap;

func first<T>(xs: List<T>) -> T { xs[0] }
func main() -> i32 {
    let fs: List<cap FileReadCap> = [FileReadCap]
    let f = first(fs)
    drop(f)
    0
}
"#;

#[test]
fn p_contract_first_element_extraction_rejected() {
    let errs = check_source(FIRST_EXTRACT_SRC)
        .expect_err("first(xs[0]) element extraction must be rejected");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "first must be rejected with E0432 (element extraction is not whole-value transfer), got {:?}",
        errs
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 反例：泛型体 `sink<T>(x: T) -> i32 { 0 }` 把线性实参静默弃置，必须拒。
const DISCARD_SRC: &str = r#"
cap FileReadCap;

func sink<T>(x: T) -> i32 { 0 }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    sink(c)
    0
}
"#;

#[test]
fn p_contract_discard_rejected() {
    let errs = check_source(DISCARD_SRC)
        .expect_err("generic body discarding a linear arg must be rejected");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "discard must be rejected with E0432 (generic body must not drop/ignore a linear arg), got {:?}",
        errs
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 正例：`identity` 直通 `List<cap>`（容器整体线性，整体转移）。
const IDENTITY_LIST_CAP_SRC: &str = r#"
cap FileReadCap;

func identity<T>(x: T) -> T { x }
func main() -> i32 {
    let fs: List<cap FileReadCap> = [FileReadCap]
    let gs = identity(fs)
    drop(gs)
    println("ok")
    0
}
"#;

#[test]
fn p_contract_identity_list_cap_passes_check() {
    check_source(IDENTITY_LIST_CAP_SRC)
        .expect("identity(List<cap>) whole-value pass-through must check");
}

#[test]
fn p_contract_identity_list_cap_dual_backend_runs() {
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(IDENTITY_LIST_CAP_SRC);
    assert_eq!(interp.trim(), "ok");
    let native = checked_codegen_compile_and_run(IDENTITY_LIST_CAP_SRC)
        .expect("identity(List<cap>) must compile_checked and run natively");
    assert_eq!(native.trim(), "ok");
}

/// 正例：`identity` 直通 `Option<cap>`。
const IDENTITY_OPT_CAP_SRC: &str = r#"
cap FileReadCap;

func identity<T>(x: T) -> T { x }
func main() -> i32 {
    let o: Option<cap FileReadCap> = Some(FileReadCap)
    let p = identity(o)
    drop(p)
    println("ok")
    0
}
"#;

#[test]
fn p_contract_identity_option_cap_passes_check() {
    check_source(IDENTITY_OPT_CAP_SRC)
        .expect("identity(Option<cap>) whole-value pass-through must check");
}

#[test]
fn p_contract_identity_option_cap_dual_backend_runs() {
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(IDENTITY_OPT_CAP_SRC);
    assert_eq!(interp.trim(), "ok");
    let native = checked_codegen_compile_and_run(IDENTITY_OPT_CAP_SRC)
        .expect("identity(Option<cap>) must compile_checked and run natively");
    assert_eq!(native.trim(), "ok");
}

/// 正例：`identity` 直通 `List<List<cap>>`（任意嵌套线性容器整体转移）。
const IDENTITY_NESTED_CAP_SRC: &str = r#"
cap FileReadCap;

func identity<T>(x: T) -> T { x }
func main() -> i32 {
    let f: List<List<cap FileReadCap>> = [[FileReadCap]]
    let g = identity(f)
    drop(g)
    println("ok")
    0
}
"#;

#[test]
fn p_contract_identity_nested_cap_passes_check() {
    check_source(IDENTITY_NESTED_CAP_SRC)
        .expect("identity(List<List<cap>>) whole-value pass-through must check");
}

/// 正例：泛型体整体 `drop(x)` 线性实参（整体转移的 drop 面）。
const DROPIT_SRC: &str = r#"
cap FileReadCap;

func dropit<T>(x: T) -> i32 { drop(x); 0 }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    dropit(c)
    0
}
"#;

#[test]
fn p_contract_whole_value_drop_passes_check() {
    check_source(DROPIT_SRC).expect("dropit<T>{ drop(x) } whole-value drop must check");
}

/// 反例：元组投影 `snd<T>(t: (T, i32)) -> T { t.0 }` 非整体转移，必须拒。
const SND_PROJECT_SRC: &str = r#"
cap FileReadCap;

func snd<T>(t: (T, i32)) -> T { t.0 }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let x = snd((c, 1))
    drop(x)
    0
}
"#;

#[test]
fn p_contract_tuple_projection_rejected() {
    let errs = check_source(SND_PROJECT_SRC)
        .expect_err("tuple projection t.0 of a linear container must be rejected");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "snd(t.0) must be rejected with E0432 (projection is not whole-value transfer), got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 反例：Option 解包 `get<T>(o: Option<T>) -> T { o.unwrap() }` 非整体转移，必须拒。
const UNWRAP_SRC: &str = r#"
cap FileReadCap;

func get<T>(o: Option<T>) -> T { o.unwrap() }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let x = get(Some(c))
    drop(x)
    0
}
"#;

#[test]
fn p_contract_option_unwrap_rejected() {
    let errs = check_source(UNWRAP_SRC)
        .expect_err("Option::unwrap of a linear container must be rejected");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "get(o.unwrap()) must be rejected with E0432 (element extraction), got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}
