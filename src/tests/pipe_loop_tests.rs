use super::*;

// ─── Pipe operator |> (v0.22.4) ──────────────────────────────

#[test]
fn pipe_basic() {
    let v = run_source(r#"
        func double(x: i32) -> i32 { x * 2 }
        func main() -> i32 { 5 |> double() }
    "#);
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn pipe_chain() {
    let v = run_source(r#"
        func add1(x: i32) -> i32 { x + 1 }
        func double(x: i32) -> i32 { x * 2 }
        func main() -> i32 { 5 |> add1() |> double() }
    "#);
    assert_eq!(v, interp::Value::Int(12));
}

#[test]
fn pipe_ident() {
    let v = run_source(r#"
        func id<T>(x: T) -> T { x }
        func main() -> i32 { 42 |> id }
    "#);
    assert_eq!(v, interp::Value::Int(42));
}

// ─── Loop keyword (v0.22.4) ──────────────────────────────────

#[test]
fn loop_basic() {
    let v = run_source(r#"
        func main() -> i32 {
            let mut count = 0
            loop {
                count = count + 1
                if count >= 5 { break }
            }
            count
        }
    "#);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn loop_break_with_value() {
    let v = run_source(r#"
        func main() -> i32 {
            let mut i = 0
            loop {
                i = i + 1
                if i >= 3 { break }
            }
            i
        }
    "#);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn loop_continue() {
    let v = run_source(r#"
        func main() -> i32 {
            let mut count = 0
            let mut i = 0
            loop {
                i = i + 1
                if i > 5 { break }
                if i % 2 == 0 { continue }
                count = count + 1
            }
            count
        }
    "#);
    assert_eq!(v, interp::Value::Int(3)); // 1, 3, 5
}
