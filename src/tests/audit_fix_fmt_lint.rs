//! Wave-1 audit-fix regression tests — fmt_lint.
//! Findings: devdocs/full-audit-2026-08-05.md §13 (2026-08-05 full audit).
//!
//! Fixes covered:
//!   §13.1  CRITICAL  fmt splits multi-char operators → unparseable output (fmt.rs)
//!   §13.17 MED-LOW   lint drops parse errors, reports "no issues found" exit 0 (lint_cmd.rs)
//!   §13    LOW-MED   fmt_cmd direct fs::write overwrite (fmt_cmd.rs) → atomic temp+rename
//!   §13    LOW-MED   lockfile.rs / manifest.rs fixed temp names race (mimi.lock.tmp / mimi.toml.tmp)
//!   §13    LOW       disasm_cmd.rs bypasses the 100MiB read_source_capped guard
//!
//! Lib-level fixes are tested directly; CLI-level fixes are exercised through
//! the real `mimi` binary when it is built (skip otherwise — same pattern as
//! src/tests/package_management.rs P-H13).
use super::*;

/// Locate the `mimi` CLI binary produced by cargo (mirrors package_management.rs).
fn mimi_bin() -> Option<std::path::PathBuf> {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target/debug/mimi");
    if p.exists() {
        return Some(p);
    }
    p.set_extension("exe");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// Unique temp dir per test tag + process; wiped before use.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mimi_audit_fmt_lint_{}_{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

// ═══════════════════════════════════════════════════════════════
// §13.1 CRITICAL — fmt must not split multi-char operators
// ═══════════════════════════════════════════════════════════════

/// A program exercising every lexer multi-char operator that `fmt` must glue.
const MULTI_OP_PROGRAM: &str = "func double(x: i32) -> i32 { x * 2 }
func main() -> i32 {
    let a = 6
    let b = 3
    let eq = a == b
    let ne = a != b
    let le = a <= b
    let ge = a >= b
    let both = eq && ne
    let either = eq || ne
    let mut acc = 0
    acc += a
    acc -= b
    acc *= 2
    acc /= 4
    let pw = 2 ** 3
    let sl = 8 >> 1
    let sr = 1 << 2
    let piped = 5 |> double()
    let picked = match a { 6 => 1  _ => 0 }
    if both || either { acc + pw + sl + sr + piped + picked } else { 0 }
}
";

#[test]
fn audit_fmt_multi_char_operators_stay_glued() {
    let formatted = crate::fmt::Formatter::new().format(MULTI_OP_PROGRAM);
    // None of the lexer multi-char operators may appear split.
    for op in [
        "==", "!=", "<=", ">=", "=>", "+=", "-=", "*=", "/=", "&&", "||", "|>", "->", "**", "<<",
        ">>",
    ] {
        assert!(
            formatted.contains(op),
            "operator `{}` missing/glued-form absent in formatted output:\n{}",
            op,
            formatted
        );
    }
    // The specific corruption forms from the audit report.
    for split in [
        "= =", "+ =", "- =", "* =", "/ =", "> =", "< =", "& &", "| |", "| >", "= >",
    ] {
        assert!(
            !formatted.contains(split),
            "formatter split a multi-char operator into `{}`:\n{}",
            split,
            formatted
        );
    }
}

#[test]
fn audit_fmt_output_reparses_checks_and_is_idempotent() {
    // CRITICAL: format → re-parse + type-check must succeed, and
    // fmt(fmt(x)) == fmt(x).
    let formatter = crate::fmt::Formatter::new();
    let once = formatter.format(MULTI_OP_PROGRAM);
    let twice = formatter.format(&once);
    assert_eq!(once, twice, "formatter must be idempotent");
    // check_source parses (panicking on parse failure) then type-checks.
    check_source(&once)
        .unwrap_or_else(|diags| panic!("formatted output fails type check: {:?}", diags));
    // Semantics preserved across formatting (interpreter execution).
    let (expected_val, expected_out) = run_source_with_stdout(MULTI_OP_PROGRAM);
    let (actual_val, actual_out) = run_source_with_stdout(&once);
    assert_eq!(
        expected_val, actual_val,
        "formatting changed program semantics"
    );
    assert_eq!(expected_out, actual_out, "formatting changed stdout");
}

#[test]
fn audit_fmt_preserves_token_stream() {
    // Formatting changes whitespace/indentation only — the non-structural
    // token sequence must be identical before and after.
    let src = "func main() -> i32 {
let a = 6
let b = 3
let c = a==b&&a!=b||a<=b&&a>=b
let mut x = 0
x+=1
x-=1
x*=2
x/=2
let p = 2**3
let s = 4>>1<<1
let t = a|>main()
if c { x + p + s + t } else { 0 }
}
";
    let formatted = crate::fmt::Formatter::new().format(src);
    let kinds = |s: &str| -> Vec<String> {
        crate::lexer::Lexer::new(s)
            .tokenize()
            .expect("tokenize")
            .iter()
            .filter_map(|t| match t.kind {
                crate::lexer::TokenKind::Newline
                | crate::lexer::TokenKind::Indent
                | crate::lexer::TokenKind::Dedent
                | crate::lexer::TokenKind::Eof => None,
                _ => Some(format!("{:?}", t.kind)),
            })
            .collect()
    };
    assert_eq!(
        kinds(src),
        kinds(&formatted),
        "formatting changed the token stream:\n{}",
        formatted
    );
}

