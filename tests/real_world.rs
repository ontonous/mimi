// ============================================================
// Real-world Mimi programs (MCDD regression suite)
// ============================================================
//
// These integration tests exercise complete, realistic Mimi programs through
// the actual `mimi run` and `mimi build` CLI paths. Cargo automatically builds
// the `mimi` binary and sets CARGO_BIN_EXE_mimi before running these tests.
//
// See AGENTS.md §13.13 (MCDD) for methodology.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn temp_dir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("real_world temp_dir unwrap failed")
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("mimi_real_world_{}_{}", std::process::id(), nanos));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn mimi_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_mimi")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("debug")
                .join("mimi")
        })
}

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn can_link() -> bool {
    static CAN_LINK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CAN_LINK.get_or_init(|| Command::new("cc").arg("--version").output().is_ok())
}

/// Strip the trailing `-> N` return-value line that `mimi run` prints.
fn normalize_run_output(s: &str) -> String {
    let mut lines: Vec<&str> = s.lines().collect();
    if lines.last().is_some_and(|l| l.starts_with("-> ")) {
        lines.pop();
    }
    lines.join("\n")
}

fn mimi_run(src_path: &std::path::Path) -> Result<String, String> {
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(src_path)
        .output()
        .map_err(|e| format!("failed to spawn mimi run: {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(format!(
            "mimi run exited with {}\nstdout:\n{}\nstderr:\n{}",
            output.status, stdout, stderr
        ));
    }
    Ok(normalize_run_output(&stdout))
}

fn mimi_build_and_run(src_path: &std::path::Path) -> Result<String, String> {
    let dir = src_path.parent().expect("src_path has parent");
    let stem = src_path
        .file_stem()
        .expect("src_path has stem")
        .to_string_lossy();
    let binary = dir.join(&*stem);

    let build_output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(src_path)
        .arg("-o")
        .arg(&binary)
        .output()
        .map_err(|e| format!("failed to spawn mimi build: {}", e))?;
    let build_stdout = String::from_utf8_lossy(&build_output.stdout).to_string();
    let build_stderr = String::from_utf8_lossy(&build_output.stderr).to_string();
    if !build_output.status.success() {
        return Err(format!(
            "mimi build exited with {}\nstdout:\n{}\nstderr:\n{}",
            build_output.status, build_stdout, build_stderr
        ));
    }

    let run_output = Command::new(&binary)
        .output()
        .map_err(|e| format!("failed to run compiled binary: {}", e))?;
    let run_stdout = String::from_utf8_lossy(&run_output.stdout).to_string();
    let run_stderr = String::from_utf8_lossy(&run_output.stderr).to_string();
    let _ = fs::remove_file(&binary);
    if !run_output.status.success() {
        return Err(format!(
            "compiled binary exited with {}\nstdout:\n{}\nstderr:\n{}",
            run_output.status, run_stdout, run_stderr
        ));
    }
    Ok(run_stdout)
}

/// Write `src` to a temp file, run it through both `mimi run` and `mimi build`,
/// and assert that both produce `expected_stdout`.
fn run_both(src: &str, expected_stdout: &str) {
    let dir = temp_dir();
    let src_path = dir.join("program.mimi");
    fs::write(&src_path, src).expect("write source");

    let run_stdout = mimi_run(&src_path).expect("mimi run failed");
    assert_eq!(
        run_stdout.trim(),
        expected_stdout.trim(),
        "mimi run stdout mismatch"
    );

    if !can_link() {
        eprintln!("SKIP: cc not available");
        fs::remove_dir_all(&dir).ok();
        return;
    }
    let build_stdout = mimi_build_and_run(&src_path).expect("mimi build failed");
    assert_eq!(
        build_stdout.trim(),
        expected_stdout.trim(),
        "mimi build stdout mismatch"
    );

    fs::remove_dir_all(&dir).ok();
}

