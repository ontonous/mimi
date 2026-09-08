//! Physical C ABI and call-order checks for the same Canonical MIR in all
//! three execution consumers. The C library is test-owned, not a symbol-name
//! shortcut in the reference executor or a compatibility bytecode arm.

use std::cell::Cell;
use std::path::PathBuf;
use std::process::Command;

use crate::core::mir::reference::{
    MirProgram, MirReferenceFfiResolver, MirReferenceInterpreter, MirRuntimeValue,
};
use crate::core::mir::MirFfiCallContract;
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};
use crate::interp::Value;

const C_SOURCE: &str = include_str!("../../tests/fixtures/mir_scalar_ffi_abi.c");
const SOURCE: &str = include_str!("../../tests/fixtures/mir_scalar_ffi_abi.mimi");

struct Oracle(Cell<i64>);

impl MirReferenceFfiResolver for Oracle {
    fn call(
        &self,
        receipt: &MirFfiCallContract,
        args: &[MirRuntimeValue],
    ) -> Result<MirRuntimeValue, String> {
        if receipt.abi != "C" {
            return Err("oracle requires the C ABI".into());
        }
        match (receipt.symbol.as_str(), args) {
            ("mir_ffi_i32" | "mir_ffi_i64", [MirRuntimeValue::Int(x)]) => {
                Ok(MirRuntimeValue::Int(*x))
            }
            ("mir_ffi_bool", [MirRuntimeValue::Bool(x)]) => Ok(MirRuntimeValue::Bool(!x)),
            ("mir_ffi_f64", [MirRuntimeValue::FloatBits(bits)]) => {
                Ok(MirRuntimeValue::FloatBits(*bits))
            }
            ("mir_ffi_f64_code", [MirRuntimeValue::FloatBits(bits)]) => {
                Ok(MirRuntimeValue::Int(i64::from(*bits == 42.5_f64.to_bits())))
            }
            ("mir_ffi_store", [MirRuntimeValue::Int(x)]) => {
                self.0.set(self.0.get() * 10 + x);
                Ok(MirRuntimeValue::Unit)
            }
            ("mir_ffi_read", []) => Ok(MirRuntimeValue::Int(self.0.get())),
            _ => Err("unexpected oracle call".into()),
        }
    }
}

struct LibraryFixture {
    dir: PathBuf,
    previous: Option<std::ffi::OsString>,
    previous_trace: Option<std::ffi::OsString>,
}

