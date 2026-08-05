//! Wave-1 audit-fix regression tests — io.
//! Findings: devdocs/full-audit-2026-08-05.md §8 (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via
//! compile_and_run). Owned emitter: src/codegen/builtins/io.rs.
use super::*;

/// Local exec harness. The shared `compile_and_run` discards stdout on
/// non-zero exits and cannot control stdin; several Wave-1 io fixes need
/// exactly that (assert traps with observable stdout, input() with a known
/// stdin). Mirrors `link_and_run_module` in tests/mod.rs using the same
/// pub(crate) building blocks (compile_only + cached_runtime_lib + cc).
fn io_compile_link_exec(
    src: &str,
    stdin_bytes: Option<&[u8]>,
) -> Result<(i32, String, String), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let obj = compile_only(src)?;
    let runtime_lib = cached_runtime_lib()?;
    let tmp_dir = std::env::temp_dir().join(format!("mimi_io_fix_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir: {}", e))?;
    let bin = tmp_dir.join("test");

    let mut cc = Command::new("cc");
    cc.arg("-no-pie");
    for flag in linker_flag() {
        cc.arg(flag);
    }
    cc.arg(&obj)
        .arg(&runtime_lib)
        .arg("-o")
        .arg(&bin)
        .arg("-lpthread")
        .arg("-ldl")
        .arg("-lm");
    let status = cc.status().map_err(|e| format!("cc: {}", e))?;
    if !status.success() {
        if let Some(parent) = obj.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(format!("link failed: {:?}", status.code()));
    }

    let mut cmd = Command::new(&bin);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    match stdin_bytes {
        Some(_) => {
            cmd.stdin(Stdio::piped());
        }
        None => {
            cmd.stdin(Stdio::null());
        }
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {}", e))?;
    if let Some(bytes) = stdin_bytes {
        if let Some(mut si) = child.stdin.take() {
            si.write_all(bytes)
                .map_err(|e| format!("stdin write: {}", e))?;
        } // dropped here → EOF after the fed bytes
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait: {}", e))?;

    if let Some(parent) = obj.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

// ── FIX 1 [CRITICAL]: eprintln must write to stderr, not stdout ──────

#[test]
fn io_fix_eprintln_not_on_stdout() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    eprintln("E_MARKER_731")
    println("OUT_MARKER_732")
    0
}
"#;
    // VM reference: eprintln goes to the real stderr, never captured stdout.
    let (_val, vm_stdout) = run_source_with_stdout(src);
    assert!(
        !vm_stdout.contains("E_MARKER_731"),
        "VM captured stdout must not contain the eprintln marker: {:?}",
        vm_stdout
    );
    assert!(vm_stdout.contains("OUT_MARKER_732"));

    // Codegen: compile_and_run returns the binary's STDOUT only. Before the
    // fix, eprintln emitted printf(...) → the marker leaked into stdout.
    let cg_stdout = compile_and_run(src).expect("codegen eprintln run failed");
    assert!(
        !cg_stdout.contains("E_MARKER_731"),
        "codegen stdout must NOT contain the eprintln marker: {:?}",
        cg_stdout
    );
    assert!(
        cg_stdout.contains("OUT_MARKER_732"),
        "println output missing: {:?}",
        cg_stdout
    );

    // Full stream split: marker must land on stderr.
    let (code, exec_stdout, exec_stderr) = io_compile_link_exec(src, None).expect("exec failed");
    assert_eq!(code, 0, "program should exit 0");
    assert!(
        !exec_stdout.contains("E_MARKER_731"),
        "exec stdout must not contain the eprintln marker: {:?}",
        exec_stdout
    );
    assert!(
        exec_stderr.contains("E_MARKER_731"),
        "exec stderr must contain the eprintln marker: {:?}",
        exec_stderr
    );
}

// ── FIX 2 [CRITICAL]: input() must check fgets; EOF → empty string ───
// TODO(#audit-wave2): full Result<string,string> shape alignment with the
// VM is decided for Wave 2; here we pin the codegen memory-safety behavior.

#[test]
fn io_fix_input_compiles_clean() {
    // Codegen path assertion that needs no stdin control: input() must
    // compile through codegen without error.
    let src = r#"
func main() -> i32 {
    let s = input()
    println(len(s))
    0
}
"#;
    compile_only(src).expect("input() codegen path must compile");
}

