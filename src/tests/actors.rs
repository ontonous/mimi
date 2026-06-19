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
    assert_eq!(run_source(src), interp::Value::String("hello".to_string()));
}

#[test]
fn actor_multiple_fields() {
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
