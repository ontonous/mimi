//! Wave-1 audit-fix regression tests — stdlib.
//! Findings: devdocs/full-audit-2026-08-05.md §13 (2026-08-05 full audit).
//! Wave-2 items: devdocs/wave2-battle-plan-2026-08-05.md (STDLIB package),
//! devdocs/wave1-review-2026-08-05.md §1.6 / §6.1.
//!
//! Discipline: each stdlib (.mimi) fix carries a regression test here.
//! Runtime behavior is exercised through the Bytecode VM via
//! `run_with_stdlib` (stdlib source + test source concatenated, same
//! inclusion pattern as stdlib_v02813.rs / audit_regression.rs); typing is
//! exercised through `check_source`.
//!
//! Wave-2 dual-backend discipline (wave1-review §6.1): this file's original
//! all-VM-only tests are exactly how the net `Ok(dangling string)` codegen
//! bug shipped — the broken backend had no watching test. Every test added
//! or updated in Wave-2 carries a `compile_and_run`-side (native codegen)
//! assertion where the stdlib surface is exercisable there; the remaining
//! legacy VM-only cases are either covered by an explicit dual companion
//! elsewhere in this file or blocked by a documented root-cause entry.
use super::*;

/// Read a std module source file (same resolution as `run_with_stdlib`).
fn audit2_stdlib_src(stdlib_name: &str) -> String {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(manifest.join("std").join(stdlib_name))
        .unwrap_or_else(|e| panic!("failed to read std/{}: {}", stdlib_name, e))
}

/// Codegen-side counterpart of `run_with_stdlib` (which is VM-only):
/// concatenate the std module source with the test source and feed the
/// combined program to `compile_and_run` (native execution). The stdlib
/// items are visible without `use` statements exactly as on the VM side,
/// so both backends see the identical program text.
fn audit2_compile_and_run_with_stdlib(stdlib_name: &str, src: &str) -> Result<String, String> {
    let combined = format!("{}\n{}", audit2_stdlib_src(stdlib_name), src);
    compile_and_run(&combined)
}

// ===== mymath.mimi — fix 1: gcd abs-normalizes, lcm overflow-safe + abs =====

#[test]
fn audit_stdlib_gcd_abs_normalized() {
    // gcd used to return a negative value when either argument was negative
    // (gcd(4, -2) == -2). The stdlib convention is the non-negative
    // representative.
    let src = r#"
func main() -> i32 {
    let a = gcd(4, -2)
    let b = gcd(-12, 8)
    let c = gcd(-17, -5)
    let d = gcd(0, -9)
    a * 1000 + b * 100 + c * 10 + d
}
"#;
    // 2, 4, 1, 9
    assert_eq!(
        run_with_stdlib("mymath.mimi", src),
        interp::Value::Int(2419)
    );

    let cg_src = r#"
func main() -> i32 {
    let a = gcd(4, -2)
    let b = gcd(-12, 8)
    let c = gcd(-17, -5)
    let d = gcd(0, -9)
    println(a * 1000 + b * 100 + c * 10 + d)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("mymath.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen gcd_abs scenario failed: {}", e));
    assert_eq!(out.trim(), "2419", "codegen must match normalized gcd");
}

#[test]
fn audit_stdlib_lcm_overflow_safe_and_abs() {
    // lcm(65536, 65536) overflowed i32 via the naive a*b/gcd form even
    // though the result (65536) fits; negatives produced negative results
    // (lcm(2, -4) == -4), violating the |a|*|b| identity.
    let src = r#"
func main() -> i32 {
    let a = lcm(65536, 65536)
    let b = lcm(2, -4)
    let c = lcm(-6, 8)
    let d = lcm(0, 5)
    let e = lcm(-3, -7)
    a + b + c + d + e
}
"#;
    // 65536 + 4 + 24 + 0 + 21
    assert_eq!(
        run_with_stdlib("mymath.mimi", src),
        interp::Value::Int(65585)
    );

    let cg_src = r#"
func main() -> i32 {
    let a = lcm(65536, 65536)
    let b = lcm(2, -4)
    let c = lcm(-6, 8)
    let d = lcm(0, 5)
    let e = lcm(-3, -7)
    println(a + b + c + d + e)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("mymath.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen lcm scenario failed: {}", e));
    assert_eq!(out.trim(), "65585", "codegen must match safe lcm");
}

// ===== mymath.mimi — batch5-04 P2-3: trait gcd/lcm methods abs-normalize ====

#[test]
fn audit_stdlib_gcd_lcm_trait_abs_normalized() {
    // The IntMath trait implementations used to disagree with the free
    // functions for negative inputs (4.gcd(-2) -> -2, etc.). They must now
    // go through the same abs-normalized free-function convention.
    let src = r#"
func main() -> i32 {
    let a: i32 = 4
    let b: i32 = -12
    let c: i32 = -17
    let d: i32 = 0
    let e: i32 = 2
    let f: i32 = -6
    println(a.gcd(-2))
    println(b.gcd(8))
    println(c.gcd(-5))
    println(d.gcd(-9))
    println(e.lcm(-4))
    println(f.lcm(8))
    0
}
"#;
    let combined = format!("{}\n{}", audit2_stdlib_src("mymath.mimi"), src);
    let stdout = run_source_with_stdout(&combined).1;
    assert_eq!(
        stdout.trim(),
        "2\n4\n1\n9\n4\n24",
        "trait gcd/lcm must match free-function abs semantics"
    );
    if can_link() {
        let out = audit2_compile_and_run_with_stdlib("mymath.mimi", src)
            .unwrap_or_else(|e| panic!("codegen trait gcd/lcm failed: {}", e));
        assert_eq!(out.trim(), "2\n4\n1\n9\n4\n24");
    }
}

