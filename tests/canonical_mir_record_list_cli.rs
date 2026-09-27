use std::path::{Path, PathBuf};
use std::process::Command;

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide the mimi binary for canonical Record/List CLI evidence")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn accepted_record_and_list_profiles_use_default_run_build_and_verify_routes() {
    let mimi = mimi_bin();
    let cases = [
        (
            "flat-copy-record-v1",
            "mir_native_record_copy.mimi",
            42,
            false,
        ),
        (
            "copy-scalar-collection-v1",
            "mir_native_generic_list_construct.mimi",
            1,
            true,
        ),
    ];

    for (profile, fixture_name, expected_exit, has_contract) in cases {
        let source = fixture(fixture_name);
        let run = Command::new(&mimi)
            .arg("run")
            .arg(&source)
            .env("MIMI_VERBOSE", "1")
            .output()
            .unwrap_or_else(|error| panic!("{profile} default run: {error}"));
        let run_stderr = String::from_utf8_lossy(&run.stderr);
        assert_eq!(
            run.status.code(),
            Some(expected_exit),
            "{profile} default run failed: {run_stderr}"
        );
        assert!(
            run.stdout.is_empty(),
            "{profile} run stdout: {:?}",
            run.stdout
        );
        // The generic default dispatcher emits a verbose marker for legacy
        // dispositions, while a few dedicated islands also print a positive
        // canonical receipt. For this profile, the CLI tripwire is the
        // absence of that legacy marker; the same-MIR unit test above proves
        // the exact profile receipt and every consumer's parity.
        assert!(
            !run_stderr.contains("canonical route disposition: legacy"),
            "{profile} default run used compatibility code: {run_stderr}"
        );

        let binary = std::env::temp_dir().join(format!(
            "mimi-canonical-{profile}-default-{}",
            std::process::id()
        ));
        let build = Command::new(&mimi)
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&binary)
            .env("MIMI_VERBOSE", "1")
            .output()
            .unwrap_or_else(|error| panic!("{profile} default build: {error}"));
        let build_stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            build.status.success(),
            "{profile} default build failed: {build_stderr}"
        );
        assert!(
            !build_stderr.contains("canonical route disposition: legacy"),
            "{profile} default build used compatibility code: {build_stderr}"
        );
        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("{profile} native execution: {error}"));
        let _ = std::fs::remove_file(&binary);
        assert_eq!(
            native.status.code(),
            Some(expected_exit),
            "{profile} native exit"
        );
        assert!(
            native.stdout.is_empty(),
            "{profile} native stdout: {:?}",
            native.stdout
        );
        assert!(
            native.stderr.is_empty(),
            "{profile} native stderr: {:?}",
            native.stderr
        );

        let verify = Command::new(&mimi)
            .arg("verify")
            .arg(&source)
            .env("MIMI_VERBOSE", "1")
            .output()
            .unwrap_or_else(|error| panic!("{profile} default verify: {error}"));
        let verify_stdout = String::from_utf8_lossy(&verify.stdout);
        let verify_stderr = String::from_utf8_lossy(&verify.stderr);
        assert!(
            verify.status.success(),
            "{profile} default verify failed: {verify_stdout}\n{verify_stderr}"
        );
        assert!(
            !verify_stderr.contains("canonical route disposition: legacy"),
            "{profile} verifier used compatibility code: {verify_stderr}"
        );
        if has_contract {
            assert!(
                verify_stdout.contains("canonical MIR ensures contract proven"),
                "{profile} verifier did not prove the fixture contract: {verify_stdout}"
            );
            assert!(
                verify_stderr.contains("canonical MIR verifier provenance:"),
                "{profile} verifier omitted its same-route proof provenance: {verify_stderr}"
            );
        } else {
            assert!(
                verify_stdout.contains("No contracts to verify"),
                "{profile} no-contract verify must remain an empty verification request: {verify_stdout}"
            );
        }
    }
}
