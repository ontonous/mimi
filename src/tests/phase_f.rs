//! 0.1.9 Phase F — AI 评测参考解守卫（0.39.111）。
//!
//! 任务集规范正例（`devdocs/mimi-eval/tasks/`，gitignored）在此嵌入并断言
//! 双后端等价。若编译器回归使任一参考解 check/跑挂，评测基线即失效——本
//! 测试必须红。任务类别：Flow / 线性种类 / Session / actor runs Flow /
//! 失败分层 / 对照 CRUD。

use super::*;

const T01_FLOW: &str = r#"
flow Order {
    state Pending { item: string }
    state Shipped { item: string }
    transition ship(Pending) -> Shipped {
        { return Shipped { item: self.item } }
    }
}
func main() -> i32 {
    println("t01_flow ok")
    0
}
"#;

const T02_LINEAR: &str = r#"
cap FileReadCap;
func pass<linear T>(x: T) -> T { x }
func main() -> i32 {
    let c: cap FileReadCap = FileReadCap
    let d = pass(c)
    drop(d)
    let t = make_token()
    let id = token_id(t)
    drop(id)
    println("t02_linear ok")
    0
}
"#;

const T03_SESSION: &str = r#"
session Half = !i32 . end
func main() -> i32 {
    let ch: SessionChan<Half> = session_open::<Half>()
    session_send(ch, 9)
    session_close(ch)
    println("t03_session ok")
    0
}
"#;

const T04_ACTOR_FLOW: &str = r#"
flow Order {
    state Pending { item: string }
    state Shipped { item: string }
    transition ship(Pending) -> Shipped {
        { return Shipped { item: self.item } }
    }
}
actor OrderWorker runs Order {
    func process() -> i32 {
        return 1
    }
}
func main() -> i32 {
    println("t04_actor_flow ok")
    0
}
"#;

const T05_FAILURE: &str = r#"
flow F {
    state A { n: i32 }
    state B { n: i32 }
    transition go(A) -> B fails string {
        { return B { n: self.n + 1 } }
    }
}
func safe_div(a: i32, b: i32) -> Result<i32, string> {
    if b == 0 { Err("div by zero") } else { Ok(a / b) }
}
func main() -> i32 {
    let r = safe_div(10, 2)
    match r {
        Ok(v) => println(v)
        Err(_) => println("no")
    }
    0
}
"#;

const T06_CRUD: &str = r#"
type Item { id: i32, name: string }
func main() -> i32 {
    let items: List<Item> = [
        Item { id: 1, name: "a" },
        Item { id: 2, name: "b" },
    ]
    let n = len(items)
    let first = items[0]
    println(n)
    println(first.name)
    let more: List<i32> = [1, 2, 3]
    let has = contains(more, 2)
    println(has)
    0
}
"#;

/// 参考解全过 check + 双后端等价（与 eval_harness 基线口径一致）。
#[test]
fn phase_f_reference_solutions_check_and_dual() {
    let cases: [(&str, &str, &str); 6] = [
        ("t01_flow", T01_FLOW, "t01_flow ok"),
        ("t02_linear", T02_LINEAR, "t02_linear ok"),
        ("t03_session", T03_SESSION, "t03_session ok"),
        ("t04_actor_flow", T04_ACTOR_FLOW, "t04_actor_flow ok"),
        ("t05_failure", T05_FAILURE, "5"),
        ("t06_crud", T06_CRUD, "2\na\ntrue"),
    ];
    for (name, src, expected) in cases {
        check_source(src).unwrap_or_else(|e| panic!("{name} reference solution must check: {e:?}"));
        if !can_link() {
            continue;
        }
        let (_v, vm) = checked_run_source_with_stdout(src);
        assert_eq!(vm.trim(), expected.trim(), "{name} VM output");
        let native =
            checked_codegen_compile_and_run(src).unwrap_or_else(|e| panic!("{name} native: {e}"));
        assert_eq!(native.trim(), expected.trim(), "{name} native output");
        assert_eq!(vm.trim(), native.trim(), "{name} dual-backend agreement");
    }
}

/// 逃生舱构造（mms{}）必须在参考解中不存在——harness 的 escape_abuse=0 口径。
#[test]
fn phase_f_reference_solutions_avoid_escape_hatches() {
    for (name, src) in [
        ("t01_flow", T01_FLOW),
        ("t02_linear", T02_LINEAR),
        ("t03_session", T03_SESSION),
        ("t04_actor_flow", T04_ACTOR_FLOW),
        ("t05_failure", T05_FAILURE),
        ("t06_crud", T06_CRUD),
    ] {
        assert!(
            !src.contains("mms{") && !src.contains("thread_local") && !src.contains("thread-local"),
            "{name} must not use an out-of-kernel escape-hatch construct"
        );
    }
}