#[test]
fn io_fix_input_eof_empty_and_line_trim() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    let s = input()
    println(len(s))
    println(s)
    0
}
"#;
    // EOF (stdin = /dev/null): BEFORE the fix fgets returned NULL and the
    // code strlen'd a fresh uninitialized 4096-byte buffer (garbage). Now a
    // deterministic empty string.
    let (code, stdout, _stderr) = io_compile_link_exec(src, None).expect("exec failed");
    assert_eq!(code, 0, "input() EOF path should exit 0");
    assert_eq!(
        stdout, "0\n\n",
        "EOF input() must be the empty string (len 0), got {:?}",
        stdout
    );

    // Fed line: trailing whitespace stripped (VM uses trim_end).
    let (code2, stdout2, _stderr2) =
        io_compile_link_exec(src, Some(b"hello there \n")).expect("exec failed");
    assert_eq!(code2, 0);
    assert_eq!(
        stdout2, "11\nhello there\n",
        "line input must be right-trimmed, got {:?}",
        stdout2
    );
}

// ── FIX 2b [CRITICAL]: input() shape — VM must return `string`, not Result ──
// §8-#86 (audit-2026-08-05): the checker types `input()` as `string` and
// std/io.mimi's `input_line` consumes it with "" as the EOF sentinel. The VM
// builtin returned an Ok/Err variant, so `line == ""` compared a variant
// against a string → never fired → input_line always Ok on EOF. The codegen
// backend already returned a bare string; the VM now aligns.

#[test]
fn io_fix_input_vm_eof_is_empty_string() {
    // VM path with the test runner's stdin (non-tty → read_line returns
    // 0 bytes → EOF → ""). Pre-fix the value was an Ok("") variant and the
    // `s == ""` comparison fell to the else arm.
    let src = r#"
func main() -> i32 {
    let s = input()
    if s == "" {
        1
    } else {
        2
    }
}
"#;
    let val = run_source(src);
    assert_eq!(
        val,
        interp::Value::Int(1),
        "EOF input() must yield the empty string sentinel"
    );
}

#[test]
fn io_fix_input_line_vm_eof_returns_err() {
    // std/io.mimi `input_line` wraps input() with the "" sentinel → Err on
    // EOF. Pre-fix the VM gave input() a Result shape, so `line == ""`
    // compared variant vs string and input_line returned Ok("") on EOF.
    let src = r#"
func main() -> i32 {
    let r = input_line()
    if r.is_err() {
        1
    } else {
        2
    }
}
"#;
    let val = run_with_stdlib("io.mimi", src);
    assert_eq!(
        val,
        interp::Value::Int(1),
        "EOF input_line() must be Err via the empty-string sentinel"
    );
}

// ── FIX 3 [HIGH]: assert(cond, msg) — message is data, not a format ──

#[test]
fn io_fix_assert_message_verbatim_and_traps() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    assert(false, "100% done %s %d")
    0
}
"#;
    // VM reference: message surfaces verbatim inside the trap error.
    let vm_res = run_source_result(src);
    let vm_err = vm_res.expect_err("VM assert(false, …) must fail");
    assert!(
        vm_err.contains("100% done %s %d"),
        "VM trap must carry the verbatim message, got {:?}",
        vm_err
    );

    // Codegen: traps (exit 1) and prints the message VERBATIM — before the
    // fix the user message was the printf format string ("100% done %s %d"
    // parsed %specifiers against garbage args → format-string UB).
    let (code, stdout, _stderr) = io_compile_link_exec(src, None).expect("exec failed");
    assert_eq!(code, 1, "assert(false) must exit 1");
    assert!(
        stdout.contains("100% done %s %d"),
        "assert message must be printed verbatim (was format-string UB), got {:?}",
        stdout
    );

    // Passing assert with a %–laden message must not disturb control flow.
    let ok_src = r#"
func main() -> i32 {
    assert(true, "100% fine")
    println("ALIVE")
    0
}
"#;
    let ok_out = compile_and_run(ok_src).expect("assert(true) run failed");
    assert!(ok_out.contains("ALIVE"));
}

// ── FIX 4 [HIGH]: sized display assembly (no fixed-buffer overflow) ──

