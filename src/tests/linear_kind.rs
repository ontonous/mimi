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
/// 0.39.59（Phase C）：Free `T` + 线性实参一律 E0432（种类不匹配）——
/// 退役调用点体分析；`identity<linear T>` 才是接线性实参的显式种类。
#[test]
fn linear_kind_free_t_linear_rejected_linear_t_passes() {
    let errs = check_source(
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
    .expect_err("Free T + cap must be rejected (kind mismatch)");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "Free T + cap must be E0432, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
    check_source(
        r#"
cap FileReadCap;

func identity<linear T>(x: T) -> T { x }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = identity(c)
    drop(d)
    0
}
"#,
    )
    .expect("identity<linear T>(cap) must pass");
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

func pass_list<linear T>(xs: List<T>) -> List<T> { xs }
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
    // 0.39.31: `is_empty` codegen 已实现（E0700 关闭）→ 全双后端。
    check_source(src).expect("is_empty(linear container) must borrow, then drop once");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "false");
    let native =
        checked_codegen_compile_and_run(src).expect("is_empty(linear container) must run natively");
    assert_eq!(native.trim(), "false");
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

/// 0.39.31: `is_empty` 自由 builtin codegen（List 线性/空 + String）双后端等价。
#[test]
fn is_empty_free_builtin_codegen_dual_backend() {
    let src = r#"
cap FileReadCap;
func main() -> i32 {
    let fs: List<cap FileReadCap> = [FileReadCap]
    let e = is_empty(fs)
    drop(fs)
    if e { println("nonempty-wrong") } else { println("nonempty") }
    let es: List<i32> = []
    if is_empty(es) { println("empty2") } else { println("empty2-wrong") }
    let s = "hi"
    if is_empty(s) { println("se-wrong") } else { println("sne") }
    0
}
"#;
    check_source(src).expect("is_empty free builtin must check");
    if !can_link() {
        return;
    }
    let expected = "nonempty\nempty2\nsne";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected);
    let native = checked_codegen_compile_and_run(src).expect("is_empty must run natively");
    assert_eq!(native.trim(), expected);
}

/// 0.39.36: `is_empty(map)` 自由 builtin codegen（空/非空 Map）双后端等价。
/// Map 与 Set 都是裸 i64 handle —— 调用点按推断类型分类（map/set）区分。
#[test]
fn is_empty_map_codegen_dual_backend() {
    let src = r#"
func main() -> i32 {
    let m = map_new()
    let m2 = map_set(m, "k", 1)
    let e = is_empty(m2)
    if e { println("map-wrong") } else { println("map-nonempty") }
    let e2 = is_empty(map_new())
    if e2 { println("map-empty") } else { println("map-wrong2") }
    0
}
"#;
    check_source(src).expect("is_empty map must check");
    if !can_link() {
        return;
    }
    let expected = "map-nonempty\nmap-empty";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected);
    let native = checked_codegen_compile_and_run(src).expect("is_empty map must run natively");
    assert_eq!(native.trim(), expected);
}

/// 0.39.36: `is_empty(set)` 自由 builtin codegen（非空 Set）双后端等价。
/// 回归：此前 set handle 误走 mimi_map_size → 原生错报 "empty"。
#[test]
fn is_empty_set_codegen_dual_backend() {
    let src = r#"
func main() -> i32 {
    let s = {1, 2}
    let e = is_empty(s)
    if e { println("set-wrong") } else { println("set-nonempty") }
    0
}
"#;
    check_source(src).expect("is_empty set must check");
    if !can_link() {
        return;
    }
    let expected = "set-nonempty";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected);
    let native = checked_codegen_compile_and_run(src).expect("is_empty set must run natively");
    assert_eq!(native.trim(), expected);
}

