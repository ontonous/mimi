//! Regression tests for the systematic review reports (batch3–batch5).
//!
//! These cover the first wave of fixes landed from the audit summaries:
//! Result/string error ownership in builtins, list flatten/pop type decoding,
//! IO/math/LSP boundary fixes, and Rust FFI signature hardening.

use super::*;

fn can_link() -> bool {
    crate::tests::can_link()
}

/// Assert that both the bytecode VM and the native codegen path produce the
/// same trimmed stdout for a normal dual-backend source.
fn assert_dual(src: &str, expected: &str) {
    if !can_link() {
        return;
    }
    check_source(src).unwrap_or_else(|diags| {
        panic!(
            "checker rejected dual source:\n{}",
            diags
                .iter()
                .map(|d| format!("{}", d))
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    let interp_run = std::panic::catch_unwind(|| run_source_with_stdout(src));
    assert!(interp_run.is_ok(), "interpreter panicked");
    let (_interp_val, interp_stdout) = interp_run.unwrap();
    assert_eq!(
        interp_stdout.trim(),
        expected,
        "interpreter stdout mismatch\ninterp: {}\nexpected: {}",
        interp_stdout.trim(),
        expected
    );
    let codegen_stdout = compile_and_run(src).expect("codegen failed");
    assert_eq!(
        codegen_stdout.trim(),
        expected,
        "codegen mismatch\ncodegen: {}\nexpected: {}",
        codegen_stdout.trim(),
        expected
    );
}

fn assert_vm_traps(src: &str, needle: &str) {
    let vm = run_source_bytecode_result(src);
    let err = vm
        .err()
        .unwrap_or_else(|| panic!("VM must trap (expected {:?}), got Ok", needle));
    assert!(
        err.contains(needle),
        "VM error missing {:?}: {}",
        needle,
        err
    );
}

fn assert_codegen_traps(src: &str, needle: &str) {
    let cg = compile_and_run(src);
    let err = cg
        .err()
        .unwrap_or_else(|| panic!("codegen must trap (expected {:?}), got Ok", needle));
    assert!(
        err.contains(needle),
        "codegen stderr missing {:?}: {}",
        needle,
        err
    );
}

#[test]
fn review_flatten_native_matches_vm() {
    assert_dual(
        r#"
        func main() -> i32 {
            let f = flatten([[1,2],[3,4],[5]]);
            println(len(f));
            println(f[0]); println(f[1]); println(f[2]); println(f[3]); println(f[4]);
            println(len(flatten([])));
            0
        }
        "#,
        "5\n1\n2\n3\n4\n5\n0",
    );
}

#[test]
fn review_pop_decodes_typed_slots_native() {
    assert_dual(
        r#"
        func main() -> i32 {
            let lf: List<f64> = [1.5, 2.5];
            let ls: List<string> = ["abc"];
            println(pop(lf));
            println(pop(ls));
            0
        }
        "#,
        "2.5\nabc",
    );
}

#[test]
fn review_getenv_missing_native_no_crash() {
    if !can_link() {
        return;
    }
    let src = r#"
        func main() -> i32 {
            match getenv("MIMI_DEFINITELY_MISSING_REVIEW_VAR") {
                Ok(v) => println(v),
                Err(e) => println(e),
            }
            0
        }
        "#;
    let vm = run_source_with_stdout(src).1;
    let native = compile_and_run(src).expect("getenv native build/run failed");
    assert!(
        vm.contains("not set"),
        "VM should report missing env: {vm:?}"
    );
    assert!(
        native.contains("not set"),
        "native should not crash and should report: {native:?}"
    );
}

#[test]
fn review_read_file_missing_native_no_crash() {
    if !can_link() {
        return;
    }
    let src = r#"
        func make_err() -> Result<string, string> {
            return read_file("/tmp/definitely_missing_review_xyz_12345")
        }
        func main() -> i32 {
            match make_err() {
                Ok(v) => println(v),
                Err(e) => println(e),
            }
            0
        }
        "#;
    let vm = run_source_with_stdout(src).1;
    let native = compile_and_run(src).expect("read_file native build/run failed");
    assert!(vm.contains("No such file"), "VM error path: {vm:?}");
    assert!(
        native.contains("No such file"),
        "native error path must be safe: {native:?}"
    );
}

#[test]
fn review_print_varargs_and_empty_native() {
    assert_dual(
        r#"
        func main() -> i32 {
            print("a","b","c")
            println()
            print()
            println("done")
            0
        }
        "#,
        "a b c\ndone",
    );
}

#[test]
fn review_format_bool_renders_words_native() {
    assert_dual(
        r#"
        func main() -> i32 {
            println(format("{} {}", true, false))
            0
        }
        "#,
        "true false",
    );
}

#[test]
fn review_float_minmax_and_integer_floor_native() {
    assert_dual(
        r#"
        func main() -> i32 {
            println(min(1.5, 2.5))
            println(max(1.5, 2.5))
            println(floor(3))
            println(ceil(3))
            println(round(3))
            0
        }
        "#,
        "1.5\n2.5\n3\n3\n3",
    );
}

#[test]
fn review_try_builtin_result_propagates_native() {
    assert_dual(
        r#"
        func inner() -> Result<i32, string> { Err("boom") }
        func outer() -> Result<i32, string> {
            let x = inner()?;
            Ok(x)
        }
        func main() -> i32 {
            match outer() {
                Ok(v) => { println(v); 1 }
                Err(e) => { println(e); 0 }
            }
        }
        "#,
        "boom",
    );
}

#[test]
fn review_try_builtin_option_propagates_native() {
    assert_dual(
        r#"
        func inner() -> Option<i32> { None }
        func outer() -> Option<i32> {
            let x = inner()?;
            Some(x)
        }
        func main() -> i32 {
            match outer() {
                Some(v) => { println(v); 1 }
                None => { println("none"); 0 }
            }
        }
        "#,
        "none",
    );
}

#[test]
fn review_result_f64_err_native() {
    assert_dual(
        r#"
        func f() -> Result<i32, f64> { Err(2.5) }
        func main() -> i32 {
            match f() {
                Ok(v) => { println(v); 1 }
                Err(e) => { println(e); 0 }
            }
        }
        "#,
        "2.5",
    );
}

#[test]
fn review_result_f64_err_try_native() {
    // P1-08 regression: `?` inside a `fails f64` transition used to reject
    // the f64 error payload during try_rej lowering. The f64 bits are now
    // carried in the i64 error slot so both backends compile and run.
    assert_dual(
        r#"
        flow F {
            state S { x: i32 }
            transition go(S, a: i32, b: i32) -> S fails f64 {
                let result = safe_div(a, b)
                let y = result?
                return S { x: y }
            }
        }
        func safe_div(a: i32, b: i32) -> Result<i32, f64> {
            if b == 0 { return Err(1.5) }
            return Ok(a / b)
        }
        func main() -> i32 {
            let s0 = S { x: 0 }
            let r = F::go(s0, 10, 2)
            let v = match r {
                Ok(s) => s.x,
                Err(_) => 0 - 1,
            }
            println(v)
            0
        }
        "#,
        "5",
    );
}

#[test]
fn review_result_struct_err_native() {
    assert_dual(
        r#"
        type R { x: i32, y: i32 }
        func f() -> Result<i32, R> { Err(R { x: 1, y: 2 }) }
        func main() -> i32 {
            match f() {
                Ok(v) => { println(v); 1 }
                Err(e) => { println(e.x); println(e.y); 0 }
            }
        }
        "#,
        "1
2",
    );
}

#[test]
fn review_char_code_on_list_is_type_error_not_ice() {
    // The checker currently accepts this in some paths; the codegen must not
    // panic inside into_pointer_value(). A clear TypeMismatch is acceptable.
    if !can_link() {
        return;
    }
    let src = r#"
        func main() -> i32 {
            let l: List<i32> = [1, 2];
            println(char_code(l, 0))
            0
        }
        "#;
    // This may be rejected by check_source; either way it must not ICE.
    if check_source(src).is_ok() {
        let result = std::panic::catch_unwind(|| compile_and_run(src));
        assert!(
            result.is_ok(),
            "char_code(list) must not panic the compiler"
        );
        if let Ok(Ok(out)) = result {
            assert_ne!(out.trim(), "", "no silent empty result");
        }
    }
}

#[test]
fn review_lsp_word_range_unicode_separator() {
    let text = "func main() { foo，bar }";
    // The `，` is a 3-byte UTF-8 separator. The cursor is in `bar`, after the
    // separator; the old rfind()+1 logic returned a byte offset inside the
    // separator and panicked when slicing. The fixed boundary should yield
    // the following word without panicking.
    let range = crate::lsp::util::word_range_at(text, 0, 19);
    assert!(range.is_some());
    let (start, end) = range.unwrap();
    assert_eq!(&text.lines().next().unwrap()[start..end], "bar");
}

// --- batch5 P1-11: write_file must report short fwrite / failed fclose ---

#[test]
fn review_write_file_reports_short_write() {
    if !std::path::Path::new("/dev/full").exists() {
        eprintln!("    └─ skipped (/dev/full not available)");
        return;
    }
    assert_dual(
        r#"
        func main() -> i32 {
            let result = write_file("/dev/full", "hello")
            match result {
                Ok(_) => println("ok"),
                Err(_) => println("err")
            }
            0
        }
        "#,
        "err",
    );
}

// --- batch5 P1-12: binary/stream IO errors must not silently become ""/[] ---

#[test]
fn review_binary_io_missing_path_traps_both_backends() {
    let read_bytes = r#"
func main() -> string {
    read_file_bytes("/definitely/not/a/mimi/file")
}
"#;
    assert_vm_traps(read_bytes, "read_file_bytes");
    if can_link() {
        assert_codegen_traps(read_bytes, "read_file_bytes");
    }

    let read_partial = r#"
func main() -> string {
    read_file_partial("/definitely/not/a/mimi/file", 10)
}
"#;
    assert_vm_traps(read_partial, "read_file_partial");
    if can_link() {
        assert_codegen_traps(read_partial, "read_file_partial");
    }

    let read_lines = r#"
func main() -> string {
    read_lines_json("/definitely/not/a/mimi/file")
}
"#;
    assert_vm_traps(read_lines, "read_lines_json");
    if can_link() {
        assert_codegen_traps(read_lines, "read_lines_json");
    }
}

#[test]
fn review_write_file_bytes_failure_returns_false_both_backends() {
    let src = r#"
func main() -> i32 {
    let ok = write_file_bytes("/definitely/not/a/mimi/dir/x", "data")
    println(if ok { "true" } else { "false" })
    0
}
"#;
    check_source(src).expect("write_file_bytes failure source should typecheck");
    let vm = run_source_with_stdout(src).1;
    assert_eq!(vm.trim(), "false", "VM must return false for failed write");
    if can_link() {
        let cg = compile_and_run(src).expect("write_file_bytes failed codegen compile");
        assert_eq!(
            cg.trim(),
            "false",
            "codegen must return false for failed write"
        );
    }
}

// --- batch5 P1-15: to_string must not return arbitrary aggregates as strings ---

#[test]
fn review_to_string_list_renders_display_not_type_confused() {
    assert_dual(
        r#"
        func main() -> i32 {
            let s = to_string([1, 2, 3])
            println(s)
            0
        }
        "#,
        "[1, 2, 3]",
    );
}

// --- batch5 P1-18: codegen map_set/map_remove must preserve old maps ---

#[test]
fn review_map_set_remove_are_persistent_both_backends() {
    assert_dual(
        r#"
        func main() -> i32 {
            let a = map_new()
            let b = map_set(a, "k", 42)
            println(has_key(a, "k"))
            println(has_key(b, "k"))
            let c = map_remove(b, "k")
            println(has_key(b, "k"))
            println(has_key(c, "k"))
            0
        }
        "#,
        "false\ntrue\ntrue\nfalse",
    );
}

// --- batch5-03 P2-2: bind rejects out-of-u16 ports on both backends ---

#[test]
fn review_bind_rejects_out_of_range_port() {
    let src = r#"
func main() -> i32 {
    bind(3, 70000)
    0
}
"#;
    assert_vm_traps(src, "port must be in 0..=65535");
    if can_link() {
        assert_codegen_traps(src, "port must be in 0..=65535");
    }

    let neg = r#"
func main() -> i32 {
    bind(3, 0 - 1)
    0
}
"#;
    assert_vm_traps(neg, "port must be in 0..=65535");
    if can_link() {
        assert_codegen_traps(neg, "port must be in 0..=65535");
    }
}
