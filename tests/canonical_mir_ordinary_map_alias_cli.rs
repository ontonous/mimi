use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return path.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide mimi for ordinary Map aliases and trap-domain evidence")
}

fn assert_route(output: &Output, profile: &str, mode: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "canonical route disposition: canonical ({profile}) mir_digest="
        )),
        "{mode} did not use the independently receipted ordinary Map route: {stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} fell back: {stderr}"
    );
}

fn temporary_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "mimi-ordinary-map-{label}-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&directory).expect("create isolated Map evidence directory");
    directory
}

fn assert_memcheck(binary: &Path, expected_success: bool) {
    if Command::new("valgrind").arg("--version").output().is_err() {
        eprintln!("Valgrind unavailable: ordinary Map native L3 case remains unverified");
        return;
    }
    let output = Command::new("valgrind")
        .args([
            "--leak-check=full",
            "--show-leak-kinds=all",
            "--errors-for-leak-kinds=definite,indirect,possible",
            "--error-exitcode=97",
        ])
        .arg(binary)
        .output()
        .expect("strict Memcheck ordinary Map native executable");
    let report = String::from_utf8_lossy(&output.stderr);
    assert_ne!(
        output.status.code(),
        Some(97),
        "Memcheck found an ordinary Map memory error: {report}"
    );
    if expected_success {
        assert!(
            output.status.success(),
            "normal Map native case failed under Memcheck: {report}"
        );
    } else {
        assert_eq!(
            output.status.code(),
            Some(1),
            "native trap must preserve status 1: {report}"
        );
    }
    let all_freed = report.contains("All heap blocks were freed -- no leaks are possible");
    for category in ["definitely lost", "indirectly lost", "possibly lost"] {
        assert!(
            all_freed || report.contains(&format!("{category}: 0 bytes in 0 blocks")),
            "strict {category} check failed: {report}"
        );
    }
    assert!(
        report.contains("ERROR SUMMARY: 0 errors"),
        "native memory errors: {report}"
    );
    eprintln!("{report}");
}

#[test]
fn ordinary_map_aliases_and_scope_cleanup_use_default_and_direct_routes() {
    let mimi = mimi_bin();
    for (fixture, profile) in [
        (
            "mir_ordinary_map_alias_scope.mimi",
            "ordinary-map-owner-lifecycle-i32-string-v1",
        ),
        (
            "mir_ordinary_map_owned_get_alias_scope.mimi",
            "ordinary-map-owned-get-i32-string-v1",
        ),
    ] {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let directory = temporary_directory(fixture);
        for direct in [false, true] {
            let mut command = Command::new(&mimi);
            command
                .env("MIMI_ROUTE_CENSUS", "1")
                .arg("run")
                .arg(&source);
            if direct {
                command.arg("--mir");
            }
            let run = command.output().expect("run ordinary aliases");
            assert!(
                run.status.success(),
                "Map run failed: {}",
                String::from_utf8_lossy(&run.stderr)
            );
            assert!(run.stdout.is_empty());
            if !direct {
                assert_route(&run, profile, "run");
            }

            let binary = directory.join(if direct { "direct" } else { "default" });
            let mut command = Command::new(&mimi);
            command
                .env("MIMI_ROUTE_CENSUS", "1")
                .arg("build")
                .arg(&source)
                .arg("--output")
                .arg(&binary);
            if direct {
                command.arg("--mir");
            }
            let build = command.output().expect("build ordinary aliases");
            assert!(
                build.status.success(),
                "Map build failed: {}",
                String::from_utf8_lossy(&build.stderr)
            );
            if !direct {
                assert_route(&build, profile, "build");
            }
            let native = Command::new(&binary)
                .output()
                .expect("execute ordinary aliases");
            assert!(
                native.status.success(),
                "native aliases failed: {}",
                String::from_utf8_lossy(&native.stderr)
            );
            assert!(native.stdout.is_empty());
            assert_memcheck(&binary, true);

            let mut command = Command::new(&mimi);
            command
                .env("MIMI_ROUTE_CENSUS", "1")
                .arg("verify")
                .arg(&source);
            if direct {
                command.arg("--mir");
            }
            let verify = command.output().expect("verify ordinary aliases");
            assert!(
                verify.status.success(),
                "Map verify failed: {}",
                String::from_utf8_lossy(&verify.stderr)
            );
            if !direct {
                assert_route(&verify, profile, "verify");
            }
        }
        std::fs::remove_dir_all(directory).expect("remove Map evidence directory");
    }
}