// ─────────────────────────────────────────────────────────────
// 0.39.32 列表字面量 cap 移动：绑定 `let fs = [c]` 后 drop 不再假 E0303
// 根因：legacy codegen 的 cap 追踪器在 compile_list_expr 不标记元素消费
//  → 绑定列表字面量里的 cap 永不消费 → 原生 E0303（checker/VM 均过）。
// 修复：store_list_elements 移动时消费元素 cap；call/method/return 可达收集
// 幂等（is_cap_consumed 守卫，避免 sink([c]) 双消费）。
// ─────────────────────────────────────────────────────────────

/// 回归：绑定 `let fs = [c]; drop(fs)` 双后端等价。
#[test]
fn linear_kind_list_literal_cap_binding_drop_dual() {
    let src = r#"
cap FileReadCap;

func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let fs: List<cap FileReadCap> = [c]
    drop(fs)
    println(0)
    0
}
"#;
    check_source(src).expect("bound list-literal cap must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "0");
    let native = checked_codegen_compile_and_run(src)
        .expect("bound list-literal cap must run natively (no E0303)");
    assert_eq!(native.trim(), "0");
}

/// 回归：`pass_list([c])` 内联列表字面量不双消费（call-arg 幂等）。
#[test]
fn linear_kind_list_literal_inline_call_arg_no_double_consume() {
    let src = r#"
cap FileReadCap;

func pass_list<linear T>(xs: List<T>) -> List<T> { xs }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let gs = pass_list([c])
    drop(gs)
    println(0)
    0
}
"#;
    check_source(src).expect("inline [c] call arg must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "0");
    let native = checked_codegen_compile_and_run(src)
        .expect("inline [c] call arg must run natively (no double-consume)");
    assert_eq!(native.trim(), "0");
}

/// 回归：`linear T` 链 + 绑定 `[c]` 原生 E0303 关闭（mc1 场景）。
#[test]
fn linear_kind_list_literal_binding_through_generic_chain() {
    let src = r#"
cap FileReadCap;
type Box<linear T> { data: T, tag: i32 }

func pass_box<linear T>(b: Box<T>) -> Box<T> { b }
func pass_list<linear T>(xs: List<T>) -> List<T> { xs }
func main() -> i32 {
    let b: Box<cap FileReadCap> = Box { data: FileReadCap, tag: 3 }
    let b2 = pass_box(b)
    let c: cap FileReadCap = FileReadCap
    let fs: List<cap FileReadCap> = [c]
    let gs = pass_list(fs)
    drop(b2)
    drop(gs)
    println(0)
    0
}
"#;
    check_source(src).expect("linear T chain with bound [c] must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "0");
    let native = checked_codegen_compile_and_run(src)
        .expect("linear T chain with bound [c] must run natively");
    assert_eq!(native.trim(), "0");
}

// ─────────────────────────────────────────────────────────────
// 0.39.33 双线性参数单态化：`linear T, U` 泛型双后端等价
// ─────────────────────────────────────────────────────────────

/// 回归：`swap2<linear T, U>`（T=cap, U=i32）整体转移 + 双后端等价。
#[test]
fn linear_kind_two_linear_params_dual_backend() {
    let src = r#"
cap FileReadCap;

func swap2<linear T, U>(a: T, b: U) -> (U, T) { (b, a) }
func main() -> i32 {
    let c1: cap FileReadCap = FileReadCap
    let (x, y) = swap2(c1, 42)
    println(x)
    drop(y)
    0
}
"#;
    check_source(src).expect("two linear params must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "42");
    let native = checked_codegen_compile_and_run(src).expect("two linear params must run natively");
    assert_eq!(native.trim(), "42");
}

// ─────────────────────────────────────────────────────────────
// RECORD-LIN-001 修复（0.39.34）：用户记录含 cap 字段按线性追踪
// 此前 is_linear(Plain)=false → let r = Plain { data: c }; drop(r); drop(c)
// 双消费被接受；drop(r) 单独用又 E0256 泄漏。
// ─────────────────────────────────────────────────────────────

/// 回归（反例）：`drop(r); drop(c)`（c 已随 r drop）→ E0304 双消费。
#[test]
fn record_lin_cap_field_double_drop_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;
type Plain { data: cap FileReadCap, tag: i32 }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let r = Plain { data: c, tag: 1 }
    drop(r)
    drop(c)
    println(0)
    0
}
"#,
    )
    .expect_err("record cap field must be tracked as linear (no double drop)");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0304),
        "drop(c) after drop(r) must be E0304 double-consume, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 回归（反例）：`drop(c)`（c 移入 r 后）→ E0304 不可再用。
