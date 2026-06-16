use super::*;

#[test]
fn actor_sync_method_call() {
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
    assert_eq!(run_source(src), interp::Value::Int(0));
}

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
    await c.get()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(1));
}

#[test]
fn actor_spawn_creates_handle() {
    let src = r#"
actor Worker {
    val: i32 = 42;

    func work() -> i32 {
        return self.val;
    }
}

func main() -> i32 {
    let w = Worker.spawn();
    w.work()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn actor_multiple_methods() {
    let src = r#"
actor Calc {
    mut x: i32 = 3;
    mut y: i32 = 7;

    func add() -> i32 {
        return self.x + self.y;
    }

    func mul() -> i32 {
        return self.x * self.y;
    }
}

func main() -> i32 {
    let c = Calc.spawn();
    let a = c.add();
    let m = c.mul();
    a + m
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(31));
}

#[test]
fn actor_await_multiple_methods() {
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
    c.increment();
    await c.get()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(2));
}

#[test]
fn actor_state_persistence() {
    let src = r#"
actor State {
    mut counter: i32 = 100;

    func get() -> i32 {
        return self.counter;
    }
}

func main() -> i32 {
    let s = State.spawn();
    await s.get()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(100));
}

#[test]
fn actor_in_function() {
    let src = r#"
actor Box {
    val: i32 = 55;

    func unwrap() -> i32 {
        return self.val;
    }
}

func make_and_use() -> i32 {
    let b = Box.spawn();
    b.unwrap()
}

func main() -> i32 {
    make_and_use()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(55));
}

#[test]
fn actor_spawn_method_return() {
    let src = r#"
actor Adder {
    base: i32 = 10;

    func add(x: i32) -> i32 {
        return self.base + x;
    }
}

func main() -> i32 {
    let a = Adder.spawn();
    a.add(5)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(15));
}
