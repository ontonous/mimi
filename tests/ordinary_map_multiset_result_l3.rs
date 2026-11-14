#![cfg(feature = "ordinary-map-set-result-l3")]

//! Isolated acceptance gate for the three multi-Set Result owner fixtures.
//!
//! This gate deliberately fails at the CheckedProgram -> canonical route
//! boundary when no exact multi-Set Result error-path route is available. It
//! records the fixture identity and the native scenarios that remain blocked.
//! It must not be replaced by source-text matching or by evidence from another
//! fixture. Once an exact route and native emitter are available, this is the
//! place to add the failpoint/native/Memcheck executions.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mimi::core::mir::{self, materialize_canonical_mir_route};
use mimi::lexer::Lexer;

const EVIDENCE_ENV: &str = "MIMI_ORDINARY_MAP_MULTISET_RESULT_L3_EVIDENCE_DIR";

const FIXTURES: &[Fixture] = &[
    Fixture {
        key: "record",
        relative_path: "tests/fixtures/mir_ordinary_map_any_owner_v2_record.mimi",
        source: include_bytes!("fixtures/mir_ordinary_map_any_owner_v2_record.mimi"),
        expected_sha256: "7befca311daf9129a1495f06461309b9b62cc7a6b0d826d02ef9041870b79cc5",
        set_operations: 3,
        expected_success_stdout: "owned label\n",
    },
    Fixture {
        key: "record_unit",
        relative_path: "tests/fixtures/mir_ordinary_map_any_owner_v2_record_unit.mimi",
        source: include_bytes!("fixtures/mir_ordinary_map_any_owner_v2_record_unit.mimi"),
        expected_sha256: "85f7c9ee2a386b2e5caf1ae8f51753b9c96832625a4729f92bf34a837cce8ca5",
        set_operations: 3,
        expected_success_stdout: "owned label\n",
    },
    Fixture {
        key: "clone",
        relative_path: "tests/fixtures/mir_ordinary_map_owned_any_v2_clone.mimi",
        source: include_bytes!("fixtures/mir_ordinary_map_owned_any_v2_clone.mimi"),
        expected_sha256: "91bdc8f2471dd0eaa7910d5cab43763b3ff311f18cfdbc8980aa4c97538f4d90",
        set_operations: 2,
        expected_success_stdout: "true\ntrue\n",
    },
];

struct Fixture {
    key: &'static str,
    relative_path: &'static str,
    source: &'static [u8],
    expected_sha256: &'static str,
    set_operations: u64,
    expected_success_stdout: &'static str,
}

#[test]
#[ignore = "requires a dedicated multi-Set Result native route and strict Memcheck; invoke the L3 runner"]
fn ordinary_map_multiset_result_record_native_l3_requires_exact_route() {
    probe_fixture("record");
}

#[test]
#[ignore = "requires a dedicated multi-Set Result native route and strict Memcheck; invoke the L3 runner"]
fn ordinary_map_multiset_result_record_unit_native_l3_requires_exact_route() {
    probe_fixture("record_unit");
}

#[test]
#[ignore = "requires a dedicated multi-Set Result native route and strict Memcheck; invoke the L3 runner"]
fn ordinary_map_multiset_result_clone_native_l3_requires_exact_route() {
    probe_fixture("clone");
}

