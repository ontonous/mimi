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
use crate::interp::bytecode::{
    compile_mir_program, BytecodeVM, CanonicalFfiDescriptor, CanonicalFfiScalarType,
};
use crate::interp::Value;

const C_SOURCE: &str = include_str!("../../tests/fixtures/mir_scalar_ffi_abi.c");
const SOURCE: &str = include_str!("../../tests/fixtures/mir_scalar_ffi_abi.mimi");
const MIXED_WIDTH_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_widen_arg(int64_t x) { return x + 1; }
int32_t mir_ffi_i32_result(int64_t x) { return (int32_t)x; }
"#;
const MIXED_WIDTH_SOURCE: &str = r#"
extern "C" {
    func mir_ffi_widen_arg(x: i64) -> i64;
    func mir_ffi_i32_result(x: i64) -> i32;
}
func main() -> i32 {
    println(mir_ffi_widen_arg(41 as i32))
    println(mir_ffi_i32_result(7 as i64))
    0
}
"#;
const MIXED_F64_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_expect_f64(double x) { return x == 7.0 ? 42 : -1; }
"#;
const MIXED_F64_SOURCE: &str = r#"
extern "C" { func mir_ffi_expect_f64(x: f64) -> i64; }
func main() -> i64 { mir_ffi_expect_f64(7 as i32) }
"#;
const RESULT_CONVERSION_F64_C_SOURCE: &str = r#"
#include <stdint.h>
double mir_ffi_result_f64(int64_t x) { return x == 7 ? 42.75 : -1.0; }
"#;
const RESULT_CONVERSION_F64_SOURCE: &str = r#"
extern "C" { func mir_ffi_result_f64(x: i64) -> f64; }
func main() -> f64 { mir_ffi_result_f64(7 as i64) }
"#;
const RESULT_CONVERSION_I64_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_result_i64(int64_t x) { return x == 7 ? 42 : -1; }
"#;
const RESULT_CONVERSION_I64_SOURCE: &str = r#"
extern "C" { func mir_ffi_result_i64(x: i64) -> i64; }
func main() -> i64 { mir_ffi_result_i64(7 as i64) }
"#;
const RESULT_RANGE_I64_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_result_i64_overflow(int64_t x) { (void)x; return 2147483648LL; }
"#;
const RESULT_RANGE_I64_SOURCE: &str = r#"
extern "C" { func mir_ffi_result_i64_overflow(x: i64) -> i64; }
func main() -> i64 { mir_ffi_result_i64_overflow(7 as i64) }
"#;
const RESULT_RANGE_F64_C_SOURCE: &str = r#"
#include <math.h>
#include <stdint.h>
double mir_ffi_result_f64_nan(int64_t x) { (void)x; return NAN; }
"#;
const RESULT_RANGE_F64_SOURCE: &str = r#"
extern "C" { func mir_ffi_result_f64_nan(x: i64) -> f64; }
func main() -> f64 { mir_ffi_result_f64_nan(7 as i64) }
"#;
const MULTI_CALL_NONFINITE_C_SOURCE: &str = r#"
#include <math.h>
#include <stdint.h>
int64_t mir_ffi_prefix_value(int64_t x) { return x + 100; }
double mir_ffi_nonfinite(int64_t x) { (void)x; return NAN; }
"#;
const MULTI_CALL_NONFINITE_SOURCE: &str = r#"
extern "C" {
    func mir_ffi_prefix_value(x: i64) -> i64;
    func mir_ffi_nonfinite(x: i64) -> f64;
}
func main() -> f64 {
    let first = mir_ffi_prefix_value(7 as i64)
    println(first)
    mir_ffi_nonfinite(8 as i64)
}
"#;
const F64_I32_RANGE_C_SOURCE: &str = r#"
#include <stdint.h>
double mir_ffi_f64_i32_edge(int64_t x) { (void)x; return 2147483647.75; }
double mir_ffi_f64_i32_oob(int64_t x) { (void)x; return 2147483648.0; }
"#;
const F64_I32_RANGE_SOURCE: &str = r#"
extern "C" {
    func mir_ffi_f64_i32_edge(x: i64) -> f64;
    func mir_ffi_f64_i32_oob(x: i64) -> f64;
}
func main() -> f64 {
    mir_ffi_f64_i32_edge(1 as i64)
    println(0 as i64)
    mir_ffi_f64_i32_oob(2 as i64)
}
"#;
const MULTI_CALL_REQUIRES_C_SOURCE: &str = r#"
#include <stdint.h>
static int64_t call_count;
int64_t mir_ffi_requires_sequence(int64_t value) {
    ++call_count;
    return call_count * 100 + value;
}
"#;
const MULTI_CALL_REQUIRES_SOURCE: &str = r#"
extern "C" { func mir_ffi_requires_sequence(x: i64) -> i64 requires: x >= 0; }
func main() -> i64 {
    let first = mir_ffi_requires_sequence(7 as i64)
    println(first)
    mir_ffi_requires_sequence(-8 as i64)
}
"#;
const MISSING_SYMBOL_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_present_only(int64_t value) { return value + 1; }
"#;
const MISSING_SYMBOL_SOURCE: &str = r#"
extern "C" { func mir_ffi_absent_symbol(value: i64) -> i64; }
func main() -> i64 { mir_ffi_absent_symbol(7 as i64) }
"#;
const REBINDABLE_SYMBOL_A_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_rebindable(int64_t value) { return value + 11; }
"#;
const REBINDABLE_SYMBOL_B_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_rebindable(int64_t value) { return value + 22; }
"#;
const ALIASED_F64_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_expect_alias_f64(double x) { return x == 7.0 ? 42 : -1; }
"#;
const ALIASED_F64_SOURCE: &str = r#"
type Scalar = f64
type Real = Scalar
extern "C" { func mir_ffi_expect_alias_f64(value: Real) -> i64; }
func main() -> i64 {
    println(mir_ffi_expect_alias_f64(7 as i64))
    0
}
"#;
const IMPORTED_ALIAS_F64_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_import_alias_f64(double x) { return x == 7.0 ? 42 : -1; }
"#;
const ALIASED_SCALAR_MATRIX_C_SOURCE: &str = r#"
#include <stdbool.h>
#include <stdint.h>
int32_t mir_ffi_alias_i32(int32_t x) { return x + 1; }
int64_t mir_ffi_alias_i64(int64_t x) { return x + 1; }
bool mir_ffi_alias_bool(bool x) { return !x; }
double mir_ffi_alias_f64(double x) { return x; }
void mir_ffi_alias_unit(void) {}
"#;
const ALIASED_SCALAR_MATRIX_SOURCE: &str = r#"
type AliasI32 = i32
type AliasI64 = i64
type AliasBool = bool
type AliasF64 = f64
type AliasUnit = ()
extern "C" {
    func mir_ffi_alias_i32(value: AliasI32) -> AliasI32;
    func mir_ffi_alias_i64(value: AliasI64) -> AliasI64;
    func mir_ffi_alias_bool(value: AliasBool) -> AliasBool;
    func mir_ffi_alias_f64(value: AliasF64) -> AliasF64;
    func mir_ffi_alias_unit() -> AliasUnit;
}
func main() -> i64 {
    let i32_value = mir_ffi_alias_i32(41 as i32)
    let i64_value = mir_ffi_alias_i64(41 as i64)
    let bool_value = mir_ffi_alias_bool(false)
    let float_value = mir_ffi_alias_f64(7.0)
    mir_ffi_alias_unit()
    if bool_value {
        i64_value + (i32_value as i64)
    } else {
        0
    }
}
"#;

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
fn scalar_ffi_mixed_width_argument_conversion_matches_three_consumers() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MIXED_WIDTH_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(MIXED_WIDTH_SOURCE)
        .tokenize()
        .expect("lex mixed-width C ABI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse mixed-width C ABI fixture");
    let checked = crate::core::check_program(&file).expect("check mixed-width C ABI fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize mixed-width C ABI MIR");
    assert_eq!(mir.ffi_calls().len(), 2);
    let mut receipts = mir.ffi_calls().values();
    let widen = receipts
        .find(|receipt| receipt.symbol == "mir_ffi_widen_arg")
        .expect("widening FFI receipt");
    assert_eq!(
        widen.parameter_conversions,
        vec![crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            to: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
        }]
    );
    let i32_result = mir
        .ffi_calls()
        .values()
        .find(|receipt| receipt.symbol == "mir_ffi_i32_result")
        .expect("i32-result FFI receipt");
    assert_eq!(
        i32_result.result_conversion,
        Some(crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            to: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
        })
    );

    struct MixedWidthOracle;
    impl MirReferenceFfiResolver for MixedWidthOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), arguments) {
                ("mir_ffi_widen_arg", [MirRuntimeValue::Int(value)]) => {
                    if receipt.parameter_conversions.first().copied()
                        != Some(crate::core::mir::MirFfiAbiConversion {
                            from: MirAbiClass::Integer {
                                bits: 32,
                                signed: true,
                            },
                            to: MirAbiClass::Integer {
                                bits: 64,
                                signed: true,
                            },
                        })
                    {
                        return Err("widening receipt mismatch".into());
                    }
                    Ok(MirRuntimeValue::Int(value + 1))
                }
                ("mir_ffi_i32_result", [MirRuntimeValue::Int(value)]) => {
                    if receipt.result_conversion
                        != Some(crate::core::mir::MirFfiAbiConversion {
                            from: MirAbiClass::Integer {
                                bits: 32,
                                signed: true,
                            },
                            to: MirAbiClass::Integer {
                                bits: 32,
                                signed: true,
                            },
                        })
                    {
                        return Err("i32-result receipt mismatch".into());
                    }
                    Ok(MirRuntimeValue::Int(*value))
                }
                _ => Err("unexpected mixed-width FFI call".into()),
            }
        }
    }

    let oracle = MixedWidthOracle;
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference mixed-width FFI execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "42\n7\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free mixed-width FFI bytecode");
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("bytecode mixed-width FFI execution"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "42\n7\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_mixed_width");
    generator
        .compile_mir_native(&mir)
        .expect("native mixed-width FFI lowering");
    generator
        .module
        .verify()
        .expect("valid mixed-width LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(MIXED_WIDTH_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native mixed-width FFI execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "42\n7\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_result_conversion_matches_three_consumers() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_CONVERSION_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(RESULT_CONVERSION_F64_SOURCE)
        .tokenize()
        .expect("lex result conversion FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse result conversion FFI fixture");
    let checked = crate::core::check_program(&file).expect("check result conversion FFI fixture");
    let mut mir =
        MirProgram::from_checked_program(&checked).expect("materialize result conversion FFI MIR");
    let owner = crate::core::NodeId("function:main".into());
    let (instruction_id, result_id) = mir
        .ffi_calls()
        .iter()
        .next()
        .map(|(instruction, receipt)| {
            (
                instruction.clone(),
                receipt.result.clone().expect("result conversion value"),
            )
        })
        .expect("result conversion receipt");
    let i64_type = mir
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("i64 result TypeDesc");
    // Surface calls currently inherit the declaration result type.  This
    // canonical fixture models a caller-side i64 result slot so the existing
    // checker-owned f64 -> i64 result conversion receipt is exercised by all
    // consumers without reopening surface type inference.
    mir.replace_function_result_and_value_type_for_test_only(&owner, &result_id, i64_type.clone());
    let result_conversion = crate::core::mir::MirFfiAbiConversion {
        from: MirAbiClass::Float { bits: 64 },
        to: MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
    };
    let mut receipts = mir.ffi_calls().clone();
    receipts
        .get_mut(&instruction_id)
        .expect("result conversion receipt")
        .result_conversion = Some(result_conversion);
    mir.replace_ffi_calls_for_test_only(receipts);
    let receipt = mir
        .ffi_calls()
        .get(&instruction_id)
        .expect("updated result conversion receipt");
    assert_eq!(
        receipt.result_type,
        mir.type_catalog()
            .iter()
            .find_map(|(id, descriptor)| {
                (descriptor.abi == MirAbiClass::Float { bits: 64 }).then(|| id.clone())
            })
            .expect("f64 declaration TypeDesc")
    );
    assert_eq!(receipt.result_conversion, Some(result_conversion));
    assert_eq!(
        mir.functions()[&owner].values[&result_id].ty,
        i64_type,
        "the result slot must be the conversion target"
    );

    struct ResultConversionOracle;
    impl MirReferenceFfiResolver for ResultConversionOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_result_f64" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            if arguments != [MirRuntimeValue::Int(7)] {
                return Err(format!(
                    "unexpected result conversion arguments {arguments:?}"
                ));
            }
            if receipt.result_conversion
                != Some(crate::core::mir::MirFfiAbiConversion {
                    from: MirAbiClass::Float { bits: 64 },
                    to: MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    },
                })
            {
                return Err("result conversion receipt mismatch".into());
            }
            Ok(MirRuntimeValue::FloatBits(42.75_f64.to_bits()))
        }
    }

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&ResultConversionOracle)
        .execute(&owner, &[])
        .expect("reference result conversion FFI execution");
    assert_eq!(reference, MirRuntimeValue::Int(42));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-result-conversion".into())
        .expect("verify result conversion MIR");
    assert!(verification.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Proven | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free result conversion bytecode");
    assert!(bytecode.ast.is_none());
    let bytecode_value = BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode result conversion FFI execution");
    assert_eq!(bytecode_value, Value::Int(42));

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_result");
    generator
        .compile_mir_native(&mir)
        .expect("native result conversion FFI lowering");
    generator
        .module
        .verify()
        .expect("valid result conversion LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(RESULT_CONVERSION_F64_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native result conversion FFI execution");
    assert_eq!(native.exit_code, Some(42));
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_integer_narrow_result_conversion_matches_three_consumers() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_CONVERSION_I64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(RESULT_CONVERSION_I64_SOURCE)
        .tokenize()
        .expect("lex integer narrow result conversion FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse integer narrow result conversion FFI fixture");
    let checked = crate::core::check_program(&file)
        .expect("check integer narrow result conversion FFI fixture");
    let mut mir = MirProgram::from_checked_program(&checked)
        .expect("materialize integer narrow result conversion FFI MIR");
    let owner = crate::core::NodeId("function:main".into());
    let (instruction_id, result_id) = mir
        .ffi_calls()
        .iter()
        .next()
        .map(|(instruction, receipt)| {
            (
                instruction.clone(),
                receipt.result.clone().expect("integer narrow result value"),
            )
        })
        .expect("integer narrow result receipt");
    let i32_type = mir
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("i32 result TypeDesc");
    // As in the floating-point result fixture, the surface declaration owns
    // the call result today.  This test-only adjustment models a caller-side
    // narrow ABI slot so the integer result conversion is executed physically
    // by every canonical consumer.
    mir.replace_function_result_and_value_type_for_test_only(&owner, &result_id, i32_type.clone());
    let result_conversion = crate::core::mir::MirFfiAbiConversion {
        from: MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
        to: MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
    };
    let mut receipts = mir.ffi_calls().clone();
    receipts
        .get_mut(&instruction_id)
        .expect("integer narrow result receipt")
        .result_conversion = Some(result_conversion);
    mir.replace_ffi_calls_for_test_only(receipts);
    let receipt = mir
        .ffi_calls()
        .get(&instruction_id)
        .expect("updated integer narrow result receipt");
    assert_eq!(
        receipt.result_type,
        mir.type_catalog()
            .iter()
            .find_map(|(id, descriptor)| {
                (descriptor.abi
                    == MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    })
                .then(|| id.clone())
            })
            .expect("i64 declaration TypeDesc")
    );
    assert_eq!(receipt.result_conversion, Some(result_conversion));
    assert_eq!(
        mir.functions()[&owner].values[&result_id].ty,
        receipt
            .result_conversion
            .as_ref()
            .and_then(|conversion| {
                (conversion.to
                    == MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    })
                .then(|| i32_type.clone())
            })
            .expect("i32 conversion target")
    );

    struct IntegerNarrowResultOracle;
    impl MirReferenceFfiResolver for IntegerNarrowResultOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_result_i64" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            if arguments != [MirRuntimeValue::Int(7)] {
                return Err(format!(
                    "unexpected integer narrow result arguments {arguments:?}"
                ));
            }
            if receipt.result_conversion
                != Some(crate::core::mir::MirFfiAbiConversion {
                    from: MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    },
                    to: MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    },
                })
            {
                return Err("integer narrow result conversion receipt mismatch".into());
            }
            Ok(MirRuntimeValue::Int(42))
        }
    }

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&IntegerNarrowResultOracle)
        .execute(&owner, &[])
        .expect("reference integer narrow result conversion FFI execution");
    assert_eq!(reference, MirRuntimeValue::Int(42));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-result-narrow".into())
        .expect("verify integer narrow result conversion MIR");
    assert!(verification.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Proven | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free integer narrow result bytecode");
    assert!(bytecode.ast.is_none());
    let bytecode_value = BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode integer narrow result conversion FFI execution");
    assert_eq!(bytecode_value, Value::Int(42));

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_narrow");
    generator
        .compile_mir_native(&mir)
        .expect("native integer narrow result conversion FFI lowering");
    generator
        .module
        .verify()
        .expect("valid integer narrow result conversion LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(RESULT_CONVERSION_I64_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native integer narrow result conversion FFI execution");
    assert_eq!(native.exit_code, Some(42));
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_integer_narrow_result_range_failure_matches_consumers() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_RANGE_I64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(RESULT_RANGE_I64_SOURCE)
        .tokenize()
        .expect("lex integer narrow result range fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse integer narrow result range fixture");
    let checked = crate::core::check_program(&file).expect("check integer narrow result range");
    let mut mir = MirProgram::from_checked_program(&checked)
        .expect("materialize integer narrow result range MIR");
    let owner = crate::core::NodeId("function:main".into());
    let (instruction_id, result_id) = mir
        .ffi_calls()
        .iter()
        .next()
        .map(|(instruction, receipt)| {
            (
                instruction.clone(),
                receipt
                    .result
                    .clone()
                    .expect("integer narrow range result value"),
            )
        })
        .expect("integer narrow range receipt");
    let i32_type = mir
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("i32 range target TypeDesc");
    // Keep the range check at the canonical receipt boundary.  The source
    // result is intentionally too large for the caller's i32 slot, so every
    // executable consumer must reject the conversion before returning.
    mir.replace_function_result_and_value_type_for_test_only(&owner, &result_id, i32_type);
    let result_conversion = crate::core::mir::MirFfiAbiConversion {
        from: MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
        to: MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
    };
    let mut receipts = mir.ffi_calls().clone();
    receipts
        .get_mut(&instruction_id)
        .expect("integer narrow range receipt")
        .result_conversion = Some(result_conversion);
    mir.replace_ffi_calls_for_test_only(receipts);

    struct OutOfRangeResultOracle;
    impl MirReferenceFfiResolver for OutOfRangeResultOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_result_i64_overflow" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            if arguments != [MirRuntimeValue::Int(7)] {
                return Err(format!("unexpected range arguments {arguments:?}"));
            }
            if receipt.result_conversion
                != Some(crate::core::mir::MirFfiAbiConversion {
                    from: MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    },
                    to: MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    },
                })
            {
                return Err("integer narrow range receipt mismatch".into());
            }
            Ok(MirRuntimeValue::Int(i64::from(i32::MAX) + 1))
        }
    }

    let reference_error = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&OutOfRangeResultOracle)
        .execute(&owner, &[])
        .expect_err("reference must reject an out-of-range FFI result");
    assert!(
        reference_error.to_string().contains("outside i32"),
        "{reference_error}"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-result-range".into())
        .expect("verify range-guarded result conversion MIR");
    assert!(verification.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Proven | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free integer narrow range bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    let bytecode_error = vm
        .run_value()
        .expect_err("bytecode must reject an out-of-range FFI result");
    assert!(
        bytecode_error.to_string().contains("outside i32"),
        "{bytecode_error}"
    );
    assert_eq!(vm.stdout(), "");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_range");
    generator
        .compile_mir_native(&mir)
        .expect("native integer narrow range FFI lowering");
    generator
        .module
        .verify()
        .expect("valid integer narrow range LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(RESULT_RANGE_I64_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native integer narrow range FFI execution");
    assert_ne!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "");
    assert!(
        native.stderr.contains("E0802")
            && native
                .stderr
                .contains("FFI integer result conversion out of range"),
        "{}",
        native.stderr
    );
}