#[test]
fn audit_stdlib_factorial_free_overflow_guard() {
    // The IntMath method caps at 12! and returns -1; the free function had
    // no guard and 13! overflows i32 (traps under checked arithmetic).
    let src = r#"
func main() -> i32 {
    let a = factorial(5)
    let b = factorial(12)
    let c = factorial(13)
    let d = factorial(-1)
    let mut ok = 0
    if a == 120 { ok = ok + 1 }
    if b == 479001600 { ok = ok + 1 }
    if c == -1 { ok = ok + 1 }
    if d == -1 { ok = ok + 1 }
    ok
}
"#;
    assert_eq!(run_with_stdlib("mymath.mimi", src), interp::Value::Int(4));

    let cg_src = r#"
func main() -> i32 {
    let a = factorial(5)
    let b = factorial(12)
    let c = factorial(13)
    let d = factorial(-1)
    let mut ok = 0
    if a == 120 { ok = ok + 1 }
    if b == 479001600 { ok = ok + 1 }
    if c == -1 { ok = ok + 1 }
    if d == -1 { ok = ok + 1 }
    println(ok)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("mymath.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen factorial guard failed: {}", e));
    assert_eq!(out.trim(), "4", "codegen must match factorial guards");
}

// ===== mymath.mimi — fix 3: try_pow_int overflow bounds for negative bases ==

#[test]
fn audit_stdlib_try_pow_int_negative_base_edges() {
    // Old check `result < MIN / base` for base < 0 was inverted: it rejected
    // every small positive result (try_pow_int(-2, 3) returned Err) and let
    // real overflow through. New bounds check both product directions.
    let src = r#"
func main() -> i32 {
    let mut score = 0
    let r1 = match try_pow_int(-2, 3) { Ok(v) => v, Err(_) => 999999 }
    if r1 == -8 { score = score + 1 }
    let r2 = match try_pow_int(-2, 31) { Ok(v) => v, Err(_) => 999999 }
    if r2 == -2147483648 { score = score + 10 }
    let r3 = match try_pow_int(-2, 32) { Ok(_) => 999999, Err(_) => -1 }
    if r3 == -1 { score = score + 100 }
    let r4 = match try_pow_int(2, 30) { Ok(v) => v, Err(_) => 999999 }
    if r4 == 1073741824 { score = score + 1000 }
    let r5 = match try_pow_int(7, 11) { Ok(v) => v, Err(_) => 999999 }
    if r5 == 1977326743 { score = score + 10000 }
    let r6 = match try_pow_int(7, 12) { Ok(_) => 999999, Err(_) => -1 }
    if r6 == -1 { score = score + 100000 }
    let r7 = match try_pow_int(-1, 3) { Ok(v) => v, Err(_) => 999999 }
    if r7 == -1 { score = score + 1000000 }
    score
}
"#;
    // (-2)^3 = -8, (-2)^31 = MIN, (-2)^32 overflow, 2^30, 7^11, 7^12
    // overflow, (-1)^3 = -1.
    assert_eq!(
        run_with_stdlib("mymath.mimi", src),
        interp::Value::Int(1111111)
    );

    let cg_src = r#"
func main() -> i32 {
    let mut score = 0
    let r1 = match try_pow_int(-2, 3) { Ok(v) => v, Err(_) => 999999 }
    if r1 == -8 { score = score + 1 }
    let r2 = match try_pow_int(-2, 31) { Ok(v) => v, Err(_) => 999999 }
    if r2 == -2147483648 { score = score + 10 }
    let r3 = match try_pow_int(-2, 32) { Ok(_) => 999999, Err(_) => -1 }
    if r3 == -1 { score = score + 100 }
    let r4 = match try_pow_int(2, 30) { Ok(v) => v, Err(_) => 999999 }
    if r4 == 1073741824 { score = score + 1000 }
    let r5 = match try_pow_int(7, 11) { Ok(v) => v, Err(_) => 999999 }
    if r5 == 1977326743 { score = score + 10000 }
    let r6 = match try_pow_int(7, 12) { Ok(_) => 999999, Err(_) => -1 }
    if r6 == -1 { score = score + 100000 }
    let r7 = match try_pow_int(-1, 3) { Ok(v) => v, Err(_) => 999999 }
    if r7 == -1 { score = score + 1000000 }
    println(score)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("mymath.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen try_pow_int negative base failed: {}", e));
    assert_eq!(
        out.trim(),
        "1111111",
        "codegen must match negative-base pow guards"
    );
}

// ===== mymath.mimi — fix 4: random_exponential guards lambda <= 0 ===========

