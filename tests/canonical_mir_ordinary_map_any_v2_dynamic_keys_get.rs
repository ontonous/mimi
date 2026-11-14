use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{
    materialize_canonical_mir_route, validate_ordinary_map_owned_any_v2_dynamic_keys_get_route,
    CanonicalMirRouteProfile, OrdinaryMapOwnedAnyV2Admission,
    ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE, ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
};
use mimi::core::resolved::NodeId;

const PROFILE: &str = ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE;
const SOURCE: &str = include_str!("fixtures/mir_ordinary_map_any_owner_v2_dynamic_keys_get.mimi");
const PREFIX: &str = "pré";
const EXPECTED_PREFIX_OUTPUT: &[u8] = b"pr\xc3\xa9\0\xe9\x9b\xaa\ntrue\n";
const EXPECTED_EMPTY_ARGUMENT_OUTPUT: &[u8] = b"\0\xe9\x9b\xaa\ntrue\n";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_dynamic_keys_get.mimi")
}

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
        .expect("Cargo must provide mimi for DynamicKeysGet CLI evidence")
}

fn checked_route() -> (
    mimi::core::resolved::CheckedProgram,
    mimi::core::mir::CanonicalMirRouteMaterialization,
) {
    let tokens = mimi::lexer::Lexer::new(SOURCE)
        .tokenize()
        .expect("DynamicKeysGet fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("DynamicKeysGet fixture parses");
    let checked = mimi::core::check_program(&file).expect("DynamicKeysGet fixture typechecks");
    let route = materialize_canonical_mir_route(&checked, None)
        .expect("exact DynamicKeysGet admission materializes");
    (checked, route)
}

fn route_identity(output: &Output, consumer: &str, mode: &str) -> (String, String) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let matching = stderr
        .lines()
        .filter(|line| line.contains(&format!("route-admission consumer={consumer} mode={mode}")))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "expected one {consumer}/{mode} route receipt: {stderr}"
    );
    let line = matching[0];
    assert!(
        line.contains(&format!("checker_profile={PROFILE}")),
        "{line}"
    );
    let field = |name: &str| {
        line.split_whitespace()
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("missing {name} in route receipt: {line}"))
            .to_owned()
    };
    (field("mir_digest"), field("receipt_identity_digest"))
}

fn run_command(mimi: &Path, direct: bool, argument: Option<&str>) -> Output {
    let mut command = Command::new(mimi);
    command
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("run")
        .arg(fixture());
    if direct {
        command.arg("--mir");
    }
    if let Some(argument) = argument {
        command.arg("--").arg(argument);
    }
    command.output().expect("run DynamicKeysGet CLI")
}

