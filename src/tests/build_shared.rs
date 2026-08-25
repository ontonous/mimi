use crate::codegen;
use crate::lexer;
use crate::parser;

/// Parse and type-check a Mimi source string.
fn parse_and_check(src: &str) -> crate::ast::File {
    let tokens = lexer::Lexer::new(src)
        .tokenize()
        .expect("src/tests/build_shared.rs:9 unwrap failed");
    let file = parser::Parser::new(tokens)
        .parse_file()
        .expect("src/tests/build_shared.rs:10 unwrap failed");
    let check = crate::core::check(&file);
    assert!(check.is_ok(), "type check failed: {:?}", check.err());
    file
}

/// Compile a Mimi source string to an object file (internal helper).
fn compile_to_object(src: &str, module_name: &str, obj_path: &std::path::Path) {
    let file = parse_and_check(src);
    let context = inkwell::context::Context::create();
    let mut gen = codegen::CodeGenerator::new(&context, module_name);
    gen.compile_file(&file)
        .expect("src/tests/build_shared.rs:21 unwrap failed");
    // Shared-library outputs keep bare exported symbols: the host dlopen/
    // dlsym contract resolves them by source name (SYMBOL-NAMESPACE-001).
    gen.compile_to_object_shared(obj_path)
        .expect("src/tests/build_shared.rs:22 unwrap failed");
}

/// Link an object file + Rust runtime into a shared library.
fn link_shared(obj_path: &std::path::Path, output_so: &std::path::Path, no_std: bool) {
    let runtime_lib = crate::tests::cached_runtime_lib().expect("cached_runtime_lib");

    let mut cmd = std::process::Command::new("cc");
    cmd.arg("-shared").arg("-fPIC");
    if no_std {
        cmd.arg("-nostdlib");
    } else {
        cmd.arg("-lpthread").arg("-ldl").arg("-lm");
    }
    cmd.arg("-Wl,--whole-archive")
        .arg(&runtime_lib)
        .arg("-Wl,--no-whole-archive");
    let status = cmd
        .arg(obj_path)
        .arg("-o")
        .arg(output_so)
        .status()
        .expect("link");
    assert!(status.success(), "linking should succeed");
}