#[test]
fn audit_stdlib_random_exponential_invalid_lambda_sentinel() {
    // λ <= 0 used to divide by zero (trap). Now returns the -1.0 sentinel,
    // which is never a valid Exp sample (samples are >= 0).
    let src = r#"
func main() -> i32 {
    let z = random_exponential(0.0)
    let n = random_exponential(-3.5)
    let mut ok = 0
    if z == -1.0 { ok = ok + 1 }
    if n == -1.0 { ok = ok + 1 }
    ok
}
"#;
    assert_eq!(run_with_stdlib("mymath.mimi", src), interp::Value::Int(2));

    let cg_src = r#"
func main() -> i32 {
    let z = random_exponential(0.0)
    let n = random_exponential(-3.5)
    let mut ok = 0
    if z == -1.0 { ok = ok + 1 }
    if n == -1.0 { ok = ok + 1 }
    println(ok)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("mymath.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen random_exponential sentinel failed: {}", e));
    assert_eq!(
        out.trim(),
        "2",
        "codegen must match invalid-lambda sentinel"
    );
}

// ===== strings.mimi — fix 5: trim_left/trim_right strip the whitespace set ==
// Wave-2 (§1.6, closed 0.36.79): the private is_ws_char helper was inlined
// into trim_left/trim_right because the std module loader carries only pub
// items across `use std::strings` — private helpers are invisible to
// consumers while the pub bodies calling them are not (E0401). The same
// mechanism was later applied to random.mimi's remove_at
// (`random_remove_ith`, 0.36.65). Dual-backend guard:
// audit2_std_trim_left_right_dual below.

// VM-only companion of audit2_std_trim_left_right_dual (which carries the
// codegen side). Kept as a focused whitespace-set regression.
#[test]
fn audit_stdlib_trim_left_right_whitespace_set() {
    // trim_left/trim_right only stripped " " while the docs (and trim())
    // cover space/tab/newline/CR.
    let src = r#"
func main() -> string {
    let a = trim_left("\t\n hello \t")
    let b = trim_right("\t hello \t\n ")
    let c = trim_left("   a b ")
    let d = trim_right("a b   ")
    a + "|" + b + "|" + c + "|" + d
}
"#;
    assert_eq!(
        run_with_stdlib("strings.mimi", src),
        interp::Value::String(Arc::new("hello \t|\t hello|a b |a b".to_string()))
    );
}

// ===== strings.mimi — fix 6: words() drops empty tokens =====================

#[test]
fn audit_stdlib_reverse_number_overflow_safe() {
    // reverse_number(2147483647) must not overflow i32; it returns -1 as
    // the documented sentinel for unrepresentable reversed values.
    let src = r#"
func main() -> i32 {
    let mut ok = 0
    if reverse_number(1234) == 4321 { ok = ok + 1 }
    if reverse_number(2147483647) == -1 { ok = ok + 1 }
    if reverse_number(-2147483647) == -1 { ok = ok + 1 }
    ok
}
"#;
    assert_eq!(run_with_stdlib("mymath.mimi", src), interp::Value::Int(3));

    let cg_src = r#"
func main() -> i32 {
    let mut ok = 0
    if reverse_number(1234) == 4321 { ok = ok + 1 }
    if reverse_number(2147483647) == -1 { ok = ok + 1 }
    if reverse_number(-2147483647) == -1 { ok = ok + 1 }
    println(ok)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("mymath.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen reverse_number overflow guard failed: {}", e));
    assert_eq!(out.trim(), "3", "codegen must match reverse_number guard");
}

#[test]
fn audit_stdlib_words_filters_empty_tokens() {
    // "a  b" split by " " yields ["a", "", "b"]; the empty token made
    // words()/count_words() report 3 words.
    let src = r#"
func main() -> i32 {
    let w = words("a  b")
    let n = count_words("  hello   world  ")
    let w2 = words("  hello   world  ")
    let mut ok = 0
    if len(w) == 2 { ok = ok + 1 }
    if w[0] == "a" { ok = ok + 1 }
    if w[1] == "b" { ok = ok + 1 }
    if n == 2 { ok = ok + 1 }
    if len(w2) == 2 { ok = ok + 1 }
    ok
}
"#;
    assert_eq!(run_with_stdlib("strings.mimi", src), interp::Value::Int(5));

    let cg_src = r#"
func main() -> i32 {
    let w = words("a  b")
    let n = count_words("  hello   world  ")
    let w2 = words("  hello   world  ")
    let mut ok = 0
    if len(w) == 2 { ok = ok + 1 }
    if w[0] == "a" { ok = ok + 1 }
    if w[1] == "b" { ok = ok + 1 }
    if n == 2 { ok = ok + 1 }
    if len(w2) == 2 { ok = ok + 1 }
    println(ok)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("strings.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen words empty-token guard failed: {}", e));
    assert_eq!(
        out.trim(),
        "5",
        "codegen must match words empty-token filtering"
    );
}

// ===== strings.mimi/text.mimi — negative repeat/indent guards =============

#[test]
fn audit_stdlib_repeat_indent_non_positive_guards() {
    let src = r#"
func main() -> i32 {
    let mut ok = 0
    if repeat("ab", 0) == "" { ok = ok + 1 }
    if repeat("ab", -2) == "" { ok = ok + 1 }
    if indent("a", 0) == "a" { ok = ok + 1 }
    if indent("a", -1) == "a" { ok = ok + 1 }
    ok
}
"#;
    assert_eq!(run_with_stdlib("strings.mimi", src), interp::Value::Int(4));

    let cg_src = r#"
func main() -> i32 {
    let mut ok = 0
    if repeat("ab", 0) == "" { ok = ok + 1 }
    if repeat("ab", -2) == "" { ok = ok + 1 }
    if indent("a", 0) == "a" { ok = ok + 1 }
    if indent("a", -1) == "a" { ok = ok + 1 }
    println(ok)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("strings.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen repeat/indent guard failed: {}", e));
    assert_eq!(out.trim(), "4", "codegen must match repeat/indent guard");
}

