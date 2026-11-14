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

#[test]
fn narrow_map_any_v2_profiles_reject_scalar_ffi_without_default_or_explicit_fallback() {
    const DECLARATIONS: &str = r#"
extern "C" { func scalar_probe(value: i32) -> i32; }
func unused_scalar_ffi_helper() -> i32 { scalar_probe(0) }
"#;
    let computed_key = format!(
        "{DECLARATIONS}\n{}",
        include_str!("fixtures/mir_ordinary_map_any_owner_v2_computed_key_get.mimi")
    );
    let checked_get = format!(
        r#"
{DECLARATIONS}
func main() -> bool {{
    let empty = map_new()
    let root = map_set(empty, "answer", 41)
    let pair = map_get(root, "answer")
    drop(empty)
    drop(root)
    let found = pair.0
    let payload = pair.1 as i32
    drop(pair)
    found
}}
"#
    );
    let cases = [
        (
            "computed-key-get",
            computed_key.as_str(),
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::ComputedKeyGet,
        ),
        (
            "checked-get",
            checked_get.as_str(),
            mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::CheckedGet,
        ),
    ];

    let mimi = mimi_bin();
    for (profile, source, expected_admission) in cases {
        let tokens = mimi::lexer::Lexer::new(source)
            .tokenize()
            .unwrap_or_else(|error| panic!("{profile} mixed source lexes: {error}"));
        let file = mimi::parser::Parser::new(tokens)
            .parse_file()
            .unwrap_or_else(|error| panic!("{profile} mixed source parses: {error}"));
        let checked = mimi::core::check_program(&file)
            .unwrap_or_else(|error| panic!("{profile} mixed source typechecks: {error:?}"));
        let admission = mimi::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(
            admission.scalar_ffi,
            "{profile} fixture must retain scalar FFI"
        );
        assert_eq!(
            admission.ordinary_map_owned_any_v2, expected_admission,
            "{profile} must retain its exact narrow Map/Any owner profile"
        );
        const {
            assert!(
                !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
                "narrow route admission must not open global Map/Any V2 readiness"
            );
        };

        let stem = format!("mimi-ffi-{profile}-composition-{}", std::process::id());
        let source_path = std::env::temp_dir().join(format!("{stem}.mimi"));
        let binary_path = std::env::temp_dir().join(format!("{stem}-built"));
        std::fs::write(&source_path, source)
            .unwrap_or_else(|error| panic!("write {profile} source: {error}"));

        for explicit_mir in [false, true] {
            for mode in ["run", "build", "verify"] {
                let label = if explicit_mir {
                    format!("explicit --mir {mode}")
                } else {
                    format!("default {mode}")
                };
                let mut args = vec![std::ffi::OsString::from(mode)];
                if explicit_mir {
                    args.push("--mir".into());
                }
                args.push(source_path.as_os_str().to_os_string());
                if mode == "build" {
                    args.push("--output".into());
                    args.push(binary_path.as_os_str().to_os_string());
                }
                let output = Command::new(&mimi)
                    .args(args)
                    .env("MIMI_VERBOSE", "1")
                    .output()
                    .unwrap_or_else(|error| panic!("start {profile} {label}: {error}"));
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    !output.status.success(),
                    "{profile} {label} must reject Map-owner plus scalar FFI: stdout={stdout:?} stderr={stderr:?}"
                );
                assert!(
                    stdout.is_empty(),
                    "{profile} {label} must reject before execution: {stdout:?}"
                );
                assert!(
                    stderr.contains("MIR-COVERAGE-001")
                        && stderr.contains("cannot be combined")
                        && stderr.contains("scalar FFI"),
                    "{profile} {label} must report the no-combined-route boundary: {stderr}"
                );
                assert!(
                    !stderr.contains("canonical route disposition: legacy"),
                    "{profile} {label} must not continue through a legacy fallback: {stderr}"
                );
                if mode == "build" {
                    assert!(
                        !binary_path.exists(),
                        "{profile} {label} must not leave a native binary"
                    );
                }
            }
        }

        let _ = std::fs::remove_file(&source_path);
        let _ = std::fs::remove_file(&binary_path);
    }
}