// ===================== Literal sub-pattern fall-through =====================
// 0.35.7: `B(false)` under a `B(true)` arm must fall through to the next arm,
// not abort. Before the fix the arm was entered on tag match alone and the
// pattern binder asserted on the mismatched literal — crashing the program.
// Also covers the generic-param/user-type name shadow (`type T` + prelude
// `eq<T>`) that broke the legacy generic skeleton compile with
// "eq requires same types".

#[test]
fn real_world_literal_pattern_fallthrough() {
    run_both(
        r#"
        type T {
            A
            B(bool)
        }

        func f(t: T) -> string {
            match t {
                A => "a"
                B(true) => "bt"
                B(false) => "bf"
            }
        }

        func main() {
            println(f(A()))
            println(f(B(true)))
            println(f(B(false)))
        }
    "#,
        "a\nbt\nbf",
    );
}

// ===================== List intrinsics display + realloc ownership =====================
// 0.35.11 (O1 correctness slice):
// 1. The resolved slice emitter's trap block lacked a terminator — LLVM
//    verify rejected the caller and the whole body silently demoted to the
//    legacy emitter. `sort` + `data[1..4]` in one function was the repro.
// 2. Legacy print dispatch inferred the callee NAME ("map", "reverse", …)
//    as the result type for list-returning builtins, misrouting the
//    {i64 len, ptr data} struct to the string fast path (empty output).
// 3. A list literal bound to a local constructed into a temp registered as
//    the buffer owner; push/pop realloc through the local left the temp
//    slot stale → scope-exit free double-freed it (tcache abort at O0).

#[test]
fn real_world_list_intrinsic_display_and_realloc() {
    run_both(
        r#"
        func main() -> i32 {
            let data = [3, 1, 4, 1, 5]
            println("sort:", sort(data))
            println("slice:", data[1..4])
            println("reverse:", reverse(data))
            println("map:", map(data, fn(v: i32) -> i32 { v * 2 }))
            println("filter:", filter(data, fn(v: i32) -> bool { v % 2 == 0 }))
            let m = map(data, fn(v: i32) -> i32 { v + 1 })
            println("bound:", m)
            let mut ys = [1, 2, 3]
            push(ys, 4)
            println("pushed:", len(ys))
            let popped = pop(ys)
            println("popped:", popped)
            0
        }
    "#,
        "sort: [1, 1, 3, 4, 5]\nslice: [1, 4, 1]\nreverse: [5, 1, 4, 1, 3]\nmap: [6, 2, 8, 2, 10]\nfilter: [4]\nbound: [4, 2, 5, 2, 6]\npushed: 4\npopped: 4",
    );
}

// ===================== Tuple fn-element extraction call =====================
// 0.35.14 (DX backlog #18): `let f = t.0` where t's first element is a
// named function used to fail codegen with E0700 "undefined function 'f'"
// (the dispatcher resolved f as a NAMED function; only direct
// `let f = func_name` bindings were registered as fn-pointer vars). The
// fix tracks tuple-literal function elements at bind time and routes the
// extraction binding through the indirect-call path (i64 ptrtoint slot).

#[test]
fn real_world_tuple_fn_element_call() {
    run_both(
        r#"
        func add_impl(a: i32, b: i32) -> i32 {
            a + b
        }

        func half(x: f64) -> f64 {
            x / 2.0
        }

        func main() {
            let t = (add_impl, 7)
            let f = t.0
            println(f(1, 2))
            let u = (half, 9)
            let g = u.0
            println(g(5.0))
        }
    "#,
        "3\n2.5",
    );
}

// ===================== Standard library: strings =====================
// `use std::strings` merges pub functions into the current scope.

#[test]
fn real_world_strings_module() {
    run_both(
        r#"
        use std::strings

        func main() -> i32 {
            let n = count_substring("hello world", "l")
            println(n)
            if contains("hello world", "world") { println("yes") } else { println("no") }
            0
        }
    "#,
        "3\nyes",
    );
}

