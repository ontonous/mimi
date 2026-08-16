// Actor mailbox stress: many sequential inc() calls through the resolved
// actor-method call path (mimi_actor_call).
use super::run_program;

const ACTOR_INC_SOURCE: &str = r#"
actor Counter {
    count: i32
    func inc() { self.count = self.count + 1 }
    func get() -> i32 { self.count }
}
func main() -> i32 {
    let c = Counter.spawn()
    for i in range(0, 200) {
        c.inc()
    }
    println(c.get())
    0
}
"#;

#[test]
fn stress_actor_mailbox_inc_chain_smoke() {
    let out = run_program(ACTOR_INC_SOURCE).expect("actor mailbox chain smoke failed");
    assert_eq!(out.trim(), "200");
}

#[test]
#[ignore = "heavy: 2000 sequential actor mailbox calls; run explicitly with --ignored"]
fn stress_actor_mailbox_inc_chain_heavy() {
    let src = ACTOR_INC_SOURCE.replace("range(0, 200)", "range(0, 2000)");
    let out = run_program(&src).expect("actor mailbox chain heavy failed");
    assert_eq!(out.trim(), "2000");
}
