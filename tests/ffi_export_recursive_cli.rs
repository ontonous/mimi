use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fresh_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "mimi_ffi_export_recursive_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ))
}

#[test]
fn recursive_c_export_build_uses_internal_mimi_abi() {
    let dir = fresh_dir();
    fs::create_dir_all(&dir).expect("create isolated export test directory");
    let source = dir.join("recursive_export.mimi");
    let executable = dir.join("recursive_export");
    fs::write(
        &source,
        r#"
            extern "C" func factorial(value: i32) -> i32 {
                requires: value >= 0
                if value <= 1 { 1 } else { value * factorial(value - 1) }
            }
            func main() -> i32 {
                println(factorial(5))
                0
            }
        "#,
    )
    .expect("write recursive C-export fixture");

    let build = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("build")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("spawn native build");
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build.status.success(),
        "recursive exported body must build through its Mimi-internal symbol: {build_stderr}"
    );

    let run = Command::new(&executable)
        .output()
        .expect("run recursive-export executable");
    assert!(
        run.status.success(),
        "recursive-export executable failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"120\n");

    let explicit_mir_output = dir.join("must-not-exist");
    let explicit_mir = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("build")
        .arg(&source)
        .arg("--mir")
        .arg("-o")
        .arg(&explicit_mir_output)
        .output()
        .expect("spawn explicit MIR build");
    assert!(
        !explicit_mir.status.success(),
        "an exported C wrapper must remain outside the migrated MIR profile"
    );
    assert!(
        !explicit_mir_output.exists(),
        "explicit MIR rejection must happen before creating a native artifact"
    );

    let explicit_mir_run = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("run")
        .arg(&source)
        .arg("--mir")
        .output()
        .expect("spawn explicit MIR run");
    assert!(
        !explicit_mir_run.status.success(),
        "explicit MIR run must reject the missing export-wrapper contract"
    );
    assert!(
        explicit_mir_run.stdout.is_empty(),
        "explicit MIR run must reject before executing main"
    );

    let explicit_mir_verify = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("verify")
        .arg(&source)
        .arg("--mir")
        .output()
        .expect("spawn explicit MIR verification");
    assert!(
        !explicit_mir_verify.status.success(),
        "explicit MIR verification must reject the missing export-wrapper contract"
    );

    let explicit_mir_dump = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("mir")
        .arg(&source)
        .output()
        .expect("spawn canonical MIR dump");
    assert!(
        !explicit_mir_dump.status.success(),
        "MIR dump must reject an exported body whose C wrapper is not modeled"
    );

    fs::remove_dir_all(&dir).ok();
}
