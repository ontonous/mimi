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

/// 0.39.59（Phase C）：正例用 `identity<linear T>` 以 `cap` 实参调用（种类语言）。
/// Free `T` + 线性实参现在一律 E0432（见 `p_contract_free_t_linear_always_rejected`）。
const IDENTITY_CAP_SRC: &str = r#"
cap FileReadCap;

func identity<linear T>(x: T) -> T { x }
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
    check_source(IDENTITY_CAP_SRC).expect("identity<linear T>(cap) must check");
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
        "VM must run identity<linear T>(cap)"
    );
    // Production compile_checked native path.
    let native = checked_codegen_compile_and_run(IDENTITY_CAP_SRC)
        .expect("identity(cap) must compile_checked and run natively");
    assert_eq!(
        native.trim(),
        "ok",
        "native must run identity<linear T>(cap)"
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

/// 0.39.59（Phase C）：正例用 `identity<linear T>` 直通 `List<cap>`。
const IDENTITY_LIST_CAP_SRC: &str = r#"
cap FileReadCap;

func identity<linear T>(x: T) -> T { x }
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
    check_source(IDENTITY_LIST_CAP_SRC).expect("identity<linear T>(List<cap>) must check");
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

/// 0.39.59（Phase C）：正例用 `identity<linear T>` 直通 `Option<cap>`。
const IDENTITY_OPT_CAP_SRC: &str = r#"
cap FileReadCap;

func identity<linear T>(x: T) -> T { x }
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
    check_source(IDENTITY_OPT_CAP_SRC).expect("identity<linear T>(Option<cap>) must check");
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

/// 0.39.59（Phase C）：正例用 `identity<linear T>` 直通 `List<List<cap>>`。
const IDENTITY_NESTED_CAP_SRC: &str = r#"
cap FileReadCap;

func identity<linear T>(x: T) -> T { x }
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
    check_source(IDENTITY_NESTED_CAP_SRC).expect("identity<linear T>(List<List<cap>>) must check");
}

/// 0.39.59（Phase C）：`dropit<T>{ drop(x) }` 整体 drop 线性实参**升格 L 合同**。
/// Free `T` + 线性实参一律 E0432；drop-tolerant 用 `linear drop T`（见
/// `linear_kind::linear_drop_kind_drop_cap_dual`）。
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
fn p_contract_whole_value_drop_reclassified_ll_contract() {
    let errs = check_source(DROPIT_SRC).expect_err(
        "dropit<T>{ drop(x) } with a cap must be rejected (L contract, Free T + linear)",
    );
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "whole-drop Free T + linear must be E0432 (kind mismatch), got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 0.39.59（Phase C）：Free `T` + 线性实参**一律 E0432**（种类不匹配），
/// 即使泛型体整体直通也不再放行（退役调用点体分析）。
const FREE_T_LINEAR_SRC: &str = r#"
cap FileReadCap;

func identity<T>(x: T) -> T { x }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = identity(c)
    drop(d)
    0
}
"#;

#[test]
fn p_contract_free_t_linear_always_rejected() {
    let errs = check_source(FREE_T_LINEAR_SRC)
        .expect_err("Free T + linear arg must be rejected even for whole-value pass-through");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "Free T + cap must be E0432 (kind mismatch), got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
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

/// 0.39.17-20 错误码完善：E0432 保持 P 合同迁移码，但消息必须带迁移提示
/// （`linear T` 改写建议 / 具体签名），帮助用户从黑盒直通迁移到显式种类。
#[test]
fn p_contract_e0432_message_carries_migration_hint() {
    let errs = check_source(FIRST_EXTRACT_SRC)
        .expect_err("first(xs[0]) must still be rejected with E0432");
    let rendered = errs
        .iter()
        .map(|d| format!("{d}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("E0432"),
        "migration rejection must keep E0432 code, got:\n{rendered}"
    );
    assert!(
        rendered.contains("`linear T`"),
        "E0432 message must carry the `linear T` migration hint, got:\n{rendered}"
    );
    assert!(
        rendered.contains("kind mismatch"),
        "E0432 message must state the kind-mismatch reason, got:\n{rendered}"
    );
}