#[test]
fn ordinary_map_domain_releases_caller_roots_when_a_scalar_callee_traps() {
    let mimi = mimi_bin();
    let directory = temporary_directory("callee-traps");
    for (label, helper, code, snapshots, profile) in [
        (
            "division",
            "func trap_math(z: i32) -> i32 { 1 / z }",
            "E0801",
            "",
            "ordinary-map-owner-lifecycle-i32-string-v1",
        ),
        (
            "overflow",
            "func trap_math(z: i32) -> i32 { let maximum = 2147483647; maximum + z + 1 }",
            "E0802", "", "ordinary-map-owner-lifecycle-i32-string-v1",
        ),
        (
            "retained-snapshot-division",
            "func trap_math(z: i32) -> i32 { 1 / z }",
            "E0801",
            "let key = \"text\"; let snapshot = map_get_string(alias, key); let text = snapshot.unwrap();",
            "ordinary-map-owned-get-i32-string-v1",
        ),
        (
            "retained-snapshot-overflow",
            "func trap_math(z: i32) -> i32 { let maximum = 2147483647; maximum + z + 1 }",
            "E0802",
            "let key = \"text\"; let snapshot = map_get_string(alias, key); let text = snapshot.unwrap();",
            "ordinary-map-owned-get-i32-string-v1",
        ),
    ] {
        let source = directory.join(format!("{label}.mimi"));
        let cleanup = if snapshots.is_empty() { "" } else { "drop(snapshot); drop(text); drop(key)" };
        std::fs::write(
            &source,
            format!(
                r#"
{helper}
func main() -> i32 {{
    let empty = map_new()
    let numbers = map_set(empty, "number", 42)
    let mixed = map_set(numbers, "text", "kept")
    let alias = mixed
    {snapshots}
    let value = trap_math(map_size(empty))
    {cleanup}
    map_size(alias) + value
}}
"#
            ),
        )
        .expect("write cross-frame Map trap fixture");
        for direct in [false, true] {
            let mut command = Command::new(&mimi);
            command
                .env("MIMI_ROUTE_CENSUS", "1")
                .arg("run")
                .arg(&source);
            if direct {
                command.arg("--mir");
            }
            let run = command
                .output()
                .expect("run caller-root scalar trap fixture");
            assert_eq!(
                run.status.code(),
                Some(1),
                "VM trap should preserve failure status"
            );
            assert!(
                String::from_utf8_lossy(&run.stderr).contains(code),
                "VM missed original failure class: {}",
                String::from_utf8_lossy(&run.stderr)
            );
            if !direct {
                assert_route(
                    &run,
                    profile,
                    "trap run",
                );
            }
            let binary = directory.join(format!("{label}-{direct}"));
            let mut command = Command::new(&mimi);
            command
                .env("MIMI_ROUTE_CENSUS", "1")
                .arg("build")
                .arg(&source)
                .arg("--output")
                .arg(&binary);
            if direct {
                command.arg("--mir");
            }
            let build = command
                .output()
                .expect("build caller-root scalar trap fixture");
            assert!(
                build.status.success(),
                "trap fixture must be a supported production program: {}",
                String::from_utf8_lossy(&build.stderr)
            );
            if !direct {
                assert_route(
                    &build,
                    profile,
                    "trap build",
                );
            }
            let native = Command::new(&binary)
                .output()
                .expect("execute caller-root scalar trap fixture");
            assert_eq!(native.status.code(), Some(1));
            assert!(
                String::from_utf8_lossy(&native.stderr).contains(code),
                "native missed original failure class: {}",
                String::from_utf8_lossy(&native.stderr)
            );
            assert_eq!(native.stdout, run.stdout);
            assert_memcheck(&binary, false);
        }
    }
    std::fs::remove_dir_all(directory).expect("remove caller-root trap directory");
}