#[test]
fn scalar_ffi_float_to_integer_result_nonfinite_failure_matches_consumers() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_RANGE_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(RESULT_RANGE_F64_SOURCE)
        .tokenize()
        .expect("lex non-finite result conversion fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse non-finite result conversion fixture");
    let checked = crate::core::check_program(&file).expect("check non-finite result conversion");
    let mut mir = MirProgram::from_checked_program(&checked)
        .expect("materialize non-finite result conversion MIR");
    let owner = crate::core::NodeId("function:main".into());
    let (instruction_id, result_id) = mir
        .ffi_calls()
        .iter()
        .next()
        .map(|(instruction, receipt)| {
            (
                instruction.clone(),
                receipt
                    .result
                    .clone()
                    .expect("non-finite result conversion value"),
            )
        })
        .expect("non-finite result conversion receipt");
    let i64_type = mir
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("i64 non-finite result target TypeDesc");
    // Model the caller-side integer result slot so this test reaches the
    // checker-owned FloatToSignedInteger receipt in every consumer.
    mir.replace_function_result_and_value_type_for_test_only(&owner, &result_id, i64_type);
    let result_conversion = crate::core::mir::MirFfiAbiConversion {
        from: MirAbiClass::Float { bits: 64 },
        to: MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
    };
    let mut receipts = mir.ffi_calls().clone();
    receipts
        .get_mut(&instruction_id)
        .expect("non-finite result conversion receipt")
        .result_conversion = Some(result_conversion);
    mir.replace_ffi_calls_for_test_only(receipts);

    struct NonFiniteResultOracle;
    impl MirReferenceFfiResolver for NonFiniteResultOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_result_f64_nan" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            if arguments != [MirRuntimeValue::Int(7)] {
                return Err(format!("unexpected non-finite arguments {arguments:?}"));
            }
            if receipt.result_conversion
                != Some(crate::core::mir::MirFfiAbiConversion {
                    from: MirAbiClass::Float { bits: 64 },
                    to: MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    },
                })
            {
                return Err("non-finite result conversion receipt mismatch".into());
            }
            Ok(MirRuntimeValue::FloatBits(f64::NAN.to_bits()))
        }
    }

    let reference_error = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&NonFiniteResultOracle)
        .execute(&owner, &[])
        .expect_err("reference must reject a non-finite FFI result conversion");
    assert!(reference_error
        .to_string()
        .contains("outside target integer range"));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-result-nonfinite".into())
        .expect("verify non-finite result conversion MIR");
    assert!(verification.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Proven | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free non-finite result bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    let bytecode_error = vm
        .run_value()
        .expect_err("bytecode must reject a non-finite FFI result conversion");
    assert!(bytecode_error
        .to_string()
        .contains("outside target integer range"));
    assert_eq!(vm.stdout(), "");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_nonfinite");
    generator
        .compile_mir_native(&mir)
        .expect("native non-finite result conversion lowering");
    generator
        .module
        .verify()
        .expect("valid non-finite result conversion LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(RESULT_RANGE_F64_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native non-finite result conversion execution");
    assert_ne!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "");
    assert!(
        native.stderr.contains("E0802")
            && native
                .stderr
                .contains("FFI integer result conversion out of range"),
        "{}",
        native.stderr
    );
}

#[test]
fn scalar_ffi_multi_call_nonfinite_result_preserves_prefix_effect() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MULTI_CALL_NONFINITE_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(MULTI_CALL_NONFINITE_SOURCE)
        .tokenize()
        .expect("lex multi-call non-finite FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse multi-call non-finite FFI fixture");
    let checked = crate::core::check_program(&file).expect("check multi-call non-finite FFI");
    let mut mir = MirProgram::from_checked_program(&checked)
        .expect("materialize multi-call non-finite FFI MIR");
    assert_eq!(mir.ffi_calls().len(), 2);
    let owner = crate::core::NodeId("function:main".into());
    let (instruction_id, result_id) = mir
        .ffi_calls()
        .iter()
        .find_map(|(instruction, receipt)| {
            (receipt.symbol == "mir_ffi_nonfinite").then(|| {
                (
                    instruction.clone(),
                    receipt
                        .result
                        .clone()
                        .expect("multi-call non-finite result value"),
                )
            })
        })
        .expect("multi-call non-finite receipt");
    let i64_type = mir
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("i64 multi-call non-finite result target TypeDesc");
    // The source function is f64 because the second extern declaration owns
    // that call result.  Rebind only the tail result slot to i64 so the
    // checker-owned result conversion is exercised after the first call has
    // already produced observable output.
    mir.replace_function_result_and_value_type_for_test_only(&owner, &result_id, i64_type);
    let result_conversion = crate::core::mir::MirFfiAbiConversion {
        from: MirAbiClass::Float { bits: 64 },
        to: MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
    };
    let mut receipts = mir.ffi_calls().clone();
    receipts
        .get_mut(&instruction_id)
        .expect("multi-call non-finite receipt")
        .result_conversion = Some(result_conversion);
    mir.replace_ffi_calls_for_test_only(receipts);

    struct PrefixThenNonFiniteOracle(Cell<i64>);
    impl MirReferenceFfiResolver for PrefixThenNonFiniteOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), arguments) {
                ("mir_ffi_prefix_value", [MirRuntimeValue::Int(7)]) => {
                    self.0.set(self.0.get() + 1);
                    Ok(MirRuntimeValue::Int(107))
                }
                ("mir_ffi_nonfinite", [MirRuntimeValue::Int(8)]) => {
                    self.0.set(self.0.get() + 1);
                    if receipt.result_conversion
                        != Some(crate::core::mir::MirFfiAbiConversion {
                            from: MirAbiClass::Float { bits: 64 },
                            to: MirAbiClass::Integer {
                                bits: 64,
                                signed: true,
                            },
                        })
                    {
                        return Err("multi-call non-finite receipt mismatch".into());
                    }
                    Ok(MirRuntimeValue::FloatBits(f64::NAN.to_bits()))
                }
                _ => Err(format!(
                    "unexpected multi-call non-finite invocation: {} {arguments:?}",
                    receipt.symbol
                )),
            }
        }
    }

    let oracle = PrefixThenNonFiniteOracle(Cell::new(0));
    let reference_error = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&owner, &[])
        .expect_err("reference must reject the second non-finite result conversion");
    assert!(reference_error
        .to_string()
        .contains("outside target integer range"));
    assert_eq!(
        oracle.0.get(),
        2,
        "reference must invoke the successful prefix and then the failing call"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-multi-nonfinite".into())
        .expect("verify multi-call non-finite result conversion MIR");
    assert!(verification.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Proven | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free multi-call non-finite bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    let bytecode_error = vm
        .run_value()
        .expect_err("bytecode must reject the second non-finite result conversion");
    assert!(bytecode_error
        .to_string()
        .contains("outside target integer range"));
    assert_eq!(vm.stdout(), "107\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_multi_nan");
    generator
        .compile_mir_native(&mir)
        .expect("native multi-call non-finite result conversion lowering");
    generator
        .module
        .verify()
        .expect("valid multi-call non-finite result conversion LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(MULTI_CALL_NONFINITE_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native multi-call non-finite result conversion execution");
    assert_ne!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "107\n");
    assert!(
        native.stderr.contains("E0802")
            && native
                .stderr
                .contains("FFI integer result conversion out of range"),
        "{}",
        native.stderr
    );
}

#[test]
fn scalar_ffi_float_narrow_result_conversion_matches_three_consumers() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_CONVERSION_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(RESULT_CONVERSION_F64_SOURCE)
        .tokenize()
        .expect("lex floating narrow result conversion FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse floating narrow result conversion FFI fixture");
    let checked = crate::core::check_program(&file)
        .expect("check floating narrow result conversion FFI fixture");
    let mut mir = MirProgram::from_checked_program(&checked)
        .expect("materialize floating narrow result conversion FFI MIR");
    let owner = crate::core::NodeId("function:main".into());
    let (instruction_id, result_id) = mir
        .ffi_calls()
        .iter()
        .next()
        .map(|(instruction, receipt)| {
            (
                instruction.clone(),
                receipt
                    .result
                    .clone()
                    .expect("floating narrow result conversion value"),
            )
        })
        .expect("floating narrow result conversion receipt");
    let i32_type = mir
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("i32 floating narrow result target TypeDesc");
    // The surface declaration owns an f64 call result today.  Rebind the
    // canonical result slot to i32 so the checker-owned f64 -> i32 receipt is
    // exercised by reference, bytecode, and native consumers alike.
    mir.replace_function_result_and_value_type_for_test_only(&owner, &result_id, i32_type);
    let result_conversion = crate::core::mir::MirFfiAbiConversion {
        from: MirAbiClass::Float { bits: 64 },
        to: MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
    };
    let mut receipts = mir.ffi_calls().clone();
    receipts
        .get_mut(&instruction_id)
        .expect("floating narrow result conversion receipt")
        .result_conversion = Some(result_conversion);
    mir.replace_ffi_calls_for_test_only(receipts);
    let receipt = mir
        .ffi_calls()
        .get(&instruction_id)
        .expect("updated floating narrow result conversion receipt");
    assert_eq!(receipt.result_conversion, Some(result_conversion));
    assert_eq!(
        mir.functions()[&owner].values[&result_id].ty,
        mir.type_catalog()
            .iter()
            .find_map(|(id, descriptor)| {
                (descriptor.abi
                    == MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    })
                .then(|| id.clone())
            })
            .expect("i32 floating narrow result slot TypeDesc")
    );

    struct FloatNarrowResultOracle;
    impl MirReferenceFfiResolver for FloatNarrowResultOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_result_f64" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            if arguments != [MirRuntimeValue::Int(7)] {
                return Err(format!(
                    "unexpected floating narrow result arguments {arguments:?}"
                ));
            }
            if receipt.result_conversion
                != Some(crate::core::mir::MirFfiAbiConversion {
                    from: MirAbiClass::Float { bits: 64 },
                    to: MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    },
                })
            {
                return Err("floating narrow result conversion receipt mismatch".into());
            }
            Ok(MirRuntimeValue::FloatBits(42.75_f64.to_bits()))
        }
    }

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&FloatNarrowResultOracle)
        .execute(&owner, &[])
        .expect("reference floating narrow result conversion FFI execution");
    assert_eq!(reference, MirRuntimeValue::Int(42));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-result-float-narrow".into())
        .expect("verify floating narrow result conversion MIR");
    assert!(verification.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Proven | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free floating narrow result bytecode");
    assert!(bytecode.ast.is_none());
    let bytecode_value = BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode floating narrow result conversion FFI execution");
    assert_eq!(bytecode_value, Value::Int(42));

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_float_narrow");
    generator
        .compile_mir_native(&mir)
        .expect("native floating narrow result conversion FFI lowering");
    generator
        .module
        .verify()
        .expect("valid floating narrow result conversion LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(RESULT_CONVERSION_F64_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native floating narrow result conversion FFI execution");
    assert_eq!(native.exit_code, Some(42));
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_mixed_argument_and_result_conversions_match_three_consumers() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MIXED_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(MIXED_F64_SOURCE)
        .tokenize()
        .expect("lex mixed argument/result conversion FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse mixed argument/result conversion FFI fixture");
    let checked = crate::core::check_program(&file)
        .expect("check mixed argument/result conversion FFI fixture");
    let mut mir = MirProgram::from_checked_program(&checked)
        .expect("materialize mixed argument/result conversion FFI MIR");
    let owner = crate::core::NodeId("function:main".into());
    let (instruction_id, argument_id, result_id) = mir
        .ffi_calls()
        .iter()
        .next()
        .map(|(instruction, receipt)| {
            (
                instruction.clone(),
                receipt
                    .arguments
                    .first()
                    .cloned()
                    .expect("mixed conversion argument value"),
                receipt
                    .result
                    .clone()
                    .expect("mixed conversion result value"),
            )
        })
        .expect("mixed argument/result conversion receipt");
    let i32_type = mir
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("i32 mixed conversion result target TypeDesc");
    // The argument conversion is checker-generated from the surface call
    // (`i32` actual to `f64` declaration).  Rebind only the result slot to
    // exercise that production argument receipt together with i64 -> i32
    // result narrowing in one canonical call boundary.
    mir.replace_function_result_and_value_type_for_test_only(&owner, &result_id, i32_type);
    let result_conversion = crate::core::mir::MirFfiAbiConversion {
        from: MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
        to: MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
    };
    let mut receipts = mir.ffi_calls().clone();
    let receipt = receipts
        .get_mut(&instruction_id)
        .expect("mixed conversion receipt");
    assert_eq!(receipt.parameter_conversions.len(), 1);
    assert_eq!(
        receipt.parameter_conversions[0],
        crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            to: MirAbiClass::Float { bits: 64 },
        }
    );
    receipt.result_conversion = Some(result_conversion);
    mir.replace_ffi_calls_for_test_only(receipts);

    struct MixedConversionOracle;
    impl MirReferenceFfiResolver for MixedConversionOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_expect_f64" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            if arguments != [MirRuntimeValue::FloatBits(7.0_f64.to_bits())] {
                return Err(format!(
                    "unexpected mixed conversion arguments {arguments:?}"
                ));
            }
            if receipt.parameter_conversions
                != vec![crate::core::mir::MirFfiAbiConversion {
                    from: MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    },
                    to: MirAbiClass::Float { bits: 64 },
                }]
            {
                return Err("mixed conversion parameter receipt mismatch".into());
            }
            if receipt.result_conversion
                != Some(crate::core::mir::MirFfiAbiConversion {
                    from: MirAbiClass::Integer {
                        bits: 64,
                        signed: true,
                    },
                    to: MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    },
                })
            {
                return Err("mixed conversion result receipt mismatch".into());
            }
            Ok(MirRuntimeValue::Int(42))
        }
    }

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&MixedConversionOracle)
        .execute(&owner, &[])
        .expect("reference mixed argument/result conversion FFI execution");
    assert_eq!(reference, MirRuntimeValue::Int(42));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-mixed-conversion".into())
        .expect("verify mixed argument/result conversion MIR");
    assert!(verification.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Proven | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free mixed conversion bytecode");
    assert!(bytecode.ast.is_none());
    let bytecode_value = BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode mixed argument/result conversion FFI execution");
    assert_eq!(bytecode_value, Value::Int(42));

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_mixed_conversion");
    generator
        .compile_mir_native(&mir)
        .expect("native mixed argument/result conversion FFI lowering");
    generator
        .module
        .verify()
        .expect("valid mixed argument/result conversion LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(MIXED_F64_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native mixed argument/result conversion FFI execution");
    assert_eq!(native.exit_code, Some(42));
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
    assert_eq!(
        mir.functions()[&owner].values[&argument_id].ty,
        mir.type_catalog()
            .iter()
            .find_map(|(id, descriptor)| {
                (descriptor.abi
                    == MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    })
                .then(|| id.clone())
            })
            .expect("mixed conversion argument TypeDesc")
    );
}

