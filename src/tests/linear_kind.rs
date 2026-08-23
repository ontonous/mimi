//! 0.1.9 Phase A — `linear T` 种类。
//!
//! 路线：`devdocs/v0.39/README.md` Phase A（0.39.9–30）；分片规划
//! `devdocs/v0.39/phase-a-plan.md`。
//! - 0.39.2：语法 + AST —— `linear` 上下文软关键字（generic 参数位置识别，
//!   其余位置仍是普通标识符），`GenericParam.kind` 记录 Linear/Free。
//! - 0.39.9：语义 —— `linear T` 参数**定义时** transfer-only 体校验（E0841），
//!   调用点对线性实参 kind 兼容放行；Free `T` 仍走迁移 blackbox（P 合同不变）。
//! 后续切片：感染（`List<T>`/记录字段 kind 流）、种类不匹配独立码收口、单态化。

use super::*;

fn has_code(errors: &[crate::diagnostic::Diagnostic], code: &str) -> bool {
    errors.iter().any(|e| e.code.as_deref() == Some(code))
}

/// 正例：`pass<linear T>` 直通 `cap`，check 过且双后端同跑。
const LINEAR_PASS_CAP_SRC: &str = r#"
cap FileReadCap;

func pass<linear T>(x: T) -> T { x }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = pass(c)
    drop(d)
    println("ok")
    0
}
"#;

#[test]
fn linear_kind_pass_through_cap_passes_check() {
    check_source(LINEAR_PASS_CAP_SRC).expect("pass<linear T> whole-value pass-through must check");
}

#[test]
fn linear_kind_pass_through_cap_dual_backend_runs() {
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(LINEAR_PASS_CAP_SRC);
    assert_eq!(interp.trim(), "ok");
    let native = checked_codegen_compile_and_run(LINEAR_PASS_CAP_SRC)
        .expect("pass<linear T> must compile_checked and run natively");
    assert_eq!(native.trim(), "ok");
}

/// `linear` 是上下文软关键字：generic 参数位置外仍是普通标识符。
#[test]
fn linear_kind_still_usable_as_identifier() {
    check_source(
        r#"
func main() -> i32 {
    let linear = 7
    println(linear)
    0
}
"#,
    )
    .expect("`linear` outside generic-param position must stay a normal identifier");
}

/// 当前迁移合同下，**未标 kind** 的 `T` 做整体转移（直通）也可接线性实参——
/// 这是 P 合同的核心（`contract_p` 已钉死）。Free/Linear 的**种类不匹配**强制
/// 属于后续 Phase A 切片（上 `linear T` 种类 + 感染时再收紧）；本切片只保证
/// `linear T` 语法被接受且不改变既有 P 合同行为。
#[test]
fn linear_kind_unmarked_pass_through_contract_unchanged() {
    check_source(
        r#"
cap FileReadCap;

func identity<T>(x: T) -> T { x }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = identity(c)
    drop(d)
    0
}
"#,
    )
    .expect("unmarked T whole-value pass-through with cap must stay legal (P 合同)");
}

/// `linear T` 参数仍受整体转移约束：元素提取（投影）必须拒。
#[test]
fn linear_kind_projection_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;

func first<linear T>(xs: List<T>) -> T { xs[0] }
func main() -> i32 {
    let fs: List<cap FileReadCap> = [FileReadCap]
    let f = first(fs)
    drop(f)
    0
}
"#,
    )
    .expect_err("linear T element extraction must be rejected");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0841),
        "linear T projection xs[0] must be rejected at definition time with E0841, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 反例（定义时）：`sink<linear T>{ 0 }` 静默弃置线性实参 → E0841。
#[test]
fn linear_kind_def_time_discard_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;

func sink<linear T>(x: T) -> i32 { 0 }
func main() -> i32 { 0 }
"#,
    )
    .expect_err("linear T body discarding the param must be rejected at definition time");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0841),
        "discard must be E0841 at definition time, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 反例（定义时）：`dropit<linear T>{ drop(x); 0 }` 对 T 做 drop → E0841
