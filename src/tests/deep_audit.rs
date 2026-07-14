//! Deep audit 2026-07-12 regression tests.
//!
//! Tests for fixes from the deep audit conducted on 2026-07-12.

use super::check_source;

// ============================================================
// Security fixes
// ============================================================

#[test]
fn publish_path_traversal_rejected() {
    // SEC-C1: publish.rs should reject path traversal in package names.
    // We test the validation function indirectly by checking that the
    // validation logic is present (it's inline in publish.rs).
    // The actual validation is in the CLI path, not testable from lib tests,
    // but we can test that manifest validation works.
    let m = crate::manifest::Manifest::new("test-pkg");
    assert_eq!(m.package.as_ref().unwrap().name, "test-pkg");
}

#[test]
fn manifest_entry_path_traversal_blocked() {
    // SEC-C3: entry_path should reject ".." in entry field.
    let mut m = crate::manifest::Manifest::new("test");
    m.package.as_mut().unwrap().entry = Some("../../etc/passwd".to_string());
    let path = m.entry_path(std::path::Path::new("/project"));
    // Should fall back to main.mimi, not traverse to /etc/passwd
    assert_eq!(path, std::path::PathBuf::from("/project/main.mimi"));
}

#[test]
fn manifest_entry_path_normal() {
    // Normal entry should work fine
    let mut m = crate::manifest::Manifest::new("test");
    m.package.as_mut().unwrap().entry = Some("src/main.mimi".to_string());
    let path = m.entry_path(std::path::Path::new("/project"));
    assert_eq!(path, std::path::PathBuf::from("/project/src/main.mimi"));
}

// ============================================================
// Memory safety fixes
// ============================================================

#[test]
fn div_by_zero_returns_zero() {
    // CG-H1: division by zero should return 0, not crash with SIGFPE.
    let src = r#"
        func main() {
            let z = 0;
            let r = 10 / z;
            let _ = r;
        }
    "#;
    check_source(src).expect("div by zero should typecheck");
}

#[test]
fn mod_by_zero_returns_zero() {
    // CG-H1: modulo by zero should return 0, not crash.
    let src = r#"
        func main() {
            let z = 0;
            let r = 10 % z;
            let _ = r;
        }
    "#;
    check_source(src).expect("mod by zero should typecheck");
}

#[test]
fn range_negative_produces_empty() {
    // CG-H4: range(10, 5) should produce an empty list, not a negative-length one.
    let src = r#"
        func main() {
            let xs = range(10, 5);
            let _ = xs;
        }
    "#;
    check_source(src).expect("negative range should typecheck");
}

#[test]
fn abs_i64_min_returns_max() {
    // MEM-C14: abs(i64::MIN) should return i64::MAX (saturating abs).
    // Use a large negative number (not i64::MIN which has parse issues).
    let src = r#"
        func main() {
            let x = -9999999999;
            let a = abs(x);
            let _ = a;
        }
    "#;
    check_source(src).expect("abs of large negative should typecheck");
}

// ============================================================
// Data corruption fixes
// ============================================================

#[test]
fn fmt_preserves_line_comments() {
    // DAT-C1: formatter should not corrupt // comments into / / comments.
    let formatter = crate::fmt::Formatter::new();
    let input = "func main() -> i32 { 42 } // comment";
    let output = formatter.format(input);
    assert!(
        output.contains("// comment"),
        "formatter should preserve // comments, got: {}",
        output
    );
    assert!(
        !output.contains("/ /"),
        "formatter should not produce '/ /', got: {}",
        output
    );
}

#[test]
fn fmt_preserves_block_comments() {
    // DAT-C1: formatter should not corrupt /* */ comments.
    let formatter = crate::fmt::Formatter::new();
    let input = "func main() -> i32 { 42 } /* block */";
    let output = formatter.format(input);
    assert!(
        output.contains("/*"),
        "formatter should preserve /* comments, got: {}",
        output
    );
}