// ===================== Standard library: collections =====================

#[test]
fn real_world_collections_module() {
    run_both(
        r#"
        use std::collections

        func main() -> i32 {
            let nums = [1, 2, 3, 4, 5]
            let sum = reduce_list(nums, fn(acc: i32, x: i32) -> i32 { acc + x }, 0)
            let evens = filter_list(nums, fn(x: i32) -> bool { x % 2 == 0 })
            let doubled = map_list(nums, fn(x: i32) -> i32 { x * 2 })
            println(sum)
            println(evens)
            println(doubled)
            0
        }
    "#,
        "15\n[2, 4]\n[2, 4, 6, 8, 10]",
    );
}

// ===================== Maps (builtins) =====================
// map_get returns (bool, value); the bool indicates whether the key was found.

#[test]
fn real_world_map_builtins() {
    run_both(
        r#"
        func main() -> i32 {
            let m = map_new()
            let m2 = map_set(m, "x", 1)
            let m3 = map_set(m2, "y", 2)
            let rx = map_get(m3, "x")
            let ry = map_get(m3, "y")
            println(rx.1)
            println(ry.1)
            println(map_size(m3))
            0
        }
    "#,
        "1\n2\n2",
    );
}

// ===================== BUG O: Result/Option map with string closures =====================
// 0.36.x (BUG O): `result.map` / `option.map` / `result.map_err` with a closure that
// returns (or takes) a string/aggregate used to SEGFAULT in the native backend. Inline
// struct slots were coerced to `i64`, corrupting the layout. `materialize_slot` now
// passes inline struct/pointer slots through unchanged, fixing the crash. These `map`
// cases are the verified, deterministic half of BUG O (the dual backends agree). The
// `map_err` string-closure case is tracked separately as a non-deterministic closure
// type-inference defect (see devdocs/bug-hunt-suspected-2026-08.md, BUG O string err).

#[test]
fn real_world_result_option_map_string_closure() {
    run_both(
        r#"
        func wrap(s: string) -> Result<string, string> { Ok(s) }
        func main() {
            let w = wrap("hi")
            let a = w.map(fn(x: string) -> string { x })
            match a { Ok(v) => println(v), Err(e) => println("?") }
            let b = w.map(fn(x: string) -> string { "X" })
            match b { Ok(v) => println(v), Err(e) => println("?") }
        }
    "#,
        "hi\nX",
    );
    run_both(
        r#"
        func main() {
            let o = Some(42)
            let mapped = o.map(fn(x: i32) -> string { "hi" })
            match mapped {
                Some(s) => println(s),
                None => println("none"),
            }
        }
    "#,
        "hi",
    );
    run_both(
        r#"
        func main() {
            let r: Result<i32, string> = Ok(5);
            let mapped = r.map(fn(x: i32) -> string { "X" });
            match mapped {
                Ok(s) => println(s),
                Err(e) => println(e),
            }
        }
    "#,
        "X",
    );
}

// ===================== Standard library: mymath =====================

#[test]
fn real_world_mymath_module() {
    run_both(
        r#"
        use std::mymath

        func main() -> i32 {
            println(factorial(5))
            println(gcd(48, 18))
            println(power(2, 10))
            0
        }
    "#,
        "120\n6\n1024",
    );
}

// ===================== Concurrency primitives: channel =====================

#[test]
fn real_world_channel() {
    run_both(
        r#"
        func main() -> i32 {
            let ch = channel_new()
            channel_send(ch, 42)
            let v = channel_recv(ch)
            println(v)
            channel_drop(ch)
            0
        }
    "#,
        "42",
    );
}

// ===================== JSON =====================

#[test]
fn real_world_json() {
    run_both(
        r#"
        func main() -> i32 {
            let raw = "{\"name\":\"mimi\",\"count\":42}"
            let j = from_json(raw)
            println(json_get_string(j, "name"))
            println(json_get_int(j, "count"))
            0
        }
    "#,
        "mimi\n42",
    );
}