#[test]
fn scalar_ffi_float_narrow_result_range_preserves_prefix_effect() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, F64_I32_RANGE_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(F64_I32_RANGE_SOURCE)
        .tokenize()
        .expect("lex floating narrow result range fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse floating narrow result range fixture");
    let checked = crate::core::check_program(&file).expect("check floating narrow result range");
    let mut mir = MirProgram::from_checked_program(&checked)
        .expect("materialize floating narrow result range MIR");
    assert_eq!(mir.ffi_calls().len(), 2);
    let owner = crate::core::NodeId("function:main".into());
    let mut call_values = mir
        .ffi_calls()
        .iter()
        .map(|(instruction, receipt)| {
            (
                instruction.clone(),
                receipt.symbol.clone(),
                receipt
                    .result
                    .clone()
                    .expect("floating narrow result range value"),
            )
        })
        .collect::<Vec<_>>();
    call_values.sort_by(|left, right| left.1.cmp(&right.1));
    let (edge_instruction, _, edge_result) = call_values
        .iter()
        .find(|(_, symbol, _)| symbol == "mir_ffi_f64_i32_edge")
        .cloned()
        .expect("floating narrow result edge call");
    let (oob_instruction, _, oob_result) = call_values
        .iter()
        .find(|(_, symbol, _)| symbol == "mir_ffi_f64_i32_oob")
        .cloned()
        .expect("floating narrow result out-of-range call");
    let i32_type = mir
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("i32 floating narrow result range target TypeDesc");
    mir.replace_function_result_and_value_type_for_test_only(
        &owner,
        &edge_result,
        i32_type.clone(),
    );
    mir.replace_function_result_and_value_type_for_test_only(&owner, &oob_result, i32_type);
    let result_conversion = crate::core::mir::MirFfiAbiConversion {
        from: MirAbiClass::Float { bits: 64 },
        to: MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
    };
    let mut receipts = mir.ffi_calls().clone();
    receipts
        .get_mut(&edge_instruction)
        .expect("floating narrow edge receipt")
        .result_conversion = Some(result_conversion);
    receipts
        .get_mut(&oob_instruction)
        .expect("floating narrow out-of-range receipt")
        .result_conversion = Some(result_conversion);
    mir.replace_ffi_calls_for_test_only(receipts);

    struct FloatNarrowRangeOracle(Cell<i64>);
    impl MirReferenceFfiResolver for FloatNarrowRangeOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            let expected = match receipt.symbol.as_str() {
                "mir_ffi_f64_i32_edge" => {
                    if arguments != [MirRuntimeValue::Int(1)] {
                        return Err(format!("unexpected edge arguments {arguments:?}"));
                    }
                    2_147_483_647.75_f64
                }
                "mir_ffi_f64_i32_oob" => {
                    if arguments != [MirRuntimeValue::Int(2)] {
                        return Err(format!("unexpected out-of-range arguments {arguments:?}"));
                    }
                    2_147_483_648.0_f64
                }
                symbol => return Err(format!("unexpected floating narrow symbol {symbol}")),
            };
            self.0.set(self.0.get() + 1);
            if receipt.result_conversion
                != Some(crate::core::mir::MirFfiAbiConversion {
                    from: MirAbiClass::Float { bits: 64 },
                    to: MirAbiClass::Integer {
                        bits: 32,
                        signed: true,
                    },
                })
            {
                return Err("floating narrow result range receipt mismatch".into());
            }
            Ok(MirRuntimeValue::FloatBits(expected.to_bits()))
        }
    }

    let oracle = FloatNarrowRangeOracle(Cell::new(0));
    let reference_error = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&owner, &[])
        .expect_err("reference must reject the second floating narrow result");
    assert!(reference_error
        .to_string()
        .contains("outside target integer range"));
    assert_eq!(oracle.0.get(), 2);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-float-narrow-range".into())
        .expect("verify floating narrow result range MIR");
    assert!(verification.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Proven | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free floating narrow range bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    let bytecode_error = vm
        .run_value()
        .expect_err("bytecode must reject the second floating narrow result");
    assert!(bytecode_error
        .to_string()
        .contains("outside target integer range"));
    assert_eq!(vm.stdout(), "0\n");

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_f64_i32_range");
    generator
        .compile_mir_native(&mir)
        .expect("native floating narrow result range lowering");
    generator
        .module
        .verify()
        .expect("valid floating narrow result range LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(F64_I32_RANGE_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native floating narrow result range execution");
    assert_ne!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "0\n");
    assert!(
        native.stderr.contains("E0802")
            && native
                .stderr
                .contains("FFI integer result conversion out of range"),
        "{}",
        native.stderr
    );
}

#[test]
fn scalar_ffi_multi_call_requires_failure_preserves_prefix_side_effects() {
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MULTI_CALL_REQUIRES_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(MULTI_CALL_REQUIRES_SOURCE)
        .tokenize()
        .expect("lex multi-call requires fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse multi-call requires fixture");
    let checked = crate::core::check_program(&file).expect("check multi-call requires fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize multi-call requires MIR");
    assert_eq!(
        mir.ffi_calls().len(),
        2,
        "fixture must retain both call receipts"
    );
    let receipt_ids = mir.ffi_calls().keys().cloned().collect::<Vec<_>>();
    assert_ne!(receipt_ids[0], receipt_ids[1]);

    struct RequiresSequenceOracle(Cell<i64>);
    impl MirReferenceFfiResolver for RequiresSequenceOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_requires_sequence" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("requires sequence arguments {arguments:?}"));
            };
            let count = self.0.get() + 1;
            self.0.set(count);
            Ok(MirRuntimeValue::Int(count * 100 + value))
        }
    }

    let oracle = RequiresSequenceOracle(Cell::new(0));
    let reference_error = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must stop at the disproven second requires");
    assert!(
        reference_error.to_string().contains("requires")
            || reference_error.to_string().contains("precondition"),
        "{reference_error}"
    );
    assert_eq!(
        oracle.0.get(),
        1,
        "reference must invoke only the first call"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-prefix-failure".into())
        .expect("verify multi-call requires MIR");
    assert!(
        verification
            .iter()
            .any(|result| result.status == crate::verifier::VerifStatus::Disproven),
        "verifier must retain the disproven second requires: {verification:?}"
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free multi-call requires bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    let bytecode_error = vm
        .run_value()
        .expect_err("bytecode must stop before invoking the second call");
    assert!(
        bytecode_error.to_string().contains("requires")
            || bytecode_error.to_string().contains("precondition"),
        "{bytecode_error}"
    );
    assert_eq!(vm.stdout(), "107\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_prefix");
    generator
        .compile_mir_native(&mir)
        .expect("native multi-call requires lowering");
    generator
        .module
        .verify()
        .expect("valid multi-call requires LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(MULTI_CALL_REQUIRES_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native multi-call requires execution");
    assert_ne!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "107\n");
    assert!(
        native.stderr.contains("E0808") && native.stderr.contains("FFI precondition failed"),
        "{}",
        native.stderr
    );
}

#[test]
fn scalar_ffi_missing_symbol_is_rejected_at_each_host_boundary() {
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MISSING_SYMBOL_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(MISSING_SYMBOL_SOURCE)
        .tokenize()
        .expect("lex missing-symbol FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse missing-symbol FFI fixture");
    let checked = crate::core::check_program(&file).expect("check missing-symbol FFI fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize missing-symbol FFI MIR");
    assert_eq!(mir.ffi_calls().len(), 1);
    assert!(mir
        .ffi_calls()
        .values()
        .any(|receipt| receipt.symbol == "mir_ffi_absent_symbol"));

    let reference_error = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference execution must require an explicit host binding");
    assert!(reference_error
        .to_string()
        .contains("no reference FFI host binding"));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-missing-symbol".into())
        .expect("verify missing-symbol FFI MIR");
    assert!(verification.iter().all(|result| matches!(
        result.status,
        crate::verifier::VerifStatus::Verified | crate::verifier::VerifStatus::NoObligations
    )));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free missing-symbol FFI bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    let bytecode_error = vm
        .run_value()
        .expect_err("bytecode must reject an absent symbol after loading the fixture library");
    assert!(bytecode_error
        .to_string()
        .contains("failed to find canonical MIR FFI symbol"));
    assert_eq!(vm.stdout(), "");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_missing");
    generator
        .compile_mir_native(&mir)
        .expect("native missing-symbol FFI lowering");
    generator
        .module
        .verify()
        .expect("valid missing-symbol LLVM module before host link");
    let config = super::E2EConfig {
        extra_c_src: Some(MISSING_SYMBOL_C_SOURCE.into()),
        ..Default::default()
    };
    let native_error = super::link_and_observe_module(&generator, &config, counter)
        .expect_err("native link must reject the absent C symbol");
    assert!(native_error.contains("linker failed"), "{native_error}");
}

#[test]
fn scalar_ffi_runtime_rebinds_same_symbol_by_library_path() {
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_library = first.dir.join("ffi.so");
    let second_library = second.dir.join("ffi.so");
    let previous = std::env::var_os("MIMI_FFI_LIB");
    let i64_abi = crate::core::mir::types::MirAbiClass::Integer {
        bits: 64,
        signed: true,
    };
    let descriptor = CanonicalFfiDescriptor {
        caller: "function:main".into(),
        instruction: "ffi-test-call".into(),
        callee: "extern:C:test/function:mir_ffi_rebindable:0000000000000000".into(),
        symbol: "mir_ffi_rebindable".into(),
        abi: "C".into(),
        arguments: vec![CanonicalFfiScalarType::I64],
        parameter_conversions: vec![crate::core::mir::MirFfiAbiConversion {
            from: i64_abi,
            to: i64_abi,
        }],
        result: CanonicalFfiScalarType::I64,
        result_conversion: Some(crate::core::mir::MirFfiAbiConversion {
            from: i64_abi,
            to: i64_abi,
        }),
        argument_ids: vec![crate::core::mir::MirValueId::new("ffi-test-arg").unwrap()],
        requires: None,
        result_id: Some(crate::core::mir::MirValueId::new("ffi-test-result").unwrap()),
        ensures: None,
    };

    std::env::set_var("MIMI_FFI_LIB", &first_library);
    let mut runtime = crate::interp::bytecode::mir_ffi::CanonicalMirFfiRuntime::new();
    assert_eq!(
        runtime
            .call(&descriptor, &[Value::Int(1)])
            .expect("first library binding"),
        Value::Int(12)
    );
    assert_eq!(runtime.loaded_library_count_for_test(), 1);

    std::env::set_var("MIMI_FFI_LIB", &second_library);
    assert_eq!(
        runtime
            .call(&descriptor, &[Value::Int(1)])
            .expect("second library binding"),
        Value::Int(23)
    );
    assert_eq!(runtime.loaded_library_count_for_test(), 2);

    std::env::set_var("MIMI_FFI_LIB", &first_library);
    assert_eq!(
        runtime
            .call(&descriptor, &[Value::Int(1)])
            .expect("cached first library binding"),
        Value::Int(12)
    );
    assert_eq!(runtime.loaded_library_count_for_test(), 2);

    match previous {
        Some(value) => std::env::set_var("MIMI_FFI_LIB", value),
        None => std::env::remove_var("MIMI_FFI_LIB"),
    }
}

#[test]
fn scalar_ffi_reference_applies_integer_to_float_argument_conversion() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MIXED_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(MIXED_F64_SOURCE)
        .tokenize()
        .expect("lex integer-to-float FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse integer-to-float FFI fixture");
    let checked = crate::core::check_program(&file).expect("check integer-to-float FFI fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize integer-to-float FFI MIR");
    let receipt = mir
        .ffi_calls()
        .values()
        .next()
        .expect("integer-to-float FFI receipt");
    assert_eq!(
        receipt.parameter_conversions,
        vec![crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            to: MirAbiClass::Float { bits: 64 },
        }]
    );

    struct FloatOracle;
    impl MirReferenceFfiResolver for FloatOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), arguments) {
                ("mir_ffi_expect_f64", [MirRuntimeValue::FloatBits(bits)])
                    if f64::from_bits(*bits) == 7.0 =>
                {
                    Ok(MirRuntimeValue::Int(42))
                }
                _ => Err("reference host binding did not receive f64 ABI argument".into()),
            }
        }
    }

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&FloatOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference integer-to-float FFI execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(42));
    assert_eq!(reference.output, "");

    let bytecode = compile_mir_program(&mir).expect("AST-free integer-to-float FFI bytecode");
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value()
            .expect("bytecode integer-to-float FFI execution"),
        Value::Int(42)
    ));
    assert_eq!(vm.stdout(), "");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_f64");
    generator
        .compile_mir_native(&mir)
        .expect("native integer-to-float FFI lowering");
    generator
        .module
        .verify()
        .expect("valid integer-to-float LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(MIXED_F64_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native integer-to-float FFI execution");
    assert_eq!(native.exit_code, Some(42));
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_transparent_alias_chain_preserves_float_argument_abi_across_consumers() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, ALIASED_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let tokens = crate::lexer::Lexer::new(ALIASED_F64_SOURCE)
        .tokenize()
        .expect("lex transparent alias FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse transparent alias FFI fixture");
    let checked = crate::core::check_program(&file).expect("check transparent alias FFI fixture");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
        "transparent primitive aliases must enter the same default scalar FFI route"
    );
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("materialize transparent alias default route");
    assert!(
        crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route),
        "transparent primitive aliases must materialize a scalar FFI receipt"
    );
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize transparent alias FFI fixture");
    let direct_tokens = crate::lexer::Lexer::new(
        r#"
extern "C" { func mir_ffi_expect_alias_f64(value: f64) -> i64; }
func main() -> i64 {
    println(mir_ffi_expect_alias_f64(7 as i64))
    0
}
"#,
    )
    .tokenize()
    .expect("lex direct f64 FFI fixture");
    let direct_file = crate::parser::Parser::new(direct_tokens)
        .parse_file()
        .expect("parse direct f64 FFI fixture");
    let direct_checked =
        crate::core::check_program(&direct_file).expect("check direct f64 FFI fixture");
    let direct_mir = MirProgram::from_checked_program(&direct_checked)
        .expect("materialize direct f64 FFI fixture");
    assert_eq!(
        mir.type_catalog().canonical_text(),
        direct_mir.type_catalog().canonical_text(),
        "transparent aliases must not add opaque nominal TypeDesc entries"
    );
    let alias_route_receipt = mir.route_receipt("scalar-ffi-v1");
    let repeated_mir = MirProgram::from_checked_program(&checked)
        .expect("repeat materialize transparent alias FFI fixture");
    assert_eq!(
        alias_route_receipt.ffi_digest,
        repeated_mir.route_receipt("scalar-ffi-v1").ffi_digest,
        "transparent alias FFI receipt identity must be deterministic"
    );
    let receipt = mir
        .ffi_calls()
        .values()
        .next()
        .expect("transparent alias FFI receipt");
    assert_eq!(receipt.symbol, "mir_ffi_expect_alias_f64");
    assert_eq!(receipt.parameter_conversions.len(), 1);
    assert_eq!(
        receipt.parameter_conversions[0],
        crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            to: MirAbiClass::Float { bits: 64 },
        }
    );
    assert_eq!(
        receipt.result_conversion,
        Some(crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            to: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
        })
    );
    let declared_float = receipt.parameter_types[0].clone();
    assert!(matches!(
        mir.type_catalog()
            .get(&declared_float)
            .expect("alias target TypeDesc")
            .abi,
        MirAbiClass::Float { bits: 64 }
    ));

    struct AliasFloatOracle;
    impl MirReferenceFfiResolver for AliasFloatOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), arguments) {
                ("mir_ffi_expect_alias_f64", [MirRuntimeValue::FloatBits(bits)])
                    if f64::from_bits(*bits) == 7.0 =>
                {
                    Ok(MirRuntimeValue::Int(42))
                }
                _ => Err("transparent alias FFI binding did not receive f64 ABI argument".into()),
            }
        }
    }

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&AliasFloatOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference transparent alias FFI execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "42\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free transparent alias FFI bytecode");
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value()
            .expect("bytecode transparent alias FFI execution"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "42\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_alias_f64");
    generator
        .compile_mir_native(&mir)
        .expect("native transparent alias FFI lowering");
    generator
        .module
        .verify()
        .expect("valid transparent alias FFI LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(ALIASED_F64_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native transparent alias FFI execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "42\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_transparent_aliases_cover_every_scalar_endpoint_across_consumers() {
    use crate::core::mir::types::MirAbiClass;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, ALIASED_SCALAR_MATRIX_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(ALIASED_SCALAR_MATRIX_SOURCE)
            .tokenize()
            .expect("lex scalar alias matrix"),
    )
    .parse_file()
    .expect("parse scalar alias matrix");
    let checked = crate::core::check_program(&file).expect("check scalar alias matrix");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
        "every transparent primitive alias must share scalar FFI admission"
    );
    let mir = MirProgram::from_checked_program(&checked).expect("materialize scalar alias matrix");
    assert_eq!(mir.ffi_calls().len(), 5);
    let receipts = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .map(|(_, receipt)| receipt)
        .collect::<Vec<_>>();
    assert!(receipts.iter().all(|receipt| receipt.abi == "C"));
    let i32_receipt = receipts
        .iter()
        .find(|receipt| receipt.symbol == "mir_ffi_alias_i32")
        .expect("i32 alias receipt");
    assert_eq!(
        i32_receipt.parameter_conversions,
        vec![crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            to: MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
        }]
    );
    let bool_receipt = receipts
        .iter()
        .find(|receipt| receipt.symbol == "mir_ffi_alias_bool")
        .expect("bool alias receipt");
    assert_eq!(
        bool_receipt.result_conversion,
        Some(crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Bool,
            to: MirAbiClass::Bool,
        })
    );
    let unit_receipt = receipts
        .iter()
        .find(|receipt| receipt.symbol == "mir_ffi_alias_unit")
        .expect("unit alias receipt");
    assert!(unit_receipt.result.is_some());
    assert_eq!(
        unit_receipt.result_conversion,
        Some(crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Unit,
            to: MirAbiClass::Unit,
        })
    );

    struct ScalarAliasOracle;
    impl MirReferenceFfiResolver for ScalarAliasOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), arguments) {
                ("mir_ffi_alias_i32" | "mir_ffi_alias_i64", [MirRuntimeValue::Int(value)]) => {
                    Ok(MirRuntimeValue::Int(value + 1))
                }
                ("mir_ffi_alias_bool", [MirRuntimeValue::Bool(value)]) => {
                    Ok(MirRuntimeValue::Bool(!value))
                }
                ("mir_ffi_alias_f64", [MirRuntimeValue::FloatBits(bits)]) => {
                    Ok(MirRuntimeValue::FloatBits(*bits))
                }
                ("mir_ffi_alias_unit", []) => Ok(MirRuntimeValue::Unit),
                _ => Err("unexpected transparent alias scalar FFI call".into()),
            }
        }
    }

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&ScalarAliasOracle)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference scalar alias matrix");
    assert_eq!(reference, MirRuntimeValue::Int(84));

    let bytecode = compile_mir_program(&mir).expect("bytecode scalar alias matrix");
    assert!(bytecode.ast.is_none());
    assert!(matches!(
        BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode scalar alias matrix"),
        Value::Int(84)
    ));

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_alias_matrix");
    generator
        .compile_mir_native(&mir)
        .expect("native scalar alias matrix");
    generator
        .module
        .verify()
        .expect("valid scalar alias matrix LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(ALIASED_SCALAR_MATRIX_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native scalar alias matrix");
    assert_eq!(native.exit_code, Some(84));
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_generic_aliases_remain_fail_closed_at_the_c_abi_boundary() {
    for (alias, parameter, expected) in [
        (
            "type Scalar<T> = f64",
            "Scalar",
            "type 'Scalar' is not allowed across the C ABI boundary",
        ),
        (
            "type Box<T> = List<T>",
            "Box<i64>",
            "type 'List<i64>' is a Mimi list/array and cannot cross the C ABI boundary directly",
        ),
    ] {
        let source = format!(
            r#"
{alias}
extern "C" {{ func foreign(value: {parameter}) -> i64; }}
func main() -> i64 {{ 0 }}
"#
        );
        let tokens = crate::lexer::Lexer::new(&source)
            .tokenize()
            .expect("lex generic alias FFI boundary fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse generic alias FFI boundary fixture");
        let errors = crate::core::check_program(&file)
            .expect_err("generic aliases must not enter scalar FFI admission");
        let text = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("E0231"), "{alias}: {text}");
        assert!(text.contains(expected), "{alias}: {text}");
    }
}