fn assert_bool_main_exit(output: &Output, mode: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{mode} should return true as the Mimi bool exit code: stdout={:?} stderr={}",
        output.stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("[E080"),
        "{mode} reported a runtime failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dynamic_keys_get_revalidates_one_receipt_and_matches_raw_reference_and_vm_bytes() {
    const {
        assert!(
            !ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the DynamicKeysGet slice must leave global owner-v2 readiness closed"
        );
    };
    let (checked, route) = checked_route();
    assert_eq!(
        route.admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::DynamicKeysGet
    );
    assert!(route.admission.ordinary_map_owned_any_v2_dynamic_keys_get());
    assert!(!route.materialized_ordinary_map_owned_any_v2_candidate);
    assert!(
        CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DynamicKeysGet.is_admitted(route.admission)
    );

    let receipt = route
        .ordinary_map_owned_any_v2_dynamic_keys_get_route_receipt
        .as_ref()
        .expect("dedicated Checker-bound DynamicKeysGet route receipt");
    assert_eq!(receipt.profile, PROFILE);
    assert_eq!(receipt, &route.program.route_receipt(PROFILE));
    assert_eq!(
        validate_ordinary_map_owned_any_v2_dynamic_keys_get_route(&checked, &route.program)
            .expect("Checker/MIR source receipt revalidates"),
        *receipt
    );
    let bytecode = mimi::interp::bytecode::compile_mir_program_with_dynamic_keys_get_route(
        &checked,
        &route.program,
    )
    .expect("exact AST-free Bytecode route compiles");
    assert!(bytecode.ast.is_none(), "the consumer must stay AST-free");

    // Native argv projections carry an independently allocated C string.
    // The generated route must register that exact pointer in the owner
    // domain before it publishes the Mimi String or creates any Map root.
    let context = inkwell::context::Context::create();
    let mut native =
        mimi::codegen::CodeGenerator::new(&context, "dynamic_keys_get_ownership_order");
    native
        .compile_checked_mir_native_with_dynamic_keys_get_route(&checked, &route.program, receipt)
        .expect("exact Checker/MIR receipt authorizes native route");
    native.module.verify().expect("native module verifies");
    let native_ir = native.module.print_to_string().to_string();
    let args_init = native_ir
        .find("call void @mimi_args_init")
        .expect("native main seeds host argv into the Mimi runtime");
    let argv_get = native_ir
        .find("mir_dynamic_keys_get_user_arg0 = call")
        .expect("native route imports first-user-argument lookup");
    let string_registration = native_ir
        .find("call i32 @mimi_mir_ordinary_map_domain_own_string")
        .expect("native route registers its independent argv allocation");
    let map_new = native_ir
        .find("call i32 @mimi_ordinary_map_owned_any_v2_new")
        .expect("native route creates its Map only after argv projection");
    assert!(
        args_init < argv_get && argv_get < string_registration && string_registration < map_new,
        "host argv initialization and ownership must precede MapNew: init={args_init}, get={argv_get}, register={string_registration}, map_new={map_new}\n{native_ir}"
    );

    for (prefix, expected) in [
        (PREFIX, EXPECTED_PREFIX_OUTPUT),
        ("", EXPECTED_EMPTY_ARGUMENT_OUTPUT),
    ] {
        let reference = MirReferenceInterpreter::new(&route.program)
            .with_route_receipt(receipt)
            .with_cli_args(vec![prefix.to_owned()]);
        let observation = reference
            .execute_with_output(&NodeId("function:main".into()), &[])
            .expect("reference executes with the supplied user argv[0]");
        assert_eq!(observation.value, MirRuntimeValue::Bool(true));
        assert_eq!(
            observation.output.as_bytes(),
            expected,
            "reference bytes for {prefix:?}"
        );

        let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode.clone())
            .with_cli_args(vec![prefix.to_owned()]);
        assert_eq!(
            vm.run_value().expect("AST-free Bytecode executes"),
            mimi::interp::Value::Bool(true)
        );
        assert_eq!(
            vm.stdout().as_bytes(),
            expected,
            "Bytecode bytes for {prefix:?}"
        );
    }
}