#[test]
fn io_fix_deep_list_of_list_tuples_beyond_8k() {
    if !can_link() {
        return;
    }
    // 2 rows × 500 tuples: each row renders ~5.5KB (overflows the old 4096
    // inner buffer) and the whole rendering is ~11KB (overflows the old
    // 8192 outer buffer of emit_list_list_product_tuple_to_string).
    let mut rows: Vec<String> = Vec::new();
    for r in 0..2i64 {
        let tups: Vec<String> = (0..500i64)
            .map(|i| format!("({}, {})", r * 1000 + i, i))
            .collect();
        rows.push(format!("[{}]", tups.join(", ")));
    }
    let src = format!(
        r#"
func main() -> i32 {{
    let xss: List<List<(i32, i32)>> = [{}]
    println(xss)
    0
}}
"#,
        rows.join(", ")
    );
    let (_vm_val, vm_stdout) = run_source_with_stdout(&src);
    let cg_stdout = compile_and_run(&src).expect("codegen deep list run failed");
    assert!(
        cg_stdout.len() > 8192,
        "rendering must exceed the old 8KB fixed buffer (len={})",
        cg_stdout.len()
    );
    assert_eq!(
        cg_stdout,
        vm_stdout,
        "deep nested list-of-list-of-tuples rendering diverges (len vm={} cg={})",
        vm_stdout.len(),
        cg_stdout.len()
    );
}

#[test]
fn io_fix_option_result_json_wrap_long_payload() {
    if !can_link() {
        return;
    }
    // The converted some_inner/ok_wrap / list_opt_list_wrap snippets must
    // not truncate payloads beyond the old fixed 1024-byte snprintf wraps.
    // Shape 1: List<Option<List<(i32,i32)>>> — the Some payload is the whole
    // inner list JSON (~3.5KB for 300 tuples > old 1024 wrap).
    let tups: Vec<String> = (0..300i64).map(|i| format!("({}, {})", i, i + 1)).collect();
    // Shape 2: List<Result<Option<(i32,i32)>, string>> exercises the
    // {"Some":[%s]} / {"Ok":[%s]} wrap chain (emit_list_result_option_product_to_json).
    let src = format!(
        r#"
func main() -> i32 {{
    let big: List<(i32, i32)> = [{}]
    let ys: List<Option<List<(i32, i32)>>> = [Some(big), None]
    println(to_json(ys))
    let zs: List<Result<Option<(i32, i32)>, string>> = [Ok(Some((1, 2))), Ok(None), Err("e")]
    println(to_json(zs))
    0
}}
"#,
        tups.join(", ")
    );
    let (_vm_val, vm_stdout) = run_source_with_stdout(&src);
    let cg_stdout = compile_and_run(&src).expect("codegen option/result to_json run failed");
    let first_line = cg_stdout.lines().next().unwrap_or("");
    assert!(
        first_line.len() > 1024,
        "payload must exceed the old 1024-byte wraps (len={})",
        first_line.len()
    );
    assert_eq!(
        cg_stdout,
        vm_stdout,
        "Option/Result JSON wraps diverge\nvm: {:?}\ncg: {:?}",
        vm_stdout.chars().take(200).collect::<String>(),
        cg_stdout.chars().take(200).collect::<String>()
    );
}

// ── FIX 5 [HIGH]: list display must dispatch on element type ─────────

#[test]
fn io_fix_list_f64_display() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    println([1.5])
    println([2.25, 3.5])
    0
}
"#;
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    let cg_stdout = compile_and_run(src).expect("codegen float-list run failed");
    assert_eq!(cg_stdout.trim(), "[1.5]\n[2.25, 3.5]");
    assert_eq!(
        cg_stdout, vm_stdout,
        "List<f64> display diverges: vm={:?} cg={:?}",
        vm_stdout, cg_stdout
    );
}

#[test]
fn io_fix_list_i64_display() {
    if !can_link() {
        return;
    }
    // 4294967296 > i32::MAX → literal infers as i64
    // (src/core/infer/literal.rs value-aware typing). The old i32 fallback
    // truncated it to garbage. The second line forces negative i64 values.
    let src = r#"
func main() -> i32 {
    println([4294967296])
    println([10000000000, -10000000000])
    0
}
"#;
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    let cg_stdout = compile_and_run(src).expect("codegen i64-list run failed");
    assert_eq!(
        cg_stdout.trim(),
        "[4294967296]\n[10000000000, -10000000000]"
    );
    assert_eq!(
        cg_stdout, vm_stdout,
        "List<i64> display diverges: vm={:?} cg={:?}",
        vm_stdout, cg_stdout
    );
}

