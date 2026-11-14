//! Direct public-API and CLI route probes for the retained 0.1.11 owners.
//!
//! Dynamic Protocol dispatch was removed from the surface contract. These
//! tests ensure that the default Bytecode compatibility route, the checked
//! native boundary, and both checked verifier entry points reject the same
//! Checker-issued receiver identities before they can emit output or proofs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const DYNAMIC_RECEIVER_CASES: [(&str, &str); 2] = [
    (
        "nested-field",
        r#"
trait Read { func read() -> i32 }
type Counter { value: i32 }
impl Read for Counter { func read() -> i32 { self.value } }
type Holder { reader: dyn Read }
func main() -> i32 {
    let counter = Counter { value: 41 }
    let dynamic: dyn Read = counter
    let holder = Holder { reader: dynamic }
    holder.reader.read()
}
"#,
    ),
    (
        "call-expression",
        r#"
trait Read { func read() -> i32 }
type Counter { value: i32 }
impl Read for Counter { func read() -> i32 { self.value } }
func pass(value: dyn Read) -> dyn Read { value }
func main() -> i32 {
    let counter = Counter { value: 41 }
    let dynamic: dyn Read = counter
    pass(dynamic).read()
}
"#,
    ),
];

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mimi-legacy-owner-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated probe directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn parse_source(source: &str) -> mimi::ast::File {
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("dynamic Protocol source lexes");
    mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("dynamic Protocol source parses")
}

fn checked_source(source: &str) -> (mimi::ast::File, mimi::core::resolved::CheckedProgram) {
    let file = parse_source(source);
    let checked = mimi::core::check_program(&file)
        .unwrap_or_else(|errors| panic!("dynamic Protocol source checks: {errors:?}"));
    (file, checked)
}

fn assert_dynamic_boundary_error(label: &str, error: &str) {
    assert!(
        error.contains("MIR-COVERAGE-001"),
        "{label} must retain the closed route diagnostic: {error}"
    );
    assert!(
        error.contains("dynamic Protocol method dispatch"),
        "{label} must identify the dynamic Protocol receiver: {error}"
    );
}

fn assert_dynamic_boundary_diagnostics(label: &str, errors: &[mimi::diagnostic::Diagnostic]) {
    assert!(
        errors.iter().any(|error| {
            error.code.as_deref() == Some("MIR-COVERAGE-001")
                && error.message.contains("dynamic Protocol method dispatch")
        }),
        "{label} must reject with the dynamic Protocol route diagnostic: {errors:?}"
    );
}

#[test]
fn default_bytecode_compatibility_route_executes_its_named_legacy_disposition_without_artifacts() {
    let dir = TempDir::new("bytecode-legacy-route");
    let source_path = dir.path().join("legacy-route.mimi");
    fs::write(
        &source_path,
        r#"
extern "C" { func uncalled_probe(x: i64) -> i64; }
func pick(v: Option<i64>) -> i64 {
    match v {
        Some(value) => { if value > 0 { value } else { 0 as i64 } }
        None => 0 as i64
    }
}
func main() -> i64 {
    println(pick(Some(3 as i64)))
    0
}
"#,
    )
    .expect("write explicit default-legacy fixture");

    let default = run_mimi(&["run"], &source_path);
    let stdout = String::from_utf8_lossy(&default.stdout);
    let stderr = String::from_utf8_lossy(&default.stderr);
    assert!(
        default.status.success(),
        "default legacy execution: {stderr}"
    );
    assert_eq!(stdout, "3\n");
    assert!(
        stderr.contains("canonical route disposition: legacy"),
        "the reachable Bytecode AST compatibility owner must be named: {stderr}"
    );

    let explicit = run_mimi(&["run", "--mir"], &source_path);
    let explicit_stderr = String::from_utf8_lossy(&explicit.stderr);
    assert_eq!(explicit.status.code(), Some(1), "{explicit_stderr}");
    assert!(
        explicit_stderr.contains("MIR-COVERAGE-001"),
        "explicit MIR must reject this outside-profile fixture instead of retrying Bytecode: {explicit_stderr}"
    );
    assert!(explicit.stdout.is_empty());
    assert_eq!(
        fs::read_dir(dir.path())
            .expect("list Bytecode compatibility artifacts")
            .count(),
        1,
        "default legacy execution and explicit rejection must leave only the source fixture"
    );
    eprintln!(
        "OWNER_PROBE_EXECUTED=default-bytecode-ast-compatibility;OWNER_ARTIFACTS_NONE=source-fixture-only"
    );
}