#[test]
fn dynamic_keys_get_rejects_generic_receipts_and_missing_arg_before_output() {
    let (checked, route) = checked_route();
    let generic_receipt = route.program.route_receipt("generic-mir-v1");

    let generic_bytecode_error = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
        &route.program,
        &generic_receipt,
    )
    .expect_err("generic receipt cannot authorize DynamicArg0");
    assert!(
        format!("{generic_bytecode_error:?}").contains("DynamicKeysGet"),
        "generic Bytecode denial should name the exact route: {generic_bytecode_error:?}"
    );

    let missing_reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(
        route
            .ordinary_map_owned_any_v2_dynamic_keys_get_route_receipt
            .as_ref()
            .expect("exact route receipt"),
    );
    let reference_error = missing_reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect_err("reference missing argv[0] fails before MapNew");
    assert!(
        reference_error.message.contains("E0803"),
        "{reference_error}"
    );
    assert!(missing_reference.captured_output().is_empty());

    let exact_receipt = route
        .ordinary_map_owned_any_v2_dynamic_keys_get_route_receipt
        .as_ref()
        .expect("exact route receipt");
    let bytecode = mimi::interp::bytecode::compile_mir_program_with_dynamic_keys_get_route(
        &checked,
        &route.program,
    )
    .expect("exact Bytecode receipt compiles");
    let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
    let vm_error = vm.run_value().expect_err("missing argv[0] is E0803");
    assert_eq!(vm_error.code(), "E0803");
    assert!(vm.stdout().is_empty(), "no output precedes argv failure");

    let wrong_reference = MirReferenceInterpreter::new(&route.program)
        .with_route_receipt(&generic_receipt)
        .with_cli_args(vec![PREFIX.to_owned()]);
    let wrong_receipt_error = wrong_reference
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect_err("a generic raw receipt cannot authorize reference execution");
    assert!(wrong_receipt_error.message.contains("DynamicKeysGet"));
    assert!(wrong_reference.captured_output().is_empty());

    let context = inkwell::context::Context::create();
    let mut raw_native =
        mimi::codegen::CodeGenerator::new(&context, "dynamic_keys_get_generic_receipt_rejected");
    let native_error = raw_native
        .compile_checked_mir_native_with_dynamic_keys_get_route(
            &checked,
            &route.program,
            &generic_receipt,
        )
        .expect_err("a generic receipt cannot authorize the dedicated DynamicKeysGet entry");
    assert!(
        format!("{native_error:?}").contains("DynamicKeysGet"),
        "dedicated DynamicKeysGet denial should name its exact receipt mismatch: {native_error:?}"
    );

    let verifier_error =
        mimi::verifier::verify_mir_with_ordinary_map_owned_any_v2_dynamic_keys_get_route(
            &checked,
            &route.program,
            &generic_receipt,
            "dynamic-keys-get-generic-verifier-receipt".into(),
        )
        .expect_err("generic verifier receipt cannot authorize argv-derived String output");
    assert!(
        verifier_error.contains("DynamicKeysGet"),
        "generic verifier denial should name the exact route: {verifier_error}"
    );
    assert_eq!(exact_receipt.profile, PROFILE);
}