#[test]
fn scalar_ffi_imported_alias_keeps_checker_identity_after_file_merge() {
    use crate::core::mir::types::MirAbiClass;
    use std::fs;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, IMPORTED_ALIAS_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    std::env::set_var("MIMI_FFI_LIB", &library);

    let project = std::env::temp_dir().join(format!(
        "mimi-canonical-ffi-import-alias-{}-{counter}",
        std::process::id()
    ));
    fs::create_dir_all(&project).expect("create imported alias project");
    let main_path = project.join("main.mimi");
    fs::write(
        &main_path,
        r#"
use ffi_types
func main() -> i64 {
    println(call_imported_alias(7 as i64))
    0
}
"#,
    )
    .expect("write imported alias main");
    fs::write(
        project.join("ffi_types.mimi"),
        r#"
pub type Real = f64
pub type ScalarInt = i64
pub type ResultId = ScalarInt
extern "C" { func mir_ffi_import_alias_f64(value: Real) -> ResultId; }
pub func call_imported_alias(value: i64) -> i64 {
    mir_ffi_import_alias_f64(value)
}
"#,
    )
    .expect("write imported alias module");

    let source = fs::read_to_string(&main_path).expect("read imported alias main");
    let tokens = crate::lexer::Lexer::new(&source)
        .tokenize()
        .expect("lex imported alias main");
    let file = crate::loader::parser_for_path(tokens, &main_path)
        .expect("select imported alias parser")
        .parse_file()
        .expect("parse imported alias main");
    let mut loader = crate::loader::ModuleLoader::new(project.clone());
    loader
        .load_main_with_file(&main_path, file)
        .expect("load imported alias graph");
    let mut merged = loader.merge_all().expect("merge imported alias graph");
    crate::loader::merge_prelude_into(&mut merged);
    let checked = crate::core::check_program(&merged).expect("check imported alias graph");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
        "imported transparent primitive aliases must enter scalar FFI admission"
    );
    let excluded_sources = merged
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<std::collections::HashSet<_>>();
    let route =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .expect("materialize imported alias default route");
    assert!(
        crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route),
        "imported transparent primitive aliases must materialize scalar FFI"
    );
    let mir = MirProgram::from_checked_program_excluding_sources(&checked, &excluded_sources)
        .expect("materialize imported alias canonical MIR");
    let repeated_mir =
        MirProgram::from_checked_program_excluding_sources(&checked, &excluded_sources)
            .expect("repeat materialize imported alias canonical MIR");
    assert_eq!(
        mir.route_receipt("scalar-ffi-v1").ffi_digest,
        repeated_mir.route_receipt("scalar-ffi-v1").ffi_digest,
        "imported alias FFI receipt identity must be deterministic after file merge"
    );
    let receipt = mir
        .ffi_calls()
        .values()
        .find(|receipt| receipt.symbol == "mir_ffi_import_alias_f64")
        .expect("imported alias FFI receipt");
    assert_eq!(receipt.parameter_conversions.len(), 1);
    assert_eq!(
        receipt.parameter_conversions[0],
        crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            to: MirAbiClass::Float { bits: 64 },
        }
    );
    let declared = mir
        .type_catalog()
        .get(&receipt.parameter_types[0])
        .expect("imported alias declaration TypeDesc");
    assert!(matches!(declared.abi, MirAbiClass::Float { bits: 64 }));
    let result_value = receipt
        .result
        .as_ref()
        .expect("imported alias result value");
    let actual_result = mir
        .functions()
        .get(&receipt.caller)
        .and_then(|function| function.values.get(result_value))
        .map(|value| value.ty.clone())
        .expect("imported alias result TypeDesc");
    assert_eq!(actual_result, receipt.result_type);
    assert_eq!(
        crate::core::mir::MirFfiAbiConversion::for_result(
            mir.type_catalog(),
            &actual_result,
            &receipt.result_type,
        ),
        receipt.result_conversion,
        "imported transparent result aliases must use the checker-owned result conversion factory"
    );
    assert_eq!(
        receipt.result_conversion,
        Some(crate::core::mir::MirFfiAbiConversion {
            from: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
            to: MirAbiClass::Integer {
                bits: 64,
                signed: true,
            },
        })
    );

    struct ImportedAliasOracle;
    impl MirReferenceFfiResolver for ImportedAliasOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), arguments) {
                ("mir_ffi_import_alias_f64", [MirRuntimeValue::FloatBits(bits)])
                    if f64::from_bits(*bits) == 7.0 =>
                {
                    Ok(MirRuntimeValue::Int(42))
                }
                _ => Err("imported alias FFI binding did not receive f64 ABI argument".into()),
            }
        }
    }

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&ImportedAliasOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference imported alias FFI execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "42\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free imported alias FFI bytecode");
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value()
            .expect("bytecode imported alias FFI execution"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "42\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_imported_alias_f64");
    generator
        .compile_mir_native(&mir)
        .expect("native imported alias FFI lowering");
    generator
        .module
        .verify()
        .expect("valid imported alias FFI LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(IMPORTED_ALIAS_F64_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native imported alias FFI execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "42\n");
    assert_eq!(native.stderr, "");
    fs::remove_dir_all(project).expect("remove imported alias project");
}