#[test]
fn bytecode_ast_compatibility_direct_api_rejects_dynamic_protocol_receivers() {
    for (label, source) in DYNAMIC_RECEIVER_CASES {
        let file = parse_source(source);
        let mut compiler = mimi::interp::bytecode::BytecodeCompiler::new();
        let error = compiler
            .compile_file(&file)
            .expect_err("raw-AST compatibility compiler must reject removed dyn Protocol dispatch");
        assert_dynamic_boundary_error(label, &error.to_string());
    }
    eprintln!(
        "OWNER_PROBE_EXECUTED=bytecode-ast-direct-api;OWNER_ARTIFACTS_NONE=bytecode-program-not-returned"
    );
}

#[test]
fn native_checked_api_rejects_dynamic_protocol_receivers_without_main_artifact() {
    let (_file, canonical) = checked_source("func main() -> i32 { 42 }");
    let context = inkwell::context::Context::create();
    let mut canonical_generator = mimi::codegen::CodeGenerator::new(&context, "canonical-scalar");
    canonical_generator
        .compile_checked(&canonical)
        .expect("canonical checked scalar must lower through native MIR");
    assert!(
        canonical_generator.module.get_function("main").is_some(),
        "supported canonical checked source must emit main"
    );

    for (label, source) in DYNAMIC_RECEIVER_CASES {
        let (_file, checked) = checked_source(source);
        let context = inkwell::context::Context::create();
        let mut generator = mimi::codegen::CodeGenerator::new(&context, label);
        let errors = generator
            .compile_checked(&checked)
            .expect_err("checked native API must reject removed dyn Protocol dispatch");
        assert_dynamic_boundary_diagnostics(label, &errors);
        assert!(
            generator.module.get_function("main").is_none(),
            "{label}: rejected checked native API must not emit a main function"
        );
    }
    eprintln!(
        "OWNER_PROBE_EXECUTED=native-checked-api;OWNER_ARTIFACTS_NONE=dynamic-main-absent;OWNER_CANONICAL_ROUTE=checked-native-main-emitted"
    );
}

#[test]
fn checked_verifier_actor_proof_compatibility_preserves_contracts_and_rejects_mir_without_artifacts(
) {
    let dir = TempDir::new("checked-actor-proof-route");
    let original = include_str!("real_world/actor_nested_func_capture.mimi");
    for (case, predicate, expected, success) in [
        ("proven", "result == x", "pass", false),
        ("disproven", "result != x", "fail", false),
    ] {
        let source_path = dir.path().join(format!("{case}.mimi"));
        fs::write(
            &source_path,
            format!(
                "{original}\nfunc guarded(x: i32) -> i32 {{\nrequires: x > 0\nensures: {predicate}\nx\n}}"
            ),
        )
        .expect("write Actor proof compatibility source");

        let default = run_mimi(&["verify", "--stats"], &source_path);
        let stdout = String::from_utf8_lossy(&default.stdout);
        let stderr = String::from_utf8_lossy(&default.stderr);
        assert_eq!(
            default.status.success(),
            success,
            "{case}: {stdout} {stderr}"
        );
        let guarded_line = stdout
            .lines()
            .find(|line| line.contains("guarded"))
            .unwrap_or_else(|| panic!("{case} proof row missing: {stdout}"));
        assert!(
            guarded_line.contains(expected),
            "{case} verdict missing from guarded proof row: {guarded_line}"
        );
        assert!(
            stderr.contains("actor-proof-obligations-compatibility"),
            "Actor proof owner must remain named: {stderr}"
        );

        let explicit = run_mimi(&["verify", "--mir"], &source_path);
        let explicit_stderr = String::from_utf8_lossy(&explicit.stderr);
        assert_eq!(explicit.status.code(), Some(1), "{case}: {explicit_stderr}");
        assert!(
            explicit_stderr.contains("cannot discard proof obligations"),
            "explicit MIR must reject the unclosed Actor proof request: {explicit_stderr}"
        );
        assert!(explicit.stdout.is_empty());
    }
    let remaining = fs::read_dir(dir.path())
        .expect("list Actor proof route artifacts")
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let expected = ["disproven.mimi", "proven.mimi"]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        remaining, expected,
        "Actor proof route must not save artifacts"
    );
    eprintln!(
        "OWNER_PROBE_EXECUTED=checked-verifier-actor-proof-route;OWNER_ARTIFACTS_NONE=source-fixtures-only"
    );
}