// ===================== Standard library: env =====================

#[test]
fn real_world_env_module() {
    run_both(
        r#"
        use std::env

        func main() -> i32 {
            println(arg_count())
            if has_var("PATH") { println("has_path") } else { println("no_path") }
            println(get_var_or("MIMI_DEFINITELY_MISSING_VAR", "fallback"))
            0
        }
    "#,
        "0\nhas_path\nfallback",
    );
}

// ===================== Standard library: array =====================

#[test]
fn real_world_array_module() {
    run_both(
        r#"
        use std::array

        func main() -> i32 {
            let xs = ["a", "b", "c", "d"]
            println(array_slice(xs, 1, 3))
            println(array_concat(array_take(xs, 1), array_drop(xs, 3)))
            println(array_len(xs))
            0
        }
    "#,
        "[b, c]\n[a, d]\n4",
    );
}

// ===================== Multiple std modules combined =====================

#[test]
fn real_world_multiple_std_modules() {
    run_both(
        r#"
        use std::strings
        use std::collections
        use std::mymath

        func main() -> i32 {
            let nums = [1, 2, 3, 4, 5]
            println(reduce_list(nums, fn(acc: i32, x: i32) -> i32 { acc + x }, 0))
            println(filter_list(nums, fn(x: i32) -> bool { x % 2 == 0 }))
            if contains("hello world", "world") { println("yes") } else { println("no") }
            println(power(2, 10))
            println(gcd(48, 18))
            0
        }
    "#,
        "15\n[2, 4]\nyes\n1024\n6",
    );
}

// ===================== Standard library: csv =====================

#[test]
fn real_world_csv_module() {
    run_both(
        r#"
        use std::csv

        func main() -> i32 {
            let rows = parse("a,b\nc,d")
            println(rows)
            println(cell(rows, 0, 1))
            println(cell(rows, 1, 0))
            0
        }
    "#,
        "[[a, b], [c, d]]\nb\nc",
    );
}

// ===================== Flow paradigm MCDD (v0.29.9–0.29.25) =====================

// ===================== Per-function dispatch (S9) =====================

/// Exercises the per-function dispatch path: eligible functions (scalar,
/// tuple, control flow) are compiled through the resolved native emitter
/// while ineligible functions (List, closures) fall back to legacy.
/// L1 equivalence is enforced between interpreter and codegen.
#[test]
fn real_world_per_function_dispatch() {
    run_both(
        r#"
        // Eligible: pure scalar + control flow + tuple
        func fib(n: i64) -> i64 {
            let mut a: i64 = 0
            let mut b: i64 = 1
            let mut i: i64 = 0
            while i < n {
                let tmp = a + b
                a = b
                b = tmp
                i = i + 1
            }
            a
        }

        // Eligible: tuple construction + projection
        func divmod(a: i64, b: i64) -> (i64, i64) {
            (a / b, a % b)
        }

        // Ineligible: uses List (not in resolved native slice)
        func sum_list(xs: List<i64>) -> i64 {
            let mut total: i64 = 0
            let mut i: i64 = 0
            while i < len(xs) {
                total = total + xs[i]
                i = i + 1
            }
            total
        }

        func main() -> i32 {
            println(fib(10))
            let (q, r) = divmod(17, 5)
            println(q)
            println(r)
            let nums: List<i64> = [10, 20, 30]
            println(sum_list(nums))
            0
        }
    "#,
        "55\n3\n2\n60",
    );
}

// ===================== Resolved emitter expanded coverage (0.32.1) =====================
//
// The dispatch diagnostic (dispatch_diagnostic_coverage_report) showed 93%
// eligibility for scalar/tuple/string/control-flow programs. These tests
// verify L1 equivalence for the eligible features that go through the
// resolved native emitter.

