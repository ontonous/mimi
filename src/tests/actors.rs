use super::*;

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

#[test]
fn actor_method_with_param() {
    let src = r#"
actor Accumulator {
    mut total: i32 = 0;

    func add(n: i32) {
        self.total = self.total + n;
    }

    func get() -> i32 {
        return self.total;
    }
}

func main() -> i32 {
    let a = Accumulator.spawn();
    a.add(5);
    a.add(10);
    await a.get()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(15));
}

#[test]
fn actor_return_bool() {
    let src = r#"
actor Checker {
    val: bool = true;

    func check() -> bool {
        return self.val;
    }
}

func main() -> bool {
    let c = Checker.spawn();
    c.check()
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn actor_return_string() {
    let src = r#"
actor Messenger {
    msg: string = "hello";

    func get_msg() -> string {
        return self.msg;
    }
}

func main() -> string {
    let m = Messenger.spawn();
    m.get_msg()
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello".into()));
}

#[test]
fn actor_field_access() {
    let src = r#"
actor Point {
    x: i32 = 3;
    y: i32 = 4;
}

func main() -> i32 {
    let p = Point.spawn();
    p.x + p.y
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(7));
}

#[test]
fn actor_nested_in_function_multiple_calls() {
    let src = r#"
actor Holder {
    val: i32 = 99;
}

func use_actor() -> i32 {
    let h = Holder.spawn();
    h.val
}

func main() -> i32 {
    use_actor() + use_actor()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(198));
}

// Regression test for v0.28.24 item 25: actor method calls must not be
// shadowed by prelude functions of the same name (e.g. `increment`).
// The test framework normally does not auto-load prelude, so we explicitly
// merge it here to reproduce the CLI environment where the bug was observed.
#[test]
fn actor_method_not_shadowed_by_prelude() {
    let src = r#"
actor Counter {
    mut count: i32 = 0;

    func increment() {
        self.count = self.count + 1;
    }

    func get_count() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let c = Counter.spawn();
    println(c.get_count());
    c.increment();
    println(c.get_count());
    c.increment();
    println(c.get_count());
    42
}
"#;
    assert_eq!(run_with_stdlib("prelude.mimi", src), interp::Value::Int(42));
}

// Regression test for v0.28.28 item #1: actor methods must be able to call
// user-defined top-level functions. Previously, the actor worker thread
// created an Interpreter with an empty AST, so calls to user functions
// failed with "function not found". The fix makes the worker share the
// original program's func_index / type_defs.
#[test]
fn actor_method_calls_user_function() {
    let src = r#"
func double(x: i32) -> i32 {
    return x * 2;
}

actor Processor {
    val: i32 = 0;

    func process(input: i32) -> i32 {
        return double(input);
    }
}

func main() -> i32 {
    let p = Processor.spawn();
    await p.process(5)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(10));
}

#[test]
fn actor_method_calls_user_function_via_record() {
    let src = r#"
func build_msg(name: string) -> string {
    return "user:" + name;
}

actor Messenger {
    func format() -> string {
        return build_msg("alice");
    }
}

func main() -> string {
    let m = Messenger.spawn();
    await m.format()
}
"#;
    assert_eq!(run_source(src), interp::Value::String("user:alice".into()));
}

#[test]
fn actor_runs_flow_parse_and_check() {
    // v0.31.11: `actor Name runs FlowName` parses and checks when the flow exists.
    let src = r#"
flow Order {
    state Pending { item: string }
    state Shipped { item: string }
    transition ship(Pending) -> Shipped {
        do { return Shipped { item: self.item } }
    }
}

actor OrderWorker runs Order {
    func process() -> i32 {
        return 1;
    }
}

func main() -> i32 {
    0
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "actor runs flow should check: {:?}", result);
}

#[test]
fn actor_runs_flow_missing_flow_rejected() {
    // v0.31.11: `actor Name runs MissingFlow` is rejected when the flow doesn't exist.
    let src = r#"
actor OrderWorker runs MissingFlow {
    func process() -> i32 {
        return 1;
    }
}

func main() -> i32 {
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "actor runs missing flow should be rejected"
    );
}

#[test]
fn actor_runs_flow_dispatch_through_transition() {
    // v0.31.11: actor that `runs` a Flow dispatches messages through
    // the Flow transition table. The actor's flow_state updates on each turn.
    let src = r#"
flow Counter {
    state Zero { n: i32 }
    state Positive { n: i32 }
    transition inc(Zero) -> Positive {
        do { return Positive { n: self.n + 1 } }
    }
    transition bump(Positive) -> Positive {
        do { return Positive { n: self.n + 1 } }
    }
    transition get(Positive) -> Positive {
        do { return Positive { n: self.n } }
    }
}

actor CounterActor runs Counter {
}

func main() -> i32 {
    let a = CounterActor.spawn();
    let s1 = await a.inc();
    let s2 = await a.bump();
    let s3 = await a.get();
    s3.n
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}
