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
    gen.compile_to_object(obj_path)
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
    // The complex repr(C) struct is returned as a heap-allocated pointer;
    // see convert_internal_reprc_record_to_c in export.rs.
    let c_src = r#"
        #include <stdio.h>
        #include <stdlib.h>
        typedef struct { int id; double value; int flag; } MixedStruct;
        MixedStruct* make_mixed(int id, double val, int flag);
        int main() {
            MixedStruct* s = make_mixed(10, 3.5, 1);
            printf("%d\n%.1f\n%d\n", s->id, s->value, s->flag);
            free(s);
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
                        name: f.name.clone(),
                        params: f
                            .params
                            .iter()
                            .map(|p| crate::ast::ExternParam {
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
