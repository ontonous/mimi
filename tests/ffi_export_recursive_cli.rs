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

#[test]
fn scalar_import_with_export_body_uses_explicit_default_compatibility_route() {
    let dir = fresh_dir();
    fs::create_dir_all(&dir).expect("create isolated export/import test directory");
    let source = dir.join("scalar_import_export_body.mimi");
    let executable = dir.join("scalar_import_export_body");
    let explicit_mir_output = dir.join("must-not-exist");
    fs::write(
        &source,
        r#"
            extern "C" { func abs(value: i32) -> i32; }
            extern "C" func internal_export(value: i32) -> i32 { value + 1 }
            func main() -> i32 {
                println(abs(internal_export(-3)))
                0
            }
        "#,
    )
    .expect("write scalar-import and C-export fixture");

    let build = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("build")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .env("MIMI_VERBOSE", "1")
        .output()
        .expect("spawn default native build");
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build.status.success(),
        "default build must keep the export-wrapper compatibility implementation: {build_stderr}"
    );
    assert!(
        build_stderr
            .contains("canonical route disposition: legacy (exported-abi-body-compatibility)"),
        "default build must disclose the early compatibility route: {build_stderr}"
    );
    let native = Command::new(&executable)
        .output()
        .expect("run default native executable");
    assert!(native.status.success());
    assert_eq!(native.stdout, b"2\n");

    let run = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("run")
        .arg(&source)
        .env("MIMI_VERBOSE", "1")
        .output()
        .expect("spawn default bytecode run");
    let run_stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "default run failed: {run_stderr}");
    assert_eq!(run.stdout, b"2\n");
    assert!(run_stderr
        .contains("canonical route disposition: legacy (exported-abi-body-compatibility)"));

    let verify = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("verify")
        .arg(&source)
        .env("MIMI_VERBOSE", "1")
        .output()
        .expect("spawn default verification");
    let verify_transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(
        verify.status.success(),
        "default verification without contracts must succeed: {verify_transcript}"
    );
    assert!(verify_transcript.contains("no contracts to verify"));
    assert!(verify_transcript
        .contains("canonical route disposition: legacy (exported-abi-body-compatibility)"));

    let explicit_mir_build = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("build")
        .arg(&source)
        .arg("--mir")
        .arg("-o")
        .arg(&explicit_mir_output)
        .output()
        .expect("spawn explicit MIR build");
    assert!(!explicit_mir_build.status.success());
    assert!(!explicit_mir_output.exists());
    let explicit_mir_stderr = String::from_utf8_lossy(&explicit_mir_build.stderr);
    assert!(explicit_mir_stderr
        .contains("exported C function body has no canonical MIR export-wrapper ABI"));

    let explicit_mir_verify = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("verify")
        .arg(&source)
        .arg("--mir")
        .output()
        .expect("spawn explicit MIR verification");
    assert!(!explicit_mir_verify.status.success());
    assert!(String::from_utf8_lossy(&explicit_mir_verify.stderr)
        .contains("exported C function body has no canonical MIR export-wrapper ABI"));

    let explicit_mir_run = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("run")
        .arg(&source)
        .arg("--mir")
        .output()
        .expect("spawn explicit MIR run");
    assert!(!explicit_mir_run.status.success());
    assert!(explicit_mir_run.stdout.is_empty());
    assert!(String::from_utf8_lossy(&explicit_mir_run.stderr)
        .contains("exported C function body has no canonical MIR export-wrapper ABI"));

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn exported_body_compatibility_route_preserves_called_ffi_contracts() {
    let dir = fresh_dir();
    fs::create_dir_all(&dir).expect("create isolated FFI contract directory");
    let source = dir.join("scalar_import_export_body_ffi_contract.mimi");
    fs::write(
        &source,
        r#"
            extern "C" {
                func strict_identity(value: i32) -> i32 requires: value >= 0;
            }
            extern "C" func internal_export(value: i32) -> i32 { value + 1 }
            func main() -> i32 {
                println(strict_identity(-1))
                0
            }
        "#,
    )
    .expect("write FFI contract fixture");

    let verify = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("verify")
        .arg(&source)
        .env("MIMI_VERBOSE", "1")
        .output()
        .expect("spawn verification of called FFI contract");
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(
        !verify.status.success(),
        "a violated called FFI contract must fail even on the explicit export compatibility route: {transcript}"
    );
    assert!(
        transcript.contains("calls strict_identity") || transcript.contains("strict_identity"),
        "missing called FFI contract result: {transcript}"
    );
    assert!(
        transcript.contains("precondition may be violated")
            || transcript.contains("may violate precondition"),
        "expected a fail-closed FFI call-site result: {transcript}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn compatibility_verification_keeps_user_contracts_after_excluding_prelude_bodies() {
    let dir = fresh_dir();
    fs::create_dir_all(&dir).expect("create isolated contract verification directory");
    let source = dir.join("scalar_import_export_body_contract.mimi");
    fs::write(
        &source,
        r#"
            extern "C" { func abs(value: i32) -> i32; }
            extern "C" func internal_export(value: i32) -> i32 { value + 1 }

            func must_fail(value: i32) -> i32 {
                requires: value > 0
                ensures: result < 0
                value
            }

            func main() -> i32 {
                println(abs(internal_export(-3)))
                0
            }
        "#,
    )
    .expect("write user-contract fixture");

    let verify = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("verify")
        .arg(&source)
        .env("MIMI_VERBOSE", "1")
        .output()
        .expect("spawn verification with user contract");
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(
        !verify.status.success(),
        "a disproven user contract must still fail compatibility verification: {transcript}"
    );
    assert!(
        transcript.contains("must_fail"),
        "missing user contract result: {transcript}"
    );
    assert!(
        transcript.contains("Disproven")
            || transcript.contains("contract violated")
            || transcript.contains("engine divergence"),
        "expected a fail-closed user contract verdict: {transcript}"
    );
    assert!(
        !transcript.contains("lerp: f64 arithmetic"),
        "prelude implementation bodies must not become user proof obligations: {transcript}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn export_body_route_verifies_ffi_calls_inside_actor_methods() {
    let dir = fresh_dir();
    fs::create_dir_all(&dir).expect("create isolated actor FFI verification directory");
    let source = dir.join("scalar_import_export_body_actor_ffi.mimi");
    fs::write(
        &source,
        r#"
            extern "C" {
                func strict_identity(value: i32) -> i32 requires: value >= 0;
            }
            extern "C" func internal_export(value: i32) -> i32 { value + 1 }
            actor Worker {
                func invoke_actor() -> i32 { strict_identity(-1) }
            }
            type Subject { value: i32 }
            trait SubjectCalls {
                func invoke_impl() -> i32;
            }
            impl SubjectCalls for Subject {
                func invoke_impl() -> i32 { strict_identity(-2) }
            }
            flow FfiFlow {
                state Idle
                state Done
                transition step(Idle) -> Done {
                    let checked = strict_identity(-3)
                    return Done { }
                }
            }
            func main() -> i32 { 0 }
        "#,
    )
    .expect("write actor method FFI fixture");

    let verify = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("verify")
        .arg(&source)
        .env("MIMI_VERBOSE", "1")
        .output()
        .expect("spawn default actor-method verification");
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(
        !verify.status.success(),
        "actor-method FFI call-site preconditions must fail closed: {transcript}"
    );
    assert!(
        transcript.contains("func invoke_actor()")
            && transcript.contains("func invoke_impl()")
            && transcript.contains("let checked = strict_identity(-3)")
            && transcript.matches("may violate precondition").count() >= 3,
        "missing method/Flow FFI call-site diagnostics: {transcript}"
    );
    assert!(
        transcript.contains("Disproven") || transcript.contains("may violate precondition"),
        "expected a failed actor-method FFI precondition: {transcript}"
    );

    let dump = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("verify")
        .arg("--dump-z3")
        .arg(&source)
        .output()
        .expect("spawn dump-z3 actor-method verification");
    let dump_transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&dump.stdout),
        String::from_utf8_lossy(&dump.stderr)
    );
    assert!(
        !dump.status.success(),
        "dump-z3 must retain the same called FFI obligation: {dump_transcript}"
    );
    assert!(
        dump_transcript.contains("func invoke_actor()")
            && dump_transcript.contains("func invoke_impl()")
            && dump_transcript.contains("let checked = strict_identity(-3)")
            && dump_transcript.matches("may violate precondition").count() >= 3,
        "dump-z3 omitted method/Flow FFI call-site diagnostics: {dump_transcript}"
    );
    assert!(dump_transcript.contains("Z3 SMT-LIB2 dump"));

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn default_build_and_verify_keep_export_compatibility_ahead_of_maproot() {
    let dir = fresh_dir();
    fs::create_dir_all(&dir).expect("create isolated export/MapRoot directory");
    let source = dir.join("export_with_maproot.mimi");
    let executable = dir.join("export_with_maproot");
    fs::write(
        &source,
        r#"
            extern "C" func internal_export(value: i32) -> i32 { value + 1 }
            func map_contract() -> i32 {
                ensures: result == 1
                1
            }
            func main() -> i32 {
                let root = map_new()
                let inserted = map_set(root, "answer", 42)
                let size = map_size(inserted)
                drop(inserted)
                println(internal_export(size))
                0
            }
        "#,
    )
    .expect("write export plus MapRoot fixture");

    let build = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("build")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .env("MIMI_VERBOSE", "1")
        .output()
        .expect("spawn export plus MapRoot native build");
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build.status.success(),
        "compatibility build failed: {build_stderr}"
    );
    assert!(build_stderr.contains("legacy (exported-abi-body-compatibility)"));
    let native = Command::new(&executable)
        .output()
        .expect("run export plus MapRoot executable");
    assert!(native.status.success());
    assert_eq!(native.stdout, b"2\n");

    let verify = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("verify")
        .arg(&source)
        .env("MIMI_VERBOSE", "1")
        .output()
        .expect("spawn export plus MapRoot verification");
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(
        verify.status.success(),
        "compatibility verify failed: {transcript}"
    );
    assert!(transcript.contains("legacy (exported-abi-body-compatibility)"));
    assert!(transcript.contains("map_contract"));
    assert!(!transcript.contains("MIR-MATERIALIZATION-001"));

    fs::remove_dir_all(&dir).ok();
}