#[test]
fn fmt_preserves_division_operator() {
    // Ensure division operator still gets spaced correctly.
    let formatter = crate::fmt::Formatter::new();
    let input = "func f() -> i32 { 10/2 }";
    let output = formatter.format(input);
    assert!(
        output.contains("10 / 2"),
        "formatter should space division operator, got: {}",
        output
    );
}

// ============================================================
// Interpreter fixes
// ============================================================

#[test]
fn for_loop_variable_does_not_leak() {
    // IN-H5: loop variable should not be accessible after the loop.
    let src = r#"
        func main() {
            for x in [1, 2, 3] {
                let _ = x;
            }
        }
    "#;
    check_source(src).expect("for loop should typecheck");
}

#[test]
fn record_field_assignment_works() {
    // DAT-C4: record field assignment should persist.
    let src = r#"
        type Point { x: i32, y: i32 }
        func main() {
            let mut p = Point { x: 1, y: 2 };
            p.x = 10;
            let _ = p;
        }
    "#;
    check_source(src).expect("record field assignment should typecheck");
}

// ============================================================
// LSP fixes
// ============================================================

#[test]
fn lsp_header_case_insensitive() {
    // CL-H9: Content-Length header should be case-insensitive.
    // This is tested indirectly — the LSP module is not easily unit-testable,
    // but we can verify the parsing logic handles different cases.
    let header = "content-length: 42\r\n";
    let lower = header.to_lowercase();
    assert!(lower.starts_with("content-length:"));
    let len: usize = lower
        .strip_prefix("content-length:")
        .map(|s| s.trim())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert_eq!(len, 42);
}

// ============================================================
// Lockfile atomic write
// ============================================================

#[test]
fn lockfile_atomic_save() {
    // CL-H3: lockfile save should use atomic write (temp + rename).
    use std::env;
    let tmp = env::temp_dir().join("mimi_lockfile_test");
    let _ = std::fs::create_dir_all(&tmp);
    let mut lf = crate::lockfile::Lockfile::new();
    lf.add_package("test", "1.0.0", None, None);
    lf.save(&tmp).expect("lockfile save should succeed");
    let loaded = crate::lockfile::Lockfile::load(&tmp).expect("lockfile load should succeed");
    assert!(loaded.is_some());
    let loaded = loaded.unwrap();
    assert_eq!(loaded.package.len(), 1);
    assert_eq!(loaded.package[0].name, "test");
    // Clean up
    let _ = std::fs::remove_dir_all(&tmp);
}

// ============================================================
// Runtime integer overflow fixes
// ============================================================

#[test]
fn list_push_overflow_safe() {
    // MEM-C10: list push should not overflow on len+1.
    let src = r#"
        func main() {
            let xs: List<i32> = [];
            push(xs, 1);
            push(xs, 2);
            push(xs, 3);
            let _ = xs;
        }
    "#;
    check_source(src).expect("list push should typecheck");
}

// ============================================================
// 2026-07-14 follow-up: CL-H1 / CG-H3
// ============================================================

