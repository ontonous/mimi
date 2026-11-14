use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn test_dir() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "mimi-generic-first-argument-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create generic projection CLI test directory");
    dir
}

fn build(source: &PathBuf, binary: &PathBuf, explicit_mir: bool) -> Output {
    let mut command = Command::new(mimi_bin());
    command
        .current_dir(project_root())
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("build")
        .arg(source);
    if explicit_mir {
        command.arg("--mir");
    }
    command.arg("-o").arg(binary);
    command.output().expect("spawn Mimi native build")
}

#[test]
fn generic_first_argument_projection_builds_on_default_and_explicit_mir_routes() {
    for (label, declaration, signature, call, expected_exit) in [
        (
            "baseline-first-i32",
            "func choose<T>(left: T, right: T) -> T { left }",
            "-> i32",
            "choose(42, 43)",
            42,
        ),
        (
            "first-i64",
            "func choose<T>(left: T, right: T) -> T { left }",
            "-> i64",
            "choose(2147483690, 2147483691)",
            42,
        ),
        (
            "second-i32",
            "func choose<T>(left: T, right: T) -> T { right }",
            "-> i32",
            "choose(41, 43)",
            43,
        ),
        (
            "local-alias-i32",
            "func choose<T>(left: T, right: T) -> T { let chosen = right; chosen }",
            "-> i32",
            "choose(41, 43)",
            43,
        ),
        (
            "middle-i32",
            "func choose<T>(a: T, b: T, c: T) -> T { b }",
            "-> i32",
            "choose(41, 42, 43)",
            42,
        ),
        (
            "third-i64",
            "func choose<T>(a: T, b: T, c: T) -> T { c }",
            "-> i64",
            "choose(2147483690, 4294967339, 6442450988)",
            44,
        ),
        (
            "seventh-i64",
            "func choose<T>(a: T, b: T, c: T, d: T, e: T, f: T, g: T) -> T { g }",
            "-> i64",
            "choose(2147483689, 4294967338, 6442450987, 8589934636, 10737418285, 12884901934, 15032385583)",
            47,
        ),
    ] {
        let dir = test_dir();
        let source = dir.join(format!("{label}.mimi"));
        fs::write(
            &source,
            format!("{declaration}\nfunc main() {signature} {{ {call} }}\n"),
        )
        .expect("write generic projection program");

        for explicit_mir in [false, true] {
            let route = if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            };
            let binary = dir.join(format!("{label}-{route}"));
            let build_output = build(&source, &binary, explicit_mir);
            assert!(
                build_output.status.success(),
                "{label} {route} build failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&build_output.stdout),
                String::from_utf8_lossy(&build_output.stderr)
            );
            let stderr = String::from_utf8_lossy(&build_output.stderr);
            assert!(
                !stderr.contains("canonical route disposition: legacy"),
                "{label} {route} build disclosed a legacy fallback: {stderr}"
            );
            let expected_profile = if signature == "-> i32" {
                "generic-scalar-identity-i32-v1"
            } else {
                "generic-scalar-identity-i64-v1"
            };
            assert!(
                stderr.contains(&format!("checker_profile={expected_profile}"))
                    && stderr.contains(&format!("build_route_profile={expected_profile}"))
                    && stderr.contains(&format!("route_id={expected_profile}")),
                "{label} {route} build selected the wrong or incomplete canonical generic profile: {stderr}"
            );
            let run = Command::new(&binary)
                .output()
                .unwrap_or_else(|error| panic!("run {label} {route} native binary: {error}"));
            assert_eq!(
                run.status.code(),
                Some(expected_exit),
                "{label} {route} native result mismatch; stderr: {}",
                String::from_utf8_lossy(&run.stderr)
            );
        }
        fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn generic_argument_projection_with_multiple_bindings_fails_closed_on_default_and_explicit_routes()
{
    let dir = test_dir();
    let source = dir.join("multiple-bindings.mimi");
    fs::write(
        &source,
        "func choose<T>(left: T, right: T) -> T { let chosen = right; let second = chosen; second }\nfunc main() -> i32 { choose(41, 43) }\n",
    )
    .expect("write unsupported multiple-binding generic program");

    for explicit_mir in [false, true] {
        let route = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let binary = dir.join(format!("multiple-bindings-{route}"));
        let output = build(&source, &binary, explicit_mir);
        assert!(
            !output.status.success(),
            "unsupported multiple-binding {route} build unexpectedly succeeded"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        if explicit_mir {
            assert!(
                stderr.contains("canonical MIR build error: MIR lowering failed")
                    && stderr.contains(
                        "generic MIR instance body does not match an admitted canonical scalar contract"
                    ),
                "multiple-binding {route} rejection lost its exact MIR shape diagnostic: {stderr}"
            );
        } else {
            assert!(
                stderr.contains("MIR-COVERAGE-001")
                    && (stderr.contains("NITS") || stderr.contains("coverage")),
                "multiple-binding {route} rejection lost its canonical coverage diagnostic: {stderr}"
            );
        }
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "multiple-binding {route} failure disclosed a compatibility fallback: {stderr}"
        );
        assert!(
            !binary.exists(),
            "multiple-binding {route} rejection left a native binary"
        );
    }
    fs::remove_dir_all(&dir).ok();
}
