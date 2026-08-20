use super::*;

fn can_cc() -> bool {
    std::process::Command::new("cc")
        .arg("--version")
        .output()
        .is_ok()
}

#[test]
fn interp_ffi_float_identity() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:11 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_float_identity(x: f64) -> f64
        }
        func main() -> f64 {
            test_float_identity(2.5)
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    let val = result.expect("src/tests/ffi_interp_e2e.rs:22 unwrap failed");
    match val {
        interp::Value::Float(f) => assert!((f - 2.5).abs() < 0.001, "expected ~2.5, got {}", f),
        _ => panic!("expected Float, got {:?}", val),
    }
}

#[test]
fn interp_ffi_strlen_raw() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:33 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_strlen(s: string) -> i32
        }
        func main() -> i32 {
            test_strlen("Hello World")
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("src/tests/ffi_interp_e2e.rs:44 unwrap failed"),
        interp::Value::Int(11)
    );
}

#[test]
fn interp_ffi_greet_raw() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:51 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    // Must disable fork isolation: raw_string return is a pointer from child's heap,
    // which is inaccessible after fork+_exit. The parent cannot read or free child's pointer.
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_greet(x: i32) -> string
        }
        func main() -> i32 {
            if test_greet(42) == "Hello 42" { 42 } else { 0 }
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("src/tests/ffi_interp_e2e.rs:64 unwrap failed"),
        interp::Value::Int(42)
    );
}

#[test]
fn interp_ffi_nop() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:71 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_nop()
        }
        func main() -> i32 {
            test_nop()
            42
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("src/tests/ffi_interp_e2e.rs:83 unwrap failed"),
        interp::Value::Int(42)
    );
}

#[test]
fn interp_ffi_json_sum_list() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:90 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_json_sum(json: List<i32>) -> i32
        }
        func main() -> i32 {
            test_json_sum([10, 20, 30])
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("src/tests/ffi_interp_e2e.rs:101 unwrap failed"),
        interp::Value::Int(60)
    );
}

#[test]
fn interp_ffi_callback() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:108 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_callback(x: i32, cb: func(i32) -> i32) -> i32
        }
        func main() -> i32 {
            let factor = 2
            let cb = fn(n: i32) -> i32 { n * factor }
            test_callback(5, cb)
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("src/tests/ffi_interp_e2e.rs:121 unwrap failed"),
        interp::Value::Int(10)
    );
}

/// interp F2 (audit 2026-08-20, HIGH): a C callback passes a **static** string
/// literal (`.rodata`, not malloc'd). The old trampoline unconditionally
/// `libc::free`d callback `string` args, which is heap corruption on a static
/// pointer. Since callback `string` args are now borrowed (never freed by Mimi
/// — the decode already copies into `Arc<String>`), the callback must receive
/// the correct value and the program must not crash.
#[test]
fn interp_ffi_callback_static_string_not_freed() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:108 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_callback_str(cb: func(string) -> i32) -> i32
        }
        func main() -> i32 {
            let cb = fn(s: string) -> i32 {
                if s == "borrowed_static" { 100 } else { 0 }
            }
            test_callback_str(cb)
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("interp F2: callback with static string crashed (pre-fix free of .rodata pointer)"),
        interp::Value::Int(100),
    );
}

/// Cross-thread callback test: spawn a worker thread that invokes the
/// callback. Exercises the v0.28.18 cross-thread callback infrastructure
/// (SendFilePtr + CALLBACK_FILE store + evaluate_cross_thread_callback).
#[test]
fn interp_ffi_threaded_callback() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path =
        build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:threaded unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_threaded_callback(x: i32, cb: func(i32) -> i32) -> i32
        }
        func main() -> i32 {
            let factor = 3
            let cb = fn(n: i32) -> i32 { n * factor + 7 }
            test_threaded_callback(5, cb)
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    // Worker thread invokes cb(5) = 5 * 3 + 7 = 22
    assert_eq!(
        result.expect("src/tests/ffi_interp_e2e.rs:threaded unwrap failed"),
        interp::Value::Int(22)
    );
}

