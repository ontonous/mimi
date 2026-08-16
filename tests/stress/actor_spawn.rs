// Combined actor mailbox + spawn/await stress.
//
// This intentionally mixes the resolved actor-method path (mimi_actor_call)
// with the resolved spawn/await path. Even while spawn is currently eager,
// the round trip through future allocation/completion/await must stay stable.
use super::run_program;

fn actor_spawn_source(n: usize) -> String {
    format!(
        r#"
actor Worker {{
    value: i32
    func set(v: i32) {{ self.value = v }}
    func get() -> i32 {{ self.value }}
}}
func task(w: Worker, v: i32) -> i32 {{
    w.set(v)
    w.get()
}}
func main() -> i32 {{
    let w = Worker.spawn()
    let mut sum = 0
    for i in range(0, {n}) {{
        let t = spawn task(w, i)
        let r = await t
        sum = sum + r
    }}
    println(sum)
    0
}}
"#,
        n = n
    )
}

#[test]
fn stress_actor_spawn_mixed_smoke() {
    let out = run_program(&actor_spawn_source(200)).expect("actor+spawn mixed smoke failed");
    assert_eq!(out.trim(), "19900");
}

#[test]
#[ignore = "heavy: 2000 combined actor calls and spawn/await pairs; run explicitly with --ignored"]
fn stress_actor_spawn_mixed_heavy() {
    let out = run_program(&actor_spawn_source(2000)).expect("actor+spawn mixed heavy failed");
    assert_eq!(out.trim(), "1999000");
}