// ===== collections.mimi — fix 7: take/drop_n negative-n guard ===============

#[test]
fn audit_stdlib_take_drop_negative_n_guard() {
    // take(xs, -1) used to wrap through slice semantics (all-but-last);
    // now: take returns [] and drop_n returns the full list for n <= 0.
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3, 4]
    let mut ok = 0
    if len(take(xs, -1)) == 0 { ok = ok + 1 }
    let dn = drop_n(xs, -1)
    if len(dn) == 4 { ok = ok + 1 }
    if dn[0] == 1 { ok = ok + 1 }
    if dn[3] == 4 { ok = ok + 1 }
    if len(take(xs, 0)) == 0 { ok = ok + 1 }
    if len(drop_n(xs, 0)) == 4 { ok = ok + 1 }
    let tp = take(xs, 2)
    if len(tp) == 2 { ok = ok + 1 }
    if tp[1] == 2 { ok = ok + 1 }
    let dp = drop_n(xs, 1)
    if len(dp) == 3 { ok = ok + 1 }
    if dp[0] == 2 { ok = ok + 1 }
    if len(xs.take(-1)) == 0 { ok = ok + 1 }
    if len(xs.drop_n(-1)) == 4 { ok = ok + 1 }
    ok
}
"#;
    assert_eq!(
        run_with_stdlib("collections.mimi", src),
        interp::Value::Int(12)
    );

    let cg_src = r#"
func main() -> i32 {
    let xs = [1, 2, 3, 4]
    let mut ok = 0
    if len(take(xs, -1)) == 0 { ok = ok + 1 }
    let dn = drop_n(xs, -1)
    if len(dn) == 4 { ok = ok + 1 }
    if dn[0] == 1 { ok = ok + 1 }
    if dn[3] == 4 { ok = ok + 1 }
    if len(take(xs, 0)) == 0 { ok = ok + 1 }
    if len(drop_n(xs, 0)) == 4 { ok = ok + 1 }
    let tp = take(xs, 2)
    if len(tp) == 2 { ok = ok + 1 }
    if tp[1] == 2 { ok = ok + 1 }
    let dp = drop_n(xs, 1)
    if len(dp) == 3 { ok = ok + 1 }
    if dp[0] == 2 { ok = ok + 1 }
    if len(xs.take(-1)) == 0 { ok = ok + 1 }
    if len(xs.drop_n(-1)) == 4 { ok = ok + 1 }
    println(ok)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("collections.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen take/drop negative-n guard failed: {}", e));
    assert_eq!(out.trim(), "12", "codegen must match take/drop guards");
}

// ===== fs.mimi — fix 8: file_size returns bytes, not characters =============

#[test]
fn audit_stdlib_file_size_counts_bytes_not_chars() {
    // "héllo" is 5 chars but 6 UTF-8 bytes (é = 2 bytes); file_size used
    // len(content) which counts characters. Now backed by file_stat.
    let dir = std::env::temp_dir().join(format!("mimi_audit_fs_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("multibyte.txt");
    std::fs::write(&path, "héllo").expect("write test file");
    let src = format!(
        r#"
func main() -> i32 {{
    match file_size("{}") {{
        Ok(n) => n as i32
        Err(_) => -1
    }}
}}
"#,
        path.display()
    );
    let v = run_with_stdlib("fs.mimi", &src);
    assert_eq!(v, interp::Value::Int(6), "byte size of 'héllo' is 6");

    let cg_src = format!(
        r#"
func main() -> i32 {{
    match file_size("{}") {{
        Ok(n) => println(n)
        Err(_) => println(-1)
    }}
    0
}}
"#,
        path.display()
    );
    let out = audit2_compile_and_run_with_stdlib("fs.mimi", &cg_src)
        .unwrap_or_else(|e| panic!("codegen file_size bytes failed: {}", e));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.trim(), "6", "codegen must also count UTF-8 bytes");
}

#[test]
fn audit_stdlib_file_size_missing_file_is_err() {
    let src = r#"
func main() -> i32 {
    match file_size("/nonexistent_path_audit_stdlib_xyz") {
        Ok(_) => 999
        Err(_) => 1
    }
}
"#;
    assert_eq!(run_with_stdlib("fs.mimi", src), interp::Value::Int(1));

    let cg_src = r#"
func main() -> i32 {
    match file_size("/nonexistent_path_audit_stdlib_xyz") {
        Ok(_) => println(999)
        Err(_) => println(1)
    }
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("fs.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen file_size missing failed: {}", e));
    assert_eq!(out.trim(), "1", "codegen must surface missing-file error");
}

// ===== net.mimi — fix 9: recv EOF / empty body are success, not error =======
// Wave-2 (ruling 1, wave1-review §1.4/§6.1): the stdlib guard removal
// STAYS; the codegen builtins must surface real errors instead of wrapping
// NULL as Ok(dangling string). The tests below add the codegen side the
// original VM-only suite lacked.

/// Bind an ephemeral TCP listener whose one accepted connection is closed
/// immediately (peer sees EOF on recv). Returns (port, join handle).
fn audit2_eof_server() -> (i32, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port() as i32;
    let handle = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            drop(stream); // close → EOF for the peer
        }
    });
    (port, handle)
}