impl Drop for LibraryFixture {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var("MIMI_FFI_LIB", value),
            None => std::env::remove_var("MIMI_FFI_LIB"),
        }
        match &self.previous_trace {
            Some(value) => std::env::set_var("MIMI_CANONICAL_FFI_TRACE", value),
            None => std::env::remove_var("MIMI_CANONICAL_FFI_TRACE"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn library_fixture(counter: u64, c_source: &str) -> LibraryFixture {
    let fixture = LibraryFixture {
        dir: std::env::temp_dir().join(format!(
            "mimi-canonical-ffi-{}-{counter}",
            std::process::id()
        )),
        previous: std::env::var_os("MIMI_FFI_LIB"),
        previous_trace: std::env::var_os("MIMI_CANONICAL_FFI_TRACE"),
    };
    std::fs::create_dir_all(&fixture.dir).expect("create C FFI fixture directory");
    let c_path = fixture.dir.join("ffi.c");
    let library = fixture.dir.join("ffi.so");
    std::fs::write(&c_path, c_source).expect("write C ABI fixture");
    let cc = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(&c_path)
        .arg("-o")
        .arg(&library)
        .output()
        .expect("C compiler for real FFI ABI test");
    assert!(
        cc.status.success(),
        "{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    fixture
}

#[test]
fn scalar_ffi_c_abi_and_side_effect_order_match_three_consumers() {
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");

    let tokens = crate::lexer::Lexer::new(SOURCE)
        .tokenize()
        .expect("lex C ABI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse C ABI fixture");
    let checked = crate::core::check_program(&file).expect("check C ABI fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize C ABI MIR");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
        "all scalar ABIs must share production admission"
    );
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("shared scalar FFI materialization");
    assert!(crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route));
    assert_eq!(route.program.canonical_digest(), mir.canonical_digest());
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    for results in [
        crate::verifier::verify_checked(&checked, "scalar-ffi-abi".into()),
        crate::verifier::verify_checked_dual(&checked, "scalar-ffi-abi".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("public scalar FFI contract consumer");
        assert!(
            results.iter().all(|result| matches!(
                result.status,
                crate::verifier::VerifStatus::Verified
                    | crate::verifier::VerifStatus::NoObligations
            )),
            "{results:?}"
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let digest = mir.canonical_digest();
    let expected = "-2147483647\n2147483647\n4294967296\ntrue\nfalse\n1\n42\n";
    let oracle = Oracle(Cell::new(0));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("typed reference host ABI");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, expected);
    assert_eq!(oracle.0.get(), 42);

    let bytecode = compile_mir_program(&mir).expect("AST-free C ABI bytecode");
    assert!(bytecode.ast.is_none());
    assert!(bytecode.extern_names.is_empty());
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("missing.so"));
    let error = BytecodeVM::new(bytecode.clone())
        .run_value()
        .expect_err("missing library");
    assert!(error.to_string().contains("failed to load"), "{error}");

    std::env::set_var("MIMI_FFI_LIB", &library);
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("real C ABI bytecode execution"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), expected);

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_c_abi");
    generator
        .compile_mir_native(&mir)
        .expect("same MIR native C ABI");
    generator.module.verify().expect("valid LLVM C ABI module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native execution against the same C library source");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, expected);
    assert_eq!(native.stderr, "");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let context = inkwell::context::Context::create();
    let mut direct = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_direct");
    direct
        .compile_checked(&checked)
        .expect("direct native uses scalar FFI MIR");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let native = super::link_and_observe_module(
        &direct,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("direct checked native ABI execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, expected);
    assert_eq!(native.stderr, "");
    assert_eq!(mir.canonical_digest(), digest);
}

#[test]
fn scalar_ffi_materialization_rejects_unrepresented_declaration_semantics() {
    for (declaration, expected) in [
        ("func foreign(x: i64 ...) -> i64;", "variadic"),
        ("#[errno] func foreign(x: i64) -> i64;", "errno"),
        ("func foreign(&x: i64) -> i64;", "parameter mode"),
    ] {
        let source =
            format!("extern \"C\" {{ {declaration} }} func main() -> i64 {{ foreign(42 as i64) }}");
        let tokens = crate::lexer::Lexer::new(&source)
            .tokenize()
            .expect("lex declaration");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse declaration");
        let checked = crate::core::check_program(&file).expect("check declaration");
        let error = match MirProgram::from_checked_program(&checked) {
            Err(error) => format!("{error:?}"),
            Ok(_) => panic!("unrepresented declaration admitted: {declaration}"),
        };
        assert!(error.contains(expected), "{declaration}: {error}");
    }
}

#[test]
fn scalar_ffi_ensures_binds_call_result_across_three_consumers() {
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);
    let source = r#"
extern "C" {
    func mir_ffi_i64(x: i64) -> i64 ensures: result == x;
}
func main() -> i64 {
    println(mir_ffi_i64(42 as i64));
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex scalar FFI ensures fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse scalar FFI ensures fixture");
    let checked = crate::core::check_program(&file).expect("check scalar FFI ensures fixture");
    assert!(crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    let mir = MirProgram::from_checked_program(&checked).expect("materialize scalar FFI ensures");
    let receipt = mir.ffi_calls().values().next().expect("FFI receipt");
    assert!(receipt.ensures.is_some(), "ensures must be materialized");
    assert!(receipt
        .ensures
        .as_ref()
        .expect("ensures")
        .canonical_text()
        .contains("eq("));
    let digest = mir.canonical_digest();

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_mir(&mir, "scalar-ffi-ensures".into())
        .expect("MIR FFI ensures verifier");
    assert_eq!(results.len(), 1, "one call-site postcondition obligation");
    assert_eq!(
        results[0].status,
        crate::verifier::VerifStatus::Disproven,
        "an unconstrained foreign result cannot prove result == argument: {results:?}"
    );
    assert!(results[0]
        .message
        .contains("extern ensures contract disproven"));
    assert_eq!(
        results[0]
            .artifact
            .as_ref()
            .expect("definitive ensures artifact")
            .mir_hash,
        digest
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    for results in [
        crate::verifier::verify_checked(&checked, "scalar-ffi-ensures".into()),
        crate::verifier::verify_checked_dual(&checked, "scalar-ffi-ensures".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("public scalar FFI ensures verifier");
        assert_eq!(
            results.len(),
            1,
            "one public call-site postcondition result"
        );
        assert_eq!(results[0].status, crate::verifier::VerifStatus::Disproven);
        assert!(results[0]
            .artifact
            .as_ref()
            .is_some_and(|artifact| artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR));
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let oracle = Oracle(Cell::new(0));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference scalar FFI ensures execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "42\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free scalar FFI ensures bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 1);
    assert!(bytecode.canonical_ffi[0].ensures.is_some());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("bytecode scalar FFI ensures"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "42\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_ensures");
    generator
        .compile_mir_native(&mir)
        .expect("native scalar FFI ensures");
    generator
        .module
        .verify()
        .expect("valid native FFI ensures module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native scalar FFI ensures execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "42\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    assert_eq!(mir.canonical_digest(), digest);
}

#[test]
fn scalar_ffi_unit_ensures_runs_after_void_call_across_three_consumers() {
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);
    let source = r#"
extern "C" {
    func mir_ffi_store(x: i32) ensures: true;
    func mir_ffi_read() -> i64;
}
func main() -> i64 {
    mir_ffi_store(4)
    println(mir_ffi_read())
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex unit-return FFI ensures fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse unit-return FFI ensures fixture");
    let checked = crate::core::check_program(&file).expect("check unit-return FFI ensures fixture");
    assert!(crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    let mir = MirProgram::from_checked_program(&checked).expect("materialize unit-return FFI");
    let store_receipt = mir
        .ffi_calls()
        .values()
        .find(|receipt| receipt.symbol == "mir_ffi_store")
        .expect("unit-return FFI receipt");
    assert!(
        store_receipt.result.is_some(),
        "MIR retains a stable unit result identity for every call expression"
    );
    assert_eq!(
        store_receipt
            .ensures
            .as_ref()
            .map(|value| value.canonical_text()),
        Some("true".to_owned())
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_mir(&mir, "scalar-ffi-unit-ensures".into())
        .expect("MIR unit-return FFI ensures verifier");
    assert_eq!(
        results.len(),
        1,
        "unit FFI has one postcondition obligation"
    );
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Proven);
    assert!(results[0]
        .message
        .contains("extern ensures contract proven"));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let oracle = Oracle(Cell::new(0));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference unit-return FFI ensures execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "4\n");
    assert_eq!(oracle.0.get(), 4);

    let bytecode = compile_mir_program(&mir).expect("unit-return FFI bytecode");
    assert!(bytecode.ast.is_none());
    let store_descriptor = bytecode
        .canonical_ffi
        .iter()
        .find(|descriptor| descriptor.symbol == "mir_ffi_store")
        .expect("unit-return FFI bytecode descriptor");
    assert_eq!(
        store_descriptor.result,
        crate::interp::bytecode::CanonicalFfiScalarType::Unit
    );
    assert!(store_descriptor.result_id.is_some());
    assert!(store_descriptor.ensures.is_some());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("bytecode unit-return FFI ensures"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "4\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_unit_ensures");
    generator
        .compile_mir_native(&mir)
        .expect("native unit-return FFI ensures");
    generator
        .module
        .verify()
        .expect("valid native unit-return FFI module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native unit-return FFI ensures execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "4\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_unit_ensures_rejects_unit_result_reference() {
    let source = r#"
extern "C" {
    func mir_ffi_store(x: i32) ensures: result == 0;
}
func main() -> i64 {
    mir_ffi_store(4)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex unit-result FFI result reference");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse unit-result FFI result reference");
    let checked =
        crate::core::check_program(&file).expect("check unit-result FFI result reference");
    let errors = MirProgram::from_checked_program(&checked)
        .expect_err("unit FFI result must not enter scalar postcondition arithmetic");
    let message = format!("{errors:?}");
    assert!(
        message.contains("ABI Unit is outside the canonical scalar verifier contract"),
        "{message}"
    );
}

#[test]
fn scalar_ffi_bool_ensures_binds_bool_result_across_three_consumers() {
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);
    let source = r#"
extern "C" {
    func mir_ffi_bool(x: bool) -> bool ensures: result == not x;
}
func main() -> i64 {
    println(mir_ffi_bool(false))
    println(mir_ffi_bool(true))
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex bool-result FFI ensures fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse bool-result FFI ensures fixture");
    let checked = crate::core::check_program(&file).expect("check bool-result FFI ensures fixture");
    assert!(crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    let mir = MirProgram::from_checked_program(&checked).expect("materialize bool-result FFI");
    let receipts = mir
        .ffi_calls()
        .values()
        .filter(|receipt| receipt.symbol == "mir_ffi_bool")
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2, "one receipt per bool FFI call-site");
    assert!(receipts.iter().all(|receipt| receipt.result.is_some()));
    assert!(receipts.iter().all(|receipt| receipt.ensures.is_some()));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_mir(&mir, "scalar-ffi-bool-ensures".into())
        .expect("MIR bool-result FFI ensures verifier");
    assert_eq!(
        results.len(),
        2,
        "one postcondition obligation per bool call"
    );
    assert!(results
        .iter()
        .all(|result| result.status == crate::verifier::VerifStatus::Disproven));
    assert!(results
        .iter()
        .all(|result| result.message.contains("extern ensures contract disproven")));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    for results in [
        crate::verifier::verify_checked(&checked, "scalar-ffi-bool-ensures".into()),
        crate::verifier::verify_checked_dual(&checked, "scalar-ffi-bool-ensures".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("public bool-result FFI ensures verifier");
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|result| result.status == crate::verifier::VerifStatus::Disproven));
        assert!(results.iter().all(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
            })
        }));
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let oracle = Oracle(Cell::new(0));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference bool-result FFI ensures execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "true\nfalse\n");

    let bytecode = compile_mir_program(&mir).expect("bool-result FFI bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(
        bytecode
            .canonical_ffi
            .iter()
            .filter(|descriptor| descriptor.symbol == "mir_ffi_bool")
            .count(),
        2
    );
    assert!(bytecode
        .canonical_ffi
        .iter()
        .filter(|descriptor| descriptor.symbol == "mir_ffi_bool")
        .all(|descriptor| {
            descriptor.result == crate::interp::bytecode::CanonicalFfiScalarType::Bool
                && descriptor.result_id.is_some()
                && descriptor.ensures.is_some()
        }));
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("bytecode bool-result FFI ensures"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "true\nfalse\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_bool_ensures");
    generator
        .compile_mir_native(&mir)
        .expect("native bool-result FFI ensures");
    generator
        .module
        .verify()
        .expect("valid native bool-result FFI module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native bool-result FFI ensures execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "true\nfalse\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_ensures_short_circuit_division_matches_three_consumers() {
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);
    let source = r#"
extern "C" {
    func mir_ffi_i64(x: i64) -> i64 ensures: x == 0 or result / x == 1;
}
func main() -> i64 {
    println(mir_ffi_i64(0 as i64))
    println(mir_ffi_i64(7 as i64))
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex short-circuit FFI ensures fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse short-circuit FFI ensures fixture");
    let checked = crate::core::check_program(&file).expect("check short-circuit FFI ensures");
    assert!(crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    let mir = MirProgram::from_checked_program(&checked).expect("materialize short-circuit FFI");
    assert_eq!(mir.ffi_calls().len(), 2);
    assert!(mir
        .ffi_calls()
        .values()
        .all(|receipt| receipt.ensures.is_some()));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_mir(&mir, "scalar-ffi-short-circuit".into())
        .expect("MIR short-circuit FFI ensures verifier");
    assert_eq!(results.len(), 2);
    assert_eq!(
        results
            .iter()
            .filter(|result| result.status == crate::verifier::VerifStatus::Proven)
            .count(),
        1,
        "x == 0 must short-circuit before symbolic division"
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.status == crate::verifier::VerifStatus::Disproven)
            .count(),
        1,
        "the unconstrained nonzero result remains statically disproven"
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    for results in [
        crate::verifier::verify_checked(&checked, "scalar-ffi-short-circuit".into()),
        crate::verifier::verify_checked_dual(&checked, "scalar-ffi-short-circuit".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("public short-circuit FFI ensures verifier");
        assert_eq!(results.len(), 2);
        assert_eq!(
            results
                .iter()
                .filter(|result| result.status == crate::verifier::VerifStatus::Proven)
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result.status == crate::verifier::VerifStatus::Disproven)
                .count(),
            1
        );
        assert!(results.iter().all(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
            })
        }));
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let oracle = Oracle(Cell::new(0));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference short-circuit FFI ensures execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "0\n7\n");

    let bytecode = compile_mir_program(&mir).expect("short-circuit FFI bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("bytecode short-circuit FFI ensures"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "0\n7\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_short_circuit");
    generator
        .compile_mir_native(&mir)
        .expect("native short-circuit FFI ensures");
    generator
        .module
        .verify()
        .expect("valid native short-circuit FFI module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native short-circuit FFI ensures execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "0\n7\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_ensures_division_by_zero_traps_after_foreign_call() {
    struct CountingOracle(Cell<u32>);
    impl MirReferenceFfiResolver for CountingOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_i64" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = args else {
                return Err("division trap oracle expects one i64".into());
            };
            self.0.set(self.0.get() + 1);
            Ok(MirRuntimeValue::Int(*value))
        }
    }

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);
    let source = r#"
extern "C" {
    func mir_ffi_i64(x: i64) -> i64 ensures: result / x == 1;
}
func main() -> i64 {
    mir_ffi_i64(0 as i64)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex FFI postcondition division trap fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse FFI postcondition division trap fixture");
    let checked = crate::core::check_program(&file).expect("check FFI postcondition division trap");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize FFI postcondition division trap");

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_mir(&mir, "scalar-ffi-ensures-div-zero".into())
        .expect("MIR FFI postcondition division verifier");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Disproven);
    assert!(results[0]
        .message
        .contains("extern ensures contract disproven"));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let oracle = CountingOracle(Cell::new(0));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must trap on postcondition division by zero");
    assert!(
        reference
            .message
            .contains("division by zero in FFI postcondition"),
        "{reference}"
    );
    assert_eq!(
        oracle.0.get(),
        1,
        "foreign call precedes postcondition trap"
    );

    let mut vm = BytecodeVM::new(compile_mir_program(&mir).expect("division trap bytecode"));
    let vm_error = vm
        .run_value()
        .expect_err("bytecode must trap on postcondition division by zero");
    assert_eq!(vm_error.code(), "E0801");
    assert!(vm_error
        .to_string()
        .contains("division by zero in FFI postcondition"));

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_ensures_div_zero");
    generator
        .compile_mir_native(&mir)
        .expect("native FFI postcondition division trap");
    generator
        .module
        .verify()
        .expect("valid native FFI postcondition division module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native FFI postcondition division execution");
    assert_ne!(native.exit_code, Some(0));
    assert!(native.stderr.contains("E0801"), "{}", native.stderr);
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_ensures_checked_arithmetic_overflow_parity() {
    struct CountingOracle(Cell<u32>);
    impl MirReferenceFfiResolver for CountingOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_i64" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = args else {
                return Err("arithmetic oracle expects one i64".into());
            };
            self.0.set(self.0.get() + 1);
            Ok(MirRuntimeValue::Int(*value))
        }
    }

    let _guard = super::FfiEnvLock::lock();
    for (label, contract, argument, expected_status, expected_message, expected_code, output) in [
        (
            "add-overflow",
            "result + 1 > result",
            "9223372036854775807 as i64",
            crate::verifier::VerifStatus::Disproven,
            "overflow in FFI postcondition",
            Some("E0802"),
            "",
        ),
        (
            "subtract-overflow",
            "result - 1 < result",
            "(-9223372036854775807 as i64) - (1 as i64)",
            crate::verifier::VerifStatus::Disproven,
            "overflow in FFI postcondition",
            Some("E0802"),
            "",
        ),
        (
            "multiply-overflow",
            "result * 2 > result",
            "9223372036854775807 as i64",
            crate::verifier::VerifStatus::Disproven,
            "overflow in FFI postcondition",
            Some("E0802"),
            "",
        ),
        (
            "divide-overflow",
            "result / -1 == 9223372036854775807",
            "(-9223372036854775807 as i64) - (1 as i64)",
            crate::verifier::VerifStatus::Disproven,
            "overflow in FFI postcondition",
            Some("E0802"),
            "",
        ),
        (
            "short-circuit-overflow",
            "x == 0 or result + 1 > result",
            "0 as i64",
            crate::verifier::VerifStatus::Proven,
            "",
            None,
            "0\n",
        ),
    ] {
        let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let fixture = library_fixture(counter, C_SOURCE);
        let library = fixture.dir.join("ffi.so");
        std::env::set_var("MIMI_FFI_LIB", &library);
        let source = format!(
            r#"
extern "C" {{
    func mir_ffi_i64(x: i64) -> i64 ensures: {contract};
}}
func main() -> i64 {{
    println(mir_ffi_i64({argument}));
    0
}}
"#
        );
        let tokens = crate::lexer::Lexer::new(&source)
            .tokenize()
            .unwrap_or_else(|error| panic!("{label}: lex failed: {error}"));
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .unwrap_or_else(|error| panic!("{label}: parse failed: {error}"));
        let checked = crate::core::check_program(&file)
            .unwrap_or_else(|error| panic!("{label}: check failed: {error:?}"));
        let mir = MirProgram::from_checked_program(&checked)
            .unwrap_or_else(|error| panic!("{label}: materialization failed: {error:?}"));

        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = crate::verifier::verify_mir(&mir, label.into())
            .unwrap_or_else(|error| panic!("{label}: verifier failed: {error}"));
        assert_eq!(results.len(), 1, "{label}: one postcondition result");
        assert_eq!(results[0].status, expected_status, "{label}: {results:?}");
        assert!(
            results[0].message.contains("extern ensures contract"),
            "{label}: {results:?}"
        );
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
        for results in [
            crate::verifier::verify_checked(&checked, label.into()),
            crate::verifier::verify_checked_dual(&checked, label.into()),
            crate::verifier::verify_ffi_checked(&checked),
        ] {
            let results =
                results.unwrap_or_else(|error| panic!("{label}: public verifier: {error}"));
            assert_eq!(results.len(), 1, "{label}: public verifier result");
            assert_eq!(results[0].status, expected_status, "{label}: {results:?}");
        }
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

        let oracle = CountingOracle(Cell::new(0));
        let reference = MirReferenceInterpreter::new(&mir)
            .with_ffi_resolver(&oracle)
            .execute_with_output(&crate::core::NodeId("function:main".into()), &[]);
        assert_eq!(oracle.0.get(), 1, "{label}: foreign call must happen once");
        if expected_code.is_some() {
            let error = reference.unwrap_err();
            assert!(error.message.contains(expected_message), "{label}: {error}");
        } else {
            let value = reference.unwrap_or_else(|error| panic!("{label}: reference: {error}"));
            assert_eq!(value.value, MirRuntimeValue::Int(0));
            assert_eq!(value.output, output);
        }

        let mut vm = BytecodeVM::new(
            compile_mir_program(&mir)
                .unwrap_or_else(|error| panic!("{label}: bytecode: {error:?}")),
        );
        let vm_result = vm.run_value();
        if let Some(code) = expected_code {
            let error = vm_result.unwrap_err();
            assert_eq!(error.code(), code, "{label}: {error}");
            assert!(
                error.to_string().contains(expected_message),
                "{label}: {error}"
            );
            assert_eq!(vm.stdout(), "", "{label}: trap must precede println");
        } else {
            assert!(
                matches!(vm_result, Ok(Value::Int(0))),
                "{label}: {vm_result:?}"
            );
            assert_eq!(vm.stdout(), output);
        }

        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_arithmetic");
        generator
            .compile_mir_native(&mir)
            .unwrap_or_else(|error| panic!("{label}: native compile: {error:?}"));
        generator
            .module
            .verify()
            .unwrap_or_else(|error| panic!("{label}: native verify: {error}"));
        let config = super::E2EConfig {
            extra_c_src: Some(C_SOURCE.into()),
            ..Default::default()
        };
        let native = super::link_and_observe_module(
            &generator,
            &config,
            super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        )
        .unwrap_or_else(|error| panic!("{label}: native execution: {error}"));
        if let Some(code) = expected_code {
            assert_ne!(native.exit_code, Some(0), "{label}: native must trap");
            assert!(native.stderr.contains(code), "{label}: {}", native.stderr);
            assert_eq!(
                native.stdout, "",
                "{label}: native trap must precede println"
            );
        } else {
            assert_eq!(native.exit_code, Some(0), "{label}: {}", native.stderr);
            assert_eq!(native.stdout, output);
            assert_eq!(native.stderr, "");
        }
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn scalar_ffi_ensures_remainder_sign_and_short_circuit_parity() {
    struct IdentityOracle(Cell<u32>);
    impl MirReferenceFfiResolver for IdentityOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_i64" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = args else {
                return Err("remainder oracle expects one i64".into());
            };
            self.0.set(self.0.get() + 1);
            Ok(MirRuntimeValue::Int(*value))
        }
    }

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));

    for (label, contract, argument, expected_status, expected_code, expected_output) in [
        (
            "negative-sign",
            "result / 3 == -2 and result % 3 == -1",
            "-7 as i64",
            crate::verifier::VerifStatus::Disproven,
            None,
            "-7\n",
        ),
        (
            "short-circuit-remainder",
            "x == 0 or result % x == -1",
            "0 as i64",
            crate::verifier::VerifStatus::Proven,
            None,
            "0\n",
        ),
        (
            "remainder-by-zero",
            "result % x == 0",
            "0 as i64",
            crate::verifier::VerifStatus::Disproven,
            Some("E0801"),
            "",
        ),
    ] {
        let source = format!(
            r#"
extern "C" {{
    func mir_ffi_i64(x: i64) -> i64 ensures: {contract};
}}
func main() -> i64 {{
    println(mir_ffi_i64({argument}));
    0
}}
"#
        );
        let tokens = crate::lexer::Lexer::new(&source)
            .tokenize()
            .unwrap_or_else(|error| panic!("{label}: lex failed: {error}"));
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .unwrap_or_else(|error| panic!("{label}: parse failed: {error}"));
        let checked = crate::core::check_program(&file)
            .unwrap_or_else(|error| panic!("{label}: check failed: {error:?}"));
        let mir = MirProgram::from_checked_program(&checked)
            .unwrap_or_else(|error| panic!("{label}: materialization failed: {error:?}"));

        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let results = crate::verifier::verify_mir(&mir, label.into())
            .unwrap_or_else(|error| panic!("{label}: MIR verifier failed: {error}"));
        assert_eq!(results.len(), 1, "{label}: one ensures result");
        assert_eq!(results[0].status, expected_status, "{label}: {results:?}");
        assert!(
            results[0].message.contains("extern ensures contract"),
            "{label}: {results:?}"
        );
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

        let oracle = IdentityOracle(Cell::new(0));
        let reference = MirReferenceInterpreter::new(&mir)
            .with_ffi_resolver(&oracle)
            .execute_with_output(&crate::core::NodeId("function:main".into()), &[]);
        assert_eq!(oracle.0.get(), 1, "{label}: foreign call must happen once");
        if expected_code.is_some() {
            let error = reference.unwrap_err();
            let code = expected_code.expect("error code present");
            assert!(error.message.contains(code), "{label}: {error}");
            assert!(error.message.contains("postcondition"), "{label}: {error}");
        } else {
            let value = reference.unwrap_or_else(|error| panic!("{label}: reference: {error}"));
            assert_eq!(value.value, MirRuntimeValue::Int(0));
            assert_eq!(value.output, expected_output);
        }

        let mut vm = BytecodeVM::new(
            compile_mir_program(&mir)
                .unwrap_or_else(|error| panic!("{label}: bytecode: {error:?}")),
        );
        let vm_result = vm.run_value();
        if let Some(code) = expected_code {
            let error = vm_result.unwrap_err();
            assert_eq!(error.code(), code, "{label}: {error}");
            assert!(
                error.to_string().contains("postcondition"),
                "{label}: {error}"
            );
            assert_eq!(vm.stdout(), "", "{label}: trap must precede println");
        } else {
            assert!(
                matches!(vm_result, Ok(Value::Int(0))),
                "{label}: {vm_result:?}"
            );
            assert_eq!(vm.stdout(), expected_output);
        }

        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_remainder");
        generator
            .compile_mir_native(&mir)
            .unwrap_or_else(|error| panic!("{label}: native compile: {error:?}"));
        generator
            .module
            .verify()
            .unwrap_or_else(|error| panic!("{label}: native verify: {error}"));
        let config = super::E2EConfig {
            extra_c_src: Some(C_SOURCE.into()),
            ..Default::default()
        };
        let native = super::link_and_observe_module(
            &generator,
            &config,
            super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        )
        .unwrap_or_else(|error| panic!("{label}: native execution: {error}"));
        if let Some(code) = expected_code {
            assert_ne!(native.exit_code, Some(0), "{label}: native must trap");
            assert!(native.stderr.contains(code), "{label}: {}", native.stderr);
            assert_eq!(native.stdout, "", "{label}: trap must precede println");
        } else {
            assert_eq!(native.exit_code, Some(0), "{label}: {}", native.stderr);
            assert_eq!(native.stdout, expected_output);
            assert_eq!(native.stderr, "");
        }
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn scalar_ffi_ensures_multi_argument_negative_divisor_keeps_result_identity() {
    struct PairOracle(Cell<u32>);
    impl MirReferenceFfiResolver for PairOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_pair" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(left), MirRuntimeValue::Int(_right)] = args else {
                return Err("pair oracle expects two i64 arguments".into());
            };
            self.0.set(self.0.get() + 1);
            Ok(MirRuntimeValue::Int(*left))
        }
    }

    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_pair(int64_t left, int64_t right) { (void)right; return left; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_pair(left: i64, right: i64) -> i64
        ensures: result / right == left / right and result % right == left % right;
}
func main() -> i64 {
    let first = mir_ffi_pair(-7 as i64, -3 as i64);
    let second = mir_ffi_pair(7 as i64, -3 as i64);
    println(first);
    println(second);
    0
}
"#;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("multi-argument remainder FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("multi-argument remainder FFI materialization");
    assert_eq!(mir.ffi_calls().len(), 2, "two calls must keep two receipts");
    let receipts = mir.ffi_calls().values().collect::<Vec<_>>();
    assert!(receipts.iter().all(|receipt| receipt.arguments.len() == 2));
    let first_result = receipts[0].result.clone().expect("first result identity");
    let second_result = receipts[1].result.clone().expect("second result identity");
    assert_ne!(first_result, second_result, "call results must not alias");
    assert!(receipts.iter().all(|receipt| receipt.ensures.is_some()));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let mir_results = crate::verifier::verify_mir(&mir, "multi-argument-remainder".into())
        .expect("multi-argument remainder MIR verifier");
    assert_eq!(mir_results.len(), 2);
    assert!(mir_results.iter().all(|result| {
        result.status == crate::verifier::VerifStatus::Disproven
            && result.message.contains("extern ensures contract")
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    for results in [
        crate::verifier::verify_checked(&checked, "multi-argument-remainder".into()),
        crate::verifier::verify_checked_dual(&checked, "multi-argument-remainder-dual".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("public multi-argument remainder verifier");
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|result| { result.status == crate::verifier::VerifStatus::Disproven }));
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let oracle = PairOracle(Cell::new(0));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("multi-argument remainder reference");
    assert_eq!(oracle.0.get(), 2);
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "-7\n7\n");

    let mut vm = BytecodeVM::new(compile_mir_program(&mir).expect("multi-argument remainder VM"));
    assert!(vm.program().ast.is_none());
    assert!(matches!(
        vm.run_value().expect("multi-argument remainder VM run"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "-7\n7\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "multi_ffi_remainder");
    generator
        .compile_mir_native(&mir)
        .expect("multi-argument remainder native compile");
    generator
        .module
        .verify()
        .expect("multi-argument remainder native verify");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("multi-argument remainder native execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "-7\n7\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_ensures_mixed_i32_i64_abi_preserves_width_and_result_identity() {
    struct MixedOracle;
    impl MirReferenceFfiResolver for MixedOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_mixed" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(left), MirRuntimeValue::Int(right)] = args else {
                return Err("mixed oracle expects i32/i64 arguments".into());
            };
            if *right == 0 {
                return Err("mixed oracle received zero divisor".into());
            }
            Ok(MirRuntimeValue::Int(*left))
        }
    }

    const C_SOURCE: &str = r#"
#include <stdint.h>
int32_t mir_ffi_mixed(int32_t left, int64_t right) { (void)right; return left; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_mixed(left: i32, right: i64) -> i32
        ensures: result == left and right != 0;
}
func main() -> i32 {
    let value = mir_ffi_mixed(-7 as i32, 3 as i64);
    println(value);
    0
}
"#;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("mixed i32/i64 scalar FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("mixed i32/i64 scalar FFI materialization");
    let receipts = mir.ffi_calls().values().collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].arguments.len(), 2);
    assert!(receipts[0].result.is_some());
    assert!(receipts[0].ensures.is_some());

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let mir_results = crate::verifier::verify_mir(&mir, "mixed-i32-i64".into())
        .expect("mixed i32/i64 MIR verifier");
    assert_eq!(mir_results.len(), 1);
    assert_eq!(
        mir_results[0].status,
        crate::verifier::VerifStatus::Disproven
    );
    assert!(mir_results[0]
        .message
        .contains("extern ensures contract disproven"));
    for results in [
        crate::verifier::verify_checked(&checked, "mixed-i32-i64".into()),
        crate::verifier::verify_checked_dual(&checked, "mixed-i32-i64-dual".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("mixed i32/i64 public verifier");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, crate::verifier::VerifStatus::Disproven);
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&MixedOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("mixed i32/i64 reference execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "-7\n");

    let bytecode = compile_mir_program(&mir).expect("mixed i32/i64 bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("mixed i32/i64 VM"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "-7\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mixed_i32_i64_ffi");
    generator
        .compile_mir_native(&mir)
        .expect("mixed i32/i64 native compile");
    generator
        .module
        .verify()
        .expect("mixed i32/i64 native verify");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("mixed i32/i64 native execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "-7\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_mixed_width_min_value_and_zero_short_circuit_stay_defined() {
    struct MixedBoundaryOracle;
    impl MirReferenceFfiResolver for MixedBoundaryOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_mixed_min" && receipt.symbol != "mir_ffi_mixed_zero" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(left), MirRuntimeValue::Int(_right)] = args else {
                return Err("mixed boundary oracle expects i32/i64 arguments".into());
            };
            Ok(MirRuntimeValue::Int(*left))
        }
    }

    const C_SOURCE: &str = r#"
#include <stdint.h>
int32_t mir_ffi_mixed_min(int32_t left, int64_t right) { (void)right; return left; }
int32_t mir_ffi_mixed_zero(int32_t left, int64_t right) { (void)right; return left; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_mixed_min(left: i32, right: i64) -> i32
        ensures: result == left and right != 0;
    func mir_ffi_mixed_zero(left: i32, right: i64) -> i32
        ensures: result == left and (right == 0 or right / right == 1);
}
func main() -> i32 {
    let min_value = mir_ffi_mixed_min((-2147483648) as i32, 1 as i64);
    let zero_value = mir_ffi_mixed_zero((-2147483648) as i32, 0 as i64);
    println(min_value);
    println(zero_value);
    0
}
"#;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("mixed-width boundary FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("mixed-width boundary FFI materialization");
    let receipts = mir.ffi_calls().values().collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().all(|receipt| receipt.arguments.len() == 2));
    assert!(receipts.iter().all(|receipt| receipt.result.is_some()));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let mir_results = crate::verifier::verify_mir(&mir, "mixed-boundary".into())
        .expect("mixed-width boundary MIR verifier");
    assert_eq!(mir_results.len(), 2);
    assert!(mir_results
        .iter()
        .all(|result| result.status == crate::verifier::VerifStatus::Disproven));
    for results in [
        crate::verifier::verify_checked(&checked, "mixed-boundary".into()),
        crate::verifier::verify_checked_dual(&checked, "mixed-boundary-dual".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("mixed-width boundary public verifier");
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|result| result.status == crate::verifier::VerifStatus::Disproven));
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&MixedBoundaryOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("mixed-width boundary reference execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "-2147483648\n-2147483648\n");

    let bytecode = compile_mir_program(&mir).expect("mixed-width boundary bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("mixed-width boundary VM"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "-2147483648\n-2147483648\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mixed_width_boundary_ffi");
    generator
        .compile_mir_native(&mir)
        .expect("mixed-width boundary native compile");
    generator
        .module
        .verify()
        .expect("mixed-width boundary native verify");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("mixed-width boundary native execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "-2147483648\n-2147483648\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_receipt_digest_pins_argument_order_result_and_contract_phase() {
    const SOURCE: &str = r#"
extern "C" {
    func digest_pair(left: i64, right: i64) -> i64
        ensures: result / right == left / right and result % right == left % right;
}
func main() -> i64 {
    let first = digest_pair(-7 as i64, -3 as i64);
    let second = digest_pair(7 as i64, -3 as i64);
    0
}
"#;
    const SWAPPED_ARGS: &str = r#"
extern "C" {
    func digest_pair(left: i64, right: i64) -> i64
        ensures: result / right == left / right and result % right == left % right;
}
func main() -> i64 {
    let first = digest_pair(-3 as i64, -7 as i64);
    let second = digest_pair(-3 as i64, 7 as i64);
    0
}
"#;
    const REQUIRES_PHASE: &str = r#"
extern "C" {
    func digest_pair(left: i64, right: i64) -> i64
        requires: right != 0;
}
func main() -> i64 {
    let first = digest_pair(-7 as i64, -3 as i64);
    let second = digest_pair(7 as i64, -3 as i64);
    0
}
"#;

    let materialize = |source: &str| {
        let checked =
            crate::core::check_program(&super::parse(source)).expect("digest fixture check");
        MirProgram::from_checked_program(&checked).expect("digest fixture materialization")
    };
    let first = materialize(SOURCE);
    let second = materialize(SOURCE);
    assert_eq!(first.canonical_digest(), second.canonical_digest());
    assert_eq!(first.ffi_calls().len(), 2);
    let receipts = first.ffi_calls().values().collect::<Vec<_>>();
    let first_result = receipts[0].result.clone().expect("first digest result");
    let second_result = receipts[1].result.clone().expect("second digest result");
    assert_ne!(first_result, second_result);
    assert!(receipts.iter().all(|receipt| receipt.arguments.len() == 2));
    assert_ne!(
        first.canonical_digest(),
        materialize(SWAPPED_ARGS).canonical_digest(),
        "argument order must affect the canonical digest"
    );
    assert_ne!(
        first.canonical_digest(),
        materialize(REQUIRES_PHASE).canonical_digest(),
        "precondition/postcondition phase must affect the canonical digest"
    );
}

#[test]
fn scalar_ffi_ensures_violation_traps_after_foreign_call_in_all_consumers() {
    struct BadOracle;
    impl MirReferenceFfiResolver for BadOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_bad" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = args else {
                return Err("bad oracle expects one i64".into());
            };
            Ok(MirRuntimeValue::Int(value + 1))
        }
    }

    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_bad(int64_t x) { return x + 1; }
"#;
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, BAD_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);
    let source = r#"
extern "C" {
    func mir_ffi_bad(x: i64) -> i64 ensures: result == x;
}
func main() -> i64 { mir_ffi_bad(41 as i64); 0 }
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex bad FFI");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse bad FFI");
    let checked = crate::core::check_program(&file).expect("check bad FFI");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize bad FFI");
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&BadOracle)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must enforce the FFI postcondition");
    assert!(
        reference.message.contains("FFI postcondition failed"),
        "{reference}"
    );

    let mut vm = BytecodeVM::new(compile_mir_program(&mir).expect("bad FFI bytecode"));
    let vm_error = vm
        .run_value()
        .expect_err("bytecode must enforce the FFI postcondition");
    assert_eq!(vm_error.code(), "E0808");
    assert!(vm_error.to_string().contains("FFI postcondition failed"));

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_bad_ensures");
    generator
        .compile_mir_native(&mir)
        .expect("native bad FFI postcondition");
    generator
        .module
        .verify()
        .expect("valid native bad FFI module");
    let config = super::E2EConfig {
        extra_c_src: Some(BAD_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native bad FFI execution");
    assert_ne!(native.exit_code, Some(0));
    assert!(native.stderr.contains("E0808"), "{}", native.stderr);
}

#[test]
fn scalar_ffi_traps_preserve_external_effect_prefix_across_three_consumers() {
    use std::cell::RefCell;
    struct TraceOracle(RefCell<Vec<i64>>);
    impl MirReferenceFfiResolver for TraceOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            let [MirRuntimeValue::Int(value)] = args else {
                return Err("trace oracle expects one integer".into());
            };
            match receipt.symbol.as_str() {
                "mir_ffi_mark" => self.0.borrow_mut().push(*value),
                "mir_ffi_i64" => {}
                _ => return Err("unknown trace oracle symbol".into()),
            }
            Ok(MirRuntimeValue::Int(*value))
        }
    }
    let c_source = format!(
        r#"{C_SOURCE}
#include <stdio.h>
#include <stdlib.h>
int64_t mir_ffi_mark(int64_t x) {{
    const char *path = getenv("MIMI_CANONICAL_FFI_TRACE");
    if (!path) abort();
    FILE *f = fopen(path, "a");
    if (!f) abort();
    fprintf(f, "%lld\n", (long long)x);
    fclose(f);
    return x;
}}
"#
    );
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, &c_source);
    let trace_path = fixture.dir.join("trace.txt");
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));
    std::env::set_var("MIMI_CANONICAL_FFI_TRACE", &trace_path);

    for (requires, body, expected_trace, error, return_value, verify_ffi) in [
        ("x > 0", "mir_ffi_mark(4 as i64); let top = mir_ffi_i64(9223372036854775807 as i64); let value = top + (1 as i64); mir_ffi_mark(9 as i64); value",
            "4\n", Some(("E0802", "addition overflow")), 0, true),
        ("x > 0", "let top = mir_ffi_i64(9223372036854775807 as i64); mir_ffi_mark(top + (1 as i64))",
            "", Some(("E0802", "addition overflow")), 0, true),
        ("x > 0", "mir_ffi_mark(4 as i64); let low = mir_ffi_i64(-9223372036854775807 as i64) - (1 as i64); let value = low - (1 as i64); mir_ffi_mark(9 as i64); value",
            "4\n", Some(("E0802", "subtraction overflow")), 0, true),
        ("x > 0", "mir_ffi_mark(4 as i64); let value = mir_ffi_i64(20 as i64) + (1 as i64); mir_ffi_mark(9 as i64); value",
            "4\n9\n", None, 21, true),
        ("x > 0", "mir_ffi_mark(4 as i64); mir_ffi_mark(-1 as i64); mir_ffi_mark(9 as i64); 21 as i64",
            "4\n", Some(("E0808", "precondition")), 0, true),
        ("x > 0", "mir_ffi_mark(4 as i64); mir_ffi_mark(-1 as i64); mir_ffi_mark(9 as i64); 21 as i64",
            "4\n-1\n9\n", None, 21, false),
        ("x > 0 || 1 / (x - x) > 0", "mir_ffi_mark(4 as i64); 21 as i64",
            "4\n", None, 21, true),
        ("x > 0 || 1 / (x - x) > 0", "mir_ffi_mark(4 as i64); mir_ffi_mark(-1 as i64); 21 as i64",
            "4\n", Some(("E0801", "division by zero")), 0, true),
        ("x <= 4 || x * 2 > 0", "mir_ffi_mark(4 as i64); mir_ffi_mark(9223372036854775807 as i64); 21 as i64",
            "4\n", Some(("E0802", "overflow in FFI precondition")), 0, true),
        ("x >= 0 || -x > 0", "mir_ffi_mark(4 as i64); let low = mir_ffi_i64(-9223372036854775807 as i64) - (1 as i64); mir_ffi_mark(low); 21 as i64",
            "4\n", Some(("E0802", "overflow in FFI precondition")), 0, true),
        ("x / 3 == -2 && x % 3 == -1", "mir_ffi_mark(-7 as i64); 21 as i64",
            "-7\n", None, 21, true),
        ("x < 0 && 1 / 0 > 0", "mir_ffi_mark(4 as i64); 21 as i64",
            "", Some(("E0808", "precondition")), 0, true),
    ] {
        let source = format!(
            "extern \"C\" {{ func mir_ffi_mark(x: i64) -> i64 requires: {requires}; func mir_ffi_i64(x: i64) -> i64; }} func main() -> i64 {{ {body} }}"
        );
        let tokens = crate::lexer::Lexer::new(&source).tokenize().expect("lex trap fixture");
        let file = crate::parser::Parser::new(tokens).parse_file().expect("parse trap fixture");
        let checked = crate::core::check_program(&file).expect("check trap fixture");
        let mir = MirProgram::from_checked_program(&checked).expect("trap fixture MIR");
        let digest = mir.canonical_digest();
        let oracle = TraceOracle(RefCell::new(Vec::new()));
        let reference = MirReferenceInterpreter::new(&mir).with_ffi_resolver(&oracle)
            .with_ffi_verification(verify_ffi).execute(&crate::core::NodeId("function:main".into()), &[]);
        let reference_trace = oracle.0.borrow().iter().map(|x| format!("{x}\n")).collect::<String>();
        assert_eq!(reference_trace, expected_trace, "{body}");

        std::fs::write(&trace_path, "").expect("clear VM effect trace");
        let mut vm = BytecodeVM::new(compile_mir_program(&mir).expect("trap fixture bytecode"));
        vm.set_verify_ffi(verify_ffi);
        let vm_result = vm.run_value();
        assert_eq!(std::fs::read_to_string(&trace_path).unwrap(), expected_trace, "{body}");
        assert_eq!(vm.stdout(), "");

        std::fs::write(&trace_path, "").expect("clear native effect trace");
        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(&context, "ffi_trap_prefix");
        generator.verify_ffi = verify_ffi;
        generator.compile_mir_native(&mir).expect("trap fixture native");
        generator.module.verify().expect("valid trap fixture LLVM");
        let config = super::E2EConfig { extra_c_src: Some(c_source.clone()), ..Default::default() };
        let native = super::link_and_observe_module(&generator, &config,
            super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
            .expect("native trap fixture execution");
        assert_eq!(std::fs::read_to_string(&trace_path).unwrap(), expected_trace, "{body}");
        assert_eq!(native.stdout, "");
        if let Some((code, fragment)) = error {
            assert!(reference.expect_err("reference trap").message.contains(fragment), "{body}");
            assert_eq!(vm_result.expect_err("VM trap").code(), code, "{body}");
            assert_ne!(native.exit_code, Some(0), "{body}");
            assert!(native.stderr.contains(code), "{body}: {}", native.stderr);
        } else {
            assert_eq!(reference.unwrap(), MirRuntimeValue::Int(return_value));
            assert!(matches!(vm_result.unwrap(), Value::Int(n) if n == return_value));
            assert_eq!(native.exit_code, Some(return_value as i32));
            assert_eq!(native.stderr, "");
        }
        assert_eq!(mir.canonical_digest(), digest);
    }
}