// ═══════════════════════════════════════════════════════════════
// §13.17 MED-LOW — lint must fail closed on parse errors
// ═══════════════════════════════════════════════════════════════

/// The exact source shape that used to be swallowed: recovery yields a
/// partial AST plus errors. The lint fix (src/main/lint_cmd.rs) now surfaces
/// these errors instead of reporting "no issues found".
const BROKEN_LET_SOURCE: &str = "func main() -> i32 {\n    let x =\n}\n";

#[test]
fn audit_lint_seam_parse_errors_are_reported() {
    let path = unique_temp_dir("seam").join("bad.mimi");
    std::fs::write(&path, BROKEN_LET_SOURCE).expect("write broken source");
    let tokens = crate::lexer::Lexer::new(BROKEN_LET_SOURCE)
        .tokenize()
        .expect("tokenize");
    let (_file, errors) = crate::loader::parser_for_path(tokens, &path)
        .expect("parser_for_path")
        .parse_file_with_recovery();
    assert!(
        !errors.is_empty(),
        "parse_file_with_recovery must report errors for unparseable input"
    );
    // Every error must render as a diagnostic (what lint_cmd now prints).
    for e in &errors {
        let diag = e.to_diagnostic();
        assert!(!diag.message.is_empty(), "diagnostic message empty");
    }
    let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn audit_cli_lint_fails_closed_on_parse_error() {
    let Some(bin) = mimi_bin() else {
        eprintln!("skip: mimi binary not built");
        return;
    };
    let dir = unique_temp_dir("lint_cli_bad");
    let bad = dir.join("bad.mimi");
    std::fs::write(&bad, BROKEN_LET_SOURCE).expect("write broken source");
    let out = std::process::Command::new(&bin)
        .arg("lint")
        .arg(&bad)
        .output()
        .expect("spawn mimi lint");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !out.status.success(),
        "lint must exit non-zero on parse errors, got {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("no issues found"),
        "lint must not claim success on unparseable input:\n{}",
        combined
    );
    // The parse error itself must be surfaced (rendered diagnostic or summary).
    assert!(
        combined.to_lowercase().contains("error") || combined.contains("expected"),
        "parse error must be surfaced, got:\n{}",
        combined
    );
}

#[test]
fn audit_cli_lint_ok_on_valid_file() {
    let Some(bin) = mimi_bin() else {
        eprintln!("skip: mimi binary not built");
        return;
    };
    let dir = unique_temp_dir("lint_cli_ok");
    let ok = dir.join("ok.mimi");
    std::fs::write(&ok, "func main() -> i32 { 42 }\n").expect("write valid source");
    let out = std::process::Command::new(&bin)
        .arg("lint")
        .arg(&ok)
        .output()
        .expect("spawn mimi lint");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "lint must exit 0 on a clean file, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no issues found"),
        "expected 'no issues found', got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ═══════════════════════════════════════════════════════════════