/// （T 可能实例化为 Session，transfer-only 禁止 drop(T)）。
#[test]
fn linear_kind_def_time_drop_of_t_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;

func dropit<linear T>(x: T) -> i32 { drop(x); 0 }
func main() -> i32 { 0 }
"#,
    )
    .expect_err("linear T body dropping T must be rejected at definition time");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0841),
        "drop(T) must be E0841 at definition time (T may be Session), got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 正例（调用点 kind 兼容）：`pass<linear T>` 定义时体校验通过后，
/// 调用点线性实参直接放行（无 E0432）。
#[test]
fn linear_kind_call_site_accepts_linear_arg() {
    check_source(
        r#"
cap FileReadCap;

func pass<linear T>(x: T) -> T { x }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = pass(c)
    drop(d)
    0
}
"#,
    )
    .expect("linear T param must accept a linear cap arg at the call site (kind-compatible)");
}

/// 反例（定义时，容器参数）：`first<linear T>(xs: List<T>) -> T { xs[0] }`
/// 对线性容器参数做元素提取 → E0841。
#[test]
fn linear_kind_container_param_projection_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;

func first<linear T>(xs: List<T>) -> T { xs[0] }
func main() -> i32 { 0 }
"#,
    )
    .expect_err("linear T container param projection must be rejected at definition time");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0841),
        "container param xs[0] must be E0841 at definition time, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────
// 0.39.10-14 感染（container / record kind 流）
// `linear T` 经容器/记录/嵌套容器整体转移，定义时校验 + 调用点放行。
// （注：`len(线性容器)` 读借用存在既有 E0304 gap，见 phase-a-plan §6；
//  这些正例不含该触发面。）
// ─────────────────────────────────────────────────────────────

/// 正例：`pass_list<linear T>(xs: List<T>) -> List<T>` 对 `List<cap>` 整体转移。
const INF_LIST_SRC: &str = r#"
cap FileReadCap;

func pass_list<linear T>(xs: List<T>) -> List<T> { xs }
func main() -> i32 {
    let fs: List<cap FileReadCap> = [FileReadCap]
    let gs = pass_list(fs)
    drop(gs)
    println("ok")
    0
}
"#;

