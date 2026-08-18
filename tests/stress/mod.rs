// ============================================================
// tests/stress/mod.rs — 高压测试 Harness（0.1.7 Phase 0）
// ============================================================
//
// 当前阶段先建立可复用的 CLI 驱动 Harness 与快速冒烟用例：
//   - 构造临时 Mimi 源文件
//   - 调用真实 `mimi run` / `mimi check` 子进程
//   - 统一报告耗时与退出码
//
// 后续 Phase 0/1 将把这里的 Harness 扩展为：
//   - Event Storm 的迭代/吞吐统计
//   - Soak Test 的 RSS 采样
//   - Chaos 注入的随机故障发生器

pub(crate) mod actor_spawn;
pub(crate) mod actor_stress;
pub(crate) mod build_concurrency;
pub(crate) mod chaos_fault;
pub(crate) mod concurrency_scale;
pub(crate) mod event_storm;
pub(crate) mod fuzz_json;
pub(crate) mod fuzz_parser;
pub(crate) mod fuzz_wire;
pub(crate) mod net_concurrency;
pub(crate) mod real_spawn;
pub(crate) mod soak_memory;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

pub(crate) fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub(crate) fn mimi_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_mimi")
        .map(PathBuf::from)
        .unwrap_or_else(|| project_root().join("target/debug/mimi"))
}

pub(crate) fn temp_dir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("stress temp_dir unwrap failed")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("mimi_stress_{}_{}", std::process::id(), nanos));
    fs::create_dir_all(&dir).expect("create stress temp dir");
    dir
}

/// Run a Mimi source through the selected subcommand.
/// Returns stderr/stdout on non-zero exit, otherwise stdout text.
pub(crate) fn run_mimi(source: &str, args: &[&str]) -> Result<String, String> {
    let dir = temp_dir();
    let src_path = dir.join("stress_case.mimi");
    fs::write(&src_path, source).expect("write stress source");

    let start = Instant::now();
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .args(args)
        .arg(&src_path)
        .output()
        .map_err(|e| format!("failed to spawn mimi: {e}"))?;
    let elapsed = start.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = fs::remove_dir_all(&dir);

    if !output.status.success() {
        return Err(format!(
            "mimi {:?} exited with {} after {:.1?}\nstdout:\n{}\nstderr:\n{}",
            args, output.status, elapsed, stdout, stderr
        ));
    }
    eprintln!("stress: {:?} completed in {:.2?}", args, elapsed);
    Ok(stdout)
}

/// Run a source through `mimi run` and return the normalized output.
pub(crate) fn run_program(source: &str) -> Result<String, String> {
    run_program_with_env(source, &[])
}

/// Like `run_program`, but with extra environment variables (used for
/// experimental runtime modes such as `MIMI_REAL_SPAWN=1`).
pub(crate) fn run_program_with_env(source: &str, envs: &[(&str, &str)]) -> Result<String, String> {
    let dir = temp_dir();
    let src_path = dir.join("stress_case.mimi");
    fs::write(&src_path, source).expect("write stress source");

    let start = Instant::now();
    let mut cmd = Command::new(mimi_bin());
    cmd.current_dir(project_root()).arg("run").arg(&src_path);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn mimi: {e}"))?;
    let elapsed = start.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = fs::remove_dir_all(&dir);

    if !output.status.success() {
        return Err(format!(
            "mimi run exited with {} after {:.1?}\nstdout:\n{}\nstderr:\n{}",
            output.status, elapsed, stdout, stderr
        ));
    }
    eprintln!("stress: run(envs={:?}) completed in {:.2?}", envs, elapsed);

    let mut normalized = stdout;
    if normalized
        .lines()
        .last()
        .is_some_and(|l| l.starts_with("-> "))
    {
        let mut lines: Vec<&str> = normalized.lines().collect();
        lines.pop();
        normalized = lines.join("\n");
    }
    Ok(normalized)
}

/// Build a Mimi source into a native executable and run it.
///
/// This is the correct path for real-thread concurrency and blocking I/O cases;
/// `mimi run`'s bytecode VM still executes `spawn`/`await` sequentially, which
/// can deadlock server/client programs that rely on concurrent sockets.
pub(crate) fn build_and_run_native(source: &str) -> Result<String, String> {
    let dir = temp_dir();
    let src_path = dir.join("stress_case.mimi");
    let exe_path = dir.join("stress_case_bin");
    fs::write(&src_path, source).expect("write stress source");

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&src_path)
        .arg("-o")
        .arg(&exe_path)
        .output()
        .map_err(|e| format!("failed to invoke mimi build: {e}"))?;
    if !build.status.success() {
        let stdout = String::from_utf8_lossy(&build.stdout).to_string();
        let stderr = String::from_utf8_lossy(&build.stderr).to_string();
        let _ = fs::remove_dir_all(&dir);
        return Err(format!(
            "mimi build failed with {}
stdout:
{}
stderr:
{}",
            build.status, stdout, stderr
        ));
    }

    let start = Instant::now();
    let output = Command::new(&exe_path)
        .current_dir(project_root())
        .output()
        .map_err(|e| format!("failed to run native binary: {e}"))?;
    let elapsed = start.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = fs::remove_dir_all(&dir);

    if !output.status.success() {
        return Err(format!(
            "native binary exited with {} after {:.2?}
stdout:
{}
stderr:
{}",
            output.status, elapsed, stdout, stderr
        ));
    }
    eprintln!("stress: native run completed in {:.2?}", elapsed);
    Ok(stdout)
}