#[test]
fn audit_stdlib_tcp_recv_eof_is_ok_empty() {
    // Bind an ephemeral port; accept the Mimi client and close immediately so
    // the client's recv sees EOF (n == 0). The recv builtin maps that to an
    // empty string; the stdlib must surface it as Ok(""), not Err(RecvFailed).
    // Blocking accept mirrors the reliable pattern in src/tests/net.rs.
    //
    // VM side (reference semantics).
    let (port, server) = audit2_eof_server();
    let src = format!(
        r#"
func main() -> i32 {{
    let fd = tcp_socket()
    if fd < 0 {{ return 1 }}
    let ret = connect(fd, "127.0.0.1", {})
    if ret < 0 {{ close_fd(fd); return 2 }}
    let r = tcp_recv(fd, 64)
    close_fd(fd)
    match r {{
        Ok(data) => if len(data) == 0 {{ 100 }} else {{ 101 }}
        Err(_) => 200
    }}
}}
"#,
        port
    );
    let v = run_with_stdlib("net.mimi", &src);
    let _ = server.join();
    assert_eq!(
        v,
        interp::Value::Int(100),
        "EOF must surface as Ok(\"\") — old code returned Err(RecvFailed)"
    );

    // Codegen side (Wave-2 §6.1 discipline — the Ok(dangling string) bug
    // shipped because this side did not exist). Same scenario via
    // compile_and_run + native execution. NOTE (coordination): runtime
    // mimi_recv conflates EOF (n==0) and error (n<0) into NULL
    // (runtime/net.rs mimi_recv); the adjudicated shape keeps EOF == Ok("").
    // If a future NULL->error fix turns EOF into an error, mimi_recv must
    // first learn to distinguish EOF from error — a red test here would
    // point at exactly that gap.
    if !can_link() {
        return;
    }
    let (cg_port, cg_server) = audit2_eof_server();
    let cg_src = format!(
        r#"
func main() -> i32 {{
    let fd = tcp_socket()
    if fd < 0 {{ return 1 }}
    let ret = connect(fd, "127.0.0.1", {})
    if ret < 0 {{ close_fd(fd); return 2 }}
    let r = tcp_recv(fd, 64)
    close_fd(fd)
    match r {{
        Ok(data) => if len(data) == 0 {{ println(100) }} else {{ println(101) }}
        Err(_) => println(200)
    }}
    0
}}
"#,
        cg_port
    );
    let out = audit2_compile_and_run_with_stdlib("net.mimi", &cg_src);
    let _ = cg_server.join();
    let out = out.unwrap_or_else(|e| panic!("codegen EOF scenario failed: {}", e));
    assert_eq!(
        out.trim(),
        "100",
        "EOF must surface as Ok(\"\") on codegen too"
    );
}

// ===== Wave-2: net stdlib surface, codegen side (ruling 1) ==================

#[test]
fn audit2_std_net_tcp_roundtrip_dual() {
    // Success-path L1 guard over the std/net.mimi wrapper surface
    // (tcp_socket/tcp_connect/tcp_send/tcp_recv): a one-shot server reads
    // the client's message and replies with a fixed payload. Both backends
    // must observe the same result. No timing dependency: the recv success
    // path (n > 0) never touches the NULL->Err question.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port() as i32;
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 5];
            let _ = stream.read_exact(&mut buf); // consume "hello"
            let _ = stream.write_all(b"ping-pong");
            let _ = stream.flush();
        }
    });
    let src = format!(
        r#"
func handle(fd: i64) -> i32 {{
    let _sent = tcp_send(fd, "hello")
    let r = tcp_recv(fd, 64)
    close_fd(fd)
    match r {{
        Ok(data) => if data == "ping-pong" {{ 100 }} else {{ 101 }}
        Err(_) => 200
    }}
}}
func main() -> i32 {{
    let cr = tcp_connect("127.0.0.1", {})
    let code = match cr {{
        Ok(fd) => handle(fd)
        Err(_) => 2
    }}
    println(code)
    code
}}
"#,
        port
    );
    // VM side.
    let v = run_with_stdlib("net.mimi", &src);
    assert_eq!(
        v,
        interp::Value::Int(100),
        "VM roundtrip must receive the server payload"
    );
    // Codegen side (fresh server on a fresh port).
    if !can_link() {
        let _ = server.join();
        return;
    }
    let _ = server.join();
    let listener2 = std::net::TcpListener::bind("127.0.0.1:0").expect("bind second port");
    let port2 = listener2.local_addr().expect("local addr").port() as i32;
    let server2 = std::thread::spawn(move || {
        use std::io::{Read, Write};
        if let Ok((mut stream, _)) = listener2.accept() {
            let mut buf = [0u8; 5];
            let _ = stream.read_exact(&mut buf);
            let _ = stream.write_all(b"ping-pong");
            let _ = stream.flush();
        }
    });
    let src2 = format!(
        r#"
func handle(fd: i64) -> i32 {{
    let _sent = tcp_send(fd, "hello")
    let r = tcp_recv(fd, 64)
    close_fd(fd)
    match r {{
        Ok(data) => if data == "ping-pong" {{ 100 }} else {{ 101 }}
        Err(_) => 200
    }}
}}
func main() -> i32 {{
    let cr = tcp_connect("127.0.0.1", {})
    let code = match cr {{
        Ok(fd) => handle(fd)
        Err(_) => 2
    }}
    println(code)
    0
}}
"#,
        port2
    );
    let out = audit2_compile_and_run_with_stdlib("net.mimi", &src2);
    let _ = server2.join();
    let out = out.unwrap_or_else(|e| panic!("codegen roundtrip failed: {}", e));
    assert_eq!(out.trim(), "100", "codegen roundtrip must match the VM");
}