#[test]
fn scalar_ffi_checked_apis_share_prelude_scope_and_contract_verdicts() {
    use crate::verifier::VerifStatus;
    for (source, expected) in [
        (
            r#"extern "C" { func transport(x: f64) -> f64; }
            func helper(x: f64) -> f64 { transport(x) }
            func main() -> i64 { let y = helper(42.5); transport(y); 0 }"#,
            VerifStatus::NoObligations,
        ),
        (
            r#"extern "C" { func foreign(x: i64) -> i64 requires: x > 0; }
            func helper(x: i64) -> i64 {
                if x > (0 as i64) { let y = x; foreign(y) } else { foreign(1 as i64) }
            }
            func main() -> i64 { let x = helper(-1 as i64); println(x); 0 }"#,
            VerifStatus::Verified,
        ),
        (
            r#"extern "C" { func foreign(x: i64) -> i64 requires: x > 0; }
            func helper(x: i64) -> i64 { let y = foreign(x); foreign(y) }
            func main() -> i64 { let x = helper(42 as i64); println(x); 0 }"#,
            VerifStatus::Failed,
        ),
    ] {
        let checked =
            crate::core::check_program(&super::parse_prod(source)).expect("check CLI scope");
        assert!(crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect("shared prelude exclusion");
        assert_eq!(
            route.program.functions().len(),
            2,
            "only user helper and main"
        );
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        for results in [
            crate::verifier::verify_checked(&checked, "ffi-route".into()),
            crate::verifier::verify_checked_dual(&checked, "ffi-route".into()),
        ] {
            let results = results.expect("checked MIR verifier");
            if expected == VerifStatus::NoObligations {
                assert!(
                    results.is_empty(),
                    "contract-free transport has no proof obligations"
                );
            } else {
                assert!(
                    results.iter().any(|result| result.status == expected),
                    "{results:?}"
                );
            }
            assert!(results.iter().all(|result| result
                .artifact
                .as_ref()
                .is_some_and(|artifact| artifact.engine
                    == crate::verifier::ProofArtifact::ENGINE_MIR
                    && artifact.mir_hash == route.program.canonical_digest())));
        }
        let ffi = crate::verifier::verify_ffi_checked(&checked).expect("FFI-only MIR verifier");
        if expected == VerifStatus::NoObligations {
            assert!(
                ffi.is_empty(),
                "opaque f64 transport claims no numerical proof"
            );
        } else {
            assert!(
                ffi.iter().any(|result| result.status == expected),
                "{ffi:?}"
            );
        }
        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(&context, "ffi_route_prelude");
        generator
            .compile_checked(&checked)
            .expect("direct native scalar FFI route");
        generator.module.verify().expect("valid native module");
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn scalar_ffi_checked_apis_reject_uncovered_graph_without_legacy() {
    for source in [
        r#"extern "C" { func foreign(x: f64) -> f64 requires: x > 0.0; }
            func main() -> f64 { foreign(42.5) }"#,
        r#"extern "C" { func foreign(x: i64) -> i64; }
            func main() -> i64 { let xs = [1, 2]; println(len(xs)); foreign(42 as i64) }"#,
    ] {
        let checked = crate::core::check_program(&super::parse_prod(source))
            .expect("check unsupported graph");
        assert!(crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        for result in [
            crate::verifier::verify_checked(&checked, String::new()),
            crate::verifier::verify_checked_dual(&checked, String::new()),
            crate::verifier::verify_ffi_checked(&checked),
        ] {
            assert!(result.is_err(), "unsupported graph must fail closed");
        }
        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(&context, "rejected_ffi_route");
        assert!(generator.compile_checked(&checked).is_err());
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn scalar_ffi_seeded_composition_matrix_shares_one_mir_across_consumers() {
    struct GeneratedOracle;
    impl MirReferenceFfiResolver for GeneratedOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "generated_foreign" {
                return Err(format!("unexpected generated symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = args else {
                return Err("generated scalar FFI expects one i64 argument".into());
            };
            Ok(MirRuntimeValue::Int(*value))
        }
    }

    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t generated_foreign(int64_t x) { return x; }
"#;
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };

    // This is a deliberately small deterministic generator.  The seed and
    // recurrence are part of the test contract, so a failure identifies the
    // exact shape/value pair without relying on ambient randomness.
    let mut seed = 0x5eed_cafe_u64;
    for case_index in 0..18_u64 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let shape = (seed % 6) as usize;
        let value = ((seed >> 11) % 63 + 1) as i64;
        let alternate = value + 1;
        let condition = (seed & 1) == 0;
        let (helper, body, expected) = match shape {
            0 => (
                "",
                format!("generated_foreign({value} as i64)"),
                value,
            ),
            1 => (
                "",
                format!("let x = generated_foreign({value} as i64); x"),
                value,
            ),
            2 => (
                "",
                format!(
                    "if {condition} {{ generated_foreign({value} as i64) }} else {{ generated_foreign({alternate} as i64) }}"
                ),
                if condition { value } else { alternate },
            ),
            3 => (
                "func relay(x: i64) -> i64 { generated_foreign(x) }",
                format!("relay({value} as i64)"),
                value,
            ),
            4 => (
                "func relay(x: i64) -> i64 { generated_foreign(x) }",
                format!(
                    "let x = relay({value} as i64); let y = generated_foreign(x); y"
                ),
                value,
            ),
            _ => (
                "func relay(x: i64) -> i64 { generated_foreign(x) }",
                format!(
                    "if {condition} {{ let x = relay({value} as i64); generated_foreign(x) }} else {{ generated_foreign({alternate} as i64) }}"
                ),
                if condition { value } else { alternate },
            ),
        };
        let expected_ffi_calls = match shape {
            0 | 1 | 3 => 1,
            2 | 4 => 2,
            _ => 3,
        };
        let source = format!(
            r#"extern "C" {{ func generated_foreign(x: i64) -> i64; }}
            {helper}
            func main() -> i64 {{ {body} }}"#
        );
        let checked = crate::core::check_program(&super::parse_prod(&source))
            .unwrap_or_else(|error| panic!("seeded case {case_index} rejected: {error:?}"));
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(admission.scalar_ffi, "seeded case {case_index}: {source}");
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .unwrap_or_else(|error| panic!("seeded case {case_index} materialization: {error}"));
        assert!(
            crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route),
            "seeded case {case_index}: {route:?}"
        );
        let mir = route.program;

        let oracle = GeneratedOracle;
        let reference = MirReferenceInterpreter::new(&mir)
            .with_ffi_resolver(&oracle)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .unwrap_or_else(|error| panic!("seeded case {case_index} reference: {error}"));
        assert_eq!(reference, MirRuntimeValue::Int(expected));

        let bytecode = compile_mir_program(&mir)
            .unwrap_or_else(|error| panic!("seeded case {case_index} bytecode: {error:?}"));
        assert!(bytecode.ast.is_none());
        let mut vm = BytecodeVM::new(bytecode);
        assert!(matches!(
            vm.run_value()
                .unwrap_or_else(|error| panic!("seeded case {case_index} VM: {error}")),
            Value::Int(value) if value == expected
        ));

        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(
            &context,
            &format!("seeded_scalar_ffi_{case_index}"),
        );
        generator
            .compile_mir_native(&mir)
            .unwrap_or_else(|error| panic!("seeded case {case_index} native: {error:?}"));
        generator
            .module
            .verify()
            .unwrap_or_else(|error| panic!("seeded case {case_index} LLVM: {error}"));
        let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let native = super::link_and_observe_module(&generator, &config, native_counter)
            .unwrap_or_else(|error| panic!("seeded case {case_index} link: {error}"));
        assert_eq!(native.exit_code, Some(expected as i32));
        assert_eq!(native.stdout, "");
        assert_eq!(native.stderr, "");
        assert_eq!(mir.ffi_calls().len(), expected_ffi_calls);
    }
}

#[test]
fn scalar_ffi_multi_argument_abi_shares_one_mir_across_consumers() {
    struct MultiArgumentOracle;
    impl MirReferenceFfiResolver for MultiArgumentOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), args) {
                ("generated_add", [MirRuntimeValue::Int(left), MirRuntimeValue::Int(right)]) => {
                    Ok(MirRuntimeValue::Int(left + right))
                }
                (
                    "generated_all",
                    [MirRuntimeValue::Bool(first), MirRuntimeValue::Bool(second), MirRuntimeValue::Bool(third)],
                ) => Ok(MirRuntimeValue::Bool(*first && *second && *third)),
                _ => Err(format!(
                    "unexpected multi-argument call: {receipt:?} {args:?}"
                )),
            }
        }
    }

    const C_SOURCE: &str = r#"