/// Delayed-callback regression lock (0.35.27 C3: FFI callback UAF).
///
/// A C library stores the callback pointer during a synchronous extern call
/// and invokes it AFTER the call returns — after the BytecodeVM that
/// registered the closure has been dropped. The pre-C3 design held a global
/// raw `*const BytecodeProgram` that dangled once the owning VM dropped:
/// invoking the stored callback then was use-after-free.
///
/// Since 0.35.27, `BytecodeClosure` carries its own program `Arc`, so the
/// delayed invocation (which takes the cross-thread evaluation path — no TLS
/// runner on the firing thread) evaluates against the closure's own program,
/// which stays alive as long as the closure does.
#[test]
fn interp_ffi_delayed_callback_after_vm_drop() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:delayed unwrap failed");

    // The real-world shape of this bug: a C library stays loaded in the
    // process while the Mimi VM that registered the callback comes and goes.
    // Hold the library open for the whole test — otherwise the VM's own
    // dlopen/dlclose cycle unloads the .so and the C-side slot re-initializes
    // (that would be a different, unrelated lifecycle issue).
    // SAFETY: so_path exists; the handle stays alive until end of test.
    let lib = unsafe { libloading::Library::new(&so_path) }
        .expect("src/tests/ffi_interp_e2e.rs:delayed dlopen failed");

    std::env::set_var("MIMI_FFI_LIB", &so_path);

    // Store the callback in the C-side slot and return. The BytecodeVM that
    // registers the closure is dropped as soon as run_source_bytecode_result
    // returns — the UAF trigger point for the old raw-pointer design.
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_delayed_callback_store(cb: func(i32) -> i32) -> i32
        }
        func main() -> i32 {
            let factor = 4
            let cb = fn(n: i32) -> i32 { n * factor + 11 }
            test_delayed_callback_store(cb)
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("src/tests/ffi_interp_e2e.rs:delayed store unwrap failed"),
        interp::Value::Int(1)
    );

    // NOW the registering VM is gone. Fire the stored callback from the test
    // process (TLS has no runner → cross-thread path). The closure's own
    // program Arc keeps the program alive: cb(3) = 3*4 + 11 = 23.
    // SAFETY: the callback pointer was stored by test_delayed_callback_store
    // while the extern call was on the stack; the 0.35.27 Arc ownership makes
    // it safe to invoke after the VM dropped (pre-C3: raw program pointer
    // dangled → UAF/undefined result).
    unsafe {
        let fire: libloading::Symbol<unsafe extern "C" fn(i32) -> i32> = lib
            .get(b"test_delayed_callback_fire")
            .expect("src/tests/ffi_interp_e2e.rs:delayed dlsym failed");
        assert_eq!(fire(3), 23);
    }
}

#[test]
fn interp_ffi_parse_int_raw_string() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:128 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_parse_int(s: string) -> i32
        }
        func main() -> i32 {
            test_parse_int("42")
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("src/tests/ffi_interp_e2e.rs:139 unwrap failed"),
        interp::Value::Int(42)
    );
}