#[test]
fn audit2_std_net_recv_on_dead_fd_error_dual() {
    // recv on a closed fd: the VM traps (bytecode runtime error, E0800) —
    // the reference shape. Pre-fix codegen wrapped mimi_recv's NULL as
    // Ok(dangling string) (red line §1.4). Adjudicated (battle-plan ruling
    // 1): compile_recv must surface NULL as an error, matching the VM.
    let src = r#"
func main() -> i32 {
    let fd = tcp_socket()
    if fd < 0 { return 1 }
    close_fd(fd)
    let r = tcp_recv(fd, 64)
    match r {
        Ok(_) => 100
        Err(_) => 200
    }
}
"#;
    // VM side: must trap, never return Ok.
    let combined = format!("{}\n{}", audit2_stdlib_src("net.mimi"), src);
    let vm = run_source_result(&combined);
    assert!(
        vm.is_err(),
        "VM recv-after-close must trap, got Ok({:?})",
        vm
    );

    // Codegen side. TIMING: asserts the ADJUDICATED shape; requires agent
    // BUILTINS's compile_recv NULL->error fix (codegen/builtins/network.rs).
    // Until that lands, codegen returns Ok("100") and this test is red —
    // deliberately: it watches the exact surface that shipped
    // Ok(dangling string). Do NOT weaken the assertion to match pre-fix
    // behavior.
    if !can_link() {
        return;
    }
    let cg_src = r#"
func main() -> i32 {
    let fd = tcp_socket()
    if fd < 0 { return 1 }
    close_fd(fd)
    let r = tcp_recv(fd, 64)
    match r {
        Ok(_) => println(100)
        Err(_) => println(200)
    }
    0
}
"#;
    let cg = audit2_compile_and_run_with_stdlib("net.mimi", cg_src);
    assert!(
        cg.is_err(),
        "codegen recv-after-close must surface an error like the VM, got Ok({:?})",
        cg
    );
}

#[test]
fn audit2_std_net_fetch_https_rejected_dual() {
    // fetch() on an https:// URL: the VM rejects it before any I/O (no TLS)
    // and traps — the reference shape. Pre-fix codegen substituted a NULL
    // from mimi_http_get with "" and returned Ok(""), swallowing the
    // failure (red line §1.4). Adjudicated: NULL must surface as an error.
    // Deterministic: the scheme check runs before DNS/connect, so no
    // network access happens on either backend.
    let vm_src = r#"
func main() -> i32 {
    let r = fetch("https://example.invalid/")
    match r {
        Ok(_) => 100
        Err(_) => 200
    }
}
"#;
    let combined = format!("{}\n{}", audit2_stdlib_src("net.mimi"), vm_src);
    let vm = run_source_result(&combined);
    assert!(
        vm.is_err(),
        "VM fetch(https://) must trap (no TLS), got Ok({:?})",
        vm
    );

    // Codegen side. TIMING: asserts the ADJUDICATED shape; requires agent
    // BUILTINS's compile_http_get NULL->error fix (replacing the NULL->""
    // select currently in compile_http_get). Red until that lands — see
    // audit2_std_net_recv_on_dead_fd_error_dual for the rationale.
    if !can_link() {
        return;
    }
    let cg_src = r#"
func main() -> i32 {
    let r = fetch("https://example.invalid/")
    match r {
        Ok(_) => println(100)
        Err(_) => println(200)
    }
    0
}
"#;
    let cg = audit2_compile_and_run_with_stdlib("net.mimi", cg_src);
    assert!(
        cg.is_err(),
        "codegen fetch(https://) must surface an error like the VM, got Ok({:?})",
        cg
    );
}

// ===== random.mimi — Wave-2 §1.6-mechanism fix: private remove_at inlined ==

#[test]
fn audit2_std_random_sample_shuffle_semantics_dual() {
    // Regression for the remove_at inlining: the private helper was
    // invisible across `use std::random` (loader carries only pub items,
    // same mechanism as strings.mimi is_ws_char / red line §1.6). Shuffle
    // must preserve the multiset of elements and random_sample(n) must
    // return n elements — RNG-independent assertions only.
    //
    // 0.36.65: the remove-at loop moved into the pub free helper
    // `random_remove_ith`, which also avoids the codegen stack-alloca
    // aliasing bug seen when the loop was inlined directly in generic impl
    // methods (the old code SIGSEGVed on native shuffle).
    let src = r#"
func xs_sum(xs: List<i32>) -> i32 {
    let mut s = 0
    for v in xs { s = s + v }
    s
}
func main() -> i32 {
    let xs = [10, 20, 30, 40, 50]
    let s = shuffle(xs)
    let p = random_sample(xs, 3)
    let mut ok = 0
    if len(s) == 5 { ok = ok + 1 }
    if xs_sum(s) == 150 { ok = ok + 1 }
    if len(p) == 3 { ok = ok + 1 }
    ok
}
"#;
    let v = run_with_stdlib("random.mimi", src);
    assert_eq!(v, interp::Value::Int(3));

    let cg_src = r#"
func xs_sum(xs: List<i32>) -> i32 {
    let mut s = 0
    for v in xs { s = s + v }
    s
}
func main() -> i32 {
    let xs = [10, 20, 30, 40, 50]
    let s = shuffle(xs)
    let p = random_sample(xs, 3)
    let mut ok = 0
    if len(s) == 5 { ok = ok + 1 }
    if xs_sum(s) == 150 { ok = ok + 1 }
    if len(p) == 3 { ok = ok + 1 }
    println(ok)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("random.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen random sample/shuffle failed: {}", e));
    assert_eq!(
        out.trim(),
        "3",
        "codegen must match VM random sample/shuffle"
    );
}