#[test]
fn linear_kind_infection_list_dual_backend() {
    check_source(INF_LIST_SRC).expect("List<linear T> whole-pass must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(INF_LIST_SRC);
    assert_eq!(interp.trim(), "ok");
    let native = checked_codegen_compile_and_run(INF_LIST_SRC)
        .expect("List<linear T> whole-pass must compile_checked and run natively");
    assert_eq!(native.trim(), "ok");
}

/// 正例：`pass_box<linear T>(b: Box<T>) -> Box<T>` 记录字段感染（Box<cap> 整体线性）。
const INF_RECORD_SRC: &str = r#"
cap FileReadCap;
type Box<linear T> { data: T, tag: i32 }

func pass_box<linear T>(b: Box<T>) -> Box<T> { b }
func main() -> i32 {
    let b: Box<cap FileReadCap> = Box { data: FileReadCap, tag: 7 }
    let b2 = pass_box(b)
    drop(b2)
    println("ok")
    0
}
"#;

#[test]
fn linear_kind_infection_record_dual_backend() {
    check_source(INF_RECORD_SRC).expect("Box<linear T> record field infection must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(INF_RECORD_SRC);
    assert_eq!(interp.trim(), "ok");
    let native = checked_codegen_compile_and_run(INF_RECORD_SRC)
        .expect("Box<linear T> whole-pass must compile_checked and run natively");
    assert_eq!(native.trim(), "ok");
}

/// 正例：`pass_opt<linear T>(o: Option<T>) -> Option<T>`。
const INF_OPT_SRC: &str = r#"
cap FileReadCap;

func pass_opt<linear T>(o: Option<T>) -> Option<T> { o }
func main() -> i32 {
    let o: Option<cap FileReadCap> = Some(FileReadCap)
    let p = pass_opt(o)
    drop(p)
    println("ok")
    0
}
"#;

#[test]
fn linear_kind_infection_option_dual_backend() {
    check_source(INF_OPT_SRC).expect("Option<linear T> whole-pass must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(INF_OPT_SRC);
    assert_eq!(interp.trim(), "ok");
    let native = checked_codegen_compile_and_run(INF_OPT_SRC)
        .expect("Option<linear T> whole-pass must compile_checked and run natively");
    assert_eq!(native.trim(), "ok");
}

/// 正例：`pass_nest<linear T>(xs: List<Box<T>>)` 任意嵌套感染。
const INF_NEST_SRC: &str = r#"
cap FileReadCap;
type Box<linear T> { data: T, tag: i32 }

func pass_nest<linear T>(xs: List<Box<T>>) -> List<Box<T>> { xs }
func main() -> i32 {
    let xs: List<Box<cap FileReadCap>> = [Box { data: FileReadCap, tag: 1 }]
    let ys = pass_nest(xs)
    drop(ys)
    println("ok")
    0
}
"#;

#[test]
fn linear_kind_infection_nested_dual_backend() {
    check_source(INF_NEST_SRC).expect("List<Box<linear T>> nested infection must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(INF_NEST_SRC);
    assert_eq!(interp.trim(), "ok");
    let native = checked_codegen_compile_and_run(INF_NEST_SRC)
        .expect("List<Box<linear T>> whole-pass must compile_checked and run natively");
    assert_eq!(native.trim(), "ok");
}

/// 反例：`take<linear T>(b: Box<T>) -> T { b.data }` 记录字段投影 → 定义时 E0841。
#[test]
fn linear_kind_infection_record_field_projection_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;
type Box<linear T> { data: T, tag: i32 }

func take<linear T>(b: Box<T>) -> T { b.data }
func main() -> i32 {
    let b: Box<cap FileReadCap> = Box { data: FileReadCap, tag: 1 }
    let d = take(b)
    drop(d)
    0
}
"#,
    )
    .expect_err("record field projection of linear T must be rejected");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0841),
        "b.data projection must be E0841 at definition time, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────
// M-ARG-001 修复：方法实参线性转移（trait 方法收 cap / linear T）
// 此前 View/Mutate 方法调用整组跳过实参消费 → r.take(c) 调用方 E0256。
// ─────────────────────────────────────────────────────────────

/// 回归：trait 方法 `take(x: cap)`，`r.take(c)` 调用方必须把 `c` 移入方法。
#[test]
fn method_arg_cap_transfer_consumes_at_caller() {
    check_source(
        r#"
cap FileReadCap;
trait Wrap {
    func take(x: cap FileReadCap) -> i32;
}
type Rec { v: i32 }
impl Wrap for Rec {
    func take(x: cap FileReadCap) -> i32 { drop(x); 0 }
}
func main() -> i32 {
    let r = Rec { v: 1 }
    let c: cap FileReadCap = FileReadCap
    r.take(c)
    0
}
"#,
    )
    .expect("method cap argument must move into the callee (no caller E0256)");
}

/// 回归：trait 方法 `pass<linear T>(x: T) -> T`，`r.pass(c)` 双后端同跑。
#[test]
fn linear_kind_method_receiver_with_linear_arg_dual_backend() {
    let src = r#"
cap FileReadCap;
trait Wrap {
    func pass<linear T>(x: T) -> T;
}
type Rec { v: i32 }
impl Wrap for Rec {
    func pass<linear T>(x: T) -> T { x }
}
func main() -> i32 {
    let r = Rec { v: 1 }
    let c: cap FileReadCap = FileReadCap
    let d = r.pass(c)
    drop(d)
    println("ok")
    0
}
"#;
    check_source(src).expect("linear T method call must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "ok");
    let native =
        checked_codegen_compile_and_run(src).expect("linear T method call must run natively");
    assert_eq!(native.trim(), "ok");
}

// ─────────────────────────────────────────────────────────────
// LEN-READ-001 修复：线性容器读指标（len/is_empty）按借用不消费
// 此前 len(fs) 自由调用被当作 Move → drop(fs) 假 E0304。
// ─────────────────────────────────────────────────────────────

/// 回归：`len(线性容器)` 读借用后仍可 drop 恰一次。
#[test]
fn linear_container_read_len_then_drop() {
    let src = r#"
cap FileReadCap;

func pass_list<T>(xs: List<T>) -> List<T> { xs }
func main() -> i32 {
    let fs: List<cap FileReadCap> = [FileReadCap]
    let gs = pass_list(fs)
    let n = len(gs)
    drop(gs)
    println(n)
    0
}
"#;
    check_source(src).expect("len(linear container) must borrow, then drop once");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "1");
    let native =
        checked_codegen_compile_and_run(src).expect("len(linear container) must run natively");
    assert_eq!(native.trim(), "1");
}

/// 回归：`is_empty(线性容器)` 读借用后仍可 drop 恰一次。
#[test]
fn linear_container_read_is_empty_then_drop() {
    let src = r#"
cap FileReadCap;

func main() -> i32 {
    let fs: List<cap FileReadCap> = [FileReadCap]
    let e = is_empty(fs)
    drop(fs)
    println(e)
    0
}
"#;
    // 只做 checker 级断言：`is_empty` 自由 builtin 尚未在 codegen 实现
    // （E0700），CFG 借用豁免本身在 checker 层已可验证。
    check_source(src).expect("is_empty(linear container) must borrow, then drop once");
}

// ─────────────────────────────────────────────────────────────
// 0.39.21-30 单态化前置 + drop glue 对齐（Phase B 边界）
// `linear T` 泛型调用单态化 VM/Resolved 同一表示；感染容器 drop glue 恰好一次；
// linear_blackbox 特判与 kind 规则无矛盾（audit：全部 5 个 blackbox 调用点
// simple.rs×2 / method.rs×2 / func.rs 定义时，均正确区分 linear T / Free T）。
// ─────────────────────────────────────────────────────────────

/// 单态化：`swap2<linear T>` 同一泛型两个实例化（cap + i32）双后端等价。
#[test]
fn linear_kind_monomorphization_multi_instantiation() {
    let src = r#"
cap FileReadCap;

func swap2<linear T>(a: T, b: T) -> (T, T) { (b, a) }
func main() -> i32 {
    let c1: cap FileReadCap = FileReadCap
    let c2: cap FileReadCap = FileReadCap
    let (x, y) = swap2(c1, c2)
    drop(x)
    drop(y)
    let (p, q) = swap2(10, 20)
    println(p + q)
    0
}
"#;
    check_source(src).expect("linear T multi-instantiation must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "30");
    let native = checked_codegen_compile_and_run(src)
        .expect("linear T multi-instantiation must run natively");
    assert_eq!(native.trim(), "30");
}

/// drop glue 恰好一次：List<cap> 3 元素整体转移后 drop，双后端等价（无泄漏/双 drop）。
#[test]
fn linear_kind_drop_glue_once_infected_list() {
    let src = r#"
cap FileReadCap;

func pass_list<linear T>(xs: List<T>) -> List<T> { xs }
func main() -> i32 {
    let fs: List<cap FileReadCap> = [FileReadCap, FileReadCap, FileReadCap]
    let gs = pass_list(fs)
    drop(gs)
    println(0)
    0
}
"#;
    check_source(src).expect("infected List drop glue once must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "0");
    let native = checked_codegen_compile_and_run(src)
        .expect("infected List drop glue once must run natively");
    assert_eq!(native.trim(), "0");
}

/// drop glue 恰好一次：记录字段感染（Box<cap>）drop 双后端等价。
#[test]
fn linear_kind_drop_glue_once_infected_record() {
    let src = r#"
cap FileReadCap;
type Box<linear T> { data: T, tag: i32 }

func pass_box<linear T>(b: Box<T>) -> Box<T> { b }
func main() -> i32 {
    let b: Box<cap FileReadCap> = Box { data: FileReadCap, tag: 1 }
    let b2 = pass_box(b)
    drop(b2)
    println(0)
    0
}
"#;
    check_source(src).expect("infected record drop glue once must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "0");
    let native = checked_codegen_compile_and_run(src)
        .expect("infected record drop glue once must run natively");
    assert_eq!(native.trim(), "0");
}