#[test]
fn interp_ffi_segfault_caught() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:146 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    // Fork isolation is enabled by default; segfault in child should not crash the test
    // We test that the interpreter returns an error (the child was killed by signal)
    let result = run_source_bytecode_result(
        r#"
        extern "C" {
            func test_segfault()
        }
        func main() -> i32 {
            test_segfault()
            42
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert!(
        result.is_err(),
        "segfault should be caught by fork isolation"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("signal")
            || err.contains("SIGSEGV")
            || err.contains("SEGV")
            || err.contains("killed"),
        "error should mention signal/SEGV: {}",
        err
    );
}

#[test]
fn interp_ffi_no_panic_segfault_caught() {
    // Test #[no_panic] attribute: signal handler catches C crash without fork
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path =
        build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:no_panic_segv unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    // Use no-fork mode to exercise call_ffi_no_panic (signal handler path)
    let result = run_source_bytecode_result(
        r#"
        #[no_panic]
        extern "C" {
            func test_segfault()
        }
        func main() -> i32 {
            test_segfault()
            42
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert!(
        result.is_err(),
        "segfault should be caught by #[no_panic] signal handler"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("SIGSEGV") || err.contains("signal 11"),
        "error should mention SIGSEGV: {}",
        err
    );
}

#[test]
fn interp_ffi_no_panic_abort_caught() {
    // Test #[no_panic] with abort (SIGABRT)
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path =
        build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:no_panic_abort unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        #[no_panic]
        extern "C" {
            func test_abort()
        }
        func main() -> i32 {
            test_abort()
            42
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert!(
        result.is_err(),
        "abort should be caught by #[no_panic] signal handler"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("SIGABRT") || err.contains("signal 6"),
        "error should mention SIGABRT: {}",
        err
    );
}

#[test]
fn interp_ffi_no_panic_normal_call_succeeds() {
    // Test #[no_panic] does not interfere with normal calls
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path =
        build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:no_panic_normal unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        #[no_panic]
        extern "C" {
            func test_nop()
        }
        func main() -> i32 {
            test_nop()
            42
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("src/tests/ffi_interp_e2e.rs:no_panic_normal unwrap failed"),
        interp::Value::Int(42)
    );
}

#[test]
fn interp_ffi_no_panic_abort_fork_mode() {
    // Test #[no_panic] with fork protection also works (verify_ffi=true)
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path =
        build_interp_ffi_so().expect("src/tests/ffi_interp_e2e.rs:no_panic_fork unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        #[no_panic]
        extern "C" {
            func test_abort()
        }
        func main() -> i32 {
            test_abort()
            42
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert!(result.is_err(), "abort should be caught (fork mode)");
}

#[test]
fn interp_ffi_struct_by_value_i32() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:struct_by_val unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        #[repr(C)]
        type TestPoint { x: i32, y: i32 }
        extern "C" {
            func test_struct_by_val(p: TestPoint) -> i32
        }
        func main() -> i32 {
            test_struct_by_val(TestPoint { x: 10, y: 20 })
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("ffi_interp_e2e.rs:struct_by_val unwrap failed"),
        interp::Value::Int(30)
    );
}

#[test]
fn interp_ffi_struct_by_value_mixed() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:mixed unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        #[repr(C)]
        type MixedStruct { id: i32, value: f64, flag: i32 }
        extern "C" {
            func test_mixed_struct(s: MixedStruct) -> f64
        }
        func main() -> f64 {
            test_mixed_struct(MixedStruct { id: 10, value: 3.5, flag: 1 })
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    let val = result.expect("ffi_interp_e2e.rs:mixed unwrap failed");
    match val {
        interp::Value::Float(f) => assert!((f - 14.5).abs() < 0.001, "expected ~14.5, got {}", f),
        _ => panic!("expected Float, got {:?}", val),
    }
}

#[test]
fn interp_ffi_struct_by_value_nested() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:nested unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
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
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("ffi_interp_e2e.rs:nested unwrap failed"),
        interp::Value::Int(6)
    );
}

#[test]
fn interp_ffi_struct_by_value_i64() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:i64 unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        #[repr(C)]
        type Timespec { sec: i64, nsec: i64 }
        extern "C" {
            func test_timespec_sum(t: Timespec) -> i64
        }
        func main() -> i64 {
            test_timespec_sum(Timespec { sec: 100, nsec: 200 })
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("ffi_interp_e2e.rs:i64 unwrap failed"),
        interp::Value::Int(300)
    );
}

#[test]
fn interp_ffi_struct_return_by_value() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let so_path = build_interp_ffi_so().expect("ffi_interp_e2e.rs:struct_ret unwrap failed");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        #[repr(C)]
        type TestPoint { x: i32, y: i32 }
        extern "C" {
            func test_make_point(x: i32, y: i32) -> TestPoint
        }
        func main() -> i32 {
            let p = test_make_point(10, 20)
            p.x + p.y
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        result.expect("ffi_interp_e2e.rs:struct_ret unwrap failed"),
        interp::Value::Int(30)
    );
}

#[test]
fn interp_ffi_no_panic_struct_ret_segfault_caught() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("mimi_ffi_struct_crash_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let c_path = tmp_dir.join("struct_crash.c");
    let so_path = tmp_dir.join("struct_crash.so");
    std::fs::write(
        &c_path,
        r#"
        #include <stddef.h>
        typedef struct { int x; int y; } Point;
        Point test_struct_crash(void) {
            volatile int* p = NULL;
            *p = 42;
            Point pt = { 1, 2 };
            return pt;
        }
    "#,
    )
    .unwrap();
    let status = std::process::Command::new("cc")
        .arg("-shared")
        .arg("-fPIC")
        .arg("-o")
        .arg(&so_path)
        .arg(&c_path)
        .status()
        .unwrap();
    assert!(status.success(), "failed to compile struct crash .so");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        #[repr(C)]
        type Point { x: i32, y: i32 }
        #[no_panic]
        extern "C" {
            func test_struct_crash() -> Point
        }
        func main() -> i32 {
            let p = test_struct_crash()
            42
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    assert!(
        result.is_err(),
        "struct crash should be caught by #[no_panic]"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("SIGSEGV") || err.contains("signal 11"),
        "error should mention SIGSEGV: {}",
        err
    );
}

#[test]
fn interp_ffi_fork_isolation_struct_ret_segfault_caught() {
    if !can_cc() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let _guard = FfiEnvLock::lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("mimi_ffi_struct_crash_fork_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let c_path = tmp_dir.join("struct_crash_fork.c");
    let so_path = tmp_dir.join("struct_crash_fork.so");
    std::fs::write(
        &c_path,
        r#"
        #include <stddef.h>
        typedef struct { int x; int y; } Point;
        Point test_struct_crash(void) {
            volatile int* p = NULL;
            *p = 42;
            Point pt = { 1, 2 };
            return pt;
        }
    "#,
    )
    .unwrap();
    let status = std::process::Command::new("cc")
        .arg("-shared")
        .arg("-fPIC")
        .arg("-o")
        .arg(&so_path)
        .arg(&c_path)
        .status()
        .unwrap();
    assert!(status.success(), "failed to compile struct crash .so");
    std::env::set_var("MIMI_FFI_LIB", &so_path);
    let result = run_source_bytecode_result(
        r#"
        #[repr(C)]
        type Point { x: i32, y: i32 }
        extern "C" {
            func test_struct_crash() -> Point
        }
        func main() -> i32 {
            let p = test_struct_crash()
            42
        }
    "#,
    );
    std::env::remove_var("MIMI_FFI_LIB");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    assert!(
        result.is_err(),
        "struct crash should be caught by fork isolation"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("signal") || err.contains("SIGSEGV") || err.contains("SEGV"),
        "error should mention signal: {}",
        err
    );
}