/// Run a Mimi source through `/usr/bin/time -v` and return `(stdout, max_rss_kb)`.
///
/// This is the initial Linux RSS probe for the Phase 0 soak harness. It is kept
/// optional: callers should skip gracefully when `/usr/bin/time` is unavailable.
pub(crate) fn run_mimi_with_max_rss_kb(source: &str, args: &[&str]) -> Option<(String, u64)> {
    let time_bin = PathBuf::from("/usr/bin/time");
    if !time_bin.exists() {
        return None;
    }

    let dir = temp_dir();
    let src_path = dir.join("stress_case.mimi");
    fs::write(&src_path, source).expect("write stress source");

    let mut cmd = Command::new(&time_bin);
    cmd.arg("-v").arg(mimi_bin()).args(args).arg(&src_path);
    cmd.current_dir(project_root());

    let output = cmd.output().ok()?;
    let _ = fs::remove_dir_all(&dir);
    if !output.status.success() {
        return None;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let max_rss = stderr
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("Maximum resident set size (kbytes): ")
        })
        .and_then(|v| v.trim().parse::<u64>().ok())?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Some((stdout, max_rss))
}

/// Build a native executable from a Mimi source and return its max RSS when
/// run under `/usr/bin/time -v`. This measures runtime memory in the compiled
/// binary, excluding the compiler/JIT process overhead seen by `mimi run`.
pub(crate) fn build_and_run_native_with_max_rss_kb(source: &str) -> Option<(String, u64)> {
    let time_bin = PathBuf::from("/usr/bin/time");
    if !time_bin.exists() {
        return None;
    }

    let dir = temp_dir();
    let src_path = dir.join("stress_case.mimi");
    let exe_path = dir.join("stress_case_bin");
    fs::write(&src_path, source).expect("write stress source");

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&src_path)
        .arg("-o")
        .arg(&exe_path)
        .output()
        .ok()?;
    if !build.status.success() {
        let _ = fs::remove_dir_all(&dir);
        return None;
    }

    let output = Command::new(&time_bin)
        .arg("-v")
        .arg(&exe_path)
        .current_dir(project_root())
        .output()
        .ok()?;
    let _ = fs::remove_dir_all(&dir);
    if !output.status.success() {
        return None;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let max_rss = stderr
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("Maximum resident set size (kbytes): ")
        })
        .and_then(|v| v.trim().parse::<u64>().ok())?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Some((stdout, max_rss))
}

/// Build a Mimi source into a native executable for long-running/probing use.
/// Returns `(temp_dir, exe_path)`; the caller is responsible for terminating
/// the executable and removing `temp_dir`.
pub(crate) fn build_native_only(source: &str) -> Option<(PathBuf, PathBuf)> {
    let dir = temp_dir();
    let src_path = dir.join("stress_case.mimi");
    let exe_path = dir.join("stress_case_bin");
    fs::write(&src_path, source).ok()?;

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&src_path)
        .arg("-o")
        .arg(&exe_path)
        .output()
        .ok()?;
    if !build.status.success() {
        let _ = fs::remove_dir_all(&dir);
        return None;
    }
    Some((dir, exe_path))
}

/// Generate a Flow chain source with `n` consecutive transition events.
/// This exercises typing, lowering/codegen, and runtime transition dispatch
/// without relying on unstable mutable Flow state.
pub(crate) fn flow_chain_source(n: usize) -> String {
    let mut src = String::from(
        r#"flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive {
        return Positive { count: self.count + 1 }
    }
    transition inc(Positive) -> Positive {
        return Positive { count: self.count + 1 }
    }
}

func main() -> i32 {
"#,
    );
    src.push_str("    let s0 = Zero { count: 0 }\n");
    for i in 0..n {
        src.push_str(&format!("    let s{} = Counter::inc(s{})\n", i + 1, i));
    }
    src.push_str(&format!("    println(s{n}.count)\n"));
    src.push_str("    0\n}\n");
    src
}

/// Generate a Mimi program spawning `n` tasks and summing their results.
/// The current 0.1.7 runtime may still evaluate sequentially, but this still
/// exercises spawn/await parse, type-check, and task handle round-trip paths.
pub(crate) fn spawn_sum_source(n: usize) -> String {
    let mut src = String::from("func id(x: i32) -> i32 { x }\n\nfunc main() -> i32 {\n");
    for i in 0..n {
        src.push_str(&format!("    let t{i} = spawn id({i})\n"));
    }
    for i in 0..n {
        src.push_str(&format!("    let r{i} = await t{i}\n"));
    }
    src.push_str("    let mut sum = 0\n");
    for i in 0..n {
        src.push_str(&format!("    sum = sum + r{i}\n"));
    }
    src.push_str("    println(sum)\n");
    src.push_str("    0\n}\n");
    src
}

/// Generate a program that repeatedly allocates temporary lists in a loop.
/// Useful for VM memory-stability soak probes.
pub(crate) fn alloc_loop_source(n: usize) -> String {
    format!(
        r#"func main() -> i32 {{
    let mut acc = 0
    for i in range(0, {n}) {{
        let xs = [i, i + 1, i + 2]
        acc += len(xs)
    }}
    println(acc)
    0
}}
"#,
        n = n
    )
}
