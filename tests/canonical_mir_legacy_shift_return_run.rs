use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "mimi-legacy-shift-return-cli-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ))
}

fn route_admission_line(stderr: &str, mode: &str) -> String {
    stderr
        .lines()
        .find(|line| {
            line.starts_with("MIMI_ROUTE_CENSUS route-admission consumer=run")
                && line.contains(&format!("mode={mode}"))
        })
        .unwrap_or_else(|| panic!("missing run {mode} route receipt in stderr:\n{stderr}"))
        .to_owned()
}

fn census_value<'a>(line: &'a str, key: &str) -> &'a str {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(key))
        .unwrap_or_else(|| panic!("route census line has no {key}: {line}"))
}

#[test]
fn default_run_migrates_only_exact_shift_return_to_checked_ast_free_bytecode() {
    let dir = temp_dir();
    fs::create_dir_all(&dir).expect("create temp directory");
    let source = dir.join("shift_return.mimi");
    fs::write(&source, "func main() -> i32 {\n    3 << 2\n}").expect("write source");

    let default = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("run")
        .arg(&source)
        .env("MIMI_ROUTE_CENSUS", "1")
        .output()
        .expect("spawn default run");
    let default_stderr = String::from_utf8_lossy(&default.stderr);
    assert_eq!(default.status.code(), Some(12), "{default_stderr}");
    assert_eq!(default.stdout, b"");
    assert!(
        default_stderr.contains(
            "canonical route disposition: canonical (run-legacy-remainder-shift-return-v1)"
        ),
        "default run must report the exact checked MIR bytecode profile: {default_stderr}"
    );
    assert!(
        !default_stderr.contains("canonical route disposition: legacy"),
        "the exact route must not call the AST compatibility consumer: {default_stderr}"
    );
    let line = route_admission_line(&default_stderr, "default");
    assert_eq!(
        census_value(&line, "checker_profile="),
        "run-legacy-remainder-shift-return-v1"
    );
    for key in ["mir_digest=", "receipt_identity_digest="] {
        let digest = census_value(&line, key);
        assert_eq!(digest.len(), 64, "invalid {key} in {line}");
        assert!(
            digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid {key} in {line}"
        );
    }

    // Explicit --mir follows its own checked route contract. It must either
    // execute the same MIR result or fail as a canonical error; it must never
    // use the compatibility AST compiler.
    let explicit = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("run")
        .arg(&source)
        .arg("--mir")
        .env("MIMI_ROUTE_CENSUS", "1")
        .output()
        .expect("spawn explicit MIR run");
    let explicit_stderr = String::from_utf8_lossy(&explicit.stderr);
    assert_eq!(explicit.status.code(), Some(12), "{explicit_stderr}");
    assert_eq!(explicit.stdout, b"");
    assert!(
        explicit_stderr.contains("canonical route disposition: explicit canonical"),
        "explicit --mir must report its checked canonical disposition: {explicit_stderr}"
    );
    assert!(
        !explicit_stderr.contains("canonical route disposition: legacy"),
        "explicit --mir must not retry through the compatibility compiler: {explicit_stderr}"
    );
    let explicit_line = route_admission_line(&explicit_stderr, "explicit-mir");
    assert_eq!(
        census_value(&explicit_line, "checker_profile="),
        "run-canonical-v1"
    );

    fs::remove_dir_all(dir).ok();
}

#[test]
fn other_plain_scalar_shift_sources_keep_their_existing_legacy_behavior() {
    let dir = temp_dir();
    fs::create_dir_all(&dir).expect("create temp directory");
    let source = dir.join("shift_with_output.mimi");
    fs::write(
        &source,
        "func main() -> i32 {\n    let base = 3\n    let shifted = base << 2\n    println(shifted)\n    0\n}",
    )
    .expect("write source");

    let default = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("run")
        .arg(&source)
        .env("MIMI_ROUTE_CENSUS", "1")
        .output()
        .expect("spawn compatibility run");
    let stderr = String::from_utf8_lossy(&default.stderr);
    assert!(default.status.success(), "{stderr}");
    assert_eq!(default.stdout, b"12\n");
    assert!(
        stderr.contains(
            "canonical route disposition: legacy (plain-scalar-island-preflight-compatibility)"
        ),
        "nonmatching shift source must retain the named legacy disposition: {stderr}"
    );

    let explicit = Command::new(env!("CARGO_BIN_EXE_mimi"))
        .arg("run")
        .arg(&source)
        .arg("--mir")
        .env("MIMI_ROUTE_CENSUS", "1")
        .output()
        .expect("spawn explicit checked MIR run");
    let explicit_stderr = String::from_utf8_lossy(&explicit.stderr);
    assert_eq!(explicit.status.code(), Some(0), "{explicit_stderr}");
    assert_eq!(explicit.stdout, b"12\n");
    assert!(
        explicit_stderr.contains("canonical route disposition: explicit canonical"),
        "explicit --mir must use its own checked route contract: {explicit_stderr}"
    );
    assert!(
        !explicit_stderr.contains("canonical route disposition: legacy"),
        "explicit --mir must not retry this shape through the legacy AST compiler: {explicit_stderr}"
    );

    fs::remove_dir_all(dir).ok();
}
