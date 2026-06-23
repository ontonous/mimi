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

#[test]
fn interp_ffi_no_panic_segfault_caught() {
    // Test #[no_panic] attribute: signal handler catches C crash without fork
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:no_panic_segv unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    // Use no-fork mode to exercise call_ffi_no_panic (signal handler path)
    let result = run_source_result_no_fork(r#"
        #[no_panic]
        extern "C" {
            func test_segfault()
        }
        func main() -> i32 {
            test_segfault()
            42
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert!(result.is_err(), "segfault should be caught by #[no_panic] signal handler");
    let err = result.unwrap_err();
    assert!(err.contains("SIGSEGV") || err.contains("signal 11"),
        "error should mention SIGSEGV: {}", err);
}

#[test]
fn interp_ffi_no_panic_abort_caught() {
    // Test #[no_panic] with abort (SIGABRT)
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:no_panic_abort unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result_no_fork(r#"
        #[no_panic]
        extern "C" {
            func test_abort()
        }
        func main() -> i32 {
            test_abort()
            42
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert!(result.is_err(), "abort should be caught by #[no_panic] signal handler");
    let err = result.unwrap_err();
    assert!(err.contains("SIGABRT") || err.contains("signal 6"),
        "error should mention SIGABRT: {}", err);
}

#[test]
fn interp_ffi_no_panic_normal_call_succeeds() {
    // Test #[no_panic] does not interfere with normal calls
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:no_panic_normal unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result_no_fork(r#"
        #[no_panic]
        extern "C" {
            func test_nop()
        }
        func main() -> i32 {
            test_nop()
            42
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("src/tests/ffi_interp_e2e.rs:no_panic_normal unwrap failed"),
        interp::Value::Int(42));
}

#[test]
fn interp_ffi_no_panic_abort_fork_mode() {
    // Test #[no_panic] with fork protection also works (verify_ffi=true)
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:no_panic_fork unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        #[no_panic]
        extern "C" {
            func test_abort()
        }
        func main() -> i32 {
            test_abort()
            42
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert!(result.is_err(), "abort should be caught (fork mode)");
}

#[test]
fn interp_ffi_struct_by_value_i32() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:struct_by_val unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        #[repr(C)]
        type TestPoint { x: i32, y: i32 }
        extern "C" {
            func test_struct_by_val(p: TestPoint) -> i32
        }
        func main() -> i32 {
            test_struct_by_val(TestPoint { x: 10, y: 20 })
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("ffi_interp_e2e.rs:struct_by_val unwrap failed"), interp::Value::Int(30));
}

#[test]
fn interp_ffi_struct_by_value_mixed() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:mixed unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        #[repr(C)]
        type MixedStruct { id: i32, value: f64, flag: i32 }
        extern "C" {
            func test_mixed_struct(s: MixedStruct) -> f64
        }
        func main() -> f64 {
            test_mixed_struct(MixedStruct { id: 10, value: 3.5, flag: 1 })
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    let val = result.expect("ffi_interp_e2e.rs:mixed unwrap failed");
    match val {
        interp::Value::Float(f) => assert!((f - 14.5).abs() < 0.001, "expected ~14.5, got {}", f),
        _ => panic!("expected Float, got {:?}", val),
    }
}

#[test]
fn interp_ffi_struct_by_value_nested() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:nested unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        #[repr(C)]
        type Inner { a: i32, b: i32 }
        #[repr(C)]
        type Outer { inner: Inner, c: i32 }
        extern "C" {
            func test_nested_struct(o: Outer) -> i32
        }
        func main() -> i32 {
            test_nested_struct(Outer { inner: Inner { a: 1, b: 2 }, c: 3 })
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("ffi_interp_e2e.rs:nested unwrap failed"), interp::Value::Int(6));
}

#[test]
fn interp_ffi_struct_by_value_i64() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:i64 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result(r#"
        #[repr(C)]
        type Timespec { sec: i64, nsec: i64 }
        extern "C" {
            func test_timespec_sum(t: Timespec) -> i64
        }
        func main() -> i64 {
            test_timespec_sum(Timespec { sec: 100, nsec: 200 })
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("ffi_interp_e2e.rs:i64 unwrap failed"), interp::Value::Int(300));
}

#[test]
fn interp_ffi_struct_return_by_value() {
    if !can_cc() { eprintln!("SKIP: cc not available"); return; }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:struct_ret unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_result_no_fork(r#"
        #[repr(C)]
        type TestPoint { x: i32, y: i32 }
        extern "C" {
            func test_make_point(x: i32, y: i32) -> TestPoint
        }
        func main() -> i32 {
            let p = test_make_point(10, 20)
            p.x + p.y
        }
    "#);
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(result.expect("ffi_interp_e2e.rs:struct_ret unwrap failed"), interp::Value::Int(30));
}