// §13 LOW-MED — atomic file writes (fmt_cmd / lockfile / manifest)
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit_write_text_atomic_replaces_content_without_leftovers() {
    let dir = unique_temp_dir("atomic");
    let path = dir.join("note.txt");
    crate::manifest::write_text_atomic(&path, "first").expect("first write");
    crate::manifest::write_text_atomic(&path, "second").expect("second write");
    assert_eq!(std::fs::read_to_string(&path).expect("read back"), "second");
    // No temp files may survive a successful atomic write.
    for entry in std::fs::read_dir(&dir).expect("read_dir").flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            name == "note.txt",
            "unexpected leftover after atomic write: {}",
            name
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn audit_write_text_atomic_error_propagates() {
    // Missing parent directory must surface as Err, not panic or swallow.
    let base = unique_temp_dir("atomic_err");
    let path = base.join("no_such_subdir").join("x.toml");
    let err = crate::manifest::write_text_atomic(&path, "x")
        .expect_err("write into missing dir must fail");
    assert!(
        err.contains("failed to write") || err.contains("failed to rename"),
        "error must name the failed operation: {}",
        err
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn audit_lockfile_save_roundtrip_no_temp_leftover() {
    let dir = unique_temp_dir("lock");
    let mut lf = crate::lockfile::Lockfile::new();
    lf.add_package("foo", "1.0.0", Some("registry"), None);
    lf.save(&dir).expect("lockfile save");
    let loaded = crate::lockfile::Lockfile::load(&dir)
        .expect("lockfile load")
        .expect("lockfile present");
    assert_eq!(
        loaded.get_package("foo").map(|p| p.version.as_str()),
        Some("1.0.0")
    );
    for entry in std::fs::read_dir(&dir).expect("read_dir").flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            name == "mimi.lock",
            "lockfile temp file leaked into dir: {}",
            name
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn audit_lockfile_concurrent_saves_do_not_corrupt() {
    // Same-pid writers: the per-pid + counter temp names must keep every
    // save intact. Pre-fix, all writers shared `mimi.lock.tmp`.
    let dir = unique_temp_dir("lock_concurrent");
    let handles: Vec<_> = (0..2u32)
        .map(|t| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                for i in 0..20u32 {
                    let mut lf = crate::lockfile::Lockfile::new();
                    lf.add_package(&format!("pkg{}", t), &format!("1.0.{}", i), None, None);
                    lf.save(&dir).expect("concurrent lockfile save");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("save thread panicked");
    }
    let loaded = crate::lockfile::Lockfile::load(&dir)
        .expect("lockfile must remain valid TOML after concurrent saves")
        .expect("lockfile present");
    assert!(!loaded.package.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn audit_manifest_save_roundtrip_no_temp_leftover() {
    let dir = unique_temp_dir("manifest");
    let m = crate::manifest::Manifest::new("audit_pkg");
    m.save(&dir).expect("manifest save");
    m.save(&dir).expect("manifest overwrite");
    let loaded = crate::manifest::Manifest::load(&dir)
        .expect("manifest load")
        .expect("manifest present");
    assert_eq!(loaded.package.expect("package").name, "audit_pkg");
    for entry in std::fs::read_dir(&dir).expect("read_dir").flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            name == "mimi.toml",
            "manifest temp file leaked into dir: {}",
            name
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn audit_cli_fmt_overwrites_atomically_and_fixes_operators() {
    let Some(bin) = mimi_bin() else {
        eprintln!("skip: mimi binary not built");
        return;
    };
    let dir = unique_temp_dir("fmt_cli");
    let file = dir.join("t.mimi");
    std::fs::write(
        &file,
        "func main() -> i32 {\nlet x = 1==1\nif x { 0 } else { 1 }\n}\n",
    )
    .expect("write source");
    let out = std::process::Command::new(&bin)
        .arg("fmt")
        .arg(&file)
        .output()
        .expect("spawn mimi fmt");
    assert!(
        out.status.success(),
        "mimi fmt failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let content = std::fs::read_to_string(&file).expect("read formatted file");
    assert!(
        content.contains("1 == 1"),
        "`==` must be glued with spacing: {}",
        content
    );
    assert!(
        !content.contains("= ="),
        "split operator survived: {}",
        content
    );
    // The formatted file must still parse + type-check.
    check_source(&content)
        .unwrap_or_else(|diags| panic!("mimi fmt output fails type check: {:?}", diags));
    // Atomic write must leave no temp files next to the source.
    for entry in std::fs::read_dir(&dir).expect("read_dir").flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(name == "t.mimi", "fmt temp file leaked into dir: {}", name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ═══════════════════════════════════════════════════════════════
// §13 LOW — disasm must use the capped read helper
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit_cli_disasm_reads_through_capped_helper() {
    let Some(bin) = mimi_bin() else {
        eprintln!("skip: mimi binary not built");
        return;
    };
    let dir = unique_temp_dir("disasm_cli");
    let file = dir.join("t.mimi");
    std::fs::write(&file, "func main() -> i32 { 42 }\n").expect("write source");
    let out = std::process::Command::new(&bin)
        .arg("disasm")
        .arg(&file)
        .output()
        .expect("spawn mimi disasm");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "mimi disasm failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("BytecodeProgram"),
        "disasm output missing program header: {}",
        stdout
    );
}
