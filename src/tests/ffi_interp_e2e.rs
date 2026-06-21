use super::*;

fn can_cc() -> bool {
    std::process::Command::new("cc").arg("--version").output().is_ok()
}

#[test]
fn interp_ffi_float_identity() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:11 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        extern "C" {
            func test_float_identity(x: f64) -> f64
        }
        func main() -> f64 {
            test_float_identity(3.14)
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    let val = result.expect("src/tests/ffi_interp_e2e.rs:22 unwrap failed");
    match val {
        interp::Value::Float(f) => assert!((f - 3.14).abs() < 0.001, "expected ~3.14, got {}", f),
        _ => panic!("expected Float, got {:?}", val),
    }
}

#[test]
fn interp_ffi_strlen_raw() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:33 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        extern "C" {
            func test_strlen(s: raw_string) -> i32
        }
        func main() -> i32 {
            test_strlen("Hello World")
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("src/tests/ffi_interp_e2e.rs:44 unwrap failed"), interp::Value::Int(11));
}

#[test]
fn interp_ffi_greet_raw() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:51 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    // Must disable fork isolation: raw_string return is a pointer from child's heap,
    // which is inaccessible after fork+_exit. The parent cannot read or free child's pointer.
    let result = run_source_result_no_fork(r#"
        extern "C" {
            func test_greet(x: i32) -> raw_string
        }
        func main() -> i32 {
            if test_greet(42) == "Hello 42" { 42 } else { 0 }
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("src/tests/ffi_interp_e2e.rs:64 unwrap failed"), interp::Value::Int(42));
}

#[test]
fn interp_ffi_nop() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:71 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        extern "C" {
            func test_nop()
        }
        func main() -> i32 {
            test_nop()
            42
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("src/tests/ffi_interp_e2e.rs:83 unwrap failed"), interp::Value::Int(42));
}

#[test]
fn interp_ffi_json_sum_list() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:90 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        extern "C" {
            func test_json_sum(json: List<i32>) -> i32
        }
        func main() -> i32 {
            test_json_sum([10, 20, 30])
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("src/tests/ffi_interp_e2e.rs:101 unwrap failed"), interp::Value::Int(60));
}

#[test]
fn interp_ffi_callback() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:108 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        extern "C" {
            func test_callback(x: i32, cb: func(i32) -> i32) -> i32
        }
        func main() -> i32 {
            let factor = 2
            let cb = fn(n: i32) -> i32 { n * factor }
            test_callback(5, cb)
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("src/tests/ffi_interp_e2e.rs:121 unwrap failed"), interp::Value::Int(10));
}

#[test]
fn interp_ffi_parse_int_raw_string() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:128 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        extern "C" {
            func test_parse_int(s: raw_string) -> i32
        }
        func main() -> i32 {
            test_parse_int("42")
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("src/tests/ffi_interp_e2e.rs:139 unwrap failed"), interp::Value::Int(42));
}

#[test]
fn interp_ffi_segfault_caught() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:146 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    // Fork isolation is enabled by default; segfault in child should not crash the test
    // We test that the interpreter returns an error (the child was killed by signal)
    let result = run_source_result(r#"
        extern "C" {
            func test_segfault()
        }
        func main() -> i32 {
            test_segfault()
            42
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert!(result.is_err(), "segfault should be caught by fork isolation");
    let err = result.unwrap_err();
    assert!(err.contains("signal") || err.contains("SIGSEGV") || err.contains("SEGV") || err.contains("killed"),
        "error should mention signal/SEGV: {}", err);
}