#[test]
fn checked_verifier_apis_reject_dynamic_protocol_receivers_without_proof_results() {
    for (label, source) in DYNAMIC_RECEIVER_CASES {
        let (_file, checked) = checked_source(source);
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();

        let single = mimi::verifier::verify_checked(&checked, source_hash.clone())
            .expect_err("single checked verifier must reject removed dyn Protocol dispatch");
        assert_dynamic_boundary_error(&format!("{label} verify_checked"), &single);

        let dual = mimi::verifier::verify_checked_dual(&checked, source_hash.clone())
            .expect_err("dual checked verifier must reject removed dyn Protocol dispatch");
        assert_dynamic_boundary_error(&format!("{label} verify_checked_dual"), &dual);

        let mut verifier = mimi::verifier::Verifier::new()
            .expect("configured verifier is available for direct source API boundary probe");
        let source_result = mimi::verifier::verify_source_with(source, &mut verifier)
            .expect_err("configured source verifier must reject removed dyn Protocol dispatch");
        assert_dynamic_boundary_error(&format!("{label} verify_source_with"), &source_result);
    }
    eprintln!(
        "OWNER_PROBE_EXECUTED=checked-verifier-actor-proof-api;OWNER_ARTIFACTS_NONE=all-apis-returned-errors"
    );
}

#[test]
fn ffi_verifier_only_owner_proves_and_defers_without_execution_or_file_artifacts() {
    assert!(
        mimi::verifier::is_z3_available(),
        "FFI verifier-only owner probe requires Z3; this test must never count a skip as a pass"
    );
    let dir = TempDir::new("ffi-verifier-only-owner");
    for (case, ensures, status, cli_status) in [
        (
            "proven",
            "len(result.label) >= 0",
            mimi::verifier::VerifStatus::Proven,
            true,
        ),
        (
            "disproven",
            "len(result.label) > 0",
            mimi::verifier::VerifStatus::Disproven,
            false,
        ),
        (
            "unsupported",
            "char_at(result.label, 0) == \"x\"",
            mimi::verifier::VerifStatus::NotInTrustedSubset,
            false,
        ),
    ] {
        let source = format!(
            r#"
#[repr(C)] type Pair {{ value: i64, label: string }}
extern "C" {{
    func foreign(value: Pair) -> Pair
        requires: value.value >= 0 && len(value.label) > 0
        ensures: {ensures};
}}
func caller(value: Pair) -> Pair {{
    requires: value.value >= 0 && len(value.label) > 0
    foreign(value)
}}
func main() -> i64 {{ 0 }}
"#
        );
        let (_file, checked) = checked_source(&source);
        let source_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let direct =
            mimi::verifier::verify_ffi_checked_with_source_hash(&checked, source_hash.clone())
                .expect("verifier-only FFI direct API result");
        assert_eq!(direct.len(), 1, "{case}: {direct:?}");
        assert_eq!(direct[0].status, status, "{case}: {direct:?}");
        assert_eq!(
            direct[0]
                .artifact
                .as_ref()
                .and_then(|artifact| artifact.mir_route_receipt.as_ref())
                .map(|receipt| receipt.profile.as_str()),
            (status != mimi::verifier::VerifStatus::NotInTrustedSubset)
                .then_some("verifier-ffi-owner-slice-v1"),
            "{case}: proof output must carry only the verifier-only receipt"
        );
        assert_eq!(
            direct[0].artifact.is_none(),
            status == mimi::verifier::VerifStatus::NotInTrustedSubset,
            "{case}: only unsupported Ensures has no proof artifact"
        );

        let dual = mimi::verifier::verify_checked_dual(&checked, source_hash.clone())
            .expect("dual verifier must preserve the proof-only FFI owner route");
        assert_eq!(dual.len(), 1, "{case}: {dual:?}");
        assert_eq!(dual[0].status, status, "{case}: {dual:?}");
        assert_eq!(
            dual[0].artifact.is_none(),
            status == mimi::verifier::VerifStatus::NotInTrustedSubset,
            "{case}: dual unsupported Ensures must not produce a proof artifact"
        );

        let source_path = dir.path().join(format!("{case}.mimi"));
        fs::write(&source_path, source).expect("write FFI owner CLI source");
        let cli = run_mimi(&["verify", "--stats"], &source_path);
        let stdout = String::from_utf8_lossy(&cli.stdout);
        let stderr = String::from_utf8_lossy(&cli.stderr);
        assert_eq!(
            cli.status.success(),
            cli_status,
            "{case}: {stdout} {stderr}"
        );
        assert!(
            stderr.contains("verifier-only (verifier-ffi-owner-slice-v1; no execution route)"),
            "{case}: default CLI must name the proof-only route: {stderr}"
        );
        assert!(stdout.contains("caller calls foreign"), "{case}: {stdout}");

        let binary = dir.path().join(format!("{case}.out"));
        let build = run_mimi_with_output(&["build"], &source_path, &binary);
        let build_stderr = String::from_utf8_lossy(&build.stderr);
        assert_eq!(build.status.code(), Some(1), "{case}: {build_stderr}");
        assert!(
            build_stderr.contains("MIR-FFI-DECLARATION-001"),
            "{case}: proof-only FFI must not authorize native execution: {build_stderr}"
        );
        assert!(!binary.exists(), "{case}: build must not create {binary:?}");
    }

    let remaining = fs::read_dir(dir.path())
        .expect("list FFI verifier-only route artifacts")
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let expected = ["disproven.mimi", "proven.mimi", "unsupported.mimi"]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(remaining, expected, "FFI proof route must not create files");
    eprintln!(
        "OWNER_PROBE_EXECUTED=ffi-verifier-only-owner;OWNER_ARTIFACTS_NONE=source-fixtures-only;PROOF_ARTIFACTS=expected-for-proven-and-disproven-only"
    );
}