#[test]
fn record_lin_cap_field_use_after_move_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;
type Plain { data: cap FileReadCap, tag: i32 }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let r = Plain { data: c, tag: 1 }
    drop(c)
    drop(r)
    println(0)
    0
}
"#,
    )
    .expect_err("record cap field must consume c at construction");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0304),
        "drop(c) after move into r must be E0304, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 回归（正例）：绑定 cap 进记录 → 传给消费函数，checker + 双后端等价。
#[test]
fn record_lin_cap_field_bind_then_consume_dual() {
    let src = r#"
cap FileReadCap;
type Plain { data: cap FileReadCap, tag: i32 }
func consume(r: Plain) -> i32 { drop(r); 0 }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let r = Plain { data: c, tag: 1 }
    consume(r)
    println(0)
    0
}
"#;
    check_source(src).expect("record cap bind-then-consume must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "0");
    let native = checked_codegen_compile_and_run(src)
        .expect("record cap bind-then-consume must run natively");
    assert_eq!(native.trim(), "0");
}

// ─────────────────────────────────────────────────────────────
// 0.39.35 深化单态化锁定：跨函数链 / trait 方法 / 容器过链 / SessionChan
// （blackbox 限制：递归 fail-closed，见 phase-a-plan §8 BLACKBOX-REC-001）
// ─────────────────────────────────────────────────────────────

/// 跨函数链：`wrap<linear T>` → `id<linear T>`（cap 直通）双后端等价。
#[test]
fn linear_kind_cross_function_chain_dual() {
    let src = r#"
cap FileReadCap;

func id<linear T>(x: T) -> T { x }
func wrap<linear T>(x: T) -> T { id(x) }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = wrap(c)
    drop(d)
    println(1)
    0
}
"#;
    check_source(src).expect("cross-function linear T chain must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "1");
    let native =
        checked_codegen_compile_and_run(src).expect("cross-function chain must run natively");
    assert_eq!(native.trim(), "1");
}

/// trait 方法 + linear T：`linear T` 函数体调 `r.keep(x)`（keep: T -> T）双后端。
#[test]
fn linear_kind_trait_method_linear_arg_dual() {
    let src = r#"
cap FileReadCap;
trait Keep<T> {
    func keep(x: T) -> T;
}
type Rec { v: i32 }
impl<T> Keep<T> for Rec {
    func keep(x: T) -> T { x }
}
func pass2<linear T>(r: Rec, x: T) -> T { r.keep(x) }
func main() -> i32 {
    let r = Rec { v: 1 }
    let c: cap FileReadCap = FileReadCap
    let d = pass2(r, c)
    drop(d)
    println(2)
    0
}
"#;
    check_source(src).expect("trait method + linear T must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "2");
    let native =
        checked_codegen_compile_and_run(src).expect("trait method + linear T must run natively");
    assert_eq!(native.trim(), "2");
}

/// 容器过链：`wrap2<linear T>(List<T>) -> id(x)`（List<cap> 直通）双后端。
#[test]
fn linear_kind_container_through_chain_dual() {
    let src = r#"
cap FileReadCap;

func id<linear T>(x: T) -> T { x }
func wrap2<linear T>(x: List<T>) -> List<T> { id(x) }
func main() -> i32 {
    let fs: List<cap FileReadCap> = [FileReadCap, FileReadCap]
    let gs = wrap2(fs)
    drop(gs)
    println(3)
    0
}
"#;
    check_source(src).expect("container through linear T chain must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "3");
    let native =
        checked_codegen_compile_and_run(src).expect("container through chain must run natively");
    assert_eq!(native.trim(), "3");
}

/// SessionChan 直通：`echo<linear T>` 转移会话通道，双后端等价。
#[test]
fn linear_kind_session_through_transfer_dual() {
    let src = r#"
func echo<linear T>(x: T) -> T { x }
func main() -> i32 {
    let ch = channel_new()
    let y = echo(ch)
    session_send(y, 7)
    session_recv(y)
    session_close(y)
    println(0)
    0
}
"#;
    check_source(src).expect("SessionChan through linear T must check");
    if !can_link() {
        return;
    }
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), "0");
    let native = checked_codegen_compile_and_run(src)
        .expect("SessionChan through linear T must run natively");
    assert_eq!(native.trim(), "0");
}

