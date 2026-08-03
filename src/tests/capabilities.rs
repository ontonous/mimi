use super::*;

#[test]
fn cap_combined_declaration() {
    let src = r#"
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    42
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn cap_split_returns_tuple() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let parts = c.split();
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_runtime() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    drop(write);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_single_error() {
    let src = r#"
cap FileReadCap;

func main() -> i32 {
    let c = FileReadCap;
    c.split();
    42
}
"#;
    let result = run_source_result(src);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("split() requires a combined capability"),
        "Expected split error, got: {}",
        err
    );
}

#[test]
fn cap_split_drop_one() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    drop(write);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_nested_combination() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    drop(read);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn cap_split_use_individual_parts() {
    let src = r#"
cap FileReadCap;
cap FileWriteCap;
cap FullAccess = FileReadCap + FileWriteCap;

func use_read(r: FileReadCap) -> i32 {
    1
}

func use_write(w: FileWriteCap) -> i32 {
    2
}

func main() -> i32 {
    let c = FullAccess;
    let (read, write) = c.split();
    let a = use_read(read);
    let b = use_write(write);
    a + b
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn cap_move_through_aggregate_projection_is_consumed() {
    let src = r#"
cap FileCap;

func consume(c: cap FileCap) -> i32 {
    drop(c);
    1
}

func main(c: cap FileCap) -> i32 {
    let alias = c;
    consume([alias][0])
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "unexpected errors: {result:?}");
}

#[test]
fn cap_move_through_if_expression_is_consumed_once() {
    let src = r#"
cap FileCap;

func consume(c: cap FileCap) -> i32 {
    drop(c);
    1
}

func main(c: cap FileCap, choose_left: bool) -> i32 {
    let alias = c;
    consume(if choose_left { alias } else { alias })
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "unexpected errors: {result:?}");
}

#[test]
fn cap_nested_split_rejected_at_check_time() {
    // H5 (audit 2026-08-03): split components are atomic. `ab.split()` where
    // ab came out of `c.split()` previously sailed through the checker (which
    // re-expanded AB → A,B), then died at runtime in the bytecode VM (E0800:
    // single-component cap) and at compile time in codegen with a misleading
    // E0700 ("method 'split' not compiled for type 'i64'" — the component had
    // degraded to an opaque handle). Three paths, three behaviors. Now the
    // checker rejects nested split (E0221) so run/build/check all agree.
    let src = r#"
cap A;
cap B;
cap AB = A + B;
cap ABC = AB + C;
cap C;

func main() -> i32 {
    let c = ABC;
    let (ab, c2) = c.split();
    let (a, b) = ab.split();
    drop(a);
    drop(b);
    drop(c2);
    0
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "nested split must be rejected by the checker"
    );
    let diags = result.unwrap_err();
    assert!(
        diags.iter().any(|d| {
            d.message.contains("split component") && d.message.contains("cannot be split again")
        }),
        "expected E0221 split-component diagnostic, got: {:?}",
        diags
    );
}

#[test]
fn cap_component_passed_to_declared_cap_param() {
    // H5 companion: a split component IS a legitimate value of the declared
    // cap type (`take(p: cap A)` accepts `a` from `c.split()`). CapAtom
    // unifies with Cap of the same name — only further split is rejected.
    let src = r#"
cap A;
cap B;
cap AB = A + B;

func take(p: cap A) -> i32 {
    drop(p);
    1
}

func main() -> i32 {
    let c = AB;
    let (a, b) = c.split();
    let r = take(a);
    drop(b);
    r
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}