#[test]
fn scalar_ffi_imported_alias_multiple_calls_preserve_receipt_order_and_side_effects() {
    use std::fs;

    const C_SOURCE: &str = r#"
#include <stdint.h>
static int64_t call_count;
int64_t mir_ffi_import_alias_sequence(double value) {
    ++call_count;
    return call_count * 100 + (int64_t)value;
}
int64_t mir_ffi_import_alias_sequence_extra(double value) {
    ++call_count;
    return call_count * 100 + (int64_t)value;
}
"#;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));

    let project = std::env::temp_dir().join(format!(
        "mimi-canonical-ffi-import-alias-sequence-{}-{counter}",
        std::process::id()
    ));
    fs::create_dir_all(&project).expect("create imported alias sequence project");
    let main_path = project.join("main.mimi");
    fs::write(
        &main_path,
        r#"
use ffi_types
use ffi_extra
func main() -> i64 {
    println(call_imported_alias(7 as i64, 8 as i64))
    println(call_imported_alias_extra(9 as i64))
    0
}
"#,
    )
    .expect("write imported alias sequence main");
    fs::write(
        project.join("ffi_types.mimi"),
        r#"
pub type Scalar = f64
pub type Real = Scalar
pub type ScalarInt = i64
pub type ResultId = ScalarInt
extern "C" { func mir_ffi_import_alias_sequence(value: Real) -> ResultId requires: value >= 0; }
pub func call_imported_alias(first: i64, second: i64) -> ResultId {
    requires: first >= 0 and second >= 0
    let first_result = mir_ffi_import_alias_sequence(first)
    let second_result = mir_ffi_import_alias_sequence(second)
    second_result
}
"#,
    )
    .expect("write imported alias sequence module");
    fs::write(
        project.join("ffi_extra.mimi"),
        r#"
pub type ExtraScalar = f64
pub type ExtraReal = ExtraScalar
pub type ExtraScalarInt = i64
pub type ExtraResultId = ExtraScalarInt
extern "C" { func mir_ffi_import_alias_sequence_extra(value: ExtraReal) -> ExtraResultId requires: value >= 0; }
pub func call_imported_alias_extra(value: i64) -> ExtraResultId {
    requires: value >= 0
    mir_ffi_import_alias_sequence_extra(value)
}
"#,
    )
    .expect("write imported alias extra module");

    let source = fs::read_to_string(&main_path).expect("read imported alias sequence main");
    let tokens = crate::lexer::Lexer::new(&source)
        .tokenize()
        .expect("lex imported alias sequence main");
    let file = crate::loader::parser_for_path(tokens, &main_path)
        .expect("select imported alias sequence parser")
        .parse_file()
        .expect("parse imported alias sequence main");
    let mut loader = crate::loader::ModuleLoader::new(project.clone());
    loader
        .load_main_with_file(&main_path, file)
        .expect("load imported alias sequence graph");
    let mut merged = loader
        .merge_all()
        .expect("merge imported alias sequence graph");
    crate::loader::merge_prelude_into(&mut merged);
    let checked = crate::core::check_program(&merged).expect("check imported alias sequence graph");
    let excluded_sources = merged
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<std::collections::HashSet<_>>();
    let route =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .expect("materialize imported alias sequence route");
    assert!(
        crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route),
        "imported transparent primitive aliases must retain scalar FFI route"
    );
    let mir = MirProgram::from_checked_program_excluding_sources(&checked, &excluded_sources)
        .expect("materialize imported alias sequence MIR");
    let repeated_mir =
        MirProgram::from_checked_program_excluding_sources(&checked, &excluded_sources)
            .expect("repeat materialize imported alias sequence MIR");
    let receipt = mir.route_receipt("scalar-ffi-v1");
    let repeated_receipt = repeated_mir.route_receipt("scalar-ffi-v1");
    assert_eq!(receipt.ffi_digest, repeated_receipt.ffi_digest);
    assert_eq!(receipt.mir_digest, repeated_receipt.mir_digest);
    let receipt_manifest = receipt
        .manifest_text()
        .expect("render imported alias route manifest");
    assert_eq!(
        crate::core::mir::CanonicalMirRouteReceipt::from_manifest(&receipt_manifest)
            .expect("round-trip imported alias route manifest"),
        receipt
    );
    assert_eq!(
        mir.ffi_calls().len(),
        3,
        "all imported callers must keep receipts"
    );
    let receipts = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .map(|(_, receipt)| receipt)
        .collect::<Vec<_>>();
    assert_eq!(receipts[0].symbol, "mir_ffi_import_alias_sequence");
    assert_eq!(receipts[1].symbol, "mir_ffi_import_alias_sequence");
    assert_eq!(receipts[2].symbol, "mir_ffi_import_alias_sequence_extra");
    assert_eq!(
        receipts[0].caller,
        crate::core::NodeId("function:call_imported_alias".into())
    );
    assert_eq!(
        receipts[1].caller,
        crate::core::NodeId("function:call_imported_alias".into())
    );
    assert_eq!(
        receipts[2].caller,
        crate::core::NodeId("function:call_imported_alias_extra".into())
    );
    assert_eq!(receipts[0].callee, receipts[1].callee);
    assert_ne!(receipts[1].callee, receipts[2].callee);
    assert_ne!(receipts[0].instruction, receipts[1].instruction);
    assert_ne!(receipts[1].instruction, receipts[2].instruction);

    let wrapper = mir
        .functions()
        .get(&crate::core::NodeId("function:call_imported_alias".into()))
        .expect("imported alias sequence wrapper MIR");
    let call_instruction_ids = wrapper
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ResolvedCallee::Extern(_),
                ..
            } => Some(instruction.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let wrapper_receipt_instruction_ids = receipts
        .iter()
        .filter(|call| call.caller.0 == "function:call_imported_alias")
        .map(|call| call.instruction.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        call_instruction_ids, wrapper_receipt_instruction_ids,
        "source-ordered receipt view must follow the wrapper's source call order"
    );
    let repeated_wrapper_instruction_ids = repeated_mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .filter(|(_, call)| call.caller.0 == "function:call_imported_alias")
        .map(|(_, call)| call.instruction.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        wrapper_receipt_instruction_ids,
        repeated_wrapper_instruction_ids
    );

    // The imported alias path must keep the receipt table itself authoritative:
    // moving a receipt under a forged key is rejected by every direct consumer.
    let baseline_route = mir.route_receipt("scalar-ffi-sequence-v1");
    let first_id = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .find(|(_, call)| call.caller.0 == "function:call_imported_alias")
        .map(|(instruction, _)| instruction.clone())
        .expect("first imported alias receipt key");
    let forged_map_key =
        crate::core::mir::MirInstructionId::new("inst:call:forged-imported-alias-map-key")
            .expect("forged imported alias receipt key");
    let mut key_forged_receipts = mir.ffi_calls().clone();
    let key_forged_receipt = key_forged_receipts
        .remove(&first_id)
        .expect("first imported alias receipt");
    key_forged_receipts.insert(forged_map_key, key_forged_receipt);
    let mut key_forged = mir.clone();
    key_forged.replace_ffi_calls_for_test_only(key_forged_receipts);
    let key_forged_route = key_forged.route_receipt("scalar-ffi-sequence-v1");
    assert_ne!(baseline_route.ffi_digest, key_forged_route.ffi_digest);
    assert_ne!(baseline_route.mir_digest, key_forged_route.mir_digest);
    let key_table_errors = crate::core::mir::validate_ffi_receipt_table(
        key_forged.functions(),
        key_forged.ffi_calls(),
    );
    assert!(key_table_errors
        .iter()
        .any(|error| error.contains("orphaned from a MIR extern call")));
    let key_reference_error = MirReferenceInterpreter::new(&key_forged)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged imported alias receipt key");
    assert!(key_reference_error.to_string().contains("receipt key"));
    let key_bytecode_error = compile_mir_program(&key_forged)
        .expect_err("bytecode must reject a forged imported alias receipt key");
    assert!(key_bytecode_error.iter().any(|error| {
        error.message.contains("identity/ABI validation")
            || error.message.contains("orphaned from a MIR extern call")
    }));
    let key_native_error = crate::codegen::mir::validate_mir_native(&key_forged)
        .expect_err("native validator must reject a forged imported alias receipt key");
    assert!(key_native_error.iter().any(|error| {
        error.message.contains("orphaned from a MIR extern call")
            || error.message.contains("receipt key")
    }));
    let key_capability_error = crate::verifier::validate_mir_capabilities(&key_forged)
        .expect_err("capability gate must reject a forged imported alias receipt key");
    assert!(key_capability_error.iter().any(|error| {
        error.contains("orphaned from a MIR extern call") || error.contains("receipt key")
    }));
    let key_verifier_error =
        crate::verifier::verify_mir(&key_forged, "forged-imported-alias-receipt-key".into())
            .expect_err("verifier must reject a forged imported alias receipt key");
    assert!(
        key_verifier_error.contains("orphaned from a MIR extern call")
            || key_verifier_error.contains("receipt key")
    );

    // Mutating one declaration descriptor must be rejected as a cross-call
    // declaration-shape mismatch, even though the symbol and call order stay intact.
    let first_argument = mir
        .ffi_calls()
        .get(&first_id)
        .and_then(|call| call.arguments.first())
        .expect("first imported alias argument value");
    let actual_first_type = wrapper
        .values
        .get(first_argument)
        .map(|value| value.ty.clone())
        .expect("first imported alias argument TypeDesc");
    let declared_first_type = mir
        .ffi_calls()
        .get(&first_id)
        .and_then(|call| call.parameter_types.first())
        .expect("first imported alias parameter TypeDesc");
    assert_ne!(actual_first_type, *declared_first_type);
    let mut descriptor_forged_receipts = mir.ffi_calls().clone();
    let descriptor_forged = descriptor_forged_receipts
        .get_mut(&first_id)
        .expect("first imported alias receipt for descriptor forgery");
    descriptor_forged.parameter_types[0] = actual_first_type.clone();
    descriptor_forged.parameter_conversions[0] =
        crate::core::mir::MirFfiAbiConversion::for_argument(
            mir.type_catalog(),
            &actual_first_type,
            &actual_first_type,
        )
        .expect("identity conversion for forged descriptor");
    let mut descriptor_forged_program = mir.clone();
    descriptor_forged_program.replace_ffi_calls_for_test_only(descriptor_forged_receipts);
    let descriptor_table_errors = crate::core::mir::validate_ffi_symbol_declaration_shapes(
        descriptor_forged_program.ffi_calls(),
    );
    assert!(descriptor_table_errors
        .iter()
        .any(|error| error.contains("incompatible declaration TypeDescs")));
    let descriptor_reference_error = MirReferenceInterpreter::new(&descriptor_forged_program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged imported alias descriptor");
    assert!(descriptor_reference_error
        .to_string()
        .contains("incompatible declaration TypeDescs"));
    let descriptor_bytecode_error = compile_mir_program(&descriptor_forged_program)
        .expect_err("bytecode must reject a forged imported alias descriptor");
    assert!(descriptor_bytecode_error
        .iter()
        .any(|error| error.message.contains("incompatible declaration TypeDescs")));
    let descriptor_native_error =
        crate::codegen::mir::validate_mir_native(&descriptor_forged_program)
            .expect_err("native validator must reject a forged imported alias descriptor");
    assert!(descriptor_native_error
        .iter()
        .any(|error| error.message.contains("incompatible declaration TypeDescs")));
    let descriptor_capability_error =
        crate::verifier::validate_mir_capabilities(&descriptor_forged_program)
            .expect_err("capability gate must reject a forged imported alias descriptor");
    assert!(descriptor_capability_error
        .iter()
        .any(|error| error.contains("incompatible declaration TypeDescs")));
    let descriptor_verifier_error = crate::verifier::verify_mir(
        &descriptor_forged_program,
        "forged-imported-alias-descriptor".into(),
    )
    .expect_err("verifier must reject a forged imported alias descriptor");
    assert!(descriptor_verifier_error.contains("incompatible declaration TypeDescs"));

    // Result identity and result ABI conversion are independent receipt
    // dimensions.  Each must be checked even when the declaration shape and
    // symbol remain valid.
    let result_type = mir
        .ffi_calls()
        .get(&first_id)
        .expect("first imported alias receipt")
        .result_type
        .clone();
    let result_value = mir
        .ffi_calls()
        .get(&first_id)
        .and_then(|call| call.result.as_ref())
        .expect("first imported alias result value");
    assert_eq!(
        wrapper
            .values
            .get(result_value)
            .map(|value| value.ty.clone())
            .expect("first imported alias result TypeDesc"),
        result_type
    );

    let mut result_identity_receipts = mir.ffi_calls().clone();
    result_identity_receipts
        .get_mut(&first_id)
        .expect("first imported alias receipt for result identity forgery")
        .result = Some(
        crate::core::mir::MirValueId::new("value:forged-imported-alias-result")
            .expect("forged imported alias result identity"),
    );
    let mut result_identity_forged = mir.clone();
    result_identity_forged.replace_ffi_calls_for_test_only(result_identity_receipts);
    assert_ne!(
        baseline_route.ffi_digest,
        result_identity_forged
            .route_receipt("scalar-ffi-sequence-v1")
            .ffi_digest
    );
    let result_identity_reference_error = MirReferenceInterpreter::new(&result_identity_forged)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged imported alias result identity");
    assert!(
        result_identity_reference_error
            .to_string()
            .contains("result")
            || result_identity_reference_error
                .to_string()
                .contains("identity")
    );
    let result_identity_bytecode_error = compile_mir_program(&result_identity_forged)
        .expect_err("bytecode must reject a forged imported alias result identity");
    assert!(result_identity_bytecode_error.iter().any(|error| {
        error.message.contains("result") || error.message.contains("identity/ABI validation")
    }));
    let result_identity_native_error =
        crate::codegen::mir::validate_mir_native(&result_identity_forged)
            .expect_err("native validator must reject a forged imported alias result identity");
    assert!(result_identity_native_error
        .iter()
        .any(|error| { error.message.contains("result") || error.message.contains("identity") }));
    let result_identity_capability_error =
        crate::verifier::validate_mir_capabilities(&result_identity_forged)
            .expect_err("capability gate must reject a forged imported alias result identity");
    assert!(result_identity_capability_error
        .iter()
        .any(|error| { error.contains("result") || error.contains("identity") }));
    let result_identity_verifier_error = crate::verifier::verify_mir(
        &result_identity_forged,
        "forged-imported-alias-result-identity".into(),
    )
    .expect_err("verifier must reject a forged imported alias result identity");
    assert!(
        result_identity_verifier_error.contains("result")
            || result_identity_verifier_error.contains("identity")
    );

    let mut result_conversion_receipts = mir.ffi_calls().clone();
    result_conversion_receipts
        .get_mut(&first_id)
        .expect("first imported alias receipt for result conversion forgery")
        .result_conversion = Some(crate::core::mir::MirFfiAbiConversion {
        from: crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
        to: crate::core::mir::types::MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
    });
    let mut result_conversion_forged = mir.clone();
    result_conversion_forged.replace_ffi_calls_for_test_only(result_conversion_receipts);
    let result_conversion_reference_error = MirReferenceInterpreter::new(&result_conversion_forged)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged imported alias result conversion");
    assert!(
        result_conversion_reference_error
            .to_string()
            .contains("conversion receipt")
            || result_conversion_reference_error
                .to_string()
                .contains("conversion from"),
        "{result_conversion_reference_error}"
    );
    let result_conversion_bytecode_error = compile_mir_program(&result_conversion_forged)
        .expect_err("bytecode must reject a forged imported alias result conversion");
    assert!(result_conversion_bytecode_error.iter().any(|error| {
        error.message.contains("conversion receipt")
            || error.message.contains("identity/ABI validation")
    }));
    let result_conversion_native_error =
        crate::codegen::mir::validate_mir_native(&result_conversion_forged)
            .expect_err("native validator must reject a forged imported alias result conversion");
    assert!(result_conversion_native_error
        .iter()
        .any(|error| error.message.contains("conversion receipt")));
    let result_conversion_capability_error =
        crate::verifier::validate_mir_capabilities(&result_conversion_forged)
            .expect_err("capability gate must reject a forged imported alias result conversion");
    assert!(result_conversion_capability_error
        .iter()
        .any(|error| error.contains("conversion receipt")));
    let result_conversion_verifier_error = crate::verifier::verify_mir(
        &result_conversion_forged,
        "forged-imported-alias-result-conversion".into(),
    )
    .expect_err("verifier must reject a forged imported alias result conversion");
    assert!(
        result_conversion_verifier_error.contains("conversion receipt")
            || result_conversion_verifier_error.contains("conversion from"),
        "{result_conversion_verifier_error}"
    );

    struct ImportedAliasSequenceOracle(Cell<i64>);
    impl MirReferenceFfiResolver for ImportedAliasSequenceOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_import_alias_sequence"
                && receipt.symbol != "mir_ffi_import_alias_sequence_extra"
            {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::FloatBits(bits)] = arguments else {
                return Err(format!("sequence oracle received {arguments:?}"));
            };
            let input = f64::from_bits(*bits);
            let expected_input = match self.0.get() {
                0 => 7.0,
                1 => 8.0,
                2 => 9.0,
                count => return Err(format!("unexpected sequence call count {count}")),
            };
            if input != expected_input {
                return Err(format!(
                    "sequence input order mismatch: got {input}, expected {expected_input}"
                ));
            }
            let next = self.0.get() + 1;
            self.0.set(next);
            Ok(MirRuntimeValue::Int(next * 100 + input as i64))
        }
    }

    let oracle = ImportedAliasSequenceOracle(Cell::new(0));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference imported alias sequence execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "208\n309\n");
    assert_eq!(
        oracle.0.get(),
        3,
        "reference must observe all imported callers in order"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verified = crate::verifier::verify_mir(&mir, "imported-alias-sequence".into())
        .expect("verify imported alias sequence MIR");
    assert!(verified.iter().all(|result| {
        matches!(
            result.status,
            crate::verifier::VerifStatus::Proven | crate::verifier::VerifStatus::NoObligations
        )
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("AST-free imported alias sequence bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_instruction_ids = bytecode
        .canonical_ffi
        .iter()
        .map(|descriptor| descriptor.instruction.clone())
        .collect::<Vec<_>>();
    let source_order_instruction_ids = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .map(|(_, call)| call.instruction.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        descriptor_instruction_ids, source_order_instruction_ids,
        "bytecode descriptor indices must follow canonical source-order receipts"
    );
    let mut descriptor_reuse = (*bytecode).clone();
    descriptor_reuse.canonical_ffi[2] = descriptor_reuse.canonical_ffi[0].clone();
    let descriptor_reuse_error = BytecodeVM::new(std::sync::Arc::new(descriptor_reuse))
        .run_value()
        .expect_err("bytecode must reject a reused imported alias descriptor");
    assert!(
        descriptor_reuse_error
            .to_string()
            .contains("differs from its compiler binding")
            || descriptor_reuse_error.to_string().contains("instruction")
    );
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value()
            .expect("bytecode imported alias sequence execution"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "208\n309\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_imported_alias_sequence");
    generator
        .compile_mir_native(&mir)
        .expect("native imported alias sequence lowering");
    generator
        .module
        .verify()
        .expect("valid imported alias sequence LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native imported alias sequence execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "208\n309\n");
    assert_eq!(native.stderr, "");

    let extra_id = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .find(|(_, call)| call.caller.0 == "function:call_imported_alias_extra")
        .map(|(instruction, _)| instruction.clone())
        .expect("extra imported alias receipt key");
    let mut late_failure_receipts = mir.ffi_calls().clone();
    late_failure_receipts
        .get_mut(&extra_id)
        .expect("extra imported alias receipt")
        .result_conversion = Some(crate::core::mir::MirFfiAbiConversion {
        from: crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
        to: crate::core::mir::types::MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
    });
    let mut late_failure = mir.clone();
    late_failure.replace_ffi_calls_for_test_only(late_failure_receipts);
    let late_oracle = ImportedAliasSequenceOracle(Cell::new(0));
    let late_reference_error = MirReferenceInterpreter::new(&late_failure)
        .with_ffi_resolver(&late_oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a late imported alias conversion forgery");
    assert!(
        late_reference_error
            .to_string()
            .contains("conversion receipt")
            || late_reference_error.to_string().contains("conversion from"),
        "{late_reference_error}"
    );
    assert_eq!(
        late_oracle.0.get(),
        2,
        "late receipt failure must preserve the two-call prefix"
    );
    let late_bytecode_error = compile_mir_program(&late_failure)
        .expect_err("bytecode must reject a late imported alias conversion forgery");
    assert!(late_bytecode_error
        .iter()
        .any(|error| error.message.contains("conversion receipt")));
    fs::remove_dir_all(project).expect("remove imported alias sequence project");
}

#[test]
fn scalar_ffi_imported_alias_verifier_artifact_matches_route_receipt() {
    use crate::verifier::{ProofArtifact, VerifStatus};
    use std::fs;

    let project = std::env::temp_dir().join(format!(
        "mimi-canonical-ffi-import-alias-proof-{}",
        std::process::id()
    ));
    fs::create_dir_all(&project).expect("create imported alias proof project");
    let main_path = project.join("main.mimi");
    fs::write(
        &main_path,
        "use ffi_types;\nfunc main() -> i64 { call_imported_alias(7 as i64) }\n",
    )
    .expect("write imported alias proof main");
    fs::write(
        project.join("ffi_types.mimi"),
        "pub type Scalar = f64\npub type Real = Scalar\npub type ScalarInt = i64\npub type ResultId = ScalarInt\nextern \"C\" { func mir_ffi_import_alias_proof(value: Real) -> ResultId requires: value >= 0; }\npub func call_imported_alias(value: i64) -> ResultId { requires: value > 0\n    mir_ffi_import_alias_proof(value)\n}\n",
    )
    .expect("write imported alias proof module");

    let source = fs::read_to_string(&main_path).expect("read imported alias proof main");
    let tokens = crate::lexer::Lexer::new(&source)
        .tokenize()
        .expect("lex imported alias proof main");
    let file = crate::loader::parser_for_path(tokens, &main_path)
        .expect("select imported alias proof parser")
        .parse_file()
        .expect("parse imported alias proof main");
    let mut loader = crate::loader::ModuleLoader::new(project.clone());
    loader
        .load_main_with_file(&main_path, file)
        .expect("load imported alias proof graph");
    let mut merged = loader
        .merge_all()
        .expect("merge imported alias proof graph");
    crate::loader::merge_prelude_into(&mut merged);
    let checked = crate::core::check_program(&merged).expect("check imported alias proof graph");
    let excluded_sources = merged
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<std::collections::HashSet<_>>();
    let route =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .expect("materialize imported alias proof route");
    let mir = MirProgram::from_checked_program_excluding_sources(&checked, &excluded_sources)
        .expect("materialize imported alias proof MIR");
    let receipt = mir.route_receipt("scalar-ffi-v1");
    assert_eq!(route.program.route_receipt("scalar-ffi-v1"), receipt);
    assert_eq!(receipt.ffi_digest.len(), 64);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_mir(&mir, "imported-alias-proof".into())
        .expect("verify imported alias proof MIR");
    assert_eq!(results.len(), 2, "wrapper and extern requires obligations");
    assert!(results
        .iter()
        .any(|result| result.status == VerifStatus::Proven));
    assert!(results
        .iter()
        .any(|result| result.status == VerifStatus::NoObligations));
    assert!(results.iter().all(|result| {
        result.artifact.as_ref().is_some_and(|artifact| {
            artifact.engine == ProofArtifact::ENGINE_MIR && artifact.mir_hash == receipt.mir_digest
        })
    }));
    for (label, public_results) in [
        (
            "verify_checked",
            crate::verifier::verify_checked(&checked, "imported-alias-proof-public".into()),
        ),
        (
            "verify_checked_dual",
            crate::verifier::verify_checked_dual(&checked, "imported-alias-proof-dual".into()),
        ),
    ] {
        let public_results = public_results.expect("public imported alias proof verification");
        assert_eq!(public_results.len(), results.len(), "{label}");
        assert!(
            public_results.iter().all(|result| {
                result.artifact.as_ref().is_some_and(|artifact| {
                    artifact.engine == ProofArtifact::ENGINE_MIR
                        && artifact.mir_hash == receipt.mir_digest
                        && artifact.source_hash != "imported-alias-proof"
                })
            }),
            "{label}: {public_results:?}"
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    fs::remove_dir_all(project).expect("remove imported alias proof project");
}

#[test]
fn scalar_ffi_imported_alias_negative_verifier_artifact_matches_route_receipt() {
    use crate::verifier::{ProofArtifact, VerifStatus};
    use std::fs;

    let project = std::env::temp_dir().join(format!(
        "mimi-canonical-ffi-import-alias-negative-proof-{}",
        std::process::id()
    ));
    fs::create_dir_all(&project).expect("create imported alias negative proof project");
    let main_path = project.join("main.mimi");
    fs::write(
        &main_path,
        "use ffi_types;\nfunc main() -> i64 { call_imported_alias(-7 as i64) }\n",
    )
    .expect("write imported alias negative proof main");
    fs::write(
        project.join("ffi_types.mimi"),
        "pub type Scalar = f64\npub type Real = Scalar\npub type ScalarInt = i64\npub type ResultId = ScalarInt\nextern \"C\" { func mir_ffi_import_alias_negative(value: Real) -> ResultId requires: value >= 0; }\npub func call_imported_alias(value: i64) -> ResultId { mir_ffi_import_alias_negative(value) }\n",
    )
    .expect("write imported alias negative proof module");

    let source = fs::read_to_string(&main_path).expect("read imported alias negative proof main");
    let tokens = crate::lexer::Lexer::new(&source)
        .tokenize()
        .expect("lex imported alias negative proof main");
    let file = crate::loader::parser_for_path(tokens, &main_path)
        .expect("select imported alias negative proof parser")
        .parse_file()
        .expect("parse imported alias negative proof main");
    let mut loader = crate::loader::ModuleLoader::new(project.clone());
    loader
        .load_main_with_file(&main_path, file)
        .expect("load imported alias negative proof graph");
    let mut merged = loader
        .merge_all()
        .expect("merge imported alias negative proof graph");
    crate::loader::merge_prelude_into(&mut merged);
    let checked =
        crate::core::check_program(&merged).expect("check imported alias negative proof graph");
    let excluded_sources = merged
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<std::collections::HashSet<_>>();
    let route =
        crate::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
            .expect("materialize imported alias negative proof route");
    let mir = MirProgram::from_checked_program_excluding_sources(&checked, &excluded_sources)
        .expect("materialize imported alias negative proof MIR");
    let receipt = mir.route_receipt("scalar-ffi-v1");
    assert_eq!(route.program.route_receipt("scalar-ffi-v1"), receipt);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_mir(&mir, "imported-alias-negative-proof".into())
        .expect("verify imported alias negative proof MIR");
    assert_eq!(
        results.len(),
        1,
        "one imported alias extern requires obligation"
    );
    assert!(results
        .iter()
        .any(|result| result.status == VerifStatus::Disproven));
    assert!(results.iter().all(|result| {
        result.artifact.as_ref().is_some_and(|artifact| {
            artifact.engine == ProofArtifact::ENGINE_MIR && artifact.mir_hash == receipt.mir_digest
        })
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    fs::remove_dir_all(project).expect("remove imported alias negative proof project");
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
    assert!(matches!(
        store_receipt.result_conversion,
        Some(crate::core::mir::MirFfiAbiConversion {
            from: crate::core::mir::types::MirAbiClass::Unit,
            to: crate::core::mir::types::MirAbiClass::Unit,
        })
    ));

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
    assert!(
        mir_results
            .iter()
            .all(|result| result.status == crate::verifier::VerifStatus::Disproven),
        "mixed-width boundary verifier results: {mir_results:?}"
    );
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
fn scalar_ffi_mixed_width_checked_arithmetic_uses_i64_slot_and_short_circuits() {
    struct WidthArithmeticOracle;
    impl MirReferenceFfiResolver for WidthArithmeticOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_i32_max" && receipt.symbol != "mir_ffi_i32_zero" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(_left), MirRuntimeValue::Int(_right)] = args else {
                return Err("width arithmetic oracle expects i32/i64 arguments".into());
            };
            Ok(MirRuntimeValue::Int(i32::MAX as i64))
        }
    }

    const C_SOURCE: &str = r#"
#include <stdint.h>
int32_t mir_ffi_i32_max(int32_t left, int64_t right) { (void)left; (void)right; return INT32_MAX; }
int32_t mir_ffi_i32_zero(int32_t left, int64_t right) { (void)left; (void)right; return INT32_MAX; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_i32_max(left: i32, right: i64) -> i32
        ensures: result + 1 > result;
    func mir_ffi_i32_zero(left: i32, right: i64) -> i32
        ensures: right == 0 or result + 1 > result;
}
func main() -> i32 {
    let max_value = mir_ffi_i32_max(0, 1 as i64);
    let zero_value = mir_ffi_i32_zero(0, 0 as i64);
    println(max_value);
    println(zero_value);
    0
}
"#;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("mixed-width arithmetic FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("mixed-width arithmetic FFI materialization");
    let receipts = mir.ffi_calls().values().collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().all(|receipt| receipt.arguments.len() == 2));
    assert!(receipts.iter().all(|receipt| receipt.result.is_some()));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let mir_results = crate::verifier::verify_mir(&mir, "mixed-arithmetic".into())
        .expect("mixed-width arithmetic MIR verifier");
    assert_eq!(mir_results.len(), 2);
    assert!(
        mir_results
            .iter()
            .all(|result| result.status == crate::verifier::VerifStatus::Proven),
        "mixed-width arithmetic verifier results: {mir_results:?}"
    );
    for results in [
        crate::verifier::verify_checked(&checked, "mixed-arithmetic".into()),
        crate::verifier::verify_checked_dual(&checked, "mixed-arithmetic-dual".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("mixed-width arithmetic public verifier");
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|result| result.status == crate::verifier::VerifStatus::Proven));
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&WidthArithmeticOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("mixed-width arithmetic reference execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "2147483647\n2147483647\n");

    let bytecode = compile_mir_program(&mir).expect("mixed-width arithmetic bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("mixed-width arithmetic VM"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "2147483647\n2147483647\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mixed_width_arithmetic_ffi");
    generator
        .compile_mir_native(&mir)
        .expect("mixed-width arithmetic native compile");
    generator
        .module
        .verify()
        .expect("mixed-width arithmetic native verify");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("mixed-width arithmetic native execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "2147483647\n2147483647\n");
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

    const I64_DECLARATION: &str = r#"
extern "C" { func conversion_identity(value: i64) -> i64; }
func main() -> i64 { conversion_identity(1 as i32); 0 }
"#;
    const F64_DECLARATION: &str = r#"
extern "C" { func conversion_identity(value: f64) -> f64; }
func main() -> i64 { conversion_identity(1 as i32); 0 }
"#;
    let i64_program = materialize(I64_DECLARATION);
    let f64_program = materialize(F64_DECLARATION);
    let i64_route = i64_program.route_receipt("scalar-ffi-v1");
    let f64_route = f64_program.route_receipt("scalar-ffi-v1");
    assert_eq!(i64_route.ffi_digest.len(), 64);
    assert_eq!(f64_route.ffi_digest.len(), 64);
    assert_ne!(
        i64_route.ffi_digest, f64_route.ffi_digest,
        "route receipt must expose an independently comparable FFI digest"
    );
    assert_ne!(
        i64_program.canonical_digest(),
        f64_program.canonical_digest(),
        "ABI conversion start/end classes must participate in the canonical digest"
    );
    assert!(matches!(
        i64_program
            .ffi_calls()
            .values()
            .next()
            .expect("i64 conversion receipt")
            .parameter_conversions
            .as_slice(),
        [crate::core::mir::MirFfiAbiConversion {
            from: crate::core::mir::types::MirAbiClass::Integer {
                bits: 32,
                signed: true
            },
            to: crate::core::mir::types::MirAbiClass::Integer {
                bits: 64,
                signed: true
            }
        }]
    ));
    assert!(matches!(
        f64_program
            .ffi_calls()
            .values()
            .next()
            .expect("f64 conversion receipt")
            .parameter_conversions
            .as_slice(),
        [crate::core::mir::MirFfiAbiConversion {
            from: crate::core::mir::types::MirAbiClass::Integer {
                bits: 32,
                signed: true
            },
            to: crate::core::mir::types::MirAbiClass::Float { bits: 64 }
        }]
    ));
}

#[test]
fn scalar_ffi_route_receipt_digest_pins_cross_call_site_declaration_shape() {
    const SOURCE: &str = r#"
extern "C" { func declaration_shape(value: i64) -> i64; }
func main() -> i64 {
    let narrow = declaration_shape(7 as i32);
    let wide = declaration_shape(8 as i64);
    narrow + wide
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("cross-call-site FFI shape fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("cross-call-site FFI shape fixture materialization");
    assert_eq!(program.ffi_calls().len(), 2);
    let baseline = program.route_receipt("scalar-ffi-shape-v1");
    let main = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("main MIR");
    let (forged_id, argument) = program
        .ffi_calls()
        .iter()
        .find_map(|(id, receipt)| {
            let argument = receipt.arguments.first()?;
            let actual = main.values.get(argument)?;
            (actual.ty != receipt.parameter_types[0]).then(|| (id.clone(), argument.clone()))
        })
        .expect("mixed-width FFI receipt");
    let actual_type = main
        .values
        .get(&argument)
        .map(|value| value.ty.clone())
        .expect("mixed-width argument TypeDesc");

    let mut forged_receipts = program.ffi_calls().clone();
    let forged_receipt = forged_receipts
        .get_mut(&forged_id)
        .expect("mixed-width receipt");
    forged_receipt.parameter_types[0] = actual_type.clone();
    forged_receipt.parameter_conversions[0] = crate::core::mir::MirFfiAbiConversion::for_argument(
        program.type_catalog(),
        &actual_type,
        &actual_type,
    )
    .expect("identity conversion");
    let mut forged = program;
    forged.replace_ffi_calls_for_test_only(forged_receipts);
    let forged_route = forged.route_receipt("scalar-ffi-shape-v1");

    assert_ne!(
        baseline.ffi_digest, forged_route.ffi_digest,
        "route receipt FFI digest must pin declaration shape across call sites"
    );
    assert_ne!(
        baseline.mir_digest, forged_route.mir_digest,
        "whole-program identity must include the forged declaration shape"
    );
    assert_eq!(baseline.type_desc_digest, forged_route.type_desc_digest);
    assert_eq!(baseline.abi_digest, forged_route.abi_digest);
    assert_eq!(baseline.ownership_digest, forged_route.ownership_digest);
    assert_eq!(
        baseline.flow_transition_digest,
        forged_route.flow_transition_digest
    );
    assert_eq!(baseline.root_owners, forged_route.root_owners);
    let shape_errors = crate::core::mir::validate_ffi_symbol_declaration_shapes(forged.ffi_calls());
    assert!(shape_errors.iter().any(|error| {
        error.contains(
            "FFI symbol 'declaration_shape' is used with incompatible declaration TypeDescs",
        )
    }));
}

#[test]
fn scalar_ffi_bytecode_applies_parameter_conversion_before_libffi_call() {
    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));
    let source = r#"
extern "C" { func mir_ffi_f64(x: f64) -> f64; }
func main() -> i64 {
    mir_ffi_f64(7 as i32);
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex converted scalar FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse converted scalar FFI fixture");
    let checked = crate::core::check_program(&file).expect("check converted scalar FFI fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize converted scalar FFI");
    let receipt = mir
        .ffi_calls()
        .values()
        .next()
        .expect("converted FFI receipt");
    assert_eq!(
        receipt.parameter_conversions,
        vec![crate::core::mir::MirFfiAbiConversion {
            from: crate::core::mir::types::MirAbiClass::Integer {
                bits: 32,
                signed: true,
            },
            to: crate::core::mir::types::MirAbiClass::Float { bits: 64 },
        }]
    );

    let bytecode = compile_mir_program(&mir).expect("converted scalar FFI bytecode");
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("converted scalar FFI execution"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "");
}

#[test]
fn scalar_ffi_route_receipt_digest_pins_cross_call_site_result_shape() {
    const SOURCE: &str = r#"
extern "C" { func result_shape(value: i64) -> i64; }
func f64_marker(value: f64) -> f64 { value }
func main() -> i64 {
    let first = result_shape(7 as i64);
    let second = result_shape(8 as i64);
    first + second
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("cross-call-site FFI result shape fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("cross-call-site FFI result shape fixture materialization");
    assert_eq!(program.ffi_calls().len(), 2);
    let f64_type = program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            matches!(
                descriptor.kind,
                crate::core::mir::types::MirTypeKind::Primitive(crate::core::PrimitiveType::F64)
            )
            .then(|| id.clone())
        })
        .expect("f64 TypeDesc");
    let main = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("main MIR");
    let (forged_id, result_value) = program
        .ffi_calls()
        .iter()
        .find_map(|(id, receipt)| {
            let result = receipt.result.as_ref()?;
            let actual = main.values.get(result)?;
            (actual.ty != f64_type).then(|| (id.clone(), result.clone()))
        })
        .expect("scalar FFI result receipt");
    let actual_type = main
        .values
        .get(&result_value)
        .map(|value| value.ty.clone())
        .expect("result TypeDesc");

    let baseline = program.route_receipt("scalar-ffi-result-shape-v1");
    let mut forged_receipts = program.ffi_calls().clone();
    let forged_receipt = forged_receipts
        .get_mut(&forged_id)
        .expect("result-shape receipt");
    forged_receipt.result_type = f64_type.clone();
    forged_receipt.result_conversion = Some(
        crate::core::mir::MirFfiAbiConversion::for_result(
            program.type_catalog(),
            &actual_type,
            &f64_type,
        )
        .expect("i64 to f64 result conversion"),
    );
    let mut forged = program;
    forged.replace_ffi_calls_for_test_only(forged_receipts);
    let forged_route = forged.route_receipt("scalar-ffi-result-shape-v1");

    assert_ne!(
        baseline.ffi_digest, forged_route.ffi_digest,
        "route receipt FFI digest must pin result declaration shape across call sites"
    );
    assert_ne!(
        baseline.mir_digest, forged_route.mir_digest,
        "whole-program identity must include the forged result declaration shape"
    );
    assert_eq!(baseline.type_desc_digest, forged_route.type_desc_digest);
    assert_eq!(baseline.abi_digest, forged_route.abi_digest);
    assert_eq!(baseline.ownership_digest, forged_route.ownership_digest);
    assert_eq!(
        baseline.flow_transition_digest,
        forged_route.flow_transition_digest
    );
    assert_eq!(baseline.root_owners, forged_route.root_owners);
    let shape_errors = crate::core::mir::validate_ffi_symbol_declaration_shapes(forged.ffi_calls());
    assert!(shape_errors.iter().any(|error| {
        error.contains("FFI symbol 'result_shape' is used with incompatible declaration TypeDescs")
    }));
}

#[test]
fn scalar_ffi_route_receipt_digest_pins_cross_call_site_abi_shape() {
    const SOURCE: &str = r#"
extern "C" { func abi_shape(value: i64) -> i64; }
func main() -> i64 {
    let first = abi_shape(7 as i64);
    let second = abi_shape(8 as i64);
    first + second
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("cross-call-site FFI ABI shape fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("cross-call-site FFI ABI shape fixture materialization");
    assert_eq!(program.ffi_calls().len(), 2);
    let forged_id = program
        .ffi_calls()
        .keys()
        .next()
        .cloned()
        .expect("first ABI-shape receipt");
    let baseline = program.route_receipt("scalar-ffi-abi-shape-v1");
    let mut forged_receipts = program.ffi_calls().clone();
    forged_receipts
        .get_mut(&forged_id)
        .expect("ABI-shape receipt")
        .abi = "Rust".into();
    let mut forged = program;
    forged.replace_ffi_calls_for_test_only(forged_receipts);
    let forged_route = forged.route_receipt("scalar-ffi-abi-shape-v1");

    assert_ne!(
        baseline.ffi_digest, forged_route.ffi_digest,
        "route receipt FFI digest must pin ABI shape across call sites"
    );
    assert_ne!(
        baseline.mir_digest, forged_route.mir_digest,
        "whole-program identity must include the forged ABI shape"
    );
    assert_eq!(baseline.type_desc_digest, forged_route.type_desc_digest);
    assert_eq!(baseline.abi_digest, forged_route.abi_digest);
    assert_eq!(baseline.ownership_digest, forged_route.ownership_digest);
    assert_eq!(
        baseline.flow_transition_digest,
        forged_route.flow_transition_digest
    );
    assert_eq!(baseline.root_owners, forged_route.root_owners);
    let shape_errors = crate::core::mir::validate_ffi_symbol_declaration_shapes(forged.ffi_calls());
    assert!(shape_errors.iter().any(|error| {
        error.contains("FFI symbol 'abi_shape' is used with incompatible declaration TypeDescs")
    }));
}

#[test]
fn scalar_ffi_route_receipt_digest_pins_call_site_instruction_identity() {
    const SOURCE: &str = r#"
extern "C" { func instruction_shape(value: i64) -> i64; }
func main() -> i64 {
    let first = instruction_shape(7 as i64);
    let second = instruction_shape(8 as i64);
    first + second
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("call-site instruction identity fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("call-site instruction identity fixture materialization");
    assert_eq!(program.ffi_calls().len(), 2);
    let mut ffi_ids = program.ffi_calls().keys().cloned();
    let first_id = ffi_ids.next().expect("first call-site instruction");
    let second_id = ffi_ids.next().expect("second call-site instruction");
    let baseline = program.route_receipt("scalar-ffi-call-site-v1");
    let mut forged_receipts = program.ffi_calls().clone();
    forged_receipts
        .get_mut(&first_id)
        .expect("first call-site receipt")
        .instruction = second_id.clone();
    let mut forged = program.clone();
    forged.replace_ffi_calls_for_test_only(forged_receipts);
    let forged_route = forged.route_receipt("scalar-ffi-call-site-v1");

    assert_ne!(
        baseline.ffi_digest, forged_route.ffi_digest,
        "route receipt FFI digest must pin call-site instruction identity"
    );
    assert_ne!(
        baseline.mir_digest, forged_route.mir_digest,
        "whole-program identity must include call-site instruction identity"
    );
    assert_eq!(baseline.type_desc_digest, forged_route.type_desc_digest);
    assert_eq!(baseline.abi_digest, forged_route.abi_digest);
    assert_eq!(baseline.ownership_digest, forged_route.ownership_digest);
    assert_eq!(
        baseline.flow_transition_digest,
        forged_route.flow_transition_digest
    );
    assert_eq!(baseline.root_owners, forged_route.root_owners);
    assert!(
        crate::core::mir::validate_ffi_symbol_declaration_shapes(forged.ffi_calls()).is_empty(),
        "same declaration shape must classify this as an identity failure"
    );
    let table_errors =
        crate::core::mir::validate_ffi_receipt_table(forged.functions(), forged.ffi_calls());
    assert!(
        table_errors.iter().any(|error| {
            error.contains("receipt key") && error.contains("receipt instruction")
        }),
        "{table_errors:?}"
    );
    let reference_error = MirReferenceInterpreter::new(&forged)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged call-site instruction identity");
    assert!(
        reference_error.to_string().contains("receipt key")
            || reference_error
                .to_string()
                .contains("FFI receipt disagrees with the MIR call"),
        "{reference_error}"
    );

    let forged_map_key = crate::core::mir::MirInstructionId::new("inst:call:forged-map-key")
        .expect("instruction id");
    let mut key_forged_receipts = program.ffi_calls().clone();
    let key_forged_receipt = key_forged_receipts
        .remove(&first_id)
        .expect("first call-site receipt");
    key_forged_receipts.insert(forged_map_key, key_forged_receipt);
    let mut key_forged = program;
    key_forged.replace_ffi_calls_for_test_only(key_forged_receipts);
    let key_forged_route = key_forged.route_receipt("scalar-ffi-call-site-v1");
    assert_ne!(
        baseline.ffi_digest, key_forged_route.ffi_digest,
        "route receipt FFI digest must pin the receipt table key identity"
    );
    assert_ne!(
        baseline.mir_digest, key_forged_route.mir_digest,
        "whole-program identity must include the receipt table key identity"
    );
    let table_errors = crate::core::mir::validate_ffi_receipt_table(
        key_forged.functions(),
        key_forged.ffi_calls(),
    );
    assert!(
        table_errors
            .iter()
            .any(|error| error.contains("orphaned from a MIR extern call")),
        "{table_errors:?}"
    );
}

#[test]
fn scalar_ffi_route_receipt_digest_pins_call_site_caller_and_callee_identity() {
    const SOURCE: &str = r#"
extern "C" { func owner_shape(value: i64) -> i64; }
func main() -> i64 {
    let first = owner_shape(7 as i64);
    let second = owner_shape(8 as i64);
    first + second
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("call-site caller/callee identity fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("call-site caller/callee identity fixture materialization");
    assert_eq!(program.ffi_calls().len(), 2);
    let forged_id = program
        .ffi_calls()
        .keys()
        .next()
        .cloned()
        .expect("first call-site owner receipt");
    let baseline = program.route_receipt("scalar-ffi-owner-identity-v1");

    for (label, caller, callee) in [
        (
            "caller",
            Some(crate::core::NodeId("function:forged_caller".into())),
            None,
        ),
        (
            "callee",
            None,
            Some(crate::core::NodeId("extern:forged_callee".into())),
        ),
    ] {
        let mut forged_receipts = program.ffi_calls().clone();
        let forged_receipt = forged_receipts
            .get_mut(&forged_id)
            .expect("call-site owner receipt");
        if let Some(caller) = caller {
            forged_receipt.caller = caller;
        }
        if let Some(callee) = callee {
            forged_receipt.callee = callee;
        }
        let mut forged = program.clone();
        forged.replace_ffi_calls_for_test_only(forged_receipts);
        let forged_route = forged.route_receipt("scalar-ffi-owner-identity-v1");

        assert_ne!(
            baseline.ffi_digest, forged_route.ffi_digest,
            "route receipt FFI digest must pin call-site {label} identity"
        );
        assert_ne!(
            baseline.mir_digest, forged_route.mir_digest,
            "whole-program identity must include call-site {label} identity"
        );
        assert_eq!(baseline.type_desc_digest, forged_route.type_desc_digest);
        assert_eq!(baseline.abi_digest, forged_route.abi_digest);
        assert_eq!(baseline.ownership_digest, forged_route.ownership_digest);
        assert_eq!(
            baseline.flow_transition_digest,
            forged_route.flow_transition_digest
        );
        assert_eq!(baseline.root_owners, forged_route.root_owners);
        assert!(
            crate::core::mir::validate_ffi_symbol_declaration_shapes(forged.ffi_calls()).is_empty(),
            "same declaration shape must classify {label} identity as a call-site failure"
        );
        if label == "caller" {
            let table_errors = crate::core::mir::validate_ffi_receipt_table(
                forged.functions(),
                forged.ffi_calls(),
            );
            assert!(
                table_errors
                    .iter()
                    .any(|error| error.contains("receipt caller") && error.contains("owner")),
                "{label}: {table_errors:?}"
            );
        } else {
            let table_errors = crate::core::mir::validate_ffi_receipt_table(
                forged.functions(),
                forged.ffi_calls(),
            );
            assert!(
                table_errors.iter().any(|error| {
                    error.contains("receipt callee") && error.contains("MIR instruction")
                }),
                "{label}: {table_errors:?}"
            );
        }

        let reference_error = MirReferenceInterpreter::new(&forged)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect_err("reference must reject forged call-site owner identity");
        assert!(
            reference_error
                .to_string()
                .contains("FFI receipt disagrees with the MIR call")
                || reference_error.to_string().contains("receipt caller")
                || reference_error.to_string().contains("receipt callee"),
            "{label}: {reference_error}"
        );

        let bytecode_error = crate::interp::bytecode::compile_mir_program(&forged)
            .expect_err("bytecode must reject forged call-site owner identity");
        assert!(
            bytecode_error.iter().any(|error| {
                error.message.contains("identity/ABI validation")
                    || error.message.contains("FFI contract identity disagrees")
                    || error.message.contains("absent from its caller")
                    || error.message.contains("absent caller")
            }),
            "{label}: {bytecode_error:?}"
        );

        let native_error = crate::codegen::mir::validate_mir_native(&forged)
            .expect_err("native validator must reject forged call-site owner identity");
        assert!(
            native_error.iter().any(|error| {
                error.message.contains("FFI contract identity disagrees")
                    || error.message.contains("FFI receipt identity disagrees")
            }),
            "{label}: {native_error:?}"
        );

        let capability_error = crate::verifier::validate_mir_capabilities(&forged)
            .expect_err("capability gate must reject forged call-site owner identity");
        assert!(
            capability_error
                .iter()
                .any(|error| error.contains("contract identity disagrees")),
            "{label}: {capability_error:?}"
        );

        let verifier_error = crate::verifier::verify_mir(&forged, "forged-owner-identity".into())
            .expect_err("direct verifier must reject forged call-site owner identity");
        assert!(
            verifier_error.contains("contract identity disagrees")
                || verifier_error.contains("receipt caller")
                || verifier_error.contains("receipt callee"),
            "{label}: {verifier_error}"
        );
    }
}

#[test]
fn scalar_ffi_receipt_table_rejects_duplicate_instruction_ids_across_functions() {
    const SOURCE: &str = r#"
extern "C" { func duplicate_shape(value: i64) -> i64; }
func main() -> i64 { duplicate_shape(7 as i64) }
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("duplicate instruction identity fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("duplicate instruction identity fixture materialization");
    let main_owner = crate::core::NodeId("function:main".into());
    let duplicate_owner = crate::core::NodeId("function:duplicate".into());
    let mut functions = program.functions().clone();
    let mut duplicate = functions
        .get(&main_owner)
        .cloned()
        .expect("main MIR function");
    duplicate.owner = duplicate_owner.clone();
    functions.insert(duplicate_owner, duplicate);

    let errors = crate::core::mir::validate_ffi_receipt_table(&functions, program.ffi_calls());
    assert!(
        errors
            .iter()
            .any(|error| error.contains("appears in multiple functions")),
        "{errors:?}"
    );
}

#[test]
fn scalar_ffi_receipt_table_rejects_argument_and_result_identity_drift() {
    const SOURCE: &str = r#"
extern "C" { func payload_shape(value: i64) -> i64; }
func main() -> i64 { payload_shape(7 as i64) }
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("argument/result identity fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("argument/result identity fixture materialization");
    let instruction_id = program
        .ffi_calls()
        .keys()
        .next()
        .cloned()
        .expect("payload call-site receipt");

    for label in ["arguments", "result"] {
        let mut receipts = program.ffi_calls().clone();
        let receipt = receipts
            .get_mut(&instruction_id)
            .expect("payload call-site receipt");
        if label == "arguments" {
            receipt.arguments.clear();
        } else {
            receipt.result = None;
        }
        let errors = crate::core::mir::validate_ffi_receipt_table(program.functions(), &receipts);
        let expected = if label == "arguments" {
            "receipt arguments"
        } else {
            "receipt result"
        };
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "{label}: {errors:?}"
        );
    }
}

#[test]
fn scalar_ffi_receipt_table_rejects_result_identity_overlap() {
    const SOURCE: &str = r#"
extern "C" { func overlap_shape(value: i64) -> i64; }
func main() -> i64 { overlap_shape(7 as i64) }
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("result identity overlap fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("result identity overlap fixture materialization");
    let owner = crate::core::NodeId("function:main".into());
    let instruction_id = program
        .ffi_calls()
        .keys()
        .next()
        .cloned()
        .expect("overlap call-site receipt");
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("main MIR");
    let argument_id = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Extern(_),
                arguments,
                ..
            } => arguments.first().cloned(),
            _ => None,
        })
        .expect("extern argument identity");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| instruction.id == instruction_id)
        .expect("extern call");
    let crate::core::mir::MirInstructionKind::Call { result, .. } = &mut instruction.kind else {
        panic!("expected extern call");
    };
    *result = Some(argument_id.clone());

    let mut receipts = program.ffi_calls().clone();
    receipts
        .get_mut(&instruction_id)
        .expect("overlap call-site receipt")
        .result = Some(argument_id);
    let table_errors = crate::core::mir::validate_ffi_receipt_table(&functions, &receipts);
    assert!(
        table_errors
            .iter()
            .any(|error| { error.contains("result identity overlaps an argument identity") }),
        "{table_errors:?}"
    );
}

#[test]
fn scalar_ffi_receipt_table_rejects_missing_result_identity() {
    const SOURCE: &str = r#"
extern "C" { func missing_result(value: i64) -> i64; }
func main() -> i64 { missing_result(7 as i64) }
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("missing result identity fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("missing result identity fixture materialization");
    let instruction_id = program
        .ffi_calls()
        .keys()
        .next()
        .cloned()
        .expect("missing-result call-site receipt");
    let mut functions = program.functions().clone();
    let function = functions
        .get_mut(&crate::core::NodeId("function:main".into()))
        .expect("main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| instruction.id == instruction_id)
        .expect("extern call");
    let crate::core::mir::MirInstructionKind::Call { result, .. } = &mut instruction.kind else {
        panic!("expected extern call");
    };
    *result = None;
    let mut receipts = program.ffi_calls().clone();
    receipts
        .get_mut(&instruction_id)
        .expect("missing-result call-site receipt")
        .result = None;
    let table_errors = crate::core::mir::validate_ffi_receipt_table(&functions, &receipts);
    assert!(
        table_errors
            .iter()
            .any(|error| error.contains("has no canonical result value identity")),
        "{table_errors:?}"
    );
}

#[test]
fn scalar_ffi_receipt_table_rejects_manifest_unsafe_symbol() {
    const SOURCE: &str = r#"
extern "C" { func unsafe_symbol(value: i64) -> i64; }
func main() -> i64 { unsafe_symbol(7 as i64) }
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("manifest-unsafe symbol fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("manifest-unsafe symbol fixture materialization");
    let instruction_id = program
        .ffi_calls()
        .keys()
        .next()
        .cloned()
        .expect("unsafe-symbol call-site receipt");
    let mut receipts = program.ffi_calls().clone();
    receipts
        .get_mut(&instruction_id)
        .expect("unsafe-symbol call-site receipt")
        .symbol = "unsafe symbol".into();
    let table_errors = crate::core::mir::validate_ffi_receipt_table(program.functions(), &receipts);
    assert!(
        table_errors
            .iter()
            .any(|error| error.contains("FFI symbol is not manifest-safe")
                && error.contains("whitespace or a manifest delimiter")),
        "{table_errors:?}"
    );
}

#[test]
fn scalar_ffi_per_call_receipt_rejects_manifest_unsafe_symbol_before_match() {
    const SOURCE: &str = r#"
extern "C" { func unsafe_symbol(value: i64) -> i64; }
func main() -> i64 { unsafe_symbol(7 as i64) }
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("manifest-unsafe per-call fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("manifest-unsafe per-call fixture materialization");
    let receipt = program
        .ffi_calls()
        .values()
        .next()
        .expect("unsafe-symbol call-site receipt");
    let function = program
        .functions()
        .get(&receipt.caller)
        .expect("unsafe-symbol caller");
    let instruction = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find(|instruction| instruction.id == receipt.instruction)
        .expect("unsafe-symbol MIR call");
    let crate::core::mir::MirInstructionKind::Call {
        callee: crate::core::ResolvedCallee::Extern(callee),
        result,
        arguments,
        ..
    } = &instruction.kind
    else {
        panic!("fixture must contain an extern MIR call");
    };
    let mut forged = receipt.clone();
    forged.symbol = "unsafe symbol".into();
    let errors = crate::core::mir::validate_ffi_call_contract_receipt(
        program.type_catalog(),
        function,
        &instruction.id,
        callee,
        result.as_ref(),
        arguments,
        &forged,
    );
    assert!(
        errors.iter().any(|error| {
            error.contains("FFI symbol is not manifest-safe")
                && error.contains("whitespace or a manifest delimiter")
        }),
        "{errors:?}"
    );
}

#[test]
fn scalar_ffi_manifest_symbol_safety_classifier_covers_all_rejection_classes() {
    for (symbol, reason) in [
        ("", "FFI symbol is empty"),
        ("   ", "FFI symbol is empty"),
        ("bad\nname", "FFI symbol contains a control character"),
        (
            "bad name",
            "FFI symbol contains whitespace or a manifest delimiter",
        ),
        (
            "bad=name",
            "FFI symbol contains whitespace or a manifest delimiter",
        ),
        (
            "bad,name",
            "FFI symbol contains whitespace or a manifest delimiter",
        ),
    ] {
        let error = crate::core::mir::validate_ffi_symbol_manifest_safety(symbol)
            .expect_err("malformed symbol must be rejected");
        assert!(error.contains(reason), "{symbol:?}: {error}");
    }
    for symbol in ["read", "read_2", "utf8_%HH"] {
        crate::core::mir::validate_ffi_symbol_manifest_safety(symbol)
            .expect("identifier-shaped symbols must be accepted");
    }
}

#[test]
fn scalar_ffi_symbol_safety_precedes_shape_conflict_across_direct_consumers() {
    const SOURCE: &str = r#"
extern "C" {
    func foreign_i64(value: i64) -> i64;
    func foreign_i32(value: i32) -> i32;
}
func main() -> i64 {
    let left = foreign_i64(1 as i64);
    let right = foreign_i32(2 as i32);
    left + (right as i64)
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("mixed declaration-shape fixture check");
    let canonical = MirProgram::from_checked_program(&checked)
        .expect("mixed declaration-shape fixture materialization");
    assert_eq!(
        canonical.ffi_calls().len(),
        2,
        "fixture must contain two FFI calls"
    );
    let mut receipts = canonical.ffi_calls().clone();
    for receipt in receipts.values_mut() {
        receipt.symbol = "foreign symbol".into();
    }
    let mut forged = canonical;
    forged.replace_ffi_calls_for_test_only(receipts);

    let reference_error = MirReferenceInterpreter::new(&forged)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must run symbol preflight before shape aggregation");
    assert!(
        reference_error
            .to_string()
            .contains("FFI symbol is not manifest-safe"),
        "{reference_error}"
    );

    let bytecode_errors = compile_mir_program(&forged)
        .expect_err("bytecode must run symbol preflight before shape aggregation");
    assert!(
        bytecode_errors
            .first()
            .is_some_and(|error| error.message.contains("FFI symbol is not manifest-safe")),
        "{bytecode_errors:?}"
    );

    let native_errors = crate::codegen::mir::validate_mir_native(&forged)
        .expect_err("native must run receipt preflight before shape aggregation");
    assert!(
        native_errors
            .first()
            .is_some_and(|error| error.message.contains("FFI symbol is not manifest-safe")),
        "{native_errors:?}"
    );

    let capability_errors = crate::verifier::validate_mir_capabilities(&forged)
        .expect_err("capability gate must reject the malformed symbol");
    assert!(
        capability_errors
            .iter()
            .any(|error| error.contains("FFI symbol is not manifest-safe")),
        "{capability_errors:?}"
    );

    let verifier_error = crate::verifier::verify_mir(&forged, "shape-order".into())
        .expect_err("public verifier must reject the malformed symbol");
    assert!(
        verifier_error.contains("FFI symbol 'foreign symbol' is not manifest-safe"),
        "{verifier_error}"
    );
}

#[test]
fn scalar_ffi_shape_validator_ignores_manifest_unsafe_symbols() {
    const SOURCE: &str = r#"
extern "C" {
    func foreign_i64(value: i64) -> i64;
    func foreign_i32(value: i32) -> i32;
}
func main() -> i64 {
    let left = foreign_i64(1 as i64);
    let right = foreign_i32(2 as i32);
    left + (right as i64)
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("mixed declaration-shape fixture check");
    let canonical = MirProgram::from_checked_program(&checked)
        .expect("mixed declaration-shape fixture materialization");
    let mut receipts = canonical.ffi_calls().clone();
    for receipt in receipts.values_mut() {
        receipt.symbol = "foreign symbol".into();
    }
    let mut forged = canonical;
    forged.replace_ffi_calls_for_test_only(receipts);
    assert!(
        crate::core::mir::validate_ffi_symbol_declaration_shapes(forged.ffi_calls()).is_empty(),
        "manifest-unsafe symbols are rejected by the safety gate, not declaration-shape aggregation"
    );
}

#[test]
fn scalar_ffi_predicate_receipt_is_validated_before_consumers() {
    const SOURCE: &str = r#"
extern "C" { func predicate_shape(value: i64) -> i64; }
func main() -> i64 { predicate_shape(7 as i64) }
"#;
    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("predicate receipt fixture check");
    let canonical = MirProgram::from_checked_program(&checked)
        .expect("predicate receipt fixture materialization");
    let mut receipts = canonical.ffi_calls().clone();
    receipts
        .values_mut()
        .next()
        .expect("predicate call-site receipt")
        .requires = Some(crate::core::mir::MirContractExpr::Value(
        crate::core::mir::MirValueId::new("value:missing-predicate").expect("MIR value id"),
    ));
    let mut forged = canonical;
    forged.replace_ffi_calls_for_test_only(receipts);
    let expected = "extern requires value 'value:missing-predicate' is not a call argument";
    let table_errors =
        crate::core::mir::validate_ffi_receipt_table(forged.functions(), forged.ffi_calls());
    assert!(table_errors.is_empty(), "{table_errors:?}");
    let reference_error = MirReferenceInterpreter::new(&forged)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject malformed FFI predicate");
    assert!(
        reference_error.to_string().contains(expected),
        "{reference_error}"
    );
    let bytecode_error = compile_mir_program(&forged)
        .expect_err("bytecode materializer must reject malformed FFI predicate");
    assert!(
        bytecode_error
            .iter()
            .any(|error| error.message.contains(expected)),
        "{bytecode_error:?}"
    );
    let native_error = crate::codegen::mir::validate_mir_native(&forged)
        .expect_err("native admission must reject malformed FFI predicate");
    assert!(
        native_error
            .iter()
            .any(|error| error.message.contains(expected)),
        "{native_error:?}"
    );
    let capability_error = crate::verifier::validate_mir_capabilities(&forged)
        .expect_err("capability admission must reject malformed FFI predicate");
    assert!(
        capability_error
            .iter()
            .any(|error| error.contains(expected)),
        "{capability_error:?}"
    );
    let verifier_error = crate::verifier::verify_mir(&forged, "malformed-ffi-predicate".into())
        .expect_err("MIR verifier must reject malformed FFI predicate");
    assert!(verifier_error.contains(expected), "{verifier_error}");
}

#[test]
fn scalar_ffi_direct_consumers_reject_missing_and_forged_receipts_before_execution() {
    const SOURCE: &str = r#"
extern "C" { func receipt_guard(value: i64) -> i64 requires: value >= 0; }
func main() -> i64 { receipt_guard(1 as i64) }
"#;
    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("receipt-guard FFI fixture check");
    let canonical = MirProgram::from_checked_program(&checked)
        .expect("receipt-guard FFI fixture materialization");
    let instruction_id = canonical
        .ffi_calls()
        .keys()
        .next()
        .cloned()
        .expect("receipt-guard call-site receipt");

    let mut missing = canonical.clone();
    missing.replace_ffi_calls_for_test_only(std::collections::BTreeMap::new());
    let table_errors =
        crate::core::mir::validate_ffi_receipt_table(missing.functions(), missing.ffi_calls());
    assert!(
        table_errors
            .iter()
            .any(|error| error.contains("has no FFI receipt")),
        "{table_errors:?}"
    );
    let reference_error = MirReferenceInterpreter::new(&missing)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a missing FFI receipt");
    assert!(
        reference_error
            .to_string()
            .contains("no canonical FFI receipt")
            || reference_error.to_string().contains("has no FFI receipt"),
        "{reference_error}"
    );
    let bytecode_error =
        compile_mir_program(&missing).expect_err("bytecode must reject a missing FFI receipt");
    assert!(
        bytecode_error.iter().any(|error| {
            error.message.contains("canonical bytecode FFI descriptor")
                || error.message.contains("canonical FFI receipt")
                || error.message.contains("has no FFI receipt")
        }),
        "{bytecode_error:?}"
    );
    let native_error = crate::codegen::mir::validate_mir_native(&missing)
        .expect_err("native admission must reject a missing FFI receipt");
    assert!(
        native_error.iter().any(|error| {
            error.message.contains("no canonical FFI receipt")
                || error.message.contains("no FFI receipt")
        }),
        "{native_error:?}"
    );
    let capability_error = crate::verifier::validate_mir_capabilities(&missing)
        .expect_err("capability gate must reject a missing FFI receipt");
    assert!(
        capability_error.iter().any(|error| {
            error.contains("no canonical FFI contract") || error.contains("has no FFI receipt")
        }),
        "{capability_error:?}"
    );
    let verifier_error = crate::verifier::verify_mir(&missing, "missing-ffi-receipt".into())
        .expect_err("direct verifier must reject a missing FFI receipt");
    assert!(
        verifier_error.contains("no canonical FFI contract")
            || verifier_error.contains("has no FFI receipt"),
        "{verifier_error}"
    );

    let orphan_instruction =
        crate::core::mir::MirInstructionId::new("inst:call:orphan").expect("instruction id");
    let mut orphan_receipts = canonical.ffi_calls().clone();
    let mut orphan_receipt = orphan_receipts
        .values()
        .next()
        .cloned()
        .expect("receipt-guard call-site receipt");
    orphan_receipt.instruction = orphan_instruction.clone();
    // Keep the orphan malformed in a second dimension.  The whole-program
    // receipt boundary must classify the orphan before the verifier's
    // standalone manifest-safety scan, matching every other consumer.
    orphan_receipt.symbol = "orphan symbol".into();
    orphan_receipts.insert(orphan_instruction, orphan_receipt);
    let mut orphan = canonical.clone();
    orphan.replace_ffi_calls_for_test_only(orphan_receipts);
    let reference_error = MirReferenceInterpreter::new(&orphan)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject an orphaned FFI receipt");
    assert!(
        reference_error
            .to_string()
            .contains("orphaned from a MIR extern call"),
        "{reference_error}"
    );
    let bytecode_error =
        compile_mir_program(&orphan).expect_err("bytecode must reject an orphaned FFI receipt");
    assert!(
        bytecode_error.iter().any(|error| {
            error.message.contains("orphaned") || error.message.contains("absent from its caller")
        }),
        "{bytecode_error:?}"
    );
    let native_error = crate::codegen::mir::validate_mir_native(&orphan)
        .expect_err("native admission must reject an orphaned FFI receipt");
    assert!(
        native_error
            .iter()
            .any(|error| error.message.contains("orphaned from a MIR extern call")),
        "{native_error:?}"
    );
    let capability_error = crate::verifier::validate_mir_capabilities(&orphan)
        .expect_err("capability gate must reject an orphaned FFI receipt");
    assert!(
        capability_error
            .iter()
            .any(|error| error.contains("orphaned from a MIR extern call")),
        "{capability_error:?}"
    );
    let verifier_error = crate::verifier::verify_mir(&orphan, "orphan-ffi-receipt".into())
        .expect_err("direct verifier must reject an orphaned FFI receipt");
    assert!(
        verifier_error.contains("orphaned from a MIR extern call"),
        "{verifier_error}"
    );

    struct AcceptAnyFfiResolver;
    impl crate::core::mir::reference::MirReferenceFfiResolver for AcceptAnyFfiResolver {
        fn call(
            &self,
            _: &crate::core::mir::MirFfiCallContract,
            _: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            Ok(MirRuntimeValue::Int(7))
        }
    }
    let mut wrong_symbol_receipts = canonical.ffi_calls().clone();
    wrong_symbol_receipts
        .values_mut()
        .next()
        .expect("receipt-guard call-site receipt")
        .symbol = "wrong_receipt_symbol".into();
    let mut wrong_symbol = canonical.clone();
    wrong_symbol.replace_ffi_calls_for_test_only(wrong_symbol_receipts);
    let wrong_symbol_resolver = AcceptAnyFfiResolver;
    let reference_error = MirReferenceInterpreter::new(&wrong_symbol)
        .with_ffi_resolver(&wrong_symbol_resolver)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a safe but mismatched FFI symbol");
    assert!(
        reference_error
            .to_string()
            .contains("symbol disagrees with canonical extern callee"),
        "{reference_error}"
    );
    let table_errors = crate::core::mir::validate_ffi_receipt_table(
        wrong_symbol.functions(),
        wrong_symbol.ffi_calls(),
    );
    assert!(
        table_errors
            .iter()
            .any(|error| error.contains("symbol disagrees with canonical extern callee")),
        "{table_errors:?}"
    );
    let bytecode_error = compile_mir_program(&wrong_symbol)
        .expect_err("bytecode must reject a safe but mismatched FFI symbol");
    assert!(
        bytecode_error.iter().any(|error| {
            error
                .message
                .contains("symbol disagrees with canonical extern callee")
                || error.message.contains("identity/ABI validation")
        }),
        "{bytecode_error:?}"
    );
    let native_error = crate::codegen::mir::validate_mir_native(&wrong_symbol)
        .expect_err("native admission must reject a safe but mismatched FFI symbol");
    assert!(
        native_error.iter().any(|error| {
            error
                .message
                .contains("symbol disagrees with canonical extern callee")
        }),
        "{native_error:?}"
    );
    let capability_error = crate::verifier::validate_mir_capabilities(&wrong_symbol)
        .expect_err("capability gate must reject a safe but mismatched FFI symbol");
    assert!(
        capability_error
            .iter()
            .any(|error| error.contains("symbol disagrees with canonical extern callee")),
        "{capability_error:?}"
    );
    let verifier_error = crate::verifier::verify_mir(&wrong_symbol, "wrong-ffi-symbol".into())
        .expect_err("direct verifier must reject a safe but mismatched FFI symbol");
    assert!(
        verifier_error.contains("symbol disagrees with canonical extern callee"),
        "{verifier_error}"
    );

    let mut wrong_abi_receipts = wrong_symbol.ffi_calls().clone();
    wrong_abi_receipts
        .values_mut()
        .next()
        .expect("receipt-guard call-site receipt")
        .abi = "Rust".into();
    let table_errors =
        crate::core::mir::validate_ffi_receipt_table(wrong_symbol.functions(), &wrong_abi_receipts);
    assert!(
        table_errors
            .iter()
            .any(|error| error.contains("outside the canonical C ABI")),
        "{table_errors:?}"
    );

    let mut forged_receipts = canonical.ffi_calls().clone();
    forged_receipts
        .get_mut(&instruction_id)
        .expect("receipt-guard call-site receipt")
        .parameter_conversions[0] = crate::core::mir::MirFfiAbiConversion {
        from: crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
        to: crate::core::mir::types::MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
    };
    let mut forged = canonical;
    forged.replace_ffi_calls_for_test_only(forged_receipts);
    let reference_error = MirReferenceInterpreter::new(&forged)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged FFI conversion receipt");
    assert!(
        reference_error
            .to_string()
            .contains("ABI conversion receipt"),
        "{reference_error}"
    );
    let bytecode_error = compile_mir_program(&forged)
        .expect_err("bytecode must reject a forged FFI conversion receipt");
    assert!(
        bytecode_error.iter().any(|error| {
            error.message.contains("conversion receipt")
                || error.message.contains("identity/ABI validation")
        }),
        "{bytecode_error:?}"
    );
    let native_error = crate::codegen::mir::validate_mir_native(&forged)
        .expect_err("native admission must reject a forged FFI conversion receipt");
    assert!(
        native_error
            .iter()
            .any(|error| error.message.contains("conversion receipt")),
        "{native_error:?}"
    );
    let capability_error = crate::verifier::validate_mir_capabilities(&forged)
        .expect_err("capability gate must reject a forged FFI conversion receipt");
    assert!(
        capability_error
            .iter()
            .any(|error| error.contains("conversion receipt")),
        "{capability_error:?}"
    );
    let verifier_error = crate::verifier::verify_mir(&forged, "forged-ffi-receipt".into())
        .expect_err("direct verifier must reject a forged FFI conversion receipt");
    assert!(
        verifier_error.contains("conversion receipt"),
        "{verifier_error}"
    );

    let mut forged_result_receipts = forged.ffi_calls().clone();
    forged_result_receipts
        .get_mut(&instruction_id)
        .expect("receipt-guard call-site receipt")
        .result_conversion = Some(crate::core::mir::MirFfiAbiConversion {
        from: crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
        to: crate::core::mir::types::MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
    });
    let mut forged_result = forged;
    forged_result.replace_ffi_calls_for_test_only(forged_result_receipts);
    let reference_error = MirReferenceInterpreter::new(&forged_result)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged FFI result conversion receipt");
    assert!(
        reference_error
            .to_string()
            .contains("ABI conversion receipt"),
        "{reference_error}"
    );
    let bytecode_error = compile_mir_program(&forged_result)
        .expect_err("bytecode must reject a forged FFI result conversion receipt");
    assert!(
        bytecode_error.iter().any(|error| {
            error.message.contains("conversion receipt")
                || error.message.contains("identity/ABI validation")
        }),
        "{bytecode_error:?}"
    );
    let native_error = crate::codegen::mir::validate_mir_native(&forged_result)
        .expect_err("native admission must reject a forged FFI result conversion receipt");
    assert!(
        native_error
            .iter()
            .any(|error| error.message.contains("conversion receipt")),
        "{native_error:?}"
    );
    let capability_error = crate::verifier::validate_mir_capabilities(&forged_result)
        .expect_err("capability gate must reject a forged FFI result conversion receipt");
    assert!(
        capability_error
            .iter()
            .any(|error| error.contains("conversion receipt")),
        "{capability_error:?}"
    );
    let verifier_error = crate::verifier::verify_mir(&forged_result, "forged-ffi-result".into())
        .expect_err("direct verifier must reject a forged FFI result conversion receipt");
    assert!(
        verifier_error.contains("conversion receipt"),
        "{verifier_error}"
    );
}

#[test]
fn scalar_ffi_route_receipt_is_invariant_to_ffi_table_insertion_order() {
    const SOURCE: &str = r#"
extern "C" { func table_order(value: i64) -> i64; }
func main() -> i64 {
    let first = table_order(7 as i64);
    let second = table_order(8 as i64);
    first + second
}
"#;
    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("FFI table-order fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("FFI table-order fixture materialization");
    assert_eq!(program.ffi_calls().len(), 2);
    let baseline = program.route_receipt("scalar-ffi-table-order-v1");
    let reversed = program
        .ffi_calls()
        .iter()
        .rev()
        .map(|(instruction, receipt)| (instruction.clone(), receipt.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut rebuilt = program.clone();
    rebuilt.replace_ffi_calls_for_test_only(reversed);
    let reordered = rebuilt.route_receipt("scalar-ffi-table-order-v1");

    assert_eq!(baseline, reordered);
    assert_eq!(program.canonical_digest(), rebuilt.canonical_digest());
    assert!(reordered.validate().is_ok());
    assert_eq!(
        crate::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_FIELDS,
        [
            "schema",
            "profile",
            "mir_digest",
            "type_desc_digest",
            "abi_digest",
            "ffi_digest",
            "ownership_digest",
            "flow_transition_digest",
            "root_owners",
        ]
    );
}

#[test]
fn scalar_ffi_source_order_tie_break_uses_call_span_columns() {
    const SOURCE: &str = r#"
extern "C" { func span_order(value: i64) -> i64; }
func main() -> i64 { let first = span_order(7 as i64); let second = span_order(8 as i64); first + second }
"#;
    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("FFI span-order fixture check");
    let program =
        MirProgram::from_checked_program(&checked).expect("FFI span-order fixture materialization");
    let ordered = program.ffi_call_entries_in_source_order();
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].1.caller, ordered[1].1.caller);
    assert_eq!(ordered[0].1.span.start_line, ordered[1].1.span.start_line);
    assert!(
        ordered[0].1.span.start_col < ordered[1].1.span.start_col,
        "same-line FFI calls must be ordered by their source span columns"
    );
    assert_ne!(ordered[0].1.instruction, ordered[1].1.instruction);

    let reversed = program
        .ffi_calls()
        .iter()
        .rev()
        .map(|(instruction, receipt)| (instruction.clone(), receipt.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut rebuilt = program.clone();
    rebuilt.replace_ffi_calls_for_test_only(reversed);
    let rebuilt_ordered = rebuilt.ffi_call_entries_in_source_order();
    assert_eq!(
        ordered
            .iter()
            .map(|(_, receipt)| receipt.instruction.clone())
            .collect::<Vec<_>>(),
        rebuilt_ordered
            .iter()
            .map(|(_, receipt)| receipt.instruction.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        program.route_receipt("scalar-ffi-span-order-v1"),
        rebuilt.route_receipt("scalar-ffi-span-order-v1")
    );
}

#[test]
fn scalar_ffi_duplicate_imported_declaration_keeps_checker_span_provenance() {
    use std::fs;

    let project = std::env::temp_dir().join(format!(
        "mimi-canonical-ffi-duplicate-provenance-{}",
        std::process::id()
    ));
    fs::create_dir_all(&project).expect("create duplicate declaration project");
    let main_path = project.join("main.mimi");
    let left_path = project.join("left.mimi");
    let right_path = project.join("right.mimi");
    fs::write(
        &main_path,
        "use left;\nuse right;\nfunc main() -> i64 { 0 }\n",
    )
    .expect("write duplicate declaration main");
    fs::write(
        &left_path,
        "extern \"C\" {\n    func clash(value: i64) -> i64;\n}\npub func call_left(value: i64) -> i64 { clash(value) }\n",
    )
    .expect("write left duplicate declaration");
    fs::write(
        &right_path,
        "extern \"C\" {\n    func clash(value: i64) -> i64;\n}\npub func call_right(value: i64) -> i64 { clash(value) }\n",
    )
    .expect("write right duplicate declaration");

    let source = fs::read_to_string(&main_path).expect("read duplicate declaration main");
    let tokens = crate::lexer::Lexer::new(&source)
        .tokenize()
        .expect("lex duplicate declaration main");
    let file = crate::loader::parser_for_path(tokens, &main_path)
        .expect("select duplicate declaration parser")
        .parse_file()
        .expect("parse duplicate declaration main");
    let mut loader = crate::loader::ModuleLoader::new(project.clone());
    loader
        .load_main_with_file(&main_path, file)
        .expect("load duplicate declaration graph");
    let mut merged = loader
        .merge_all()
        .expect("merge duplicate declaration graph");
    crate::loader::merge_prelude_into(&mut merged);

    let diagnostics = crate::core::check_program(&merged)
        .expect_err("duplicate imported extern declarations must fail checker");
    let duplicate = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0402))
        .expect("duplicate imported extern diagnostic");
    assert!(duplicate
        .message
        .contains("duplicate extern function 'clash'"));
    let source_record = merged
        .sources
        .record(duplicate.span.source_id)
        .expect("duplicate diagnostic source record");
    assert_eq!(
        source_record.disk_path.as_deref(),
        right_path.canonicalize().ok().as_deref(),
        "duplicate declaration diagnostic must point at the later imported declaration"
    );
    assert_ne!(
        source_record.disk_path.as_deref(),
        Some(main_path.as_path())
    );
    assert_eq!(
        duplicate.notes.len(),
        1,
        "duplicate declaration keeps prior span note"
    );
    assert_eq!(
        duplicate.notes[0].message,
        "previous extern declaration is here"
    );
    let previous_record = merged
        .sources
        .record(duplicate.notes[0].span.source_id)
        .expect("previous duplicate declaration source record");
    assert_eq!(
        previous_record.disk_path.as_deref(),
        left_path.canonicalize().ok().as_deref(),
        "duplicate declaration note must point at the first imported declaration"
    );
    fs::remove_dir_all(project).expect("remove duplicate declaration project");
}

#[test]
fn scalar_ffi_same_symbol_accepts_mixed_call_site_widths_from_one_declaration() {
    struct SharedWidthOracle;
    impl MirReferenceFfiResolver for SharedWidthOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_shared_width" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = args else {
                return Err("shared-width oracle expects one integer".into());
            };
            Ok(MirRuntimeValue::Int(*value))
        }
    }

    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_shared_width(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_shared_width(value: i64) -> i64;
}
func main() -> i64 {
    let narrow = mir_ffi_shared_width(7 as i32);
    let wide = mir_ffi_shared_width(8 as i64);
    println(narrow);
    println(wide);
    0
}
"#;

    let _guard = super::FfiEnvLock::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    std::env::set_var("MIMI_FFI_LIB", fixture.dir.join("ffi.so"));
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("mixed call-site width FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("one declaration may serve mixed-width call sites");
    let receipts = mir
        .ffi_calls()
        .values()
        .filter(|receipt| receipt.symbol == "mir_ffi_shared_width")
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    assert!(matches!(
        receipts[0].parameter_conversions.as_slice(),
        [crate::core::mir::MirFfiAbiConversion {
            from: crate::core::mir::types::MirAbiClass::Integer {
                bits: 32,
                signed: true
            },
            to: crate::core::mir::types::MirAbiClass::Integer {
                bits: 64,
                signed: true
            }
        }]
    ));
    assert!(matches!(
        receipts[1].parameter_conversions.as_slice(),
        [crate::core::mir::MirFfiAbiConversion {
            from: crate::core::mir::types::MirAbiClass::Integer {
                bits: 64,
                signed: true
            },
            to: crate::core::mir::types::MirAbiClass::Integer {
                bits: 64,
                signed: true
            }
        }]
    ));

    let owner = crate::core::NodeId("function:main".into());
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&SharedWidthOracle)
        .execute_with_output(&owner, &[])
        .expect("mixed call-site width reference execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "7\n8\n");

    let bytecode = compile_mir_program(&mir).expect("mixed call-site width bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("mixed call-site width VM"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "7\n8\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "shared_width_ffi");
    generator
        .compile_mir_native(&mir)
        .expect("mixed call-site width native compile");
    generator
        .module
        .verify()
        .expect("mixed call-site width native verify");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("mixed call-site width native execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "7\n8\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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
fn scalar_ffi_non_c_abi_stays_outside_canonical_route() {
    let source = r#"
        extern "Rust" { func foreign(value: i64) -> i64; }
        func main() -> i64 { foreign(42 as i64) }
    "#;
    // Keep this admission-only fixture free of the production prelude: the
    // unsupported ABI must be the reported boundary, rather than an unrelated
    // compatibility helper blocking MIR construction first.
    let checked =
        crate::core::check_program(&super::parse_prod(source)).expect("non-C ABI scalar fixture");
    let admission = crate::core::mir::classify_canonical_mir_route_admission(&checked);
    assert!(
        !admission.scalar_ffi,
        "only the checker-owned C scalar ABI may cross scalar FFI admission"
    );
    let error = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect_err("non-C ABI must remain an explicit compatibility boundary");
    match error {
        crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility {
            admission: preserved,
            message,
        } => {
            assert!(!preserved.scalar_ffi);
            assert!(
                message.contains("ABI 'Rust' is outside the canonical C ABI"),
                "unexpected non-C ABI boundary: {message}"
            );
        }
        other => panic!("non-C ABI must not become a complete canonical admission: {other:?}"),
    }
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_ffi_checked(&checked)
        .expect("contract-free non-C ABI has no FFI proof obligations");
    assert!(
        results.is_empty(),
        "non-C ABI without contracts: {results:?}"
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_declaration_boundaries_precede_compatibility_materialization() {
    for (source, expected) in [
        (
            r#"
                extern "Rust" { func foreign(value: i64) -> i64; }
                func main() -> i64 { foreign(42 as i64) }
            "#,
            "ABI 'Rust' is outside the canonical C ABI",
        ),
        (
            r#"
                #[errno]
                extern "C" { func foreign(value: i64) -> i64; }
                func main() -> i64 { foreign(42 as i64) }
            "#,
            "block-level errno conversion",
        ),
        (
            r#"
                #[no_panic]
                extern "C" { func foreign(value: i64) -> i64; }
                func main() -> i64 { foreign(42 as i64) }
            "#,
            "no_panic FFI protection",
        ),
        (
            r#"
                extern "C" { #[errno] func foreign(value: i64) -> i64; }
                func main() -> i64 { foreign(42 as i64) }
            "#,
            "errno conversion",
        ),
        (
            r#"
                extern "C" { func foreign(value: i64 ...) -> i64; }
                func main() -> i64 { foreign(42 as i64) }
            "#,
            "variadic ABI",
        ),
        (
            r#"
                extern "C" { func foreign(&value: i64) -> i64; }
                func main() -> i64 { foreign(42 as i64) }
            "#,
            "parameter mode",
        ),
    ] {
        let checked = crate::core::check_program(&super::parse_prod(source))
            .expect("declaration-boundary fixture");
        assert!(!crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
        crate::core::CheckedProgram::reset_test_legacy_body_access();
        let error = crate::core::mir::materialize_canonical_mir_route(&checked, None)
            .expect_err("unsupported declaration semantics must remain compatibility");
        match error {
            crate::core::mir::CanonicalMirRouteMaterializationError::Compatibility {
                message,
                ..
            } => {
                assert!(message.contains(expected), "{expected}: {message}");
                assert!(
                    !message.contains("prelude"),
                    "declaration boundary was obscured by compatibility materialization: {message}"
                );
            }
            other => panic!("unsupported declaration must remain compatibility: {other:?}"),
        }
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    }
}

#[test]
fn scalar_ffi_materialization_rejects_unrepresented_no_panic_semantics() {
    let source = r#"
        #[no_panic]
        extern "C" { func foreign(value: i64) -> i64; }
        func main() -> i64 { foreign(42 as i64) }
    "#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("no_panic declaration fixture");
    let error = MirProgram::from_checked_program(&checked)
        .expect_err("no_panic protection must not be silently dropped from MIR");
    let message = error.to_string();
    assert!(
        message.contains("unsupported no_panic FFI protection semantics"),
        "unexpected no_panic materialization diagnostic: {message}"
    );
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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