#[test]
fn io_fix_list_bool_display() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    println([true, false, true])
    0
}
"#;
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    let cg_stdout = compile_and_run(src).expect("codegen bool-list run failed");
    assert_eq!(cg_stdout.trim(), "[true, false, true]");
    assert_eq!(
        cg_stdout, vm_stdout,
        "List<bool> display diverges: vm={:?} cg={:?}",
        vm_stdout, cg_stdout
    );
}

// ── FIX 6 [HIGH]: Map/Set display routing — full outer type match ────

#[test]
fn io_fix_list_of_map_string_display() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    let xs = from_json::<List<Map<string, string>>>("[{\"a\":\"hi\"},{\"b\":\"yo\"}]")
    println(xs)
    0
}
"#;
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    let cg_stdout = compile_and_run(src).expect("codegen List<Map> run failed");
    assert_eq!(cg_stdout.trim(), "[{\"a\":\"hi\"}, {\"b\":\"yo\"}]");
    assert_eq!(
        cg_stdout, vm_stdout,
        "List<Map<string,string>> display diverges: vm={:?} cg={:?}",
        vm_stdout, cg_stdout
    );
}

#[test]
fn io_fix_map_and_set_scalar_routing() {
    if !can_link() {
        return;
    }
    // Direct Map/Set scalar routing: value/element type decides the runtime
    // helper; the outer type name is matched as a whole (no substring
    // false-positives from nested type names).
    let src = r#"
func main() -> i32 {
    let ms = from_json::<Map<string, string>>("{\"k\":\"v\"}")
    let mf = from_json::<Map<string, f64>>("{\"pi\":3.25}")
    let mb = from_json::<Map<string, bool>>("{\"ok\":true}")
    println(ms)
    println(mb)
    println(mf)
    0
}
"#;
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    let cg_stdout = compile_and_run(src).expect("codegen Map routing run failed");
    assert_eq!(
        cg_stdout, vm_stdout,
        "Map scalar routing diverges: vm={:?} cg={:?}",
        vm_stdout, cg_stdout
    );
}

// ── FIX 7 [MEDIUM]: format() must substitute all args (no cap at 8) ──

#[test]
fn io_fix_format_more_than_eight_substitutions() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    let s = format("{} {} {} {} {} {} {} {} {} {}", 1, 2, 3, 4, 5, 6, 7, 8, 9, 10)
    println(s)
    0
}
"#;
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    assert_eq!(vm_stdout.trim(), "1 2 3 4 5 6 7 8 9 10");
    let cg_stdout = compile_and_run(src).expect("codegen format run failed");
    assert_eq!(
        cg_stdout.trim(),
        "1 2 3 4 5 6 7 8 9 10",
        "format() must replace all 10 placeholders (old cap was 8)"
    );
    assert_eq!(cg_stdout, vm_stdout, "format >8 args diverges");
}

#[test]
fn io_fix_format_few_and_zero_substitutions() {
    if !can_link() {
        return;
    }
    // Zero-arg and <8-arg shapes must keep the legacy behavior.
    let src = r#"
func main() -> i32 {
    println(format("plain"))
    println(format("a={} b={}", 1, "x"))
    println(format("leftover {} {}", 1))
    0
}
"#;
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    let cg_stdout = compile_and_run(src).expect("codegen format run failed");
    assert_eq!(cg_stdout, vm_stdout, "format edge shapes diverge");
    assert_eq!(cg_stdout.trim(), "plain\na=1 b=x\nleftover 1 {}");
}

// ── FIX 8 [MEDIUM]: println float — shortest round-trip, VM parity ───

#[test]
fn io_fix_println_float_full_precision() {
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    println(123456789.123456789)
    println(3.14)
    println(-1.0)
    println(0.000001)
    0
}
"#;
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    let cg_stdout = compile_and_run(src).expect("codegen float println run failed");
    // %g printed "1.23457e+08" (6 sig digits); Rust `{}` prints the full
    // shortest round-trip digits.
    assert!(
        cg_stdout.starts_with("123456789.12345679"),
        "expected full-precision first line, got {:?}",
        cg_stdout
    );
    assert_eq!(
        cg_stdout, vm_stdout,
        "float println diverges: vm={:?} cg={:?}",
        vm_stdout, cg_stdout
    );
}