// ─────────────────────────────────────────────────────────────
// 0.39.37 SET-REMOVE-CODEGEN-001 闭合：resolved codegen 下全部 Set 方法
// 此前 `s.size()`/`s.remove(v)`/… 以 ResolvedCallee::Builtin("builtin.method.
// set.*") 形式到达 resolved codegen，未接线 → E0709（只有 ProtocolMethod 形式
// 走到 emit_builtin_set_protocol_method）。修复：Builtin 形式也路由到 set
// 协议处理器，且 set 值实参按 mimi_set_* 的 i64 签名做位宽扩到 i64。
// ─────────────────────────────────────────────────────────────

/// 回归：resolved codegen Set 方法全矩阵（size/is_empty/contains/insert/
/// remove/to_list）+ remove 结果喂自由 is_empty，双后端等价。
#[test]
fn set_method_matrix_resolved_dual_backend() {
    let src = r#"
func main() -> i32 {
    let s = {1, 2, 3}
    println(s.size())
    if s.is_empty() { println("wrong") } else { println("nonempty") }
    if s.contains(2) { println("has2") } else { println("wrong2") }
    let s2 = s.insert(4)
    println(s2.size())
    let s3 = s2.remove(4)
    println(s3.size())
    let lst = s3.to_list()
    println(len(lst))
    let e = is_empty(s3)
    if e { println("wrong3") } else { println("final-nonempty") }
    0
}
"#;
    check_source(src).expect("set method matrix must check");
    if !can_link() {
        return;
    }
    let expected = "3\nnonempty\nhas2\n4\n3\n3\nfinal-nonempty";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM output");
    let native = checked_codegen_compile_and_run(src).expect("set method matrix must run natively");
    assert_eq!(native.trim(), expected, "native output");
}

// ─────────────────────────────────────────────────────────────
// 0.39.58 `linear drop T` 种类（Phase C drop 面裁决：候选 (a)）
// drop-tolerant 线性种类：可 drop 亦可转移，实例化必须可 drop（非 Session）。
// 正集：dropit<linear drop T>{drop(x)} 接 cap；转移面同 linear T。
// 负集：linear drop T 实例化 SessionChan → 拒（T 可 drop 约束被违反）。
// ─────────────────────────────────────────────────────────────

/// 正集：`linear drop T` 定义时允许整体 drop T，接 cap 双后端。
#[test]
fn linear_drop_kind_drop_cap_dual() {
    let src = r#"
cap FileReadCap;

func dropit<linear drop T>(x: T) -> i32 { drop(x); 1 }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    println(dropit(c))
    0
}
"#;
    check_source(src).expect("linear drop T + cap drop must check");
    if !can_link() {
        return;
    }
    let expected = "1";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 正集：`linear drop T` 亦允许整体转移（drop 是转移的子集）。