/// String parameters and return values are PrimitiveType::String — eligible.
#[test]
fn real_world_resolved_string_param_return() {
    run_both(
        r#"
        func greet(name: string) -> string {
            f"hello, {name}"
        }
        func main() -> i32 {
            println(greet("mimi"))
            0
        }
    "#,
        "hello, mimi",
    );
}

/// FString interpolation with multiple variables.
#[test]
fn real_world_resolved_fstring_multi_var() {
    run_both(
        r#"
        func describe(x: i64, y: i64) -> string {
            f"x={x} y={y} sum={x + y}"
        }
        func main() -> i32 {
            println(describe(3, 7))
            0
        }
    "#,
        "x=3 y=7 sum=10",
    );
}

/// Multi-function call chain: pipeline(x) = step2(step1(x)).
#[test]
fn real_world_resolved_call_chain() {
    run_both(
        r#"
        func step1(x: i64) -> i64 { x + 1 }
        func step2(x: i64) -> i64 { x * 2 }
        func step3(x: i64) -> i64 { x - 3 }
        func pipeline(x: i64) -> i64 { step3(step2(step1(x))) }
        func main() -> i32 {
            println(pipeline(5))
            0
        }
    "#,
        "9",
    );
}

/// Tuple destructuring in let binding.
#[test]
fn real_world_resolved_tuple_destructure() {
    run_both(
        r#"
        func minmax(a: i64, b: i64) -> (i64, i64) {
            if a < b { (a, b) } else { (b, a) }
        }
        func main() -> i32 {
            let (lo, hi) = minmax(9, 4)
            println(lo)
            println(hi)
            0
        }
    "#,
        "4\n9",
    );
}

/// Match with literal patterns and wildcard.
#[test]
fn real_world_resolved_match_literals() {
    run_both(
        r#"
        func day_name(d: i32) -> string {
            match d {
                1 => "mon",
                2 => "tue",
                3 => "wed",
                4 => "thu",
                5 => "fri",
                _ => "weekend",
            }
        }
        func main() -> i32 {
            println(day_name(3))
            println(day_name(7))
            0
        }
    "#,
        "wed\nweekend",
    );
}

/// Early return inside nested if/while.
#[test]
fn real_world_resolved_early_return() {
    run_both(
        r#"
        func first_ge(xs_len: i64, threshold: i64) -> i64 {
            let mut i: i64 = 0
            while i < xs_len {
                if i >= threshold {
                    return i
                }
                i = i + 1
            }
            0 - 1
        }
        func main() -> i32 {
            println(first_ge(10, 5))
            println(first_ge(3, 7))
            0
        }
    "#,
        "5\n-1",
    );
}

/// Builtin math chain: sqrt → floor → cast.
#[test]
fn real_world_resolved_builtin_math_chain() {
    run_both(
        r#"
        func isqrt(n: f64) -> i64 {
            let s = sqrt(n)
            let f = floor(s)
            f as i64
        }
        func main() -> i32 {
            println(isqrt(144.0))
            println(isqrt(2.0))
            0
        }
    "#,
        "12\n1",
    );
}

/// While loop with mutable accumulator and nested if.
#[test]
fn real_world_resolved_while_nested_if() {
    run_both(
        r#"
        func count_even_below(n: i64) -> i64 {
            let mut count: i64 = 0
            let mut i: i64 = 0
            while i < n {
                if i % 2 == 0 {
                    count = count + 1
                }
                i = i + 1
            }
            count
        }
        func main() -> i32 {
            println(count_even_below(10))
            0
        }
    "#,
        "5",
    );
}