#[test]
fn dynamic_keys_get_default_and_explicit_cli_consumers_share_exact_route_receipts() {
    let mimi = mimi_bin();
    let source = fixture();
    let mut identities = Vec::new();

    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        for (argument, expected) in [
            (PREFIX, EXPECTED_PREFIX_OUTPUT),
            ("", EXPECTED_EMPTY_ARGUMENT_OUTPUT),
        ] {
            let run = run_command(&mimi, direct, Some(argument));
            assert_bool_main_exit(&run, &format!("{mode} run argument {argument:?}"));
            assert_eq!(
                run.stdout, expected,
                "{mode} run raw bytes for {argument:?}"
            );
            let identity = route_identity(&run, "run", mode);
            let stderr = String::from_utf8_lossy(&run.stderr);
            assert!(
                !stderr.contains("canonical route disposition: legacy"),
                "{mode} run fell back to legacy: {stderr}"
            );
            identities.push((format!("{mode}-run-{argument:?}"), identity));
        }

        let binary = std::env::temp_dir().join(format!(
            "mimi-dynamic-keys-get-{mode}-{}",
            std::process::id()
        ));
        let mut build_command = Command::new(&mimi);
        build_command
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build")
            .arg(&source);
        if direct {
            build_command.arg("--mir");
        }
        let build = build_command
            .arg("--output")
            .arg(&binary)
            .output()
            .expect("build DynamicKeysGet native binary");
        assert!(
            build.status.success(),
            "{mode} build failed: stdout={} stderr={}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let build_identity = route_identity(&build, "build", mode);
        identities.push((format!("{mode}-build"), build_identity.clone()));
        assert!(binary.is_file(), "{mode} build omitted its native artifact");

        for (argument, expected) in [
            (PREFIX, EXPECTED_PREFIX_OUTPUT),
            ("", EXPECTED_EMPTY_ARGUMENT_OUTPUT),
        ] {
            let native = Command::new(&binary)
                .arg(argument)
                .output()
                .unwrap_or_else(|error| panic!("run {mode} native with {argument:?}: {error}"));
            assert_bool_main_exit(&native, &format!("{mode} native argument {argument:?}"));
            assert_eq!(
                native.stdout, expected,
                "{mode} native raw bytes for {argument:?}"
            );
            assert!(
                native.stderr.is_empty(),
                "unexpected native stderr: {}",
                String::from_utf8_lossy(&native.stderr)
            );
        }

        let mut verify_command = Command::new(&mimi);
        verify_command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify")
            .arg(&source);
        if direct {
            verify_command.arg("--mir");
        }
        let verify = verify_command
            .output()
            .expect("verify DynamicKeysGet through the dedicated verifier route");
        assert!(
            verify.status.success(),
            "{mode} verify failed: stdout={} stderr={}",
            String::from_utf8_lossy(&verify.stdout),
            String::from_utf8_lossy(&verify.stderr)
        );
        assert!(
            String::from_utf8_lossy(&verify.stdout).contains("No contracts to verify"),
            "{mode} no-requires fixture should report no contracts to verify: {}",
            String::from_utf8_lossy(&verify.stdout)
        );
        let verify_identity = route_identity(&verify, "verify", mode);
        assert_eq!(
            verify_identity, build_identity,
            "{mode} verifier changed route identity"
        );
        identities.push((format!("{mode}-verify"), verify_identity));

        {
            let output = run_command(&mimi, direct, None);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{mode} missing-argument run should exit 1: stdout={:?} stderr={stderr}",
                output.stdout
            );
            assert_eq!(output.stdout, b"", "missing argv must precede all output");
            assert!(
                stderr.contains("E0803"),
                "missing argv must retain E0803: {stderr}"
            );
            assert!(
                stderr.contains(&format!("checker_profile={PROFILE}")),
                "missing-argument run did not enter the exact route preflight: {stderr}"
            );
        }

        let missing_native = Command::new(&binary)
            .output()
            .expect("run DynamicKeysGet native without argv[0]");
        assert_eq!(
            missing_native.status.code(),
            Some(1),
            "native missing-argument run should exit 1: stderr={}",
            String::from_utf8_lossy(&missing_native.stderr)
        );
        assert_eq!(missing_native.stdout, b"", "native E0803 precedes output");
        assert!(
            String::from_utf8_lossy(&missing_native.stderr).contains("E0803"),
            "native missing argv must preserve E0803: {}",
            String::from_utf8_lossy(&missing_native.stderr)
        );
        let _ = std::fs::remove_file(&binary);
    }

    let default_identity = identities
        .iter()
        .find(|(label, _)| label == "default-run-\"pré\"")
        .expect("default run identity")
        .1
        .clone();
    for (label, identity) in &identities {
        assert_eq!(
            identity, &default_identity,
            "{label} consumed a different DynamicKeysGet MIR/route receipt"
        );
    }
}

#[test]
fn dynamic_keys_get_disasm_uses_the_exact_bytecode_adapter_and_test_cli_stays_nonexecuting() {
    let mimi = mimi_bin();
    let source = fixture();
    let disasm = Command::new(&mimi)
        .arg("disasm")
        .arg(&source)
        .output()
        .expect("disassemble DynamicKeysGet fixture");
    assert!(
        disasm.status.success(),
        "disasm rejected an admitted exact route: stdout={} stderr={}",
        String::from_utf8_lossy(&disasm.stdout),
        String::from_utf8_lossy(&disasm.stderr)
    );
    let listing = String::from_utf8_lossy(&disasm.stdout);
    assert!(
        listing.contains("DynamicArg0") || listing.contains("dynamic_arg0"),
        "disassembly omitted its exact argv projection opcode: {listing}"
    );

    // The exact profile contains only `main`; `mimi test` returns before
    // route compilation when there are no test_* functions. It must not run
    // main or pretend to supply the argv value.
    let test = Command::new(&mimi)
        .arg("test")
        .arg(&source)
        .output()
        .expect("query test functions in the single-main fixture");
    assert!(
        test.status.success(),
        "test CLI should report no test functions"
    );
    assert!(
        String::from_utf8_lossy(&test.stdout).contains("No test functions found"),
        "test CLI should remain non-executing for the one-main profile: {}",
        String::from_utf8_lossy(&test.stdout)
    );
}