#include <stdbool.h>
#include <stdint.h>
int64_t generated_add(int64_t left, int64_t right) { return left + right; }
bool generated_all(bool first, bool second, bool third) {
    return first && second && third;
}
"#;
    const SOURCE: &str = r#"
extern "C" {
    func generated_add(left: i64, right: i64) -> i64
        requires: left >= 0 and right >= 0;
    func generated_all(first: bool, second: bool, third: bool) -> bool;
}
func relay(value: i64) -> i64 {
    if value > 0 as i64 {
        generated_add(value, 0 as i64)
    } else {
        generated_add(0 as i64, 0 as i64)
    }
}
func main() -> i64 {
    let value = generated_add(20 as i64, 22 as i64);
    if generated_all(true, true, true) { relay(value) } else { 0 }
}
"#;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));
    let file = super::parse_prod(SOURCE);
    let checked = crate::core::check_program(&file).expect("multi-argument C ABI fixture");
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    assert!(admission.scalar_ffi);
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("multi-argument scalar FFI materialization");
    assert!(crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route));
    assert_eq!(route.program.ffi_calls().len(), 4);
    let mir = route.program;
    let oracle = MultiArgumentOracle;
    assert_eq!(
        MirReferenceInterpreter::new(&mir)
            .with_ffi_resolver(&oracle)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("multi-argument reference"),
        MirRuntimeValue::Int(42)
    );

    let mut vm = BytecodeVM::new(compile_mir_program(&mir).expect("multi-argument bytecode"));
    assert!(vm.program().ast.is_none());
    assert!(matches!(
        vm.run_value().expect("multi-argument VM"),
        Value::Int(42)
    ));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verified = crate::verifier::verify_checked(&checked, "multi-argument".into())
        .expect("multi-argument verifier");
    assert!(verified.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Verified | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let dual = crate::verifier::verify_checked_dual(&checked, "multi-argument-dual".into())
        .expect("multi-argument dual verifier");
    assert!(dual.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Verified | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let ffi = crate::verifier::verify_ffi_checked(&checked).expect("multi-argument FFI verifier");
    assert!(ffi.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Verified | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "multi_argument_ffi");
    generator
        .compile_checked(&checked)
        .expect("multi-argument direct native route");
    generator.module.verify().expect("multi-argument LLVM");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native = super::link_and_observe_module(&generator, &config, native_counter)
        .expect("multi-argument native link");
    assert_eq!(native.exit_code, Some(42));
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_seeded_unsupported_compositions_reject_without_legacy() {
    const CASES: &[(&str, &str)] = &[
        (
            "float-contract",
            r#"extern "C" { func foreign(x: f64) -> f64 requires: x > 0.0; }
            func main() -> f64 { foreign(42.5) }"#,
        ),
        (
            "collection-composition",
            r#"extern "C" { func foreign(x: i64) -> i64; }
            func main() -> i64 { let xs = [1, 2]; println(len(xs)); foreign(42 as i64) }"#,
        ),
        (
            "defer-composition",
            r#"extern "C" { func foreign(x: i64) -> i64 requires: x > 0; }
            func main() -> i64 { defer { foreign(1 as i64) }; 0 }"#,
        ),
    ];
    let mut seed = 0xdec0_dead_u64;
    for round in 0..12_u64 {
        seed = seed
            .wrapping_mul(2862933555777941757)
            .wrapping_add(3037000493);
        let (label, source) = CASES[(seed as usize) % CASES.len()];
        let checked = crate::core::check_program(&super::parse_prod(source))
            .unwrap_or_else(|error| panic!("{label} round {round} check: {error:?}"));
        assert!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
            "{label} must be recognized before hard rejection"
        );
        let error = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect_err("unsupported scalar FFI composition must fail closed");
        assert!(
            error.to_string().contains("scalar-ffi-v1"),
            "{label}: {error}"
        );
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        assert!(crate::verifier::verify_checked(&checked, String::new()).is_err());
        assert!(crate::verifier::verify_checked_dual(&checked, String::new()).is_err());
        assert!(crate::verifier::verify_ffi_checked(&checked).is_err());
        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(&context, "rejected_seeded_ffi");
        assert!(generator.compile_checked(&checked).is_err());
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "{label} round {round} touched a compatibility owner"
        );
    }
}