fn run_mimi(arguments: &[&str], source: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mimi"));
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(arguments)
        .arg(source)
        .env("MIMI_VERBOSE", "1")
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "spawn Mimi {:?} for {}: {error}",
                arguments,
                source.display()
            )
        })
}

#[test]
fn default_and_explicit_routes_reject_dynamic_protocol_receiver_shapes_without_artifacts() {
    let dir = TempDir::new("dyn-protocol-routes");
    for (label, source) in DYNAMIC_RECEIVER_CASES {
        let source_path = dir.path().join(format!("{label}.mimi"));
        fs::write(&source_path, source).expect("write dynamic Protocol source");
        let default_binary = dir.path().join(format!("{label}-default.out"));
        let explicit_binary = dir.path().join(format!("{label}-explicit.out"));
        for (stage, output, artifact) in [
            ("run", run_mimi(&["run"], &source_path), None),
            ("disasm", run_mimi(&["disasm"], &source_path), None),
            ("verify", run_mimi(&["verify"], &source_path), None),
            ("run --mir", run_mimi(&["run", "--mir"], &source_path), None),
            (
                "verify --mir",
                run_mimi(&["verify", "--mir"], &source_path),
                None,
            ),
            (
                "build",
                run_mimi_with_output(&["build"], &source_path, &default_binary),
                Some(default_binary.as_path()),
            ),
            (
                "build --mir",
                run_mimi_with_output(&["build", "--mir"], &source_path, &explicit_binary),
                Some(explicit_binary.as_path()),
            ),
        ] {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{label} {stage} must reject before a compatibility consumer: {stderr}"
            );
            assert!(
                output.stdout.is_empty(),
                "{label} {stage} must not execute or emit a proof result: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(
                stderr.contains("MIR-COVERAGE-001")
                    && stderr.contains("dynamic Protocol method dispatch"),
                "{label} {stage} must identify the closed dynamic Protocol boundary: {stderr}"
            );
            assert!(
                !stderr.contains("canonical route disposition: legacy"),
                "{label} {stage} must not fall back to the Bytecode AST compatibility owner: {stderr}"
            );
            if let Some(artifact) = artifact {
                assert!(
                    !artifact.exists(),
                    "{label} {stage} must not create native artifact {}",
                    artifact.display()
                );
            }
        }
    }

    let remaining = fs::read_dir(dir.path())
        .expect("list direct-route probe artifacts")
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let expected = ["call-expression.mimi", "nested-field.mimi"]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        remaining, expected,
        "rejected routes must leave only their two source fixtures"
    );
    eprintln!(
        "OWNER_PROBE_EXECUTED=default-cli-dynamic-protocol-routes;OWNER_ARTIFACTS_NONE=source-fixtures-only"
    );
}

fn run_mimi_with_output(arguments: &[&str], source: &Path, output_path: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mimi"));
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(arguments)
        .arg(source)
        .arg("-o")
        .arg(output_path)
        .env("MIMI_VERBOSE", "1")
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "spawn Mimi {:?} for {}: {error}",
                arguments,
                source.display()
            )
        })
}
