//! Wave-1 audit-fix regression tests — stdlib.
//! Findings: devdocs/full-audit-2026-08-05.md §13 (2026-08-05 full audit).
//! Discipline: each stdlib (.mimi) fix carries a regression test here.
//! Runtime behavior is exercised through the Bytecode VM via
//! `run_with_stdlib` (stdlib source + test source concatenated, same
//! inclusion pattern as stdlib_v02813.rs / audit_regression.rs); typing is
//! exercised through `check_source`.
use super::*;


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
}

// ===== mymath.mimi — fix 2: free factorial gains the n>12 overflow guard ====

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
}

// ===== strings.mimi — fix 5: trim_left/trim_right strip the whitespace set ==

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
        interp::Value::String("hello \t|\t hello|a b |a b".to_string())
    );
}

// ===== strings.mimi — fix 6: words() drops empty tokens =====================

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
        Ok(n) => n
        Err(_) => -1
    }}
}}
"#,
        path.display()
    );
    let v = run_with_stdlib("fs.mimi", &src);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(v, interp::Value::Int(6), "byte size of 'héllo' is 6");
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
}

// ===== net.mimi — fix 9: recv EOF / empty body are success, not error =======

#[test]
fn audit_stdlib_tcp_recv_eof_is_ok_empty() {
    // Bind an ephemeral port; accept the Mimi client and close immediately so
    // the client's recv sees EOF (n == 0). The recv builtin maps that to an
    // empty string; the stdlib must surface it as Ok(""), not Err(RecvFailed).
    // Blocking accept mirrors the reliable pattern in src/tests/net.rs.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port() as i32;
    let server = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            drop(stream); // close → EOF for the peer
        }
    });
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
}

// ===== result.mimi — fix 10: map/map_result rebuild Err at Result<U, E> =====

#[test]
fn audit_stdlib_map_result_preserves_err_value() {
    // The Err branch used to return the original Result<T, E> where
    // Result<U, E> is declared. The Err payload must be carried into a
    // freshly-built Err at the target type.
    let src = r#"
func main() -> i32 {
    let ok_r: Result<i32, string> = Ok(21)
    let mapped_ok = map_result(ok_r, fn(x: i32) -> string { to_string(x) })
    let err_r: Result<i32, string> = Err("boom")
    let mapped_err = map_result(err_r, fn(x: i32) -> string { to_string(x) })
    let mut score = 0
    match mapped_ok {
        Ok(s) => if s == "21" { score = score + 1 } else { score = score }
        Err(_) => score = score
    }
    match mapped_err {
        Ok(_) => score = score
        Err(e) => if e == "boom" { score = score + 10 } else { score = score }
    }
    score
}
"#;
    assert_eq!(run_with_stdlib("result.mimi", src), interp::Value::Int(11));
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
}

#[test]
fn audit_stdlib_result_module_typechecks_with_strict_map() {
    // The audit flagged the old Err-branch (`self` typed Result<T, E>
    // returned where Result<U, E> is declared) as a possible unification
    // hole [UNVERIFIED]. After the fix the module must still typecheck
    // under a use that instantiates U != T.
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