#[test]
fn scalar_ffi_recursive_helpers_fail_closed_without_legacy() {
    const CASES: &[(&str, &str, usize)] = &[
        (
            "self-recursion",
            r#"extern "C" { func foreign(x: i64) -> i64 requires: x >= 0; }
            func recurse(x: i64) -> i64 {
                if x > (0 as i64) {
                    recurse(x - (1 as i64))
                } else {
                    foreign(0 as i64)
                }
            }
            func main() -> i64 { recurse(1 as i64) }"#,
            1,
        ),
        (
            "mutual-recursion",
            r#"extern "C" { func foreign(x: i64) -> i64 requires: x >= 0; }
            func alpha(x: i64) -> i64 {
                if x > (0 as i64) {
                    beta(x - (1 as i64))
                } else {
                    foreign(0 as i64)
                }
            }
            func beta(x: i64) -> i64 {
                if x > (0 as i64) {
                    alpha(x - (1 as i64))
                } else {
                    foreign(0 as i64)
                }
            }
            func main() -> i64 { alpha(1 as i64) }"#,
            2,
        ),
    ];

    for (label, source, expected_ffi_calls) in CASES {
        let checked = crate::core::check_program(&super::parse_prod(source))
            .unwrap_or_else(|error| panic!("{label} check: {error:?}"));
        let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
        assert!(
            admission.scalar_ffi,
            "{label} must cross scalar FFI admission"
        );
        let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .unwrap_or_else(|error| panic!("{label} materialization: {error}"));
        assert!(
            crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route),
            "{label} must carry a canonical FFI receipt"
        );
        assert_eq!(
            route.program.ffi_calls().len(),
            *expected_ffi_calls,
            "{label}"
        );

        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let error = crate::verifier::verify_checked(&checked, format!("{label}-single"))
            .expect_err("recursive scalar helper must fail the canonical verifier");
        assert!(error.contains("recursive"), "{label}: {error}");
        let error = crate::verifier::verify_checked_dual(&checked, format!("{label}-dual"))
            .expect_err("recursive scalar helper must fail the dual verifier");
        assert!(error.contains("recursive"), "{label}: {error}");
        let error = crate::verifier::verify_ffi_checked(&checked)
            .expect_err("recursive scalar helper must fail the FFI verifier");
        assert!(error.contains("recursive"), "{label}: {error}");

        let context = inkwell::context::Context::create();
        let mut generator = crate::codegen::CodeGenerator::new(&context, "recursive_ffi");
        let diagnostics = generator
            .compile_checked(&checked)
            .expect_err("direct native route must reject recursive MIR");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("recursive")),
            "{label}: {diagnostics:?}"
        );
        assert!(
            crate::core::CheckedProgram::test_legacy_body_access().is_empty(),
            "{label} touched a compatibility owner"
        );
    }
}

