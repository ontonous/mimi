use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SOURCE: &str =
    include_str!("../tests/fixtures/mir_ordinary_map_owned_any_v2_direct_tuple_get_l1.mimi");
const CHECKER_PROFILE: &str = mimi::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L1_CORE_PROFILE;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn write_output(directory: &Path, name: &str, output: &Output) {
    fs::write(directory.join(format!("{name}.stdout.log")), &output.stdout)
        .unwrap_or_else(|error| panic!("write {name} stdout: {error}"));
    fs::write(directory.join(format!("{name}.stderr.log")), &output.stderr)
        .unwrap_or_else(|error| panic!("write {name} stderr: {error}"));
    fs::write(
        directory.join(format!("{name}.exit-status")),
        output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "terminated-by-signal".into()),
    )
    .unwrap_or_else(|error| panic!("write {name} status: {error}"));
}

fn run_cli(explicit_mir: bool, source: &Path, binary: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mimi"));
    for key in std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_string_lossy().starts_with("MIMI_"))
        .collect::<Vec<_>>()
    {
        command.env_remove(key);
    }
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("build");
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg(source)
        .arg("--output")
        .arg(binary)
        .output()
        .unwrap_or_else(|error| panic!("start Mimi native build: {error}"))
}

fn route_census_value<'a>(stderr: &'a str, field_name: &str) -> &'a str {
    let line = stderr
        .lines()
        .find(|line| line.contains("MIMI_ROUTE_CENSUS route-admission"))
        .unwrap_or_else(|| panic!("missing route-admission census in stderr: {stderr}"));
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{field_name}=")))
        .unwrap_or_else(|| panic!("missing {field_name} in route census: {line}"))
}

fn sha256(bytes: &[u8]) -> String {
    use std::io::Write as _;
    use std::process::Stdio;

    let mut child = Command::new("sha256sum")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("sha256sum is available for L3 source identity");
    child
        .stdin
        .take()
        .expect("sha256sum stdin is piped")
        .write_all(bytes)
        .expect("write source bytes to sha256sum");
    let output = child
        .wait_with_output()
        .expect("wait for sha256sum source identity");
    assert!(
        output.status.success(),
        "sha256sum failed: {}",
        text(&output.stderr)
    );
    text(&output.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum emitted a digest")
        .to_owned()
}

#[test]
#[ignore = "strict production L3 gate; run scripts/verify-ordinary-map-direct-tuple-get-l3.sh"]
fn direct_tuple_get_default_and_explicit_native_builds_are_strict_memcheck_clean() {
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "this L3 gate requires Valgrind rather than silently skipping it"
    );
    let evidence_root = PathBuf::from(
        std::env::var_os("MIMI_DIRECT_TUPLE_GET_L3_ARTIFACT_DIR")
            .expect("runner supplies an external, attempt-specific evidence directory"),
    );
    fs::create_dir_all(&evidence_root).unwrap_or_else(|error| {
        panic!("create evidence root {}: {error}", evidence_root.display())
    });
    let attempt = evidence_root.join("attempt-1");
    fs::create_dir(&attempt).unwrap_or_else(|error| {
        panic!(
            "create fresh attempt directory {}: {error}",
            attempt.display()
        )
    });

    let source = attempt.join("direct-tuple-get.mimi");
    fs::write(&source, SOURCE).expect("write exact checked fixture source");
    let source_digest = sha256(SOURCE.as_bytes());
    fs::write(
        attempt.join("source.sha256"),
        format!("{source_digest}  direct-tuple-get.mimi\n"),
    )
    .expect("record exact fixture hash");

    let version = Command::new("valgrind")
        .arg("--version")
        .output()
        .expect("Valgrind must be installed for strict L3");
    assert!(
        version.status.success(),
        "Valgrind version query failed: {}",
        text(&version.stderr)
    );
    fs::write(attempt.join("valgrind.version.txt"), text(&version.stdout))
        .expect("record Valgrind version");

    let mut accepted_identity: Option<(String, String)> = None;
    let mut summary = String::new();
    for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
        let mode_dir = attempt.join(mode);
        fs::create_dir(&mode_dir)
            .unwrap_or_else(|error| panic!("create {mode} artifact directory: {error}"));
        let binary = mode_dir.join("direct-tuple-get-native");
        let build = run_cli(explicit_mir, &source, &binary);
        write_output(&mode_dir, "build", &build);
        assert!(
            build.status.success(),
            "{mode} native build failed:\nstdout:\n{}\nstderr:\n{}",
            text(&build.stdout),
            text(&build.stderr),
        );
        assert!(
            binary.is_file(),
            "{mode} build did not create {}",
            binary.display()
        );
        let stderr = text(&build.stderr);
        let checker_profile = route_census_value(&stderr, "checker_profile");
        assert_eq!(
            checker_profile, CHECKER_PROFILE,
            "{mode} selected another Checker profile"
        );
        let mir_digest = route_census_value(&stderr, "mir_digest").to_owned();
        let receipt_identity = route_census_value(&stderr, "receipt_identity_digest").to_owned();
        assert_eq!(mir_digest.len(), 64, "{mode} MIR digest is malformed");
        assert_eq!(
            receipt_identity.len(),
            64,
            "{mode} receipt digest is malformed"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains("canonical route disposition: compatibility")
                && !stderr.contains("VerifierOnly"),
            "{mode} build did not consume the executable L1 route: {stderr}"
        );
        if let Some((expected_mir, expected_receipt)) = &accepted_identity {
            assert_eq!(
                &mir_digest, expected_mir,
                "default and explicit routes changed MIR"
            );
            assert_eq!(
                &receipt_identity, expected_receipt,
                "default and explicit routes changed Checker receipt"
            );
        } else {
            accepted_identity = Some((mir_digest.clone(), receipt_identity.clone()));
        }
        fs::write(mode_dir.join("route-identity.txt"), format!(
            "checker_profile={checker_profile}\nmir_digest={mir_digest}\nreceipt_identity_digest={receipt_identity}\n"
        )).expect("record route identity");

        let memcheck = Command::new("valgrind")
            .args([
                "--leak-check=full",
                "--show-leak-kinds=all",
                "--errors-for-leak-kinds=definite,indirect,possible,reachable",
                "--error-exitcode=97",
            ])
            .arg(&binary)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap_or_else(|error| panic!("start {mode} native binary under Memcheck: {error}"));
        write_output(&mode_dir, "memcheck", &memcheck);
        assert!(
            memcheck.status.success(),
            "{mode} Memcheck failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
            memcheck.status.code(),
            text(&memcheck.stdout),
            text(&memcheck.stderr),
        );
        assert!(
            memcheck.stdout.is_empty(),
            "{mode} program unexpectedly printed output"
        );
        let memcheck_stderr = text(&memcheck.stderr);
        assert!(
            memcheck_stderr.contains("All heap blocks were freed -- no leaks are possible"),
            "{mode} Memcheck did not report complete heap release:\n{memcheck_stderr}"
        );
        assert!(
            memcheck_stderr.contains("ERROR SUMMARY: 0 errors"),
            "{mode} Memcheck reported errors:\n{memcheck_stderr}"
        );
        summary.push_str(&format!(
            "{mode}: build=PASS native_exit=0 stdout_bytes=0 memcheck=PASS mir_digest={mir_digest} receipt_identity_digest={receipt_identity}\n"
        ));
    }
    fs::write(attempt.join("run-summary.txt"), summary).expect("write L3 summary");
    assert_eq!(
        sha256(SOURCE.as_bytes()),
        source_digest,
        "fixture source changed during L3"
    );
}
