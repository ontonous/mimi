use super::*;

#[test]
fn actor_await_method() {
    let src = r#"
actor Counter {
    count: i32 = 0;

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
    let val = c.get();
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
    count: i32 = 0;

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
    value: i32 = 0;

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
    let result = calc.get();
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
    name: string = "world";

    func greet() -> string {
        return "Hello, " + self.name;
    }
}

func main() {
    let g = Greeter.spawn();
    let msg = g.greet();
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
    total: i32 = 0;

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
    a.get()
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
    assert_eq!(
        run_source(src),
        interp::Value::String(Arc::new("hello".into()))
    );
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
    count: i32 = 0;

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
    p.process(5)
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
    m.format()
}
"#;
    assert_eq!(
        run_source(src),
        interp::Value::String(Arc::new("user:alice".into()))
    );
}

#[test]
fn actor_runs_flow_parse_and_check() {
    // v0.31.11: `actor Name runs FlowName` parses and checks when the flow exists.
    let src = r#"
flow Order {
    state Pending { item: string }
    state Shipped { item: string }
    transition ship(Pending) -> Shipped {
        { return Shipped { item: self.item } }
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
        { return Positive { n: self.n + 1 } }
    }
    transition bump(Positive) -> Positive {
        { return Positive { n: self.n + 1 } }
    }
    transition get(Positive) -> Positive {
        { return Positive { n: self.n } }
    }
}

actor CounterActor runs Counter {
}

func main() -> i32 {
    let a = CounterActor.spawn();
    let s1 = a.inc();
    let s2 = a.bump();
    let s3 = a.get();
    s3.n
}
"#;
    // 0.35.14 (DX backlog #13): full typed check now passes — checker/infer
    // register flow transitions as synthetic actor methods (layer ①) and the
    // resolved directory carries `function:{Actor}::{transition}` callable
    // identity (layer ③). Bytecode dispatch runs the program at runtime.
    let checked = check_source(src);
    assert!(
        checked.is_ok(),
        "runs_flow dispatch should check: {:?}",
        checked
    );
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn actor_runs_flow_transition_arg_type_checked() {
    // 0.35.14 (DX backlog #13): transition event params are typechecked at
    // the actor method call site (E0211), arity mismatches emit E0257, and
    // unknown methods keep E0221.
    let src = r#"
flow Counter {
    state Zero { n: i32 }
    state Positive { n: i32 }
    transition add(Zero, x: i32) -> Positive {
        { return Positive { n: self.n + x } }
    }
}

actor W runs Counter {
}

func main() -> i32 {
    let a = W.spawn();
    let s = a.add("oops");
    0
}
"#;
    let result = check_source(src);
    let errors = result.expect_err("wrong-typed transition arg must be rejected");
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_deref() == Some(crate::diagnostic::codes::E0211)),
        "expected E0211, got: {:?}",
        errors
    );
}

#[test]
fn actor_runs_flow_fails_transition_returns_result() {
    // 0.35.14 (DX backlog #13): a `fails E` transition surfaces as
    // Result<ToState, (FromState, E)> at the actor method call site — the
    // same shape the VM dispatch returns.
    let src = r#"
flow F {
    state A { n: i32 }
    state B { n: i32 }
    transition go(A) -> B fails string {
        { return B { n: self.n + 1 } }
    }
}

actor W runs F {
}

func main() -> i32 {
    let a = W.spawn();
    let r: i32 = a.go();
    r
}
"#;
    let result = check_source(src);
    let errors = result.expect_err("fails transition returns Result, not i32");
    assert!(
        errors
            .iter()
            .any(|d| d.message.contains("Result<B, (A, string)>")),
        "expected Result<B, (A, string)> in diagnostic, got: {:?}",
        errors
    );
}

#[test]
fn actor_runs_flow_rejects_mut_field() {
    // 0.1.8 Phase D (SD-5 废止): any user-visible business `mut` actor field is
    // rejected, including `runs Flow` actors. State must be carried by the Flow.
    let src = r#"
flow Counter {
    state Zero { n: i32 }
    transition inc(Zero) -> Zero {
        { return Zero { n: self.n + 1 } }
    }
}

actor CounterActor runs Counter {
    mut count: i32 = 0;
}

func main() -> i32 {
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "actor runs flow with mut field should be rejected"
    );
}

#[test]
fn actor_business_mut_rejected() {
    // 0.1.8 Phase D (SD-5 废止) negative lock: SD-5 逃生舱已删除。plain actor 的
    // 业务 `mut` 字段与 `runs Flow` actor 一样非法，诊断必须建议把状态迁进 Flow。
    let src = r#"
actor Bank {
    mut balance: i32 = 0
}
func main() -> i32 { 0 }
"#;
    let errors = check_source(src).expect_err("business mut actor field must be rejected");
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_deref() == Some(crate::diagnostic::codes::E0402)),
        "expected E0402, got: {:?}",
        errors
    );
    let text = format!("{:?}", errors);
    assert!(
        text.contains("business state must live in a Flow"),
        "expected rewrite guidance, got: {text}"
    );
}

#[test]
fn actor_runs_flow_ok() {
    // 0.1.8 Phase D positive lock: `actor Name runs FlowName` remains the
    // supported business-actor shape.
    let src = r#"
flow Account {
    state Active { balance: i32 }
    transition init(Active) -> Active {
        return Active { balance: self.balance }
    }
}

actor Teller runs Account {
}

func main() -> i32 { 0 }
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "actor runs Flow should still check, got: {:?}",
        result
    );
}