#[test]
fn ffi_checked_ignores_unrelated_callable_contract_without_legacy() {
    let source = r#"
        extern "C" { func foreign(value: string) -> i64; }
        func guarded(value: i64) -> i64 {
            requires: value > 0
            0
        }
        func main() -> i64 { guarded(1 as i64) }
    "#;
    let checked = crate::core::check_program(&super::parse_prod(source))
        .expect("unrelated callable contract fixture");
    assert!(!crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_ffi_checked(&checked)
        .expect("FFI-only verifier should have no extern obligations");
    assert!(
        results.is_empty(),
        "unrelated callable contract: {results:?}"
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn ffi_checked_preserves_legacy_for_unmigrated_string_contract() {
    if !crate::verifier::is_z3_available() {
        return;
    }
    let source = r#"
        extern "C" { func foreign(value: string) -> i64 requires: value != ""; }
        func main() -> i64 { foreign("ok") }
    "#;
    let checked = crate::core::check_program(&super::parse_prod(source))
        .expect("unmigrated string FFI contract fixture");
    assert!(!crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results =
        crate::verifier::verify_ffi_checked(&checked).expect("legacy string FFI contract verifier");
    assert_eq!(results.len(), 1, "string FFI call-site proof: {results:?}");
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        vec![crate::core::LegacyBodyConsumer::FfiVerifierCompatibility]
    );
}

#[test]
fn ffi_checked_does_not_open_legacy_for_uncalled_extern_contract() {
    if !crate::verifier::is_z3_available() {
        return;
    }
    let source = r#"
        extern "C" { func foreign(value: string) -> i64 requires: value != ""; }
        func main() -> i64 { 0 }
    "#;
    let checked = crate::core::check_program(&super::parse_prod(source))
        .expect("uncalled string FFI contract fixture");
    assert!(!crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_ffi_checked(&checked)
        .expect("uncalled extern contract has no FFI call-site obligations");
    assert!(results.is_empty(), "uncalled contract: {results:?}");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn ffi_checked_ignores_uncontracted_called_extern_when_other_contract_is_uncalled() {
    if !crate::verifier::is_z3_available() {
        return;
    }
    let source = r#"
        extern "C" {
            func guarded(value: string) -> i64 requires: value != "";
            func plain(value: string) -> i64;
        }
        func main() -> i64 { plain("ok") }
    "#;
    let checked = crate::core::check_program(&super::parse_prod(source))
        .expect("mixed called/uncalled string FFI fixture");
    assert!(!crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_ffi_checked(&checked)
        .expect("called uncontracted extern has no FFI obligation");
    assert!(
        results.is_empty(),
        "called uncontracted extern: {results:?}"
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn ffi_checked_compatibility_keeps_only_called_contract_externs() {
    if !crate::verifier::is_z3_available() {
        return;
    }
    let source = r#"
        extern "C" {
            func guarded(value: string) -> i64 requires: value != "";
            func plain(value: string) -> i64;
        }
        func main() -> i64 { guarded("ok") + plain("ok") }
    "#;
    let checked = crate::core::check_program(&super::parse_prod(source))
        .expect("mixed called contract/uncontracted string FFI fixture");
    assert!(!crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_ffi_checked(&checked)
        .expect("called string contract should use compatibility verifier");
    assert_eq!(results.len(), 1, "called contract results: {results:?}");
    assert!(results[0].func_name.contains("guarded"));
    assert!(!results[0].func_name.contains("plain"));
    assert_eq!(
        crate::core::CheckedProgram::test_legacy_body_access(),
        vec![crate::core::LegacyBodyConsumer::FfiVerifierCompatibility]
    );
}