/// 0.34.35b (M-006): dlopen round-trip ABI probe — compile a Mimi shared
/// library, dlopen it from a C program, call the exported function through
/// dlsym, and assert the C-observed output.
///
/// This is the missing ABI correctness coverage the 0.34.35 FFI audit flagged:
/// static `-L -l` linking exercises the same symbol table but dlopen forces the
/// runtime loader path (and catches struct-by-value parameter/return slot
/// mismatches that static linking can silently paper over).
fn dlopen_roundtrip(mimi_src: &str, c_probe: &str, expected: &str, tag: &str) {
    let tmp = std::env::temp_dir().join(format!("mimi_dlopen_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("dlopen: mkdir");

    let obj_path = tmp.join(format!("{}.o", tag));
    let so_path = tmp.join(format!("lib{}.so", tag));
    compile_to_object(mimi_src, tag, &obj_path);
    link_shared(&obj_path, &so_path, false);

    // C probe: dlopen + dlsym + call + print.
    let c_path = tmp.join("probe.c");
    let c_bin = tmp.join("probe");
    std::fs::write(&c_path, c_probe).expect("dlopen: write probe.c");
    let so_abs = so_path.canonicalize().expect("dlopen: canonicalize so");
    let cc_status = std::process::Command::new("cc")
        .arg("-no-pie")
        .arg(&c_path)
        .arg("-o")
        .arg(&c_bin)
        .arg("-ldl")
        .status()
        .expect("dlopen: cc probe");
    assert!(cc_status.success(), "dlopen: probe compilation failed");

    let output = std::process::Command::new(&c_bin)
        .arg(&so_abs)
        .output()
        .expect("dlopen: run probe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "dlopen: probe failed, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout.trim(),
        expected,
        "dlopen: ABI mismatch for {} (stdout={:?})",
        tag,
        stdout
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// 0.34.35b (M-006) shape matrix: 8-byte INTEGER-class record, both
/// parameter and return direction through dlopen.
#[test]
fn dlopen_reprc_s8_roundtrip() {
    let mimi_src = r#"
        #[repr(C)]
        type S8 { a: i32, b: i32 }
        extern "C" func make_s8(a: i32, b: i32) -> S8 {
            S8 { a, b }
        }
        extern "C" func sum_s8(s: S8) -> i32 {
            s.a + s.b
        }
    "#;
    let c_probe = r#"
        #include <dlfcn.h>
        #include <stdio.h>
        #include <stdlib.h>
        typedef struct { int a; int b; } S8;
        typedef S8 (*make_fn)(int, int);
        typedef int (*sum_fn)(S8);
        int main(int argc, char** argv) {
            void* h = dlopen(argv[1], RTLD_NOW);
            if (!h) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 1; }
            make_fn make = (make_fn)dlsym(h, "make_s8");
            sum_fn sum = (sum_fn)dlsym(h, "sum_s8");
            if (!make || !sum) { fprintf(stderr, "dlsym: %s\n", dlerror()); return 1; }
            S8 r = make(3, 4);
            printf("%d %d %d\n", r.a, r.b, sum(r));
            dlclose(h);
            return 0;
        }
    "#;
    dlopen_roundtrip(mimi_src, c_probe, "3 4 7", "s8");
}

/// 0.34.35b (M-006): 16-byte two-INTEGER record (SysV: two GP registers).
#[test]
fn dlopen_reprc_s16_roundtrip() {
    let mimi_src = r#"
        #[repr(C)]
        type S16 { a: i64, b: i64 }
        extern "C" func make_s16(a: i64, b: i64) -> S16 {
            S16 { a, b }
        }
        extern "C" func diff_s16(s: S16) -> i64 {
            s.b - s.a
        }
    "#;
    let c_probe = r#"
        #include <dlfcn.h>
        #include <stdio.h>
        #include <stdlib.h>
        typedef struct { long long a; long long b; } S16;
        typedef S16 (*make_fn)(long long, long long);
        typedef long long (*diff_fn)(S16);
        int main(int argc, char** argv) {
            void* h = dlopen(argv[1], RTLD_NOW);
            if (!h) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 1; }
            make_fn make = (make_fn)dlsym(h, "make_s16");
            diff_fn diff = (diff_fn)dlsym(h, "diff_s16");
            if (!make || !diff) { fprintf(stderr, "dlsym: %s\n", dlerror()); return 1; }
            S16 r = make(10, 25);
            printf("%lld %lld %lld\n", r.a, r.b, diff(r));
            dlclose(h);
            return 0;
        }
    "#;
    dlopen_roundtrip(mimi_src, c_probe, "10 25 15", "s16");
}

/// 0.34.35b (M-006): 24-byte MEMORY-class record — byval parameter AND sret
/// return (the M-010/N-1 root-case shape that SIGSEGV'd the debug compiler).
#[test]
fn dlopen_reprc_s24_byval_sret_roundtrip() {
    let mimi_src = r#"
        #[repr(C)]
        type S24 { id: i32, value: f64, flag: i32 }
        extern "C" func make_s24(id: i32, value: f64, flag: i32) -> S24 {
            S24 { id, value, flag }
        }
        extern "C" func total_s24(s: S24) -> f64 {
            (s.id + s.flag) as f64 + s.value
        }
    "#;
    let c_probe = r#"
        #include <dlfcn.h>
        #include <stdio.h>
        #include <stdlib.h>
        typedef struct { int id; double value; int flag; } S24;
        typedef S24 (*make_fn)(int, double, int);
        typedef double (*total_fn)(S24);
        int main(int argc, char** argv) {
            void* h = dlopen(argv[1], RTLD_NOW);
            if (!h) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 1; }
            make_fn make = (make_fn)dlsym(h, "make_s24");
            total_fn total = (total_fn)dlsym(h, "total_s24");
            if (!make || !total) { fprintf(stderr, "dlsym: %s\n", dlerror()); return 1; }
            S24 r = make(10, 3.5, 1);
            printf("%d %.1f %d %.1f\n", r.id, r.value, r.flag, total(r));
            dlclose(h);
            return 0;
        }
    "#;
    dlopen_roundtrip(mimi_src, c_probe, "10 3.5 1 14.5", "s24");
}

/// 0.34.35b (M-006): 16-byte SSE-class record (two f64 — SysV: two SSE regs).
#[test]
fn dlopen_reprc_sse16_roundtrip() {
    let mimi_src = r#"
        #[repr(C)]
        type SSE16 { a: f64, b: f64 }
        extern "C" func make_sse16(a: f64, b: f64) -> SSE16 {
            SSE16 { a, b }
        }
        extern "C" func mul_sse16(s: SSE16) -> f64 {
            s.a * s.b
        }
    "#;
    let c_probe = r#"
        #include <dlfcn.h>
        #include <stdio.h>
        #include <stdlib.h>
        typedef struct { double a; double b; } SSE16;
        typedef SSE16 (*make_fn)(double, double);
        typedef double (*mul_fn)(SSE16);
        int main(int argc, char** argv) {
            void* h = dlopen(argv[1], RTLD_NOW);
            if (!h) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 1; }
            make_fn make = (make_fn)dlsym(h, "make_sse16");
            mul_fn mul = (mul_fn)dlsym(h, "mul_sse16");
            if (!make || !mul) { fprintf(stderr, "dlsym: %s\n", dlerror()); return 1; }
            SSE16 r = make(2.5, 4.0);
            printf("%.1f %.1f %.1f\n", r.a, r.b, mul(r));
            dlclose(h);
            return 0;
        }
    "#;
    dlopen_roundtrip(mimi_src, c_probe, "2.5 4.0 10.0", "sse16");
}

