use super::*;

fn assert_scalar_ffi_boundary(source: &str, expected_detail: &str) {
    let error = run_source_bytecode_result(source)
        .expect_err("unsupported FFI must fail before reaching a host binding");
    if error.contains("MIR-FFI-DECLARATION-001") {
        assert!(
            error.contains(expected_detail),
            "expected boundary detail {expected_detail:?}, got: {error}"
        );
    } else {
        // Some FFI-shaped source types are rejected earlier by the checker
        // with the more specific type-boundary diagnostic.
        assert!(
            error.contains("[E0231]"),
            "expected a stable FFI boundary code, got: {error}"
        );
    }
}

#[test]
fn checked_bytecode_helper_runs_scalar_ffi_through_canonical_mir() {
    if !can_link() {
        eprintln!("SKIP: C linker not available");
        return;
    }
    let mut guard = FfiEnvGuard::lock();
    let library = build_interp_ffi_so().expect("build the shared FFI test library");
    guard.set_path(&library);
    let result = run_source_bytecode_result(
        r#"
        extern "C" { func test_float_identity(x: f64) -> f64; }
        func main() -> f64 { test_float_identity(2.5) }
    "#,
    )
    .expect("receipt-bearing scalar FFI execution");
    assert!(
        matches!(result, interp::Value::Float(value) if (value - 2.5).abs() < 0.001),
        "unexpected scalar result: {result:?}"
    );
}

#[test]
fn checked_bytecode_helper_maps_unit_ffi_result_to_void() {
    if !can_link() {
        eprintln!("SKIP: C linker not available");
        return;
    }
    let mut guard = FfiEnvGuard::lock();
    let library = build_interp_ffi_so().expect("build the shared FFI test library");
    guard.set_path(&library);
    assert_eq!(
        run_source_bytecode_result(
            r#"
            extern "C" { func test_nop(); }
            func main() -> i32 { test_nop(); 42 }
        "#,
        )
        .expect("receipt-bearing Unit FFI execution"),
        interp::Value::Int(42)
    );
}

#[test]
fn unsupported_ffi_shapes_fail_closed_before_legacy_bytecode() {
    for (source, detail) in [
        (
            r#"
            extern "C" { func foreign(value: string) -> i32; }
            func main() -> i32 { foreign("text") }
            "#,
            "parameter type is outside canonical scalar FFI",
        ),
        (
            r#"
            extern "C" { func foreign() -> string; }
            func main() -> i32 { if foreign() == "text" { 1 } else { 0 } }
            "#,
            "result type is outside canonical scalar FFI",
        ),
        (
            r#"
            extern "C" { func foreign(values: List<i32>) -> i32; }
            func main() -> i32 { foreign([1, 2]) }
            "#,
            "parameter type is outside canonical scalar FFI",
        ),
        (
            r#"
            type Pair { value: i32 }
            extern "C" { func foreign(value: Pair) -> i32; }
            func main() -> i32 { foreign(Pair { value: 1 }) }
            "#,
            "parameter type is outside canonical scalar FFI",
        ),
        (
            r#"
            extern "C" { func foreign(callback: func(i32) -> i32) -> i32; }
            func main() -> i32 { foreign(fn(value: i32) -> i32 { value }) }
            "#,
            "parameter type is outside canonical scalar FFI",
        ),
        (
            r#"
            extern "Rust" { func foreign(value: i64) -> i64; }
            func main() -> i64 { foreign(42 as i64) }
            "#,
            "ABI 'Rust' is outside the canonical C ABI",
        ),
        (
            r#"
            #[no_panic]
            extern "C" { func foreign(value: i64) -> i64; }
            func main() -> i64 { foreign(42 as i64) }
            "#,
            "no_panic FFI protection",
        ),
        (
            r#"
            extern "C" { func foreign(value: i64 ...) -> i64; }
            func main() -> i64 { foreign(42 as i64) }
            "#,
            "variadic ABI",
        ),
    ] {
        assert_scalar_ffi_boundary(source, detail);
    }
}
