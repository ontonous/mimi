// ============================================================
// Trap semantics — dual-backend CLI regression (0.39.136)
// ============================================================
//
// Locks the orderly trap shutdown contract: when a Mimi program traps
// (E0801 div-by-zero, E0802 overflow, …), BOTH backends must
//
//   1. exit with status 1 (not SIGABRT/134 — the old native abort()
//      raised SIGABRT and discarded buffered stdout), and
//   2. preserve stdout printed before the trap, and
//   3. print an `[E08xx]` diagnostic on stderr.
//
// Lib-level counterparts: src/tests/usability_fixes.rs (rc + E-code via
// compile harness). This file covers the binary stdout-preservation face,
// which requires actually executing `mimi build` output.

use std::path::PathBuf;
use std::process::Command;

fn mimi_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_mimi")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/mimi"))
}

fn can_link() -> bool {
    static CAN_LINK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CAN_LINK.get_or_init(|| Command::new("cc").arg("--version").output().is_ok())
}

/// Build `src` with `mimi build`, run the binary, and assert the trap
/// contract. Returns nothing; panics on violation.
fn assert_native_trap_contract(src: &str, pre_trap_stdout: &str, ecode: &str) {
    let dir = std::env::temp_dir().join(format!("mimi-trap-cli-{}-{}", std::process::id(), ecode));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create tmp dir");
    let path = dir.join("trap.mimi");
    std::fs::write(&path, src).expect("write source");
    let bin = dir.join("trap_bin");

    let build = Command::new(mimi_bin())
        .args(["build", path.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .output()
        .expect("spawn mimi build");
    assert!(
        build.status.success(),
        "mimi build failed for {ecode}: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&bin).output().expect("run trap binary");
    assert_eq!(run.status.code(), Some(1), "native trap rc must be 1");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        stdout.trim(),
        pre_trap_stdout,
        "native trap must preserve pre-trap stdout"
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains(ecode),
        "native trap stderr must carry {ecode}: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_trap_div_zero_preserves_stdout_rc1() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    assert_native_trap_contract(
        r#"
func main() -> i32 {
    let a = 10
    let b = 0
    println("before")
    let x = a / b
    println(x)
    0
}
"#,
        "before",
        "E0801",
    );
}

#[test]
fn cli_trap_overflow_preserves_stdout_rc1() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    assert_native_trap_contract(
        r#"
func main() -> i32 {
    println("before")
    let big = 9223372036854775807
    let x = big + 1
    println(x)
    0
}
"#,
        "before",
        "E0802",
    );
}
