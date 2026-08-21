// CHK-F02 (audit 2026-08-20, reworked 0.1.8): `Any` is now a ONE-DIRECTIONAL
// (bottom) type. An `Any`-typed value (e.g. the second element of `map_get`'s
// `(bool, Any)`) may flow *down* into a concrete type when it is the value
// being used, but it may NO LONGER be sunk into a concrete-typed parameter or
// binding — that call-site type confusion is now closed.
//
// These are the user-visible PoCs: the old bidirectional `Any` let an
// `Any`-typed value masquerade as any concrete type, so `takes_i32(v)` (with
// `v: Any`) type-checked silently. After the fix it is rejected.
use super::*;

#[test]
fn chk_f02_any_value_rejected_as_concrete_param() {
    // An `Any`-typed value from `map_get` must NOT be accepted where a concrete
    // `i32` parameter is expected.
    let src = r#"
    func takes_i32(x: i32) -> i32 { x }
    func main() -> i32 {
        let m = map_new();
        let m = map_set(m, "k", 5);
        let (found, v) = map_get(m, "k");
        takes_i32(v)
    }
    "#;
    let file = parse(src);
    let res = core::check_program(&file);
    assert!(
        res.is_err(),
        "passing an Any-typed value to an i32 parameter must be rejected (CHK-F02)"
    );
}

#[test]
fn chk_f02_any_value_rejected_as_concrete_let_binding() {
    // Sinking an `Any`-typed value into an explicitly-typed `i32` binding is
    // also rejected.
    let src = r#"
    func main() -> i32 {
        let m = map_new();
        let m = map_set(m, "k", 5);
        let (found, v) = map_get(m, "k");
        let x: i32 = v;
        x
    }
    "#;
    let file = parse(src);
    let res = core::check_program(&file);
    assert!(
        res.is_err(),
        "sinking an Any-typed value into an i32 let-binding must be rejected (CHK-F02)"
    );
}

#[test]
fn chk_f02_any_flows_down_as_concrete_value() {
    // Companion: `Any` still flows *down* into a concrete type when it is the
    // value being used (not sunk into a concrete parameter), preserving the
    // `map_get` idiom — the `Any` result is usable as `i32` directly.
    let src = r#"
    func main() -> i32 {
        let m = map_new();
        let m = map_set(m, "k", 5);
        let (found, v) = map_get(m, "k");
        if found { v } else { 0 }
    }
    "#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}
