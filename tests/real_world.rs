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
    Command::new("cc").arg("--version").output().is_ok()
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

// TODO(v0.28.27): codegen reduce_list/reduce over List<i32> fails with
// "reduce: first arg must be a list".

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
            println(array_len(xs))
            0
        }
    "#,
        "[b, c]\n4",
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
            println(get(rows, 0, 1))
            println(get(rows, 1, 0))
            0
        }
    "#,
        "[[a, b], [c, d]]\nb\nc",
    );
}

// ===================== Flow paradigm MCDD (v0.29.9–0.29.25) =====================

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
    let mut sources: Vec<PathBuf> = fs::read_dir(&root)
        .expect("read tests/real_world")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|ext| ext == "mimi")
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with("flow_"))
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