// ===== result.mimi — fix 10: map/map_result rebuild Err at Result<U, E> =====
// Wave-2 (battle-plan ruling 4): the original test wrote
// `Err(_) => score = score` inside match arms; bare assignment is not a
// parseable arm body ("unexpected token in pattern ="). Rewritten to legal
// expression-form arms preserving the INTENT (Err value preservation
// through map_result); the parser is not opened (PM territory).

#[test]
fn audit_stdlib_map_result_preserves_err_value() {
    // The Err branch used to return the original Result<T, E> where
    // Result<U, E> is declared. The Err payload must be carried into a
    // freshly-built Err at the target type.
    let vm_src = r#"
func main() -> i32 {
    let ok_r: Result<i32, string> = Ok(21)
    let mapped_ok = map_result(ok_r, fn(x: i32) -> string { to_string(x) })
    let err_r: Result<i32, string> = Err("boom")
    let mapped_err = map_result(err_r, fn(x: i32) -> string { to_string(x) })
    let ok_score = match mapped_ok {
        Ok(s) => if s == "21" { 1 } else { 0 }
        Err(_) => 0
    }
    let err_score = match mapped_err {
        Ok(_) => 0
        Err(e) => if e == "boom" { 10 } else { 0 }
    }
    ok_score + err_score
}
"#;
    assert_eq!(
        run_with_stdlib("result.mimi", vm_src),
        interp::Value::Int(11)
    );

    // Codegen side (Wave-2 §6.1 discipline): same program, native run.
    // The map_result<T,E,U> generic wrapper's closure-typed param has a
    // known legacy monomorphization gap (std/result.mimi comment), so the
    // codegen side exercises the BUILTIN map directly, with EXPLICIT result
    // type annotations — legacy needs them to reconstruct the string Err
    // payload (match.rs Q1 relies on the scrutinee's Result<T,E> AST type;
    // without the annotation the builtin-map return type is unknown and
    // `Err(e) => e == "boom"` fails "eq requires same types").
    if !can_link() {
        return;
    }
    let cg_src = r#"
func main() -> i32 {
    let ok_r: Result<i32, string> = Ok(21)
    let mapped_ok: Result<bool, string> = ok_r.map(fn(x: i32) -> bool { x == 21 })
    let err_r: Result<i32, string> = Err("boom")
    let mapped_err: Result<bool, string> = err_r.map(fn(x: i32) -> bool { x == 21 })
    let ok_score = match mapped_ok {
        Ok(v) => if v { 1 } else { 0 }
        Err(_) => 0
    }
    let err_score = match mapped_err {
        Ok(_) => 0
        Err(e) => if e == "boom" { 10 } else { 0 }
    }
    println(ok_score + err_score)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("result.mimi", cg_src);
    let out = out.unwrap_or_else(|e| panic!("codegen map_result scenario failed: {}", e));
    assert_eq!(out.trim(), "11", "codegen must preserve the Err payload");
}

#[test]
fn audit_stdlib_result_ext_map_method_rebuilds_err() {
    // Same fix through the ResultExt::map trait method on an Err value.
    let src = r#"
func main() -> i32 {
    let r: Result<i32, string> = Err("kaputt")
    let m = r.map(fn(x: i32) -> bool { x > 0 })
    match m {
        Ok(_) => 0
        Err(e) => if e == "kaputt" { 1 } else { 2 }
    }
}
"#;
    assert_eq!(run_with_stdlib("result.mimi", src), interp::Value::Int(1));

    let cg_src = r#"
func main() -> i32 {
    let r: Result<i32, string> = Err("kaputt")
    let m: Result<bool, string> = r.map(fn(x: i32) -> bool { x > 0 })
    match m {
        Ok(_) => println(0)
        Err(e) => if e == "kaputt" { println(1) } else { println(2) }
    }
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("result.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen ResultExt::map Err rebuild failed: {}", e));
    assert_eq!(out.trim(), "1", "codegen must preserve Err through .map()");
}

#[test]
fn audit_stdlib_result_module_typechecks_with_strict_map() {
    // The audit flagged the old Err-branch (`self` typed Result<T, E>
    // returned where Result<U, E> is declared) as a possible unification
    // hole [UNVERIFIED]. After the fix the module must still typecheck
    // under a use that instantiates U != T.
    //
    // Wave-2 (item 4): this test fails on the NEW resolved lowering —
    // "member generic binder … has no canonical instantiation" for all
    // ResultExt methods. TRUE BUG in core/ir/lower.rs (agent IR); the
    // result.mimi source stays semantically correct and must NOT be
    // rewritten to dodge it. This test flips green when IR lands.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stdlib_src = std::fs::read_to_string(manifest.join("std").join("result.mimi"))
        .expect("read std/result.mimi");
    let src = format!(
        r#"{}
func main() -> i32 {{
    let r: Result<i32, string> = Err("e")
    let m: Result<string, string> = map_result(r, fn(x: i32) -> string {{ to_string(x) }})
    match m {{
        Ok(_) => 0
        Err(_) => 1
    }}
}}
"#,
        stdlib_src
    );
    assert!(
        check_source(&src).is_ok(),
        "result.mimi with rebuilt-Err map must typecheck: {:?}",
        check_source(&src)
    );
}

// ===== strings.mimi — Wave-2 §1.6: inlined-helper dual guard ===============

#[test]
fn audit2_std_trim_left_right_dual() {
    // Dual-backend guard for the is_ws_char inlining (red line §1.6): the
    // loop+break form must not depend on short-circuit `&&` (the resolved
    // emitter is known to evaluate `&&` eagerly on some paths), and must
    // agree across backends.
    let src = r#"
func main() -> i32 {
    let a = trim_left("\t\n hello \t")
    let b = trim_right("\t hello \t\n ")
    let c = trim_left("   a b ")
    let d = trim_right("a b   ")
    let e = trim_left("   ")
    let f = trim_right("")
    println(a + "|" + b + "|" + c + "|" + d + "|" + e + "|" + f + "|end")
    0
}
"#;
    let combined = format!("{}\n{}", audit2_stdlib_src("strings.mimi"), src);
    // VM side (reference semantics).
    let (v, out) = run_source_with_stdout(&combined);
    assert_eq!(v, interp::Value::Int(0));
    assert_eq!(
        out.trim(),
        "hello \t|\t hello|a b |a b|||end",
        "VM trim_left/trim_right whitespace set"
    );
    // Codegen side.
    if !can_link() {
        return;
    }
    let cg = compile_and_run(&combined)
        .unwrap_or_else(|e| panic!("codegen trim scenario failed: {}", e));
    assert_eq!(
        cg.trim(),
        "hello \t|\t hello|a b |a b|||end",
        "codegen trim must match the VM"
    );
}

// ===== mymath.mimi — batch5 P1-39: overflow guards for large valid inputs =====

#[test]
fn audit_stdlib_mymath_large_input_overflow_guards() {
    let src = r#"
func main() -> i32 {
    let mut ok = 0
    if is_prime(2147483647) { ok = ok + 1 }
    if fibonacci(47) == -1 { ok = ok + 1 }
    if next_power_of_two(1073741825) == -1 { ok = ok + 1 }
    if mod_pow(2, 1000000, 1000000007) >= 0 { ok = ok + 1 }
    ok
}
"#;
    assert_eq!(
        run_with_stdlib("mymath.mimi", src),
        interp::Value::Int(4),
        "VM: large-input mymath helpers must not trap and must return sentinels"
    );

    let cg_src = r#"
func main() -> i32 {
    let mut ok = 0
    if is_prime(2147483647) { ok = ok + 1 }
    if fibonacci(47) == -1 { ok = ok + 1 }
    if next_power_of_two(1073741825) == -1 { ok = ok + 1 }
    if mod_pow(2, 1000000, 1000000007) >= 0 { ok = ok + 1 }
    println(ok)
    0
}
"#;
    let out = audit2_compile_and_run_with_stdlib("mymath.mimi", cg_src)
        .unwrap_or_else(|e| panic!("codegen mymath overflow guards failed: {e}"));
    assert_eq!(
        out.trim(),
        "4",
        "codegen must match VM large-input mymath guards"
    );
}

// ===== strings.mimi — batch5 P2-4: truncate negative max_len =====

#[test]
fn audit_stdlib_truncate_negative_max_len_dual() {
    let src = r#"
func main() -> i32 {
    println("[" + truncate("hello", -1) + "]")
    println("[" + truncate("hello", 0) + "]")
    println("[" + truncate("hello", 3) + "]")
    println("[" + truncate("hello", 5) + "]")
    0
}
"#;
    let combined = format!("{}\n{}", audit2_stdlib_src("strings.mimi"), src);
    let (v, out) = run_source_with_stdout(&combined);
    assert_eq!(v, interp::Value::Int(0));
    assert_eq!(
        out.trim(),
        "[]\n[]\n[hel...]\n[hello]",
        "VM truncate must define non-positive max_len as empty"
    );

    if !can_link() {
        return;
    }
    let cg = compile_and_run(&combined)
        .unwrap_or_else(|e| panic!("codegen truncate negative max_len failed: {}", e));
    assert_eq!(
        cg.trim(),
        "[]\n[]\n[hel...]\n[hello]",
        "codegen truncate must match the VM"
    );
}

// ===== mimispec/lexer.mimi — batch5 P2-5: remove dummy first token =====

#[test]
fn audit_stdlib_mimispec_lexer_no_dummy_first_token() {
    let src = r#"
func main() -> i32 {
    let toks = tokenize("foo bar")
    println(len(toks))
    println(toks[0].0)
    println(toks[1].0)
    let empty_toks = tokenize("")
    println(len(empty_toks))
    let (toks2, errs2) = tokenize_with_errors("foo !")
    println(len(errs2))
    println(errs2[0].0)
    0
}
"#;
    let combined = format!("{}\n{}", audit2_stdlib_src("mimispec/lexer.mimi"), src);
    let (v, out) = run_source_with_stdout(&combined);
    assert_eq!(v, interp::Value::Int(0));
    assert_eq!(
        out.trim(),
        "4\nnewline\nident\n1\n1\nunexpected `!`, expected `!=`",
        "tokenize must not prepend a dummy empty token or duplicate eof, and errors must be exposed"
    );
}