fn probe_fixture(key: &str) {
    let fixture = FIXTURES
        .iter()
        .find(|fixture| fixture.key == key)
        .unwrap_or_else(|| panic!("unknown fixture key {key}"));
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_path = repo_root.join(fixture.relative_path);
    let disk_source = fs::read(&fixture_path)
        .unwrap_or_else(|error| panic!("read exact fixture {}: {error}", fixture_path.display()));
    assert_eq!(
        disk_source.as_slice(),
        fixture.source,
        "fixture changed after this L3 test was compiled"
    );
    let actual_sha256 = sha256(&disk_source);
    assert_eq!(
        actual_sha256, fixture.expected_sha256,
        "fixture SHA-256 changed; update the source lock only with a reviewed semantic change"
    );

    let evidence_root = evidence_root();
    let fixture_evidence = evidence_root.join(fixture.key);
    fs::create_dir(&fixture_evidence).unwrap_or_else(|error| {
        panic!(
            "create fresh fixture evidence directory {}: {error}",
            fixture_evidence.display()
        )
    });

    let source = std::str::from_utf8(&disk_source).expect("locked Mimi fixture is UTF-8");
    let tokens = Lexer::new(source)
        .tokenize()
        .expect("lex exact fixture source");
    let (mut file, parse_errors) = mimi::loader::parser_for_path(tokens, &fixture_path)
        .expect("create parser for exact fixture source")
        .parse_file_with_recovery();
    assert!(
        parse_errors.is_empty(),
        "fixture parse errors: {parse_errors:?}"
    );
    mimi::loader::merge_prelude_into(&mut file);
    if let Some(main_index) = file
        .items
        .iter()
        .position(|item| matches!(item, mimi::ast::Item::Func(function) if function.name == "main"))
    {
        let main_item = file.items.remove(main_index);
        file.items.push(main_item);
    }
    let checked = mimi::core::check_program(&file)
        .unwrap_or_else(|errors| panic!("Checker rejects exact fixture: {errors:?}"));
    let admission = mir::classify_canonical_mir_route_admission(&checked);
    let route_materialization = match materialize_canonical_mir_route(&checked, None) {
        Ok(route) => format!(
            "materialization=available\nowner_v2_admission={:?}\nrecord_unit_candidate={}\nclone_isolation_candidate={}\nmap_set_drop_control_result_candidate={}\n",
            route.admission.ordinary_map_owned_any_v2,
            route.materialized_ordinary_map_owned_any_v2_record_unit_candidate,
            route.materialized_ordinary_map_owned_any_v2_clone_isolation_candidate,
            route.materialized_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_candidate,
        ),
        Err(error) => format!(
            "materialization=rejected\nowner_v2_admission={:?}\nerror={error}\n",
            admission.ordinary_map_owned_any_v2,
        ),
    };

    let scenarios = expected_scenarios(fixture);
    let mut scenario_text = String::new();
    for scenario in &scenarios {
        let expected_publish_attempts = scenario
            .failure_after
            .map(|successful| successful + 1)
            .unwrap_or(scenario.expected_attempted_operations);
        let expected_stdout = if scenario.expected_status == 0 {
            fixture.expected_success_stdout
        } else {
            ""
        };
        scenario_text.push_str(&format!(
            "scenario={} failure_after={:?} expected_status={} expected_attempted_operations={} expected_publish_attempts={} expected_stdout={:?} expected_final_counters=roots:0,ordinary_map_handles:0,owned_get_handles:0,runtime_map_handles:0,managed_any:0,owned_strings:0,any_owner_tokens:0,lists:0 native_injected=false counters_asserted=false strict_memcheck=not_run\n",
            scenario.name,
            scenario.failure_after,
            scenario.expected_status,
            scenario.expected_attempted_operations,
            expected_publish_attempts,
            expected_stdout,
        ));
    }
    let mut identity = format!(
            "fixture={}\nfixture_relative_path={}\nfixture_sha256={}\nsource_bytes_equal_embedded=true\nchecker=accepted\nexpected_set_operations={}\nroute_expected=exact multi-Set Result fixture route\n",
        fixture.key,
        fixture.relative_path,
        actual_sha256,
        fixture.set_operations,
    );
    identity.push_str(&route_materialization);
    identity.push_str("failure_injection_interface=mimi_ordinary_map_set_result_l3_install_capacity_failure_after(u64)\n");
    identity.push_str("failure_injection_interface_reachable_for_this_fixture=false\n");
    identity.push_str("native_evidence=blocked_before_native_execution\n");
    identity.push_str("missing_interfaces=source-bound cleanup receipt for every New/Set Result branch; MIR validation/lowering that proves each branch settles its exact roots/Any values once; route-authorized native emitter for the full chained operation count; per-scenario native status/checkpoint/counter observations\n");
    write_new(
        &fixture_evidence.join("route-probe.txt"),
        identity.as_bytes(),
    );
    write_new(
        &fixture_evidence.join("required-scenarios.txt"),
        scenario_text.as_bytes(),
    );

    panic!(
        "L3 BLOCKED for {}: Checker accepted the exact SHA-256-pinned source, but the canonical materialization does not supply an exact multi-Set Result error-cleanup route/emitter contract. No generated native code ran, no failpoint was installed for this fixture, no owner counters were asserted, and strict Memcheck did not run. Evidence: {}\n{}Required interface: Checker source receipt + branch-local cleanup CFG/MIR receipt for every operation; exact route validator and native emitter; then execute every listed failpoint scenario and assert status, operation/checkpoint count, zero final owner counters, and strict Memcheck.",
        fixture.key,
        fixture_evidence.display(),
        route_materialization,
    );
}

struct Scenario {
    name: String,
    failure_after: Option<u64>,
    expected_status: i32,
    expected_attempted_operations: u64,
}

fn expected_scenarios(fixture: &Fixture) -> Vec<Scenario> {
    let mut scenarios = vec![Scenario {
        name: "success".to_owned(),
        failure_after: None,
        expected_status: 0,
        expected_attempted_operations: 1 + fixture.set_operations,
    }];
    scenarios.push(Scenario {
        name: "new-err".to_owned(),
        failure_after: Some(0),
        expected_status: 3,
        expected_attempted_operations: 1,
    });
    for set_index in 1..=fixture.set_operations {
        scenarios.push(Scenario {
            name: format!("set-{set_index}-err"),
            failure_after: Some(set_index),
            expected_status: 2,
            expected_attempted_operations: set_index + 1,
        });
    }
    scenarios
}

fn evidence_root() -> PathBuf {
    let value = std::env::var_os(EVIDENCE_ENV)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("the L3 runner must set {EVIDENCE_ENV}"));
    let requested = PathBuf::from(value);
    assert!(requested.is_absolute(), "L3 evidence root must be absolute");
    let metadata = fs::symlink_metadata(&requested).unwrap_or_else(|error| {
        panic!(
            "inspect runner-created evidence root {}: {error}",
            requested.display()
        )
    });
    assert!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "L3 evidence root must be a real, non-symlink directory"
    );
    fs::canonicalize(requested).expect("canonicalize runner-created evidence root")
}

fn write_new(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap_or_else(|error| panic!("create evidence file {}: {error}", path.display()));
    file.write_all(bytes)
        .unwrap_or_else(|error| panic!("write evidence file {}: {error}", path.display()));
}

fn sha256(bytes: &[u8]) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("sha256sum is required by the L3 runner");
    child
        .stdin
        .take()
        .expect("sha256sum stdin is piped")
        .write_all(bytes)
        .expect("write exact fixture bytes to sha256sum");
    let output = child.wait_with_output().expect("read sha256sum output");
    assert!(output.status.success(), "sha256sum failed");
    String::from_utf8(output.stdout)
        .expect("sha256sum output is UTF-8")
        .split_whitespace()
        .next()
        .expect("sha256sum prints a digest")
        .to_owned()
}