/// Mixed eligible + ineligible: scalar functions go resolved, List function
/// goes legacy. Both must produce identical output (L1 equivalence).
#[test]
fn real_world_resolved_mixed_dispatch() {
    run_both(
        r#"
        func square(x: i64) -> i64 { x * x }

        func sum_squares(n: i64) -> i64 {
            let mut total: i64 = 0
            let mut i: i64 = 1
            while i <= n {
                total = total + square(i)
                i = i + 1
            }
            total
        }

        func describe_result(n: i64, total: i64) -> string {
            f"sum of squares 1..{n} = {total}"
        }

        func main() -> i32 {
            let n: i64 = 5
            let total = sum_squares(n)
            println(describe_result(n, total))
            println(square(7))
            0
        }
    "#,
        "sum of squares 1..5 = 55\n49",
    );
}

/// Option<i64> construction (Some/None) through the resolved native emitter.
/// 0.32.1: Option/Result types are now eligible. This test verifies
/// construction + return without match (match Constructor patterns are
/// still legacy-only).
#[test]
fn real_world_resolved_option_construct() {
    run_both(
        r#"
        func safe_div(a: i64, b: i64) -> Option<i64> {
            if b == 0 { None } else { Some(a / b) }
        }

        func unwrap_or(opt: Option<i64>, default: i64) -> i64 {
            match opt {
                Some(v) => v,
                None => default,
            }
        }

        func main() -> i32 {
            println(unwrap_or(safe_div(10, 3), 0 - 1))
            println(unwrap_or(safe_div(10, 0), 0 - 1))
            0
        }
    "#,
        "3\n-1",
    );
}

/// List<i64> construction, indexing, and len() through the resolved native
/// emitter. 0.32.2: List/Map/Set nominal types are now eligible.
#[test]
fn real_world_resolved_list_index() {
    run_both(
        r#"
        func sum(xs: List<i64>) -> i64 {
            let mut total: i64 = 0
            let mut i: i64 = 0
            while i < len(xs) {
                total = total + xs[i]
                i = i + 1
            }
            total
        }

        func main() -> i32 {
            let nums: List<i64> = [10, 20, 30]
            println(sum(nums))
            println(len(nums))
            println(nums[0])
            println(nums[2])
            0
        }
    "#,
        "60\n3\n10\n30",
    );
}

/// User-defined record construction and field access through the resolved
/// native emitter. 0.32.5: Nominal record types are now eligible.
#[test]
fn real_world_resolved_record_field() {
    run_both(
        r#"
        type Point { x: i64, y: i64 }

        func make_point(a: i64, b: i64) -> Point {
            Point { x: a, y: b }
        }

        func distance_sq(p: Point) -> i64 {
            p.x * p.x + p.y * p.y
        }

        func main() -> i32 {
            let p = make_point(3, 4)
            println(distance_sq(p))
            println(p.x)
            println(p.y)
            0
        }
    "#,
        "25\n3\n4",
    );
}

/// Option match with Constructor patterns (Some/None) through the resolved
/// native emitter. 0.32.6: Constructor patterns are now eligible.
#[test]
fn real_world_resolved_option_match_ctor() {
    run_both(
        r#"
        func safe_div(a: i64, b: i64) -> Option<i64> {
            if b == 0 { None } else { Some(a / b) }
        }

        func unwrap_or(opt: Option<i64>, default: i64) -> i64 {
            match opt {
                Some(v) => v,
                None => default,
            }
        }

        func main() -> i32 {
            println(unwrap_or(safe_div(10, 3), 0 - 1))
            println(unwrap_or(safe_div(10, 0), 0 - 1))
            0
        }
    "#,
        "3\n-1",
    );
}

/// Match arms returning fstring through the legacy emitter. Regression
/// test for the match+fstring double free bug: the fstring emitter
/// registers two heap entries (snprintf temp + string buffer), and
/// free_heap_allocs would free the snprintf temp even when a different
/// match arm was taken (undefined pointer → double free).
#[test]
fn real_world_match_fstring_return() {
    run_both(
        r#"
        func safe_div(a: i64, b: i64) -> Option<i64> {
            if b == 0 { None } else { Some(a / b) }
        }

        func describe(opt: Option<i64>) -> string {
            match opt {
                Some(v) => f"value={v}",
                None => "nothing",
            }
        }

        func main() -> i32 {
            println(describe(safe_div(10, 3)))
            println(describe(safe_div(10, 0)))
            0
        }
    "#,
        "value=3\nnothing",
    );
}