#[test]
fn linear_drop_kind_transfer_dual() {
    let src = r#"
cap FileReadCap;

func pass<linear drop T>(x: T) -> T { x }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = pass(c)
    drop(d)
    println(2)
    0
}
"#;
    check_source(src).expect("linear drop T whole-transfer must check");
    if !can_link() {
        return;
    }
    let expected = "2";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 负集：`linear drop T` 实例化 SessionChan → 拒（Session 不可 drop，违反
/// T 可 drop 约束）。
#[test]
fn linear_drop_kind_sessionchan_instantiation_rejected() {
    let errs = check_source(
        r#"
session S = !i32 . ?i32 . end
func dropit<linear drop T>(x: T) -> i32 { drop(x); 1 }
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
    .expect_err("linear drop T instantiated with SessionChan must be rejected");
    assert!(
        errs.iter().any(|d| {
            d.code.as_deref() == Some(crate::diagnostic::codes::E0432)
                || d.code.as_deref() == Some(crate::diagnostic::codes::E0841)
        }),
        "linear drop T + SessionChan must be rejected (E0432/E0841), got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────
// 0.39.60 BLACKBOX-REC-001 关闭：线性种类自递归（线性黑盒递归回归）
// 自递归把 T 委托给自身；基例（非递归路径）仍强制消费 → 按归纳健全。
// 正集：linear T 递归直通（双后端）、linear drop T 递归 drop 基例。
// 负集：递归基例弃置 T → E0841。
// ─────────────────────────────────────────────────────────────

/// 正集：`count_down<linear T>` 自递归直通（BLACKBOX-REC-001 前 fail-closed）。
#[test]
fn linear_kind_self_recursion_transfer_dual() {
    let src = r#"
cap FileReadCap;

func count_down<linear T>(x: T, n: i32) -> T {
    if n <= 0 { x } else { count_down(x, n - 1) }
}
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = count_down(c, 3)
    drop(d)
    println(5)
    0
}
"#;
    check_source(src).expect("self-recursive linear T transfer must check");
    if !can_link() {
        return;
    }
    let expected = "5";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 正集：`linear drop T` 自递归 + drop 基例。
#[test]
fn linear_kind_self_recursion_drop_base_dual() {
    let src = r#"
cap FileReadCap;

func consume<linear drop T>(x: T, n: i32) -> i32 {
    if n <= 0 { drop(x); 7 } else { consume(x, n - 1) }
}
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    println(consume(c, 3))
    0
}
"#;
    check_source(src).expect("self-recursive linear drop T must check");
    if !can_link() {
        return;
    }
    let expected = "7";
    let (_v, interp) = checked_run_source_with_stdout(src);
    assert_eq!(interp.trim(), expected, "VM");
    let native = checked_codegen_compile_and_run(src).expect("native");
    assert_eq!(native.trim(), expected, "native");
}

/// 负集：递归基例弃置 T（不转移不 drop）→ E0841。
#[test]
fn linear_kind_self_recursion_leaky_base_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;

func count_down<linear T>(x: T, n: i32) -> i32 {
    if n <= 0 { 0 } else { count_down(x, n - 1) }
}
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    count_down(c, 3)
    0
}
"#,
    )
    .expect_err("recursive leaky base case must be rejected");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0841),
        "leaky recursive base must be E0841, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────
// 0.39.61 方法级 `linear T` 定义时体校验（E0841）
// 此前方法完全绕过 E0841/E0432（func_generics 未注册方法泛型 + check_func
// 只处理顶层函数）：泄漏 `linear T` 方法体静默弃值。0.39.61 修复：
//   - Item::Impl 方法泛型注册入 func_generics（含 kind）；
//   - check_linear_kind_param_bodies 提取共享，impl 方法路径调用；
//   - 隐式 self 偏移（funcs 签名 self@0，AST params 无 self）。
// 正集：pass<linear T> 方法直通（双后端，已有 linear_kind_method_*）。
// 负集：leak<linear T> 方法体弃值 → E0841；单路径弃值 → E0841。
// ─────────────────────────────────────────────────────────────

