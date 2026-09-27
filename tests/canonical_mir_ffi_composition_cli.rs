use std::path::PathBuf;
use std::process::Command;

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(|dir| dir.parent())
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide the mimi binary for FFI composition route evidence")
}

#[test]
fn default_cli_rejects_scalar_ffi_record_and_flow_compositions() {
    const CASES: &[(&str, &str)] = &[
        (
            "record",
            r#"
                type Point { x: i32, enabled: bool }
                extern "C" { func scalar_probe(x: i32) -> i32; }
                func main() -> i32 {
                    let point = Point { x: scalar_probe(40), enabled: true }
                    if point.enabled { point.x + 1 } else { 0 }
                }
            "#,
        ),
        (
            "flow",
            r#"
                extern "C" { func scalar_probe(x: i32) -> i32; }
                flow Counter {
                    state Zero { n: i32 }
                    transition inc(Zero) -> Zero {
                        return Zero { n: self.n + 1 }
                    }
                }
                func main() -> i32 {
                    let counter = Zero { n: scalar_probe(40) }
                    let next = Counter::inc(counter)
                    next.n
                }
            "#,
        ),
    ];

    let mimi = mimi_bin();
    for (profile, source) in CASES {
        let stem = format!("mimi-ffi-{profile}-composition-{}", std::process::id());
        let source_path = std::env::temp_dir().join(format!("{stem}.mimi"));
        let binary_path = std::env::temp_dir().join(format!("{stem}-built"));
        std::fs::write(&source_path, source)
            .unwrap_or_else(|error| panic!("write {profile} source: {error}"));

        let commands: [(&str, Vec<std::ffi::OsString>); 3] = [
            (
                "run",
                vec!["run".into(), source_path.as_os_str().to_os_string()],
            ),
            (
                "build",
                vec![
                    "build".into(),
                    source_path.as_os_str().to_os_string(),
                    "--output".into(),
                    binary_path.as_os_str().to_os_string(),
                ],
            ),
            (
                "verify",
                vec!["verify".into(), source_path.as_os_str().to_os_string()],
            ),
        ];
        for (mode, args) in commands {
            let output = Command::new(&mimi)
                .args(args)
                .env("MIMI_VERBOSE", "1")
                .output()
                .unwrap_or_else(|error| panic!("start {profile} mimi {mode}: {error}"));
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "{profile} {mode} must reject the mixed profile, got stdout={stdout:?} stderr={stderr:?}"
            );
            assert!(
                stdout.is_empty(),
                "{profile} {mode} must reject before execution: {stdout:?}"
            );
            assert!(
                stderr.contains("scalar FFI composition"),
                "{profile} {mode} did not report the shared composition boundary: {stderr}"
            );
            assert!(
                !stderr.contains("canonical route disposition: legacy"),
                "{profile} {mode} resumed in the legacy route: {stderr}"
            );
        }

        let _ = std::fs::remove_file(&source_path);
        let _ = std::fs::remove_file(&binary_path);
    }
}