/// Match with literal patterns returning fstring. Same root cause as
/// the Constructor pattern variant above.
#[test]
fn real_world_match_literal_fstring_return() {
    run_both(
        r#"
        func classify(x: i32) -> string {
            match x {
                1 => f"one ({x})",
                2 => f"two ({x})",
                _ => f"many ({x})",
            }
        }

        func main() -> i32 {
            println(classify(1))
            println(classify(5))
            0
        }
    "#,
        "one (1)\nmany (5)",
    );
}

/// Match with if-expr returning fstring in branches. `is_string_producing_expr`
/// must recurse into `Expr::If` branches because `compile_if_expr` uses
/// `compile_block_last_val` (no heap scope) — without the recursion,
/// `free_heap_allocs` would free the string buffer while the if-expr phi
/// still references it.
#[test]
fn real_world_match_if_fstring_return() {
    run_both(
        r#"
        func sign(n: i64) -> string {
            match n {
                _ if n > 0 => if true { f"positive ({n})" } else { f"positive ({n})" },
                _ if n < 0 => if true { f"negative ({n})" } else { f"negative ({n})" },
                _ => "zero",
            }
        }

        func main() -> i32 {
            println(sign(5))
            println(sign(-3))
            println(sign(0))
            0
        }
    "#,
        "positive (5)\nnegative (-3)\nzero",
    );
}

/// Dual-backend regression for every `tests/real_world/flow_*.mimi`.
///
/// Requires `cc` for the codegen path. Compares normalized stdout so L1
/// equivalence is enforced (not just exit code 0).
#[test]
fn real_world_flow_dual_backend_suite() {
    if !can_link() {
        eprintln!("SKIP real_world_flow_dual_backend_suite: cc not available");
        return;
    }
    let root = project_root().join("tests").join("real_world");
    let interpreter_only = ["flow_test_macros.mimi"];
    // 0.31.46: known language limitations — these tests document intended
    // behavior that the checker does not yet support. They are excluded
    // from the dual-backend suite until the limitation is resolved.
    // 0.35.8: flow_order_system + flow_system_trace removed — the fails E
    // string-event-param SIGSEGV (dx-backlog #20) is fixed; both now run
    // through the dual-backend suite.
    let known_limitations = [];
    let mut sources: Vec<PathBuf> = fs::read_dir(&root)
        .expect("read tests/real_world")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|s| s.to_str());
            p.extension().is_some_and(|ext| ext == "mimi")
                && name.is_some_and(|n| {
                    n.starts_with("flow_")
                        && !interpreter_only.contains(&n)
                        && !known_limitations.contains(&n)
                })
        })
        .collect();
    sources.sort();
    assert!(
        !sources.is_empty(),
        "expected at least one flow_*.mimi under tests/real_world"
    );

    let mut failures = Vec::new();
    for src in &sources {
        let name = src.file_name().unwrap().to_string_lossy().to_string();
        eprintln!("flow dual-backend: {name}");
        match (mimi_run(src), mimi_build_and_run(src)) {
            (Ok(i), Ok(c)) => {
                let i = i.trim_end();
                let c = c.trim_end();
                if i != c {
                    failures.push(format!(
                        "{name}: L1 mismatch\n  interp: {i:?}\n  codegen: {c:?}"
                    ));
                }
            }
            (Err(e), _) => failures.push(format!("{name}: interp failed: {e}")),
            (_, Err(e)) => failures.push(format!("{name}: codegen failed: {e}")),
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} flow dual-backend failure(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