/// 0.34.35b (M-006): f32 缺位负测试——f32 未注册（M-008），checker 必须
/// fail-loud 拒绝（E0407），不允许静默按 f64/i64 编组产生错误 ABI。
#[test]
fn dlopen_f32_unsupported_rejected() {
    let src = "extern \"C\" func f32_identity(x: f32) -> f32 { x }";
    let tokens = lexer::Lexer::new(src).tokenize().expect("f32 test: lex");
    let file = parser::Parser::new(tokens)
        .parse_file()
        .expect("f32 test: parse");
    let check = crate::core::check(&file);
    assert!(check.is_err(), "f32 export must be rejected at check time");
    let diags = check.expect_err("f32 diags");
    let msg = diags
        .iter()
        .map(|d| format!("{}", d))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        msg.contains("E0407"),
        "f32 rejection should carry E0407, got: {}",
        msg
    );
}

#[test]
fn parse_exported_func() {
    let src = "extern \"C\" func add(a: i64, b: i64) -> i64 { a + b }";
    let file = parse_and_check(src);
    assert_eq!(file.items.len(), 1);
}

#[test]
fn build_shared_library() {
    let tmp = std::env::temp_dir().join(format!("mimi_build_shared_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("src/tests/build_shared.rs:67 unwrap failed");

    let src = "extern \"C\" func add(a: i64, b: i64) -> i64 { a + b }";
    let obj_path = tmp.join("math.o");
    let _output_so = tmp.join("math.so");

    compile_to_object(src, "math", &obj_path);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn build_shared_library_no_std() {
    let tmp = std::env::temp_dir().join(format!("mimi_build_shared_nostd_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("src/tests/build_shared.rs:82 unwrap failed");

    let src = "extern \"C\" func double(x: i64) -> i64 { x + x }";
    let obj_path = tmp.join("double.o");
    let output_so = tmp.join("double.so");

    compile_to_object(src, "double", &obj_path);
    assert!(obj_path.exists());

    link_shared(&obj_path, &output_so, false);
    assert!(output_so.exists());

    // Verify ELF shared library
    let file_out = std::process::Command::new("file")
        .arg(&output_so)
        .output()
        .expect("file");
    let out = String::from_utf8_lossy(&file_out.stdout);
    assert!(
        out.contains("shared object") || out.contains("shared library"),
        "not a shared library: {}",
        out
    );

    // Verify symbol
    let nm_out = std::process::Command::new("nm")
        .arg("-D")
        .arg(&output_so)
        .output()
        .expect("nm");
    let nm = String::from_utf8_lossy(&nm_out.stdout);
    assert!(
        nm.contains("double") || nm.contains("_double"),
        "missing 'double' symbol: {}",
        nm
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn export_complex_reprc_record_build() {
    let tmp =
        std::env::temp_dir().join(format!("mimi_export_complex_reprc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("mkdir failed");

    let mimi_src = r#"
        #[repr(C)]
        type MixedStruct { id: i32, value: f64, flag: i32 }
        extern "C" func make_mixed(id: i32, val: f64, fl: i32) -> MixedStruct {
            MixedStruct { id, value: val, flag: fl }
        }
    "#;
    let obj_path = tmp.join("mixed.o");
    let so_path = tmp.join("libmixed.so");

    compile_to_object(mimi_src, "mixed", &obj_path);
    link_shared(&obj_path, &so_path, false);

    // Verify the symbol exists in the .so
    let nm_out = std::process::Command::new("nm")
        .arg("-D")
        .arg(&so_path)
        .output()
        .expect("nm");
    let nm = String::from_utf8_lossy(&nm_out.stdout);
    assert!(
        nm.contains("make_mixed"),
        "missing make_mixed symbol: {}",
        nm
    );

    // Write C caller program.
    // 0.34.35 (M-010): repr(C) structs now follow SysV strictly — MixedStruct
    // (24B) is returned via sret, i.e. the C caller just declares a by-value
    // return and the compiler passes the hidden buffer in rdi. (Previously
    // the wrapper returned a heap pointer, which was never the C ABI.)
    let c_src = r#"
        #include <stdio.h>
        typedef struct { int id; double value; int flag; } MixedStruct;
        MixedStruct make_mixed(int id, double val, int flag);
        int main() {
            MixedStruct s = make_mixed(10, 3.5, 1);
            printf("%d\n%.1f\n%d\n", s.id, s.value, s.flag);
            return 0;
        }
    "#;
    let c_path = tmp.join("caller.c");
    let c_bin = tmp.join("caller");
    std::fs::write(&c_path, c_src).expect("write caller.c");

    let cc_status = std::process::Command::new("cc")
        .arg("-no-pie")
        .arg(&c_path)
        .arg("-L")
        .arg(&tmp)
        .arg("-lmixed")
        .arg("-o")
        .arg(&c_bin)
        .arg("-lpthread")
        .arg("-ldl")
        .arg("-lm")
        .status()
        .expect("cc compile caller");
    assert!(cc_status.success(), "C caller compilation failed");

    // Run with LD_LIBRARY_PATH set
    let output = std::process::Command::new(&c_bin)
        .env("LD_LIBRARY_PATH", &tmp)
        .output()
        .expect("run caller");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "C caller failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.first().copied(), Some("10"), "C caller: id mismatch");
    assert_eq!(
        lines.get(1).copied(),
        Some("3.5"),
        "C caller: value mismatch"
    );
    assert_eq!(lines.get(2).copied(), Some("1"), "C caller: flag mismatch");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn emit_py_bindings_with_mimi_lib() {
    let src = "extern \"C\" { func greet(name: string) }\nextern \"C\" func add(a: i64, b: i64) -> i64 { a + b }";
    let tokens = lexer::Lexer::new(src)
        .tokenize()
        .expect("src/tests/build_shared.rs:114 unwrap failed");
    let file = parser::Parser::new(tokens)
        .parse_file()
        .expect("src/tests/build_shared.rs:115 unwrap failed");

    let mut extern_funcs = Vec::new();
    let mut exported_funcs = Vec::new();
    let type_defs = std::collections::HashMap::new();

    // Collect extern declarations
    for item in &file.items {
        use crate::ast::Item;
        match item {
            Item::ExternBlock(eb) => {
                for ef in &eb.funcs {
                    extern_funcs.push(ef.clone());
                }
            }
            Item::Func(f) => {
                if f.extern_abi.is_some() {
                    let extern_func = crate::ast::ExternFunc {
                        meta: f.meta,
                        name: f.name.clone(),
                        params: f
                            .params
                            .iter()
                            .map(|p| crate::ast::ExternParam {
                                meta: p.meta,
                                name: p.name.clone(),
                                ty: p.ty.clone(),
                                cap_mode: None,
                            })
                            .collect(),
                        ret: f.ret.clone(),
                        requires: None,
                        ensures: None,
                        variadic: false,
                        no_panic: false,
                        returns_errno: false,
                    };
                    extern_funcs.push(extern_func);
                    exported_funcs.push(f.clone());
                }
            }
            _ => {}
        }
    }

    let bindings = crate::ffi::py_bind::PyBindGenerator::new(type_defs.clone(), "greeter")
        .generate(&extern_funcs)
        .expect("src/tests/build_shared.rs:149 unwrap failed");
    assert!(bindings.contains("PYBIND11_MODULE"));
    assert!(bindings.contains("add"));
    assert!(bindings.contains("greet"));

    let cmake = crate::ffi::py_bind::generate_cmake_snippet(
        "greeter",
        "./",
        "/usr/local/lib",
        "/tmp/libgreeter.so",
    );
    assert!(cmake.contains("find_library(MIMI_USER_LIB"));
    assert!(cmake.contains("greeter PRIVATE ${MIMI_USER_LIB}"));
}