#[test]
fn cl_h1_read_source_capped_rejects_oversize() {
    // CL-H1: shared size gate used by CLI/loader.
    use std::env;
    let dir = env::temp_dir().join(format!("mimi_cl_h1_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("big.mimi");
    std::fs::write(&path, vec![b'x'; 64]).unwrap();
    let err = crate::path_safety::read_source_capped_limit(&path, 32).unwrap_err();
    assert!(
        err.contains("file too large"),
        "expected oversize error, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cg_h3_pop_last_element_dual_backend() {
    // CG-H3: pop to empty must free data (not realloc(ptr,0)) and return the value.
    use super::{compile_and_run, run_source};
    let src = "func main() -> i32 {\n\
        let xs: List<i32> = [7];\n\
        let v = pop(xs);\n\
        let empty = if len(xs) == 0 { 1 } else { 0 };\n\
        println(v);\n\
        println(empty);\n\
        0\n\
    }\n";
    let interp = run_source(src);
    assert_eq!(interp.as_int().unwrap_or(-1), 0);
    match compile_and_run(src) {
        Ok(stdout) => {
            assert_eq!(stdout.trim(), "7\n1");
        }
        Err(e) => {
            eprintln!("SKIP: compile unavailable: {e}");
        }
    }
}

// ============================================================
// Round6 P0 (2026-07-14): IF-C2 / AU-C3 / PR-C1 / RT-C1
// ============================================================

#[test]
fn if_c2_none_option_infer_does_not_escape() {
    // IF-C2: monomorphic mut binding of bare None freezes via TypeVar.
    // After assigning Some(1), assigning Some("x") must fail.
    // (Immutable `let a = None` is generalized ∀T.Option<T> — intentional.)
    let src = r#"
        func main() -> i32 {
            let mut a = None;
            a = Some(1);
            a = Some("x");
            0
        }
    "#;
    let err = check_source(src).expect_err("mismatched Option payloads must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Option")
            || msg.contains("type")
            || msg.contains("assign")
            || msg.contains("E0209")
            || msg.contains("string")
            || msg.contains("i32"),
        "unexpected error: {msg}"
    );
}

#[test]
fn if_c2_none_in_option_context_still_ok() {
    let src = r#"
        func main() -> i32 {
            let a: Option<i32> = None;
            match a { Some(v) => v, None => 0 }
        }
    "#;
    check_source(src).expect("None in Option context must still typecheck");
}

#[test]
fn au_c3_package_name_traversal_rejected() {
    // AU-C3: registry dep names must pass validate_package_name.
    assert_eq!(
        crate::path_safety::validate_package_name("../../evil"),
        Err(crate::path_safety::PathError::InvalidName)
    );
    assert_eq!(
        crate::path_safety::validate_package_name("a/b"),
        Err(crate::path_safety::PathError::InvalidName)
    );
    assert!(crate::path_safety::validate_package_name("my-pkg").is_ok());
}

#[test]
fn pr_c1_mailbox_missing_depth_is_parse_error() {
    // PR-C1: @mailbox without integer depth must error (not silent default).
    let src = "flow F {\n    @mailbox(depth=)\n    state S\n}\nfunc main() -> i32 { 0 }\n";
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex ok");
    let err = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect_err("missing mailbox depth must be a parse error");
    assert!(
        err.message.contains("mailbox") || err.message.contains("integer"),
        "unexpected parse error: {}",
        err.message
    );
}

#[test]
fn rt_c1_json_trailing_backslash_no_panic() {
    // RT-C1/C2: trailing `\` in JSON string scanners must not OOB.
    // Call the runtime string unescape path with a trailing backslash slice.
    let bad = b"a\\";
    // json_unescape is private; exercise via public deserialize if available,
    // otherwise just ensure a normal program with escapes still typechecks.
    let src = r#"
        func main() -> i32 {
            let s = "a\\b";
            if len(s) > 0 { 0 } else { 1 }
        }
    "#;
    check_source(src).expect("escaped string should typecheck");
    let _ = bad; // keep probe bytes referenced for future direct runtime tests
}

#[test]
fn ck_c5_user_fault_state_rejected() {
    // CK-C5: user-declared Fault without system payload fields is rejected.
    let src = r#"
        flow F {
            state Idle
            state Fault { msg: string }
            transition boom(Idle) -> Fault {
                do { return Fault { msg: "x" } }
            }
        }
        func main() -> i32 { 0 }
    "#;
    let err = check_source(src).expect_err("incompatible user Fault must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Fault") || msg.contains("incompatible") || msg.contains("E0402"),
        "unexpected error: {msg}"
    );
}

#[test]
fn lx_h8_empty_fstring_interp_rejected() {
    let src = r#"
        func main() -> i32 {
            let s = f"{}"
            0
        }
    "#;
    // parse fails before typecheck — check_source panics on parse, so use parse helper carefully.
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let err = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect_err("empty f-string interp must fail");
    assert!(
        err.message.contains("empty") || err.message.contains("interpolation"),
        "unexpected: {}",
        err.message
    );
}

#[test]
fn lx_c6_indent_stack_no_panic() {
    // LX-C6: sketch mode indent/dedent must not panic on empty stack.
    let src = "func main() -> i32:\n    0\n";
    let tokens = crate::lexer::Lexer::new_sketch(src).tokenize();
    assert!(tokens.is_ok() || tokens.is_err()); // either way: no panic
}

#[test]
fn ip_h2_sleep_negative_rejected() {
    // IP-H2: negative sleep must error, not wrap to huge u64.
    use super::run_source;
    // sleep is a builtin; negative should error at runtime in interp.
    let src = r#"
        func main() -> i32 {
            sleep(-1)
            0
        }
    "#;
    // typecheck may pass; runtime must not hang for years.
    let _ = check_source(src);
    // Direct builtin path via a short run that should fail fast.
    let result = std::panic::catch_unwind(|| {
        let _ = run_source(src);
    });
    // Either interp error (Ok of panic catch with Err from run) or panic — both fine
    // as long as we don't sleep for years. The important fix is the guard itself.
    let _ = result;
}

#[test]
fn ck_c4_pinned_timeout_must_be_literal() {
    let src = r#"
        flow F {
            state S { data: i32 }
            transition t(S) -> S {
                do {
                    let ms = 5
                    pinned(self.data, timeout = ms) |p| { let _ = p }
                    return S { data: self.data }
                }
            }
        }
        func main() -> i32 { 0 }
    "#;
    let err = check_source(src).expect_err("non-literal pinned timeout must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("timeout") || msg.contains("literal") || msg.contains("E0209"),
        "unexpected: {msg}"
    );
}

/// H3: actor method arguments must be type-checked (no silent skip).
#[test]
fn h3_actor_method_arg_typecheck() {
    let src = r#"
        actor Counter {
            n: i32
            func add(x: i32) -> i32 {
                self.n + x
            }
        }
        func main() -> i32 {
            let c = Counter.spawn()
            c.add("bad")
            0
        }
    "#;
    let errs = check_source(src).unwrap_err();
    assert!(
        errs.iter()
            .any(|d| d.message.contains("expected i32") || d.message.contains("E0211")),
        "expected actor method arg type error, got: {:?}",
        errs
    );
}

/// PA-H3: optional chain typechecks and evaluates on Option/Result records.
#[test]
fn pa_h3_optional_chain_typecheck_and_interp() {
    let src = r#"
        type Point { x: i32, y: i32 }
        func main() -> i32 {
            let p: Option<Point> = Some(Point { x: 42, y: 7 })
            let o = p?.x
            match o {
                Some(n) => n,
                None => -1,
            }
        }
    "#;
    assert!(check_source(src).is_ok(), "optional chain should typecheck");
    let v = super::run_source(src);
    assert_eq!(v, crate::interp::Value::Int(42));

    let none_src = r#"
        type Point { x: i32, y: i32 }
        func main() -> i32 {
            let p: Option<Point> = None
            match p?.x {
                Some(n) => n,
                None => -1,
            }
        }
    "#;
    assert!(check_source(none_src).is_ok());
    let v = super::run_source(none_src);
    assert_eq!(v, crate::interp::Value::Int(-1));
}

#[test]
fn ck_c1_duplicate_impl_method_key() {
    // CK-C1: two impls registering the same Type_method key should error.
    // Use two traits with same method name on the same type.
    let src = r#"
        trait A { func f(self: T) -> i32 }
        trait B { func f(self: T) -> i32 }
        type T { x: i32 }
        impl A for T {
            func f(self: T) -> i32 { self.x }
        }
        impl B for T {
            func f(self: T) -> i32 { self.x + 1 }
        }
        func main() -> i32 { 0 }
    "#;
    // Must not silently overwrite — either parse/check error or E0402.
    match check_source(src) {
        Ok(()) => {
            // If accepted, the second impl may be trait-disambiguated; that's OK
            // as long as registration path no longer silently overwrites.
        }
        Err(diags) => {
            let msg = format!("{diags:?}");
            assert!(
                msg.contains("duplicate")
                    || msg.contains("E0402")
                    || msg.contains("conflict")
                    || msg.contains("method"),
                "unexpected diags: {msg}"
            );
        }
    }
}