/// 负集：`linear T` 方法体静默弃值（不返回不 drop）→ E0841。
#[test]
fn linear_kind_method_leaky_body_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;
trait Wrap {
    func leak<linear T>(x: T) -> i32;
}
type Rec { v: i32 }
impl Wrap for Rec {
    func leak<linear T>(x: T) -> i32 { 0 }
}
func main() -> i32 {
    let r = Rec { v: 1 }
    let c: cap FileReadCap = FileReadCap
    r.leak(c)
    0
}
"#,
    )
    .expect_err("leaky linear T method body must be rejected (E0841)");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0841),
        "leaky linear T method must be E0841, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 负集：`linear T` 方法单路径弃值（另一路径 drop）→ E0841。
#[test]
fn linear_kind_method_partial_path_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;
trait Wrap {
    func f<linear T>(b: bool, x: T) -> i32;
}
type Rec { v: i32 }
impl Wrap for Rec {
    func f<linear T>(b: bool, x: T) -> i32 { if b { drop(x); 0 } else { 0 } }
}
func main() -> i32 {
    let r = Rec { v: 1 }
    let c: cap FileReadCap = FileReadCap
    r.f(true, c)
    0
}
"#,
    )
    .expect_err("single-path method drop must be rejected (E0841)");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0841),
        "single-path method drop must be E0841, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────
// 0.39.62 trait 方法调用点线性实参种类检查（E0432 覆盖方法，收口 0.39.61 遗留）
// 此前 trait 方法 dispatch（method.rs type_methods 路径）完全绕过 linear-arg
// 检查：Free-T 方法 + 线性实参静默通过（泄漏方法体也漏）。0.39.62 修复——
// 与 simple.rs / impl 方法同款：
//   - Free-T 方法 + 线性实参 → E0432（种类不匹配）；
//   - `linear T`/`linear drop T` 方法 kind 兼容放行（drop + Session 拒）；
//   - 具体线性参数方法（`x: cap`）不受影响（concrete 追踪）。
// ─────────────────────────────────────────────────────────────

/// 负集：Free-T trait 方法 + cap 实参 → E0432（即使方法体直通）。
#[test]
fn linear_kind_trait_method_free_t_linear_rejected() {
    let errs = check_source(
        r#"
cap FileReadCap;
trait Keep {
    func keep<T>(x: T) -> T;
}
type Rec { v: i32 }
impl Keep for Rec {
    func keep<T>(x: T) -> T { x }
}
func main() -> i32 {
    let r = Rec { v: 1 }
    let c: cap FileReadCap = FileReadCap
    let d = r.keep(c)
    drop(d)
    0
}
"#,
    )
    .expect_err("Free-T trait method + cap must be rejected (E0432)");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "Free-T trait method + cap must be E0432, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

/// 正集：`linear T` trait 方法 + cap → 通过（双后端，见
/// `linear_kind_method_receiver_with_linear_arg_dual_backend`）。

/// 负集：Free-T trait 方法 + SessionChan → E0432。
#[test]
fn linear_kind_trait_method_free_t_session_rejected() {
    let errs = check_source(
        r#"
session S = !i32 . ?i32 . end
trait Keep {
    func keep<T>(x: T) -> T;
}
type Rec { v: i32 }
impl Keep for Rec {
    func keep<T>(x: T) -> T { x }
}
func main() -> i32 {
    let r = Rec { v: 1 }
    let (ch0, ch1) = session_pair::<S>()
    let d = r.keep(ch0)
    let n = session_recv(ch1)
    session_send(ch1, n + 1)
    session_close(ch1)
    drop(d)
    0
}
"#,
    )
    .expect_err("Free-T trait method + SessionChan must be rejected (E0432)");
    assert!(
        has_code(&errs, crate::diagnostic::codes::E0432),
        "Free-T trait method + SessionChan must be E0432, got {:?}",
        errs.iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}
