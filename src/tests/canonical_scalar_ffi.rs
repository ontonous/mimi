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
func main() -> i64 { println(9); mir_ffi_absent_symbol(7 as i64); 0 }
"#;
const MISSING_LIBRARY_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_missing_library(int64_t value) { return value + 1; }
"#;
const MISSING_LIBRARY_SOURCE: &str = r#"
extern "C" { func mir_ffi_missing_library(value: i64) -> i64; }
func main() -> i64 { println(13); println(mir_ffi_missing_library(7 as i64)); 0 }
"#;
const REBINDABLE_SYMBOL_A_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_rebindable(int64_t value) { return value + 11; }
"#;
const REBINDABLE_SYMBOL_B_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_rebindable(int64_t value) { return value + 22; }
"#;
const REBINDABLE_SYMBOL_C_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_rebindable(int64_t value) { return value + 33; }
"#;
const REBINDABLE_SYMBOL_D_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_rebindable(int64_t value) { return value + 44; }
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
}

impl Drop for LibraryFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn library_fixture(counter: u64, c_source: &str) -> LibraryFixture {
    let fixture = LibraryFixture {
        dir: std::env::temp_dir().join(format!(
            "mimi-canonical-ffi-{}-{counter}",
            std::process::id()
        )),
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
    let mut guard = super::FfiEnvGuard::lock();
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
    guard.set_path(&fixture.dir.join("missing.so"));
    let error = BytecodeVM::new(bytecode.clone())
        .run_value()
        .expect_err("missing library");
    assert_eq!(error.code(), "E0800");
    assert!(error.to_string().contains("failed to load"), "{error}");

    guard.set_path(&library);
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MIXED_WIDTH_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_CONVERSION_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_CONVERSION_I64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_RANGE_I64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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
    assert_eq!(bytecode_error.code(), "E0802");
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_RANGE_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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
    assert_eq!(bytecode_error.code(), "E0802");
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MULTI_CALL_NONFINITE_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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
    assert_eq!(bytecode_error.code(), "E0802");
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, RESULT_CONVERSION_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MIXED_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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
    assert!(verification.iter().all(|result| {
        result.artifact.as_ref().is_some_and(|artifact| {
            artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
                && artifact.mir_hash == mir.canonical_digest()
        })
    }));
    let verification_again =
        crate::verifier::verify_mir(&mir, "scalar-ffi-mixed-conversion".into())
            .expect("repeat verify mixed argument/result conversion MIR");
    let projection = |results: &[crate::verifier::VerificationResult]| {
        results
            .iter()
            .map(|result| {
                (
                    result.status.clone(),
                    result.message.clone(),
                    result.constraint_count,
                    result.diagnostic.as_ref().map(|diagnostic| diagnostic.span),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        projection(&verification),
        projection(&verification_again),
        "mixed conversion verifier projection must be repeatable"
    );
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, F64_I32_RANGE_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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
    assert_eq!(bytecode_error.code(), "E0802");
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
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MULTI_CALL_REQUIRES_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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
    let ordered = mir.ffi_call_entries_in_source_order();
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    for results in [
        crate::verifier::verify_checked(&checked, "scalar-ffi-multi-call-public".into()),
        crate::verifier::verify_checked_dual(&checked, "scalar-ffi-multi-call-public".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("public multi-call requires verifier");
        assert_eq!(
            results.len(),
            2,
            "each call receipt must produce one result"
        );
        assert_eq!(
            results[0].status,
            crate::verifier::VerifStatus::Proven,
            "the first call must remain proven"
        );
        assert_eq!(
            results[1].status,
            crate::verifier::VerifStatus::Disproven,
            "the second call must remain disproven"
        );
        assert!(results.iter().all(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
                    && artifact.mir_hash == mir.canonical_digest()
            })
        }));
        assert_eq!(
            results[1]
                .diagnostic
                .as_ref()
                .expect("second call diagnostic")
                .span,
            ordered[1].1.span,
            "the failed call must retain its receipt span"
        );
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

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
    let reference_interpreter = MirReferenceInterpreter::new(&mir).with_ffi_resolver(&oracle);
    let reference_error = reference_interpreter
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
    assert_eq!(
        reference_interpreter.captured_output(),
        "107\n",
        "reference must preserve stdout produced before the failed call"
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
fn scalar_ffi_combined_ensures_failure_preserves_prefix_and_receipt_cardinality() {
    use std::cell::RefCell;

    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_prefix_mark(int64_t value) { return value; }
int64_t mir_ffi_combined_bad(int64_t value) { return value + 1; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_prefix_mark(value: i64) -> i64;
    func mir_ffi_combined_bad(value: i64) -> i64 requires: value >= 0 ensures: result == value;
}
func main() -> i64 {
    println(mir_ffi_prefix_mark(4 as i64));
    mir_ffi_combined_bad(7 as i64)
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let tokens = crate::lexer::Lexer::new(SOURCE)
        .tokenize()
        .expect("lex combined ensures prefix fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse combined ensures prefix fixture");
    let checked = crate::core::check_program(&file).expect("check combined ensures prefix fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize combined ensures prefix fixture MIR");
    assert_eq!(
        mir.ffi_calls().len(),
        2,
        "both call-sites must retain receipts"
    );
    let combined_receipt = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .find(|(_, receipt)| receipt.symbol == "mir_ffi_combined_bad")
        .map(|(_, receipt)| receipt)
        .expect("combined contract receipt");

    struct PrefixOracle(RefCell<Vec<String>>);
    impl MirReferenceFfiResolver for PrefixOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("combined prefix arguments {arguments:?}"));
            };
            self.0.borrow_mut().push(receipt.symbol.clone());
            match receipt.symbol.as_str() {
                "mir_ffi_prefix_mark" => Ok(MirRuntimeValue::Int(*value)),
                "mir_ffi_combined_bad" => Ok(MirRuntimeValue::Int(*value + 1)),
                symbol => Err(format!("unexpected combined prefix symbol {symbol}")),
            }
        }
    }

    let oracle = PrefixOracle(RefCell::new(Vec::new()));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject the combined postcondition");
    assert!(
        reference.message.contains("FFI postcondition failed"),
        "{reference}"
    );
    assert_eq!(
        oracle.0.borrow().as_slice(),
        ["mir_ffi_prefix_mark", "mir_ffi_combined_bad"],
        "reference must preserve prefix call order before the postcondition failure"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-combined-prefix".into())
        .expect("verify combined ensures prefix MIR");
    assert_eq!(
        verification.len(),
        1,
        "only the contract-bearing receipt contributes a verifier result"
    );
    assert_eq!(
        verification[0].status,
        crate::verifier::VerifStatus::Disproven
    );
    assert_eq!(verification[0].func_name, "function:main");
    assert!(verification[0]
        .message
        .contains("extern ensures contract disproven"));
    assert_eq!(
        verification[0]
            .diagnostic
            .as_ref()
            .expect("combined ensures diagnostic")
            .span,
        combined_receipt.span
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("combined ensures prefix bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let mut vm = BytecodeVM::new(bytecode);
    let bytecode_error = vm
        .run_value()
        .expect_err("bytecode must reject the combined postcondition");
    assert_eq!(bytecode_error.code(), "E0808");
    assert!(bytecode_error
        .to_string()
        .contains("FFI postcondition failed"));
    assert_eq!(
        vm.stdout(),
        "4\n",
        "bytecode must preserve the prefix output"
    );

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_combined_prefix");
    generator
        .compile_mir_native(&mir)
        .expect("native combined ensures prefix lowering");
    generator
        .module
        .verify()
        .expect("valid native combined ensures prefix module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native combined ensures prefix execution");
    assert_ne!(native.exit_code, Some(0));
    assert_eq!(
        native.stdout, "4\n",
        "native must preserve the prefix output"
    );
    assert!(
        native.stderr.contains("E0808") && native.stderr.contains("FFI postcondition failed"),
        "{}",
        native.stderr
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_combined_requires_failure_short_circuits_ensures_and_preserves_prefix() {
    use std::cell::RefCell;

    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_prefix_short_circuit(int64_t value) { return value; }
int64_t mir_ffi_combined_short_circuit(int64_t value) { return value + 1; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_prefix_short_circuit(value: i64) -> i64;
    func mir_ffi_combined_short_circuit(value: i64) -> i64 requires: value >= 0 ensures: result == value;
}
func main() -> i64 {
    println(mir_ffi_prefix_short_circuit(4 as i64));
    mir_ffi_combined_short_circuit(-7 as i64)
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let tokens = crate::lexer::Lexer::new(SOURCE)
        .tokenize()
        .expect("lex combined requires short-circuit fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse combined requires short-circuit fixture");
    let checked =
        crate::core::check_program(&file).expect("check combined requires short-circuit fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize combined requires short-circuit fixture MIR");
    assert_eq!(
        mir.ffi_calls().len(),
        2,
        "both call-sites must retain receipts"
    );
    let combined_receipt = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .find(|(_, receipt)| receipt.symbol == "mir_ffi_combined_short_circuit")
        .map(|(_, receipt)| receipt)
        .expect("combined requires receipt");

    struct PrefixOracle(RefCell<Vec<String>>);
    impl MirReferenceFfiResolver for PrefixOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("combined short-circuit arguments {arguments:?}"));
            };
            self.0.borrow_mut().push(receipt.symbol.clone());
            match receipt.symbol.as_str() {
                "mir_ffi_prefix_short_circuit" => Ok(MirRuntimeValue::Int(*value)),
                "mir_ffi_combined_short_circuit" => Ok(MirRuntimeValue::Int(*value + 1)),
                symbol => Err(format!("unexpected combined short-circuit symbol {symbol}")),
            }
        }
    }

    let oracle = PrefixOracle(RefCell::new(Vec::new()));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject the combined precondition before the host call");
    assert!(
        reference.message.contains("FFI precondition failed"),
        "{reference}"
    );
    assert_eq!(
        oracle.0.borrow().as_slice(),
        ["mir_ffi_prefix_short_circuit"],
        "reference must not call the combined host after a failed requires"
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let verification = crate::verifier::verify_mir(&mir, "scalar-ffi-combined-requires".into())
        .expect("verify combined requires short-circuit MIR");
    assert_eq!(
        verification.len(),
        1,
        "only the contract-bearing receipt contributes a verifier result"
    );
    assert_eq!(
        verification[0].status,
        crate::verifier::VerifStatus::Disproven
    );
    assert!(verification[0]
        .message
        .contains("extern requires contract disproven"));
    assert!(!verification[0]
        .message
        .contains("extern ensures contract disproven"));
    assert_eq!(
        verification[0].constraint_count, 3,
        "failed requires must retain the canonical path/definedness summary while short-circuiting ensures"
    );
    assert_eq!(
        verification[0]
            .diagnostic
            .as_ref()
            .expect("combined requires diagnostic")
            .span,
        combined_receipt.span
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let bytecode = compile_mir_program(&mir).expect("combined requires short-circuit bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let mut vm = BytecodeVM::new(bytecode);
    let bytecode_error = vm
        .run_value()
        .expect_err("bytecode must reject the combined precondition");
    assert_eq!(bytecode_error.code(), "E0808");
    assert!(bytecode_error
        .to_string()
        .contains("FFI precondition failed"));
    assert_eq!(
        vm.stdout(),
        "4\n",
        "bytecode must preserve the prefix output"
    );

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "scalar_ffi_combined_requires_short_circuit");
    generator
        .compile_mir_native(&mir)
        .expect("native combined requires short-circuit lowering");
    generator
        .module
        .verify()
        .expect("valid native combined requires short-circuit module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native combined requires short-circuit execution");
    assert_ne!(native.exit_code, Some(0));
    assert_eq!(
        native.stdout, "4\n",
        "native must preserve the prefix output"
    );
    assert!(
        native.stderr.contains("E0808") && native.stderr.contains("FFI precondition failed"),
        "{}",
        native.stderr
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_multi_callsite_public_verifiers_preserve_order_and_receipt_digest() {
    if !crate::verifier::is_z3_available() {
        eprintln!("SKIP: Z3 unavailable");
        return;
    }
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_order_first(value: i64) -> i64 requires: value >= 0 ensures: true;
    func mir_ffi_order_second(value: i64) -> i64 requires: value >= 0 ensures: result == value;
}
func main() -> i64 {
    mir_ffi_order_first(7 as i64);
    mir_ffi_order_second(-8 as i64);
    0
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("check public multi-callsite FFI verifier fixture");
    assert!(crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi);
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize public multi-callsite FFI verifier fixture");
    let ordered = mir.ffi_call_entries_in_source_order();
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].1.symbol, "mir_ffi_order_first");
    assert_eq!(ordered[1].1.symbol, "mir_ffi_order_second");
    assert!(ordered[0].1.span.start_line < ordered[1].1.span.start_line);
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("materialize shared public verifier route");
    let receipt = route.program.route_receipt("r6-457-public-verifiers");
    assert_eq!(route.program.canonical_digest(), mir.canonical_digest());
    assert_eq!(receipt.mir_digest, mir.canonical_digest());
    assert_eq!(receipt.ffi_digest.len(), 64);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let api_results = [
        crate::verifier::verify_checked(&checked, "r6-457-public-verifiers".into()),
        crate::verifier::verify_checked_dual(&checked, "r6-457-public-verifiers".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ];
    let mut projections = Vec::new();
    for results in api_results {
        let results = results.expect("public multi-callsite FFI verifier");
        assert_eq!(results.len(), 2, "each receipt must produce one result");
        assert_eq!(
            results[0].status,
            crate::verifier::VerifStatus::Proven,
            "the first combined receipt must remain the first proven result"
        );
        assert_eq!(
            results[1].status,
            crate::verifier::VerifStatus::Disproven,
            "the second requires failure must remain the second result"
        );
        assert!(results.iter().all(|result| {
            result
                .func_name
                .strip_prefix("function:")
                .unwrap_or(result.func_name.as_str())
                == "main"
                && result.artifact.as_ref().is_some_and(|artifact| {
                    artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
                        && artifact.mir_hash == receipt.mir_digest
                })
        }));
        assert_eq!(
            results[1]
                .diagnostic
                .as_ref()
                .expect("second requires diagnostic")
                .span,
            ordered[1].1.span,
            "the disproven result must retain the second receipt span"
        );
        projections.push(
            results
                .iter()
                .map(|result| {
                    (
                        result.status.clone(),
                        result.message.clone(),
                        result.constraint_count,
                    )
                })
                .collect::<Vec<_>>(),
        );
    }
    assert!(
        projections.windows(2).all(|pair| pair[0] == pair[1]),
        "public verifier APIs must expose one ordered semantic result projection"
    );
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_repeated_public_verifiers_pin_three_receipts_to_one_mir_digest() {
    if !crate::verifier::is_z3_available() {
        eprintln!("SKIP: Z3 unavailable");
        return;
    }
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_repeat_first(value: i64) -> i64 requires: value >= 0 ensures: true;
    func mir_ffi_repeat_second(value: i64) -> i64 requires: value >= 0;
    func mir_ffi_repeat_third(value: i64) -> i64 requires: value >= 0;
}
func main() -> i64 {
    mir_ffi_repeat_first(7 as i64);
    mir_ffi_repeat_second(-8 as i64);
    mir_ffi_repeat_third(-9 as i64);
    0
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("check repeated public verifier fixture");
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("materialize repeated public verifier route");
    let receipt = route
        .program
        .route_receipt("r6-466-repeat-public-verifiers");
    let ordered = route.program.ffi_call_entries_in_source_order();
    assert_eq!(ordered.len(), 3);

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let first = crate::verifier::verify_checked(&checked, "r6-466-repeat-public-verifiers".into())
        .expect("first repeated public verifier run");
    let second = crate::verifier::verify_checked(&checked, "r6-466-repeat-public-verifiers".into())
        .expect("second repeated public verifier run");
    let dual =
        crate::verifier::verify_checked_dual(&checked, "r6-466-repeat-public-verifiers".into())
            .expect("dual repeated public verifier run");
    for (label, results) in [("first", &first), ("second", &second), ("dual", &dual)] {
        assert_eq!(
            results.len(),
            3,
            "{label} must retain one result per receipt"
        );
        assert_eq!(
            results
                .iter()
                .map(|result| result.status.clone())
                .collect::<Vec<_>>(),
            vec![
                crate::verifier::VerifStatus::Proven,
                crate::verifier::VerifStatus::Disproven,
                crate::verifier::VerifStatus::Disproven,
            ],
            "{label} changed receipt verdict order"
        );
        assert!(results.iter().all(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
                    && artifact.mir_hash == receipt.mir_digest
            })
        }));
        for (index, result) in results.iter().enumerate().skip(1) {
            assert_eq!(
                result
                    .diagnostic
                    .as_ref()
                    .expect("failed receipt diagnostic")
                    .span,
                ordered[index].1.span,
                "{label} changed receipt {index} diagnostic span"
            );
        }
    }
    let projection = |results: &[crate::verifier::VerificationResult]| {
        results
            .iter()
            .map(|result| {
                (
                    result.status.clone(),
                    result.message.clone(),
                    result.constraint_count,
                    result.diagnostic.as_ref().map(|diagnostic| diagnostic.span),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(projection(&first), projection(&second));
    assert_eq!(projection(&first), projection(&dual));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_public_verifier_artifacts_bind_source_hash_by_entrypoint() {
    if !crate::verifier::is_z3_available() {
        eprintln!("SKIP: Z3 unavailable");
        return;
    }
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_source_hash_first(value: i64) -> i64 requires: value >= 0 ensures: true;
    func mir_ffi_source_hash_second(value: i64) -> i64 requires: value >= 0 ensures: result == value;
}
func main() -> i64 {
    mir_ffi_source_hash_first(7 as i64);
    mir_ffi_source_hash_second(-8 as i64);
    0
}
"#;
    let source_hash = blake3::hash(SOURCE.as_bytes()).to_hex().to_string();
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("check source-hash FFI verifier fixture");

    let assert_results = |label: &str, results: &[crate::verifier::VerificationResult]| {
        assert_eq!(results.len(), 2, "{label} must retain both FFI receipts");
        assert_eq!(results[0].status, crate::verifier::VerifStatus::Proven);
        assert_eq!(results[1].status, crate::verifier::VerifStatus::Disproven);
        assert!(results.iter().all(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
                    && artifact.mir_hash.len() == 64
            })
        }));
    };

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let checked_results = crate::verifier::verify_checked(&checked, source_hash.clone())
        .expect("checked source-hash FFI verifier");
    assert_results("verify_checked", &checked_results);
    assert!(checked_results.iter().all(|result| {
        result
            .artifact
            .as_ref()
            .is_some_and(|artifact| artifact.source_hash == source_hash)
    }));

    let dual_results = crate::verifier::verify_checked_dual(&checked, source_hash.clone())
        .expect("dual source-hash FFI verifier");
    assert_results("verify_checked_dual", &dual_results);
    assert!(dual_results.iter().all(|result| {
        result
            .artifact
            .as_ref()
            .is_some_and(|artifact| artifact.source_hash == source_hash)
    }));

    let ffi_results = crate::verifier::verify_ffi_checked(&checked)
        .expect("FFI-only source-hash boundary verifier");
    assert_results("verify_ffi_checked", &ffi_results);
    assert!(ffi_results.iter().all(|result| {
        result
            .artifact
            .as_ref()
            .is_some_and(|artifact| artifact.source_hash.is_empty())
    }));

    let source_results =
        crate::verifier::verify_ffi_source(SOURCE).expect("source entrypoint FFI verifier");
    assert_results("verify_ffi_source", &source_results);
    assert!(source_results.iter().all(|result| {
        result
            .artifact
            .as_ref()
            .is_some_and(|artifact| artifact.source_hash == source_hash)
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_branch_merge_preserves_per_callsite_result_cardinality() {
    let source = r#"
extern "C" { func mir_ffi_branch(value: i64) -> i64 requires: value >= 0; }
func branch(flag: bool) -> i64 {
    if flag {
        mir_ffi_branch(7 as i64)
    } else {
        mir_ffi_branch(-7 as i64)
    }
}
func main() -> i64 {
    branch(true)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex branch-merge FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse branch-merge FFI fixture");
    let checked = crate::core::check_program(&file).expect("check branch-merge FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize branch-merge FFI fixture MIR");
    let receipts = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .map(|(_, receipt)| receipt)
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    assert_eq!(
        receipts[0].caller,
        crate::core::NodeId("function:branch".into())
    );
    assert_eq!(
        receipts[1].caller,
        crate::core::NodeId("function:branch".into())
    );
    assert!(receipts[0].span.start_line < receipts[1].span.start_line);

    let results = crate::verifier::verify_mir(&mir, "branch-merge-ffi".into())
        .expect("verify branch-merge FFI fixture");
    assert_eq!(results.len(), receipts.len());
    assert_eq!(
        results
            .iter()
            .map(|result| result.status.clone())
            .collect::<Vec<_>>(),
        vec![
            crate::verifier::VerifStatus::Proven,
            crate::verifier::VerifStatus::Disproven,
        ],
        "each mutually exclusive branch call-site must retain one proof result"
    );
    assert!(results.iter().all(|result| {
        result.func_name == "function:branch"
            && result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
                    && artifact.mir_hash == mir.canonical_digest()
            })
    }));
    assert_eq!(
        results[1]
            .diagnostic
            .as_ref()
            .expect("disproven branch call diagnostic")
            .span,
        receipts[1].span
    );
}

#[test]
fn scalar_ffi_joined_paths_share_one_instruction_result_and_summary() {
    let source = r#"
extern "C" { func mir_ffi_join(value: i64) -> i64 requires: value >= 0; }
func join(flag: bool) -> i64 {
    let value = if flag { 7 as i64 } else { 8 as i64 }
    mir_ffi_join(value)
}
func main() -> i64 {
    join(true)
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex joined-path FFI fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse joined-path FFI fixture");
    let checked = crate::core::check_program(&file).expect("check joined-path FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize joined-path FFI fixture MIR");
    let receipts = mir.ffi_call_entries_in_source_order();
    assert_eq!(
        receipts.len(),
        1,
        "the join has one canonical call-site receipt"
    );

    let first = crate::verifier::verify_mir(&mir, "joined-path-ffi".into())
        .expect("verify joined-path FFI fixture");
    let second = crate::verifier::verify_mir(&mir, "joined-path-ffi-repeat".into())
        .expect("repeat verify joined-path FFI fixture");
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].status, crate::verifier::VerifStatus::Proven);
    assert_eq!(second[0].status, crate::verifier::VerifStatus::Proven);
    assert_eq!(
        first[0].constraint_count, second[0].constraint_count,
        "joined-path proof summary must be stable across repeated verification"
    );
    assert_eq!(
        first[0].constraint_count, 4,
        "one joined receipt must aggregate exactly one path condition plus the call condition per branch"
    );
    assert_eq!(first[0].func_name, "function:join");
    assert!(first[0].artifact.as_ref().is_some_and(|artifact| {
        artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
            && artifact.mir_hash == mir.canonical_digest()
    }));
}

#[test]
fn scalar_ffi_missing_symbol_is_rejected_at_each_host_boundary() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MISSING_SYMBOL_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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

    let reference_interpreter = MirReferenceInterpreter::new(&mir);
    let reference_error = reference_interpreter
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference execution must require an explicit host binding");
    assert!(reference_error
        .to_string()
        .contains("no reference FFI host binding"));
    assert_eq!(reference_interpreter.captured_output(), "9\n");

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
    assert_eq!(bytecode_error.code(), "E0800");
    assert!(bytecode_error
        .to_string()
        .contains("failed to find canonical MIR FFI symbol"));
    assert_eq!(vm.stdout(), "9\n");

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
fn scalar_ffi_missing_library_preserves_prefix_and_recovers() {
    struct MissingLibraryOracle;
    impl MirReferenceFfiResolver for MissingLibraryOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            args: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_missing_library" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = args else {
                return Err("missing-library oracle expects one i64".into());
            };
            Ok(MirRuntimeValue::Int(value + 1))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MISSING_LIBRARY_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    let missing = fixture.dir.join("missing.so");
    guard.set_path(&library);

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(MISSING_LIBRARY_SOURCE)
            .tokenize()
            .expect("lex missing-library FFI fixture"),
    )
    .parse_file()
    .expect("parse missing-library FFI fixture");
    let checked = crate::core::check_program(&file).expect("check missing-library FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize missing-library FFI fixture MIR");

    let reference_interpreter =
        MirReferenceInterpreter::new(&mir).with_ffi_resolver(&MissingLibraryOracle);
    let reference = reference_interpreter
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference host binding for missing-library fixture");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "13\n8\n");

    let bytecode = compile_mir_program(&mir).expect("missing-library AST-free bytecode");
    guard.set_path(&missing);
    let mut missing_vm = BytecodeVM::new(bytecode.clone());
    let missing_error = missing_vm
        .run_value()
        .expect_err("bytecode must fail closed when its library path is absent");
    assert_eq!(missing_error.code(), "E0800");
    assert!(
        missing_error.to_string().contains("failed to load"),
        "{missing_error}"
    );
    assert_eq!(missing_vm.stdout(), "13\n");

    guard.set_path(&library);
    let mut recovered_vm = BytecodeVM::new(bytecode);
    assert_eq!(
        recovered_vm
            .run_value()
            .expect("a later valid library path must recover the bytecode consumer"),
        Value::Int(0)
    );
    assert_eq!(recovered_vm.stdout(), "13\n8\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_missing_lib");
    generator
        .compile_mir_native(&mir)
        .expect("native missing-library fixture lowering");
    generator
        .module
        .verify()
        .expect("valid missing-library fixture native module");
    let config = super::E2EConfig {
        extra_c_src: Some(MISSING_LIBRARY_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native fixture remains linkable with its C definition");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "13\n8\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_failed_vm_run_is_reusable_after_stdout_snapshot() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MISSING_LIBRARY_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    let missing = fixture.dir.join("missing.so");

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(MISSING_LIBRARY_SOURCE)
            .tokenize()
            .expect("lex reusable-VM FFI fixture"),
    )
    .parse_file()
    .expect("parse reusable-VM FFI fixture");
    let checked = crate::core::check_program(&file).expect("check reusable-VM FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize reusable-VM FFI fixture MIR");
    let bytecode = compile_mir_program(&mir).expect("reusable-VM AST-free bytecode");

    guard.set_path(&missing);
    let mut vm = BytecodeVM::new(bytecode);
    let error = vm
        .run_value()
        .expect_err("the first run must fail while loading the absent library");
    assert_eq!(error.code(), "E0800");
    assert_eq!(vm.stdout(), "13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.take_stdout(), "13\n");
    assert_eq!(vm.stdout(), "");

    guard.set_path(&library);
    assert_eq!(
        vm.run_value()
            .expect("the same VM must recover after the caller consumes its failure output"),
        Value::Int(0)
    );
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.stdout(), "13\n8\n");
}

#[test]
fn scalar_ffi_runtime_rebinds_same_symbol_by_library_path() {
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_library = first.dir.join("ffi.so");
    let second_library = second.dir.join("ffi.so");
    let mut guard = super::FfiEnvGuard::lock();
    guard.set_path(&first_library);
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

    guard.set_path(&first_library);
    let mut runtime = crate::interp::bytecode::mir_ffi::CanonicalMirFfiRuntime::new();
    assert_eq!(
        runtime
            .call(&descriptor, &[Value::Int(1)])
            .expect("first library binding"),
        Value::Int(12)
    );
    assert_eq!(runtime.loaded_library_count_for_test(), 1);

    guard.set_path(&second_library);
    assert_eq!(
        runtime
            .call(&descriptor, &[Value::Int(1)])
            .expect("second library binding"),
        Value::Int(23)
    );
    assert_eq!(runtime.loaded_library_count_for_test(), 2);

    guard.set_path(&first_library);
    assert_eq!(
        runtime
            .call(&descriptor, &[Value::Int(1)])
            .expect("cached first library binding"),
        Value::Int(12)
    );
    assert_eq!(runtime.loaded_library_count_for_test(), 2);
}

#[test]
fn scalar_ffi_runtime_missing_symbol_does_not_poison_cached_libraries() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let valid = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let missing = library_fixture(counter + 1, MISSING_SYMBOL_C_SOURCE);
    let valid_library = valid.dir.join("ffi.so");
    let missing_library = missing.dir.join("ffi.so");
    let i64_abi = crate::core::mir::types::MirAbiClass::Integer {
        bits: 64,
        signed: true,
    };
    let descriptor = CanonicalFfiDescriptor {
        caller: "function:main".into(),
        instruction: "ffi-test-missing-symbol-recovery".into(),
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
        argument_ids: vec![crate::core::mir::MirValueId::new("ffi-test-missing-arg").unwrap()],
        requires: None,
        result_id: Some(crate::core::mir::MirValueId::new("ffi-test-missing-result").unwrap()),
        ensures: None,
    };

    let mut runtime = crate::interp::bytecode::mir_ffi::CanonicalMirFfiRuntime::new();
    guard.set_path(&valid_library);
    assert_eq!(
        runtime
            .call(&descriptor, &[Value::Int(1)])
            .expect("initial valid library binding"),
        Value::Int(12)
    );
    assert_eq!(runtime.loaded_library_count_for_test(), 1);

    guard.set_path(&missing_library);
    let error = runtime
        .call(&descriptor, &[Value::Int(1)])
        .expect_err("a loaded library without the canonical symbol must fail closed");
    assert_eq!(error.code(), "E0800");
    assert!(error.to_string().contains("mir_ffi_rebindable"));
    assert_eq!(
        runtime.loaded_library_count_for_test(),
        2,
        "the missing-symbol library may be cached, but its failed lookup must not corrupt the cache"
    );

    guard.set_path(&valid_library);
    assert_eq!(
        runtime
            .call(&descriptor, &[Value::Int(1)])
            .expect("valid cached library must recover after missing-symbol failure"),
        Value::Int(12)
    );
    assert_eq!(runtime.loaded_library_count_for_test(), 2);
}

#[test]
fn scalar_ffi_vm_missing_symbol_recovers_same_vm_and_isolates_compatibility() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(1)
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;
    struct Oracle;
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_rebindable" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("unexpected arguments {arguments:?}"));
            };
            Ok(MirRuntimeValue::Int(value + 11))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let valid = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let missing = library_fixture(counter + 1, MISSING_SYMBOL_C_SOURCE);
    let valid_library = valid.dir.join("ffi.so");
    let missing_library = missing.dir.join("ffi.so");

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(SOURCE)
            .tokenize()
            .expect("lex VM missing-symbol recovery fixture"),
    )
    .parse_file()
    .expect("parse VM missing-symbol recovery fixture");
    let checked = crate::core::check_program(&file).expect("check VM missing-symbol fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize VM missing-symbol recovery MIR");
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&Oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference VM recovery fixture");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "1\n12\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free VM recovery bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    guard.set_path(&missing_library);
    let missing_error = vm
        .run_value()
        .expect_err("missing-symbol VM call must fail closed after the prefix print");
    assert_eq!(missing_error.code(), "E0800");
    assert!(missing_error
        .to_string()
        .contains("failed to find canonical MIR FFI symbol"));
    assert_eq!(vm.stdout(), "1\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        1,
        "the missing-symbol library is cached without leaving a frame or poisoning the VM"
    );

    let compat_source = r#"func main() -> i64 { println(2); 0 }"#;
    let compat_file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(compat_source)
            .tokenize()
            .expect("lex compatibility isolation fixture"),
    )
    .parse_file()
    .expect("parse compatibility isolation fixture");
    let compat_checked =
        crate::core::check_program(&compat_file).expect("check compatibility isolation fixture");
    let compat_mir = MirProgram::from_checked_program(&compat_checked)
        .expect("materialize compatibility isolation MIR");
    let compat_bytecode =
        compile_mir_program(&compat_mir).expect("compile compatibility isolation bytecode");
    assert!(compat_bytecode.ast.is_none());
    assert!(compat_bytecode.canonical_ffi.is_empty());
    let mut compat_vm = BytecodeVM::new(compat_bytecode);
    assert_eq!(
        compat_vm
            .run_value()
            .expect("compatibility VM must remain usable"),
        Value::Int(0)
    );
    assert_eq!(compat_vm.stdout(), "2\n");
    assert_eq!(compat_vm.debug_stack_state(), (0, 0));

    guard.set_path(&valid_library);
    assert_eq!(
        vm.run_value()
            .expect("the same canonical VM must recover after missing-symbol failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "1\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        2,
        "recovery binds the valid library as a second isolated cache entry"
    );

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_vm_recovery");
    generator
        .compile_mir_native(&mir)
        .expect("native VM recovery fixture lowering");
    generator
        .module
        .verify()
        .expect("valid native VM recovery module");
    let config = super::E2EConfig {
        extra_c_src: Some(REBINDABLE_SYMBOL_A_C_SOURCE.into()),
        ..Default::default()
    };
    let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native = super::link_and_observe_module(&generator, &config, native_counter)
        .expect("native VM recovery fixture remains linkable");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "1\n12\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_vm_rebind_preserves_two_call_site_identities() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    println(mir_ffi_rebindable(2 as i64))
    0
}
"#;
    struct RebindOracle;
    impl MirReferenceFfiResolver for RebindOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_rebindable" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("unexpected arguments {arguments:?}"));
            };
            Ok(MirRuntimeValue::Int(value + 11))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_library = first.dir.join("ffi.so");
    let second_library = second.dir.join("ffi.so");
    guard.set_path(&first_library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("two-call-site rebind FFI fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize two-call-site rebind MIR");
    let receipts = mir
        .ffi_call_entries_in_source_order()
        .into_iter()
        .map(|(_, receipt)| {
            (
                receipt.caller.0.clone(),
                receipt.instruction.as_str().to_owned(),
                receipt.callee.0.clone(),
                receipt.symbol.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    assert_ne!(receipts[0].1, receipts[1].1);

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&RebindOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("two-call-site reference execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "12\n13\n");

    let bytecode = compile_mir_program(&mir).expect("two-call-site rebind bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("first library two-call-site VM run"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    guard.set_path(&second_library);
    assert_eq!(
        vm.run_value()
            .expect("same VM must rebind both canonical call sites"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "23\n24\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(
        vm.program()
            .canonical_ffi
            .iter()
            .map(|descriptor| {
                (
                    descriptor.caller.clone(),
                    descriptor.instruction.clone(),
                    descriptor.callee.clone(),
                    descriptor.symbol.clone(),
                )
            })
            .collect::<Vec<_>>(),
        receipts
    );

    guard.set_path(&first_library);
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_rebind_sites");
    generator
        .compile_mir_native(&mir)
        .expect("native two-call-site rebind lowering");
    generator
        .module
        .verify()
        .expect("valid native two-call-site rebind module");
    let config = super::E2EConfig {
        extra_c_src: Some(REBINDABLE_SYMBOL_A_C_SOURCE.into()),
        ..Default::default()
    };
    let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native = super::link_and_observe_module(&generator, &config, native_counter)
        .expect("native two-call-site rebind execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "12\n13\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_vm_multi_call_site_failure_rebinds_without_partial_output() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(0)
    println(mir_ffi_rebindable(1 as i64))
    println(mir_ffi_rebindable(2 as i64))
    0
}
"#;
    struct RebindOracle;
    impl MirReferenceFfiResolver for RebindOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_rebindable" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("unexpected arguments {arguments:?}"));
            };
            Ok(MirRuntimeValue::Int(value + 11))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let valid_a = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let valid_b = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let missing = library_fixture(counter + 2, MISSING_SYMBOL_C_SOURCE);
    let valid_a_library = valid_a.dir.join("ffi.so");
    let valid_b_library = valid_b.dir.join("ffi.so");
    let missing_library = missing.dir.join("ffi.so");

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("multi-call-site failure/rebind FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize multi-call-site failure/rebind MIR");
    assert_eq!(mir.ffi_calls().len(), 2);
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&RebindOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("multi-call-site reference execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "0\n12\n13\n");

    let bytecode = compile_mir_program(&mir).expect("multi-call-site failure/rebind bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    guard.set_path(&missing_library);
    let missing_error = vm
        .run_value()
        .expect_err("first missing-symbol call must fail before the second call site");
    assert_eq!(missing_error.code(), "E0800");
    assert!(missing_error
        .to_string()
        .contains("failed to find canonical MIR FFI symbol"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    guard.set_path(&valid_b_library);
    assert_eq!(
        vm.run_value()
            .expect("same VM must recover both call sites against library B"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n23\n24\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    guard.set_path(&valid_a_library);
    assert_eq!(
        vm.run_value()
            .expect("same VM must rebind both call sites back to library A"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 3);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    guard.set_path(&valid_a_library);
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_multi_rebind");
    generator
        .compile_mir_native(&mir)
        .expect("native multi-call-site failure/rebind lowering");
    generator
        .module
        .verify()
        .expect("valid native multi-call-site failure/rebind module");
    let config = super::E2EConfig {
        extra_c_src: Some(REBINDABLE_SYMBOL_A_C_SOURCE.into()),
        ..Default::default()
    };
    let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native = super::link_and_observe_module(&generator, &config, native_counter)
        .expect("native multi-call-site failure/rebind execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "0\n12\n13\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_multi_call_site_direct_and_wrapped_entries_share_failure_snapshot() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(0)
    println(mir_ffi_rebindable(1 as i64))
    println(mir_ffi_rebindable(2 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let valid = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let missing = library_fixture(counter + 1, MISSING_SYMBOL_C_SOURCE);
    let valid_library = valid.dir.join("ffi.so");
    let missing_library = missing.dir.join("ffi.so");

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("direct/wrapped multi-call-site FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize direct/wrapped multi-call-site MIR");
    let bytecode = compile_mir_program(&mir).expect("direct/wrapped multi-call-site bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let mut vm = BytecodeVM::new(bytecode);

    guard.set_path(&missing_library);
    let direct_error = vm
        .call_named("function:main", Vec::new())
        .expect_err("direct entry must fail at the first missing-symbol call site");
    assert_eq!(direct_error.code(), "E0800");
    assert!(direct_error
        .to_string()
        .contains("failed to find canonical MIR FFI symbol"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let wrapped_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must share the missing-symbol failure boundary");
    assert_eq!(wrapped_error.code(), "E0800");
    assert_eq!(wrapped_error.to_string(), direct_error.to_string());
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    guard.set_path(&valid_library);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("ordinary direct entry must recover after wrapped failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);
}

#[test]
fn scalar_ffi_multi_call_site_wrapped_postcondition_failure_replaces_snapshot() {
    const C_SOURCE: &str = r#"
#include <stdint.h>
static int call_count;
int64_t mir_ffi_multi_ensures(int64_t value) {
    ++call_count;
    return call_count == 4 ? value + 1 : value;
}
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_multi_ensures(value: i64) -> i64 ensures: result == value; }
func main() -> i64 {
    println(0)
    let first = mir_ffi_multi_ensures(1 as i64)
    println(first)
    let second = mir_ffi_multi_ensures(2 as i64)
    println(second)
    0
}
"#;
    struct Oracle {
        call_count: Cell<i64>,
    }
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_multi_ensures" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("unexpected arguments {arguments:?}"));
            };
            let count = self.call_count.get() + 1;
            self.call_count.set(count);
            Ok(MirRuntimeValue::Int(if count == 4 {
                value + 1
            } else {
                *value
            }))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("multi-call-site wrapped ensures fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize multi-call-site wrapped ensures MIR");
    assert_eq!(mir.ffi_calls().len(), 2);

    let oracle = Oracle {
        call_count: Cell::new(0),
    };
    let reference = MirReferenceInterpreter::new(&mir).with_ffi_resolver(&oracle);
    let first_reference = reference
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference wrapped ensures first run");
    assert_eq!(first_reference.value, MirRuntimeValue::Int(0));
    assert_eq!(first_reference.output, "0\n1\n2\n");
    let reference_error = reference
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference wrapped ensures second run must fail at the second site");
    assert!(reference_error.to_string().contains("postcondition"));
    assert_eq!(reference.captured_output(), "0\n1\n");
    let recovered_reference = reference
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference wrapped ensures third run must recover");
    assert_eq!(recovered_reference.output, "0\n1\n2\n");

    let bytecode = compile_mir_program(&mir).expect("multi-call-site wrapped ensures bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let mut vm = BytecodeVM::new(bytecode);
    vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect("bytecode wrapped ensures first run");
    assert_eq!(vm.stdout(), "0\n1\n2\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let wrapped_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("bytecode wrapped ensures second run must fail at the second site");
    assert_eq!(wrapped_error.code(), "E0808");
    assert!(wrapped_error.to_string().contains("postcondition"));
    assert_eq!(vm.stdout(), "0\n1\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    assert_eq!(
        vm.call_function(vm.program().entry, &[])
            .expect("bytecode ordinary entry must recover after wrapped failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n1\n2\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_multi_wrapped_ensures");
    generator
        .compile_mir_native(&mir)
        .expect("native multi-call-site wrapped ensures lowering");
    generator
        .module
        .verify()
        .expect("valid native multi-call-site wrapped ensures module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native = super::link_and_observe_module(&generator, &config, native_counter)
        .expect("native multi-call-site wrapped ensures execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "0\n1\n2\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_cached_vm_rejects_forged_descriptor_before_wrapped_reuse() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    println(mir_ffi_rebindable(2 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("cached forged-descriptor FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize cached forged-descriptor MIR");
    let bytecode = compile_mir_program(&mir).expect("cached forged-descriptor bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.call_function(vm.program().entry, &[])
            .expect("initial cached canonical FFI call"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let original = vm.program().canonical_ffi[1].clone();
    let mut forged = original.clone();
    forged.symbol = "forged_after_cache_symbol".into();
    vm.replace_canonical_ffi_descriptor_for_test_only(1, forged);

    let first = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must reject a forged cached descriptor before reuse");
    assert!(first
        .to_string()
        .contains("differs from its compiler binding"));
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        1,
        "descriptor preflight must run before reusing the cached library"
    );

    let second = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("repeated wrapped cached-descriptor rejection must be stable");
    assert_eq!(second.to_string(), first.to_string());
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.replace_canonical_ffi_descriptor_for_test_only(1, original);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("direct entry must recover after cached descriptor restoration"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
}

#[test]
fn scalar_ffi_cached_vm_rejects_forged_result_conversion_before_reuse() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;
    let forged_conversion = crate::core::mir::MirFfiAbiConversion {
        from: crate::core::mir::types::MirAbiClass::Integer {
            bits: 64,
            signed: true,
        },
        to: crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true,
        },
    };

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("cached forged-result-conversion FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize cached forged-result-conversion MIR");
    let instruction_id = mir
        .ffi_calls()
        .keys()
        .next()
        .cloned()
        .expect("cached forged-result-conversion call-site");

    let mut forged_receipts = mir.ffi_calls().clone();
    forged_receipts
        .get_mut(&instruction_id)
        .expect("cached forged-result-conversion receipt")
        .result_conversion = Some(forged_conversion);
    let mut forged_mir = mir.clone();
    forged_mir.replace_ffi_calls_for_test_only(forged_receipts);

    let reference = MirReferenceInterpreter::new(&forged_mir);
    let reference_error = reference
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged result conversion receipt");
    assert!(
        reference_error
            .to_string()
            .contains("result ABI conversion receipt disagrees"),
        "{reference_error}"
    );
    assert_eq!(
        reference.captured_output(),
        "",
        "receipt rejection must happen before the leading println"
    );

    let bytecode_error = compile_mir_program(&forged_mir)
        .expect_err("bytecode adapter must reject a forged result conversion receipt");
    assert!(bytecode_error.iter().any(|error| {
        error
            .message
            .contains("result ABI conversion receipt disagrees")
            || error.message.contains("conversion receipt")
    }));

    let native_error = crate::codegen::mir::validate_mir_native(&forged_mir)
        .expect_err("native admission must reject a forged result conversion receipt");
    assert!(native_error.iter().any(|error| {
        error
            .message
            .contains("result ABI conversion receipt disagrees")
            || error.message.contains("conversion receipt")
    }));

    let capability_error = crate::verifier::validate_mir_capabilities(&forged_mir)
        .expect_err("capability gate must reject a forged result conversion receipt");
    assert!(capability_error.iter().any(|error| {
        error.contains("result ABI conversion receipt disagrees")
            || error.contains("conversion receipt")
    }));

    let verifier_error =
        crate::verifier::verify_mir(&forged_mir, "forged-result-conversion".into())
            .expect_err("verifier must reject a forged result conversion receipt");
    assert!(
        verifier_error.contains("result ABI conversion receipt disagrees")
            || verifier_error.contains("conversion receipt"),
        "{verifier_error}"
    );

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let bytecode = compile_mir_program(&mir).expect("canonical result-conversion bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("initial canonical FFI run"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let original = vm.program().canonical_ffi[0].clone();
    let mut forged_descriptor = original.clone();
    forged_descriptor.result_conversion = Some(forged_conversion);
    vm.replace_canonical_ffi_descriptor_for_test_only(0, forged_descriptor);
    let descriptor_error = vm
        .run_value()
        .expect_err("cached VM must reject a forged result conversion before reuse");
    assert!(
        descriptor_error
            .to_string()
            .contains("differs from its compiler binding"),
        "{descriptor_error}"
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        1,
        "descriptor preflight must run before reusing the cached library"
    );

    vm.replace_canonical_ffi_descriptor_for_test_only(0, original);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("canonical VM must recover after descriptor restoration"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_result_forge");
    generator
        .compile_mir_native(&mir)
        .expect("native result-conversion lowering");
    generator
        .module
        .verify()
        .expect("valid native result-conversion module");
    let config = super::E2EConfig {
        extra_c_src: Some(REBINDABLE_SYMBOL_A_C_SOURCE.into()),
        ..Default::default()
    };
    let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native = super::link_and_observe_module(&generator, &config, native_counter)
        .expect("native result-conversion execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "12\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_cached_vm_rejects_forged_binding_snapshot_before_reuse() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("cached forged-binding FFI fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize cached forged-binding MIR");
    let bytecode = compile_mir_program(&mir).expect("cached forged-binding bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("initial cached canonical FFI run"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let original = vm.program().canonical_ffi_bindings[0].clone();
    let mut forged = original.clone();
    forged.descriptor.symbol = "forged_binding_snapshot_symbol".into();
    vm.replace_canonical_ffi_binding_for_test_only(0, forged);

    let first = vm
        .run_value()
        .expect_err("run_value must reject a forged binding snapshot before reuse");
    assert!(
        first
            .to_string()
            .contains("canonical FFI descriptor index 0")
            && first
                .to_string()
                .contains("differs from its compiler binding"),
        "{first}"
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let second = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must share forged binding rejection");
    assert_eq!(second.to_string(), first.to_string());
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.replace_canonical_ffi_binding_for_test_only(0, original);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("canonical VM must recover after binding restoration"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
}

#[test]
fn scalar_ffi_cached_vm_rejects_forged_binding_descriptor_index_before_reuse() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    println(mir_ffi_rebindable(2 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("cached forged-index FFI fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize cached forged-index MIR");
    let bytecode = compile_mir_program(&mir).expect("cached forged-index bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("initial cached multi-site run"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let original = vm.program().canonical_ffi_bindings[1].clone();
    let mut forged = original.clone();
    forged.extern_idx = vm.program().canonical_ffi_bindings[0].extern_idx;
    assert_ne!(forged.extern_idx, original.extern_idx);
    vm.replace_canonical_ffi_binding_for_test_only(1, forged);

    let first = vm
        .run_value()
        .expect_err("run_value must reject a forged multi-site descriptor index");
    assert!(
        first
            .to_string()
            .contains("descriptor index 1 disagrees with compiler binding index 0"),
        "{first}"
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let second = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must share forged multi-site index rejection");
    assert_eq!(second.to_string(), first.to_string());
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.replace_canonical_ffi_binding_for_test_only(1, original);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("canonical VM must recover after descriptor-index restoration"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
}

#[test]
fn scalar_ffi_cached_vm_rejects_reused_descriptor_after_op_and_binding_forgery() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    println(mir_ffi_rebindable(2 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("cached reused-descriptor FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize cached reused-descriptor MIR");
    let bytecode = compile_mir_program(&mir).expect("cached reused-descriptor bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("initial cached two-site run"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let original_binding = vm.program().canonical_ffi_bindings[1].clone();
    let first_binding = vm.program().canonical_ffi_bindings[0].clone();
    let first_descriptor = vm.program().canonical_ffi[0].clone();
    let mut forged_binding = original_binding.clone();
    forged_binding.extern_idx = 0;
    forged_binding.instruction = first_binding.instruction;
    forged_binding.instruction_text = first_binding.instruction_text.clone();
    forged_binding.descriptor = first_descriptor;
    vm.replace_canonical_ffi_call_extern_index_for_test_only(
        original_binding.function,
        original_binding.pc,
        0,
    );
    vm.replace_canonical_ffi_call_instruction_for_test_only(
        original_binding.function,
        original_binding.pc,
        first_binding.instruction,
    );
    vm.replace_canonical_ffi_binding_for_test_only(1, forged_binding);

    let first = vm
        .run_value()
        .expect_err("run_value must reject a reused descriptor after coordinated forgery");
    assert!(
        first
            .to_string()
            .contains("descriptor index 0 is referenced by multiple bytecode call sites"),
        "{first}"
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let second = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must reject the same reused descriptor forgery");
    assert_eq!(second.to_string(), first.to_string());
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.replace_canonical_ffi_call_extern_index_for_test_only(
        original_binding.function,
        original_binding.pc,
        original_binding.extern_idx,
    );
    vm.replace_canonical_ffi_call_instruction_for_test_only(
        original_binding.function,
        original_binding.pc,
        original_binding.instruction,
    );
    vm.replace_canonical_ffi_binding_for_test_only(1, original_binding);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("canonical VM must recover after coordinated forgery restoration"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
}

#[test]
fn scalar_ffi_cached_vm_rejects_descriptor_and_binding_table_tail_forgery() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("cached table-tail FFI fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize cached table-tail MIR");
    let bytecode = compile_mir_program(&mir).expect("cached table-tail bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("initial cached table-tail run"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let original_descriptors = vm.program().canonical_ffi.clone();
    let original_bindings = vm.program().canonical_ffi_bindings.clone();

    vm.replace_canonical_ffi_tables_for_test_only(Vec::new(), original_bindings.clone());
    let empty_error = vm
        .run_value()
        .expect_err("empty descriptor table must fail after cache population");
    assert!(
        empty_error
            .to_string()
            .contains("binding manifest is non-empty while descriptor table is empty"),
        "{empty_error}"
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let mut unreferenced_descriptors = original_descriptors.clone();
    unreferenced_descriptors.push(unreferenced_descriptors[0].clone());
    vm.replace_canonical_ffi_tables_for_test_only(
        unreferenced_descriptors,
        original_bindings.clone(),
    );
    let tail_error = vm
        .run_value()
        .expect_err("unreferenced descriptor tail must fail after cache population");
    assert!(
        tail_error
            .to_string()
            .contains("descriptor index 1 is unreferenced by bytecode"),
        "{tail_error}"
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let mut duplicate_bindings = original_bindings.clone();
    duplicate_bindings.push(duplicate_bindings[0].clone());
    vm.replace_canonical_ffi_tables_for_test_only(original_descriptors.clone(), duplicate_bindings);
    let duplicate_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("duplicate binding tail must fail through the wrapped entry");
    assert!(
        duplicate_error
            .to_string()
            .contains("duplicate call-site binding"),
        "{duplicate_error}"
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.replace_canonical_ffi_tables_for_test_only(original_descriptors, original_bindings);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("canonical VM must recover after table-tail restoration"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
}

#[test]
fn scalar_ffi_canonical_vm_caches_are_isolated_across_shared_programs() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_library = first.dir.join("ffi.so");
    let second_library = second.dir.join("ffi.so");
    guard.set_path(&first_library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("shared-program VM cache isolation fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize shared-program VM cache isolation MIR");
    let bytecode = compile_mir_program(&mir).expect("shared-program VM cache isolation bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut first_vm = BytecodeVM::new(bytecode.clone());
    let mut second_vm = BytecodeVM::new(bytecode);

    assert_eq!(
        first_vm.run_value().expect("first VM library-A run"),
        Value::Int(0)
    );
    assert_eq!(first_vm.stdout(), "12\n");
    assert_eq!(first_vm.debug_stack_state(), (0, 0));
    assert_eq!(first_vm.debug_canonical_ffi_loaded_library_count(), 1);

    guard.set_path(&second_library);
    assert_eq!(
        second_vm.run_value().expect("second VM library-B run"),
        Value::Int(0)
    );
    assert_eq!(second_vm.stdout(), "23\n");
    assert_eq!(second_vm.debug_stack_state(), (0, 0));
    assert_eq!(
        second_vm.debug_canonical_ffi_loaded_library_count(),
        1,
        "a second VM must not inherit the first VM's loaded-library cache"
    );
    assert_eq!(first_vm.stdout(), "12\n");
    assert_eq!(first_vm.debug_canonical_ffi_loaded_library_count(), 1);

    assert_eq!(
        first_vm.run_value().expect("first VM library-B rebinding"),
        Value::Int(0)
    );
    assert_eq!(first_vm.stdout(), "23\n");
    assert_eq!(first_vm.debug_stack_state(), (0, 0));
    assert_eq!(first_vm.debug_canonical_ffi_loaded_library_count(), 2);

    guard.set_path(&first_library);
    assert_eq!(
        second_vm
            .run_value()
            .expect("second VM library-A rebinding"),
        Value::Int(0)
    );
    assert_eq!(second_vm.stdout(), "12\n");
    assert_eq!(second_vm.debug_stack_state(), (0, 0));
    assert_eq!(second_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(first_vm.stdout(), "23\n");
    assert_eq!(first_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(first_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(second_vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_shared_program_vm_contract_modes_and_snapshots_are_isolated() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_contract_isolated(int64_t value) { return value + 1; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_contract_isolated(value: i64) -> i64 ensures: result == value; }
func main() -> i64 {
    println(0 as i64)
    println(mir_ffi_contract_isolated(5 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, BAD_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("shared-program contract-mode fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize shared-program contract-mode MIR");
    let bytecode = compile_mir_program(&mir).expect("shared-program contract-mode bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut unchecked_vm = BytecodeVM::new(bytecode.clone());
    let mut checked_vm = BytecodeVM::new(bytecode);

    unchecked_vm.set_verify_ffi(false);
    assert_eq!(
        unchecked_vm
            .run_value()
            .expect("unchecked VM must accept the violating host result"),
        Value::Int(0)
    );
    assert_eq!(unchecked_vm.stdout(), "0\n6\n");
    assert_eq!(unchecked_vm.debug_stack_state(), (0, 0));
    assert_eq!(unchecked_vm.debug_canonical_ffi_loaded_library_count(), 1);

    let checked_error = checked_vm
        .run_value()
        .expect_err("checked VM must reject the violating host result");
    assert_eq!(checked_error.code(), "E0808");
    assert!(checked_error
        .to_string()
        .contains("FFI postcondition failed"));
    assert_eq!(checked_vm.stdout(), "0\n");
    assert_eq!(checked_vm.debug_stack_state(), (0, 0));
    assert_eq!(checked_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(unchecked_vm.stdout(), "0\n6\n");
    assert_eq!(unchecked_vm.debug_canonical_ffi_loaded_library_count(), 1);

    checked_vm.set_verify_ffi(false);
    assert_eq!(
        checked_vm
            .call_function_wrap_ok(checked_vm.program().entry, &[], Value::Unit)
            .expect("wrapped entry must recover after disabling FFI checks"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(checked_vm.stdout(), "0\n6\n");
    assert_eq!(checked_vm.debug_stack_state(), (0, 0));
    assert_eq!(checked_vm.debug_canonical_ffi_loaded_library_count(), 1);

    unchecked_vm.set_verify_ffi(true);
    let unchecked_error = unchecked_vm
        .run_value()
        .expect_err("re-enabled checks must reject on the same VM");
    assert_eq!(unchecked_error.code(), "E0808");
    assert!(unchecked_error
        .to_string()
        .contains("FFI postcondition failed"));
    assert_eq!(unchecked_vm.stdout(), "0\n");
    assert_eq!(unchecked_vm.debug_stack_state(), (0, 0));
    assert_eq!(unchecked_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(checked_vm.stdout(), "0\n6\n");
    assert_eq!(checked_vm.debug_canonical_ffi_loaded_library_count(), 1);

    unchecked_vm.set_verify_ffi(false);
    assert_eq!(
        unchecked_vm
            .call_named("function:main", Vec::new())
            .expect("direct entry must recover after re-enabling unchecked mode"),
        Value::Int(0)
    );
    assert_eq!(unchecked_vm.stdout(), "0\n6\n");
    assert_eq!(unchecked_vm.debug_stack_state(), (0, 0));
    assert_eq!(unchecked_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(unchecked_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(checked_vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_spawned_vm_inherits_parent_contract_mode() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_spawn_contract(int64_t value) { return value + 1; }
"#;
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_spawn_contract(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_spawn_contract(value: i64) -> i64 ensures: result == value; }
func worker() -> i64 {
    mir_ffi_spawn_contract(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bad_fixture = library_fixture(counter, BAD_C_SOURCE);
    let good_fixture = library_fixture(counter + 1, GOOD_C_SOURCE);
    guard.set_path(&bad_fixture.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("spawned canonical FFI contract fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize spawned canonical FFI contract MIR");
    let mut bytecode = compile_mir_program(&mir).expect("compile spawned canonical FFI bytecode");
    assert!(bytecode.ast.is_none());
    // Spawn is still outside the current MIR lowering island.  Assemble the
    // concurrency shell around the already canonical worker call so this
    // regression exercises the real AST-free child VM and its shared program
    // metadata without widening MIR coverage just for the test.
    let worker = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:worker")
        .expect("canonical worker function") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical main function");
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");
    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let task = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: task,
        func: worker,
        args_base: task,
        argc: 0,
    });
    let result = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: result,
        ra: task,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: result });
    program.functions[main] = main_proto;
    program.entry = main as u32;

    let checked_program = bytecode.clone();
    let mut vm = BytecodeVM::new(bytecode);
    vm.set_verify_ffi(false);
    assert_eq!(
        vm.run_value()
            .expect("spawned VM must inherit disabled FFI contract verification"),
        Value::Int(6)
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    let mut generic_unchecked_vm = BytecodeVM::new(checked_program.clone());
    generic_unchecked_vm.verify_contracts = false;
    let error = generic_unchecked_vm
        .run_value()
        .expect_err("disabling ordinary contracts must not disable child FFI checks");
    assert_eq!(error.code(), "E0808");
    assert!(error.to_string().contains("FFI postcondition failed"));
    assert_eq!(generic_unchecked_vm.stdout(), "");
    assert_eq!(generic_unchecked_vm.debug_stack_state(), (0, 0));
    assert_eq!(
        generic_unchecked_vm.debug_canonical_ffi_loaded_library_count(),
        0
    );

    let mut explicitly_bound_vm = BytecodeVM::new(checked_program);
    explicitly_bound_vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    assert_eq!(
        explicitly_bound_vm
            .run_value()
            .expect("spawned VM must inherit explicit FFI library binding"),
        Value::Int(5)
    );
    assert_eq!(explicitly_bound_vm.stdout(), "");
    assert_eq!(explicitly_bound_vm.debug_stack_state(), (0, 0));
    assert_eq!(
        explicitly_bound_vm.debug_canonical_ffi_loaded_library_count(),
        0
    );
}

#[test]
fn scalar_ffi_nested_spawn_inherits_explicit_library_binding() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_contract(int64_t value) { return value + 1; }
"#;
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_contract(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_contract(value: i64) -> i64 ensures: result == value; }
func leaf() -> i64 {
    mir_ffi_nested_contract(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bad_fixture = library_fixture(counter, BAD_C_SOURCE);
    let good_fixture = library_fixture(counter + 1, GOOD_C_SOURCE);
    guard.set_path(&bad_fixture.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("nested spawned canonical FFI fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize nested spawned canonical FFI MIR");
    let mut bytecode = compile_mir_program(&mir).expect("compile nested spawned canonical FFI");
    let leaf = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:leaf")
        .expect("canonical leaf function") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical main function");
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    let mut middle_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:middle".into(), 0);
    let middle_task = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: middle_task,
        func: leaf,
        args_base: middle_task,
        argc: 0,
    });
    let middle_result = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: middle_result,
        ra: middle_task,
    });
    middle_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: middle_result });
    let middle = program.functions.len() as u32;
    program.functions.push(middle_proto);

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let outer_task = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_task,
        func: middle,
        args_base: outer_task,
        argc: 0,
    });
    let outer_task_2 = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_task_2,
        func: middle,
        args_base: outer_task_2,
        argc: 0,
    });
    let outer_result = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: outer_result,
        ra: outer_task,
    });
    let outer_result_2 = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: outer_result_2,
        ra: outer_task_2,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: outer_result });
    program.functions[main] = main_proto;

    let program = bytecode;
    let mut vm = BytecodeVM::new(program.clone());
    vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    assert_eq!(
        vm.run_value()
            .expect("nested spawned VM must inherit explicit FFI library binding"),
        Value::Int(5)
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    let mut recovered_vm = BytecodeVM::new(program);
    recovered_vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("missing.so")
            .to_string_lossy()
            .into_owned(),
    );
    let error = recovered_vm
        .run_value()
        .expect_err("nested child must report a missing explicit FFI library");
    assert_eq!(error.code(), "E0800", "{error}");
    assert!(error.to_string().contains("failed to load"), "{error}");
    assert_eq!(recovered_vm.stdout(), "");
    assert_eq!(recovered_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovered_vm.debug_canonical_ffi_loaded_library_count(), 0);
    recovered_vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("missing-again.so")
            .to_string_lossy()
            .into_owned(),
    );
    let error = recovered_vm
        .run_value()
        .expect_err("a second nested child failure must remain recoverable");
    assert_eq!(error.code(), "E0800", "{error}");
    assert!(error.to_string().contains("failed to load"), "{error}");
    assert_eq!(recovered_vm.stdout(), "");
    assert_eq!(recovered_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovered_vm.debug_canonical_ffi_loaded_library_count(), 0);
    recovered_vm.set_canonical_ffi_library_path(
        bad_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    let error = recovered_vm
        .run_value()
        .expect_err("replacing the path with a bad library must preserve FFI contracts");
    assert_eq!(error.code(), "E0808", "{error}");
    assert!(error.to_string().contains("FFI postcondition failed"));
    assert_eq!(recovered_vm.stdout(), "");
    assert_eq!(recovered_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovered_vm.debug_canonical_ffi_loaded_library_count(), 0);
    recovered_vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    assert_eq!(
        recovered_vm.run_value().expect("recovered nested FFI run"),
        Value::Int(5)
    );
    assert_eq!(recovered_vm.stdout(), "");
    assert_eq!(recovered_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovered_vm.debug_canonical_ffi_loaded_library_count(), 0);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_nested_spawn_merges_leaf_stdout_and_recovers_after_failure() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_output(int64_t value) { return value + 1; }
"#;
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_output(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_output(value: i64) -> i64 ensures: result == value; }
func leaf() -> i64 {
    println(41)
    mir_ffi_nested_output(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bad_fixture = library_fixture(counter, BAD_C_SOURCE);
    let good_fixture = library_fixture(counter + 1, GOOD_C_SOURCE);
    guard.set_path(&bad_fixture.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE)).expect("nested output fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize nested output MIR");
    let mut bytecode = compile_mir_program(&mir).expect("compile nested output bytecode");
    assert!(bytecode.ast.is_none());
    let leaf = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:leaf")
        .expect("canonical leaf function") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical main function");
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    let mut middle_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:middle".into(), 0);
    let middle_task = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: middle_task,
        func: leaf,
        args_base: middle_task,
        argc: 0,
    });
    let middle_result = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: middle_result,
        ra: middle_task,
    });
    middle_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: middle_result });
    let middle = program.functions.len() as u32;
    program.functions.push(middle_proto);

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let outer_task = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_task,
        func: middle,
        args_base: outer_task,
        argc: 0,
    });
    let outer_result = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: outer_result,
        ra: outer_task,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: outer_result });
    program.functions[main] = main_proto;

    let good_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut good_vm = BytecodeVM::new(bytecode.clone());
    good_vm.set_stdout_buf(good_stdout.clone());
    good_vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    assert_eq!(
        good_vm.run_value().expect("nested output success"),
        Value::Int(5)
    );
    assert_eq!(&*good_stdout.lock().unwrap(), "41\n");
    assert_eq!(good_vm.debug_stack_state(), (0, 0));
    assert_eq!(good_vm.debug_canonical_ffi_loaded_library_count(), 0);

    let recovery_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut recovery_vm = BytecodeVM::new(bytecode);
    recovery_vm.set_stdout_buf(recovery_stdout.clone());
    recovery_vm.set_canonical_ffi_library_path(
        bad_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    let error = recovery_vm
        .run_value()
        .expect_err("nested leaf output must preserve a failing postcondition");
    assert_eq!(error.code(), "E0808");
    assert!(error.to_string().contains("FFI postcondition failed"));
    assert_eq!(&*recovery_stdout.lock().unwrap(), "41\n");
    assert_eq!(recovery_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovery_vm.debug_canonical_ffi_loaded_library_count(), 0);

    recovery_vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    assert_eq!(
        recovery_vm
            .run_value()
            .expect("nested leaf output must recover after failure"),
        Value::Int(5)
    );
    assert_eq!(&*recovery_stdout.lock().unwrap(), "41\n41\n");
    assert_eq!(recovery_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovery_vm.debug_canonical_ffi_loaded_library_count(), 0);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_parallel_nested_spawn_isolates_library_binding_and_stdout() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_parallel_nested(int64_t value) { return value + 1; }
"#;
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_parallel_nested(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_parallel_nested(value: i64) -> i64 ensures: result == value; }
func leaf() -> i64 {
    println(41)
    mir_ffi_parallel_nested(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bad_fixture = library_fixture(counter, BAD_C_SOURCE);
    let good_fixture = library_fixture(counter + 1, GOOD_C_SOURCE);
    guard.set_path(&bad_fixture.dir.join("ffi.so"));

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("parallel nested binding fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize parallel nested binding MIR");
    let mut bytecode = compile_mir_program(&mir).expect("compile parallel nested binding bytecode");
    assert!(bytecode.ast.is_none());
    let leaf = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:leaf")
        .expect("canonical parallel leaf function") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical parallel main function");
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    let mut middle_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:middle".into(), 0);
    let middle_task = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: middle_task,
        func: leaf,
        args_base: middle_task,
        argc: 0,
    });
    let middle_result = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: middle_result,
        ra: middle_task,
    });
    middle_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: middle_result });
    let middle = program.functions.len() as u32;
    program.functions.push(middle_proto);

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let outer_task = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_task,
        func: middle,
        args_base: outer_task,
        argc: 0,
    });
    let outer_result = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: outer_result,
        ra: outer_task,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: outer_result });
    program.functions[main] = main_proto;

    let program = bytecode;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let run = |program: std::sync::Arc<crate::interp::bytecode::BytecodeProgram>,
               library_path: String,
               barrier: std::sync::Arc<std::sync::Barrier>| {
        std::thread::spawn(move || {
            let stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let mut vm = BytecodeVM::new(program);
            vm.set_stdout_buf(stdout.clone());
            vm.set_canonical_ffi_library_path(library_path);
            barrier.wait();
            let outcome = match vm.run_value() {
                Ok(value) => Ok(value),
                Err(error) => Err((error.code().to_string(), error.to_string())),
            };
            let stdout_snapshot = stdout.lock().unwrap().clone();
            (
                outcome,
                stdout_snapshot,
                vm.debug_stack_state(),
                vm.debug_canonical_ffi_loaded_library_count(),
            )
        })
    };

    let good_handle = run(
        program.clone(),
        good_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
        barrier.clone(),
    );
    let bad_handle = run(
        program,
        bad_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
        barrier,
    );
    let (good_outcome, good_stdout, good_stack, good_cache) =
        good_handle.join().expect("good nested VM thread");
    let (bad_outcome, bad_stdout, bad_stack, bad_cache) =
        bad_handle.join().expect("bad nested VM thread");

    assert_eq!(good_outcome.expect("good nested VM result"), Value::Int(5));
    assert_eq!(good_stdout, "41\n");
    assert_eq!(good_stack, (0, 0));
    assert_eq!(good_cache, 0);
    let (code, message) = bad_outcome.expect_err("bad nested VM must reject postcondition");
    assert_eq!(code, "E0808");
    assert!(message.contains("FFI postcondition failed"), "{message}");
    assert_eq!(bad_stdout, "41\n");
    assert_eq!(bad_stack, (0, 0));
    assert_eq!(bad_cache, 0);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_parallel_nested_reentry_reuses_binding() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_parallel_reentry(int64_t value) { return value + 1; }
"#;
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_parallel_reentry(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_parallel_reentry(value: i64) -> i64 ensures: result == value; }
func leaf() -> i64 {
    println(41)
    mir_ffi_parallel_reentry(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bad_fixture = library_fixture(counter, BAD_C_SOURCE);
    let good_fixture = library_fixture(counter + 1, GOOD_C_SOURCE);
    guard.set_path(&bad_fixture.dir.join("ffi.so"));

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("parallel nested reentry fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize parallel nested reentry MIR");
    let mut bytecode = compile_mir_program(&mir).expect("compile parallel nested reentry bytecode");
    assert!(bytecode.ast.is_none());
    let leaf = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:leaf")
        .expect("canonical reentry leaf function") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical reentry main function");
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    let mut middle_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:middle".into(), 0);
    let middle_task = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: middle_task,
        func: leaf,
        args_base: middle_task,
        argc: 0,
    });
    let middle_result = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: middle_result,
        ra: middle_task,
    });
    middle_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: middle_result });
    let middle = program.functions.len() as u32;
    program.functions.push(middle_proto);

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let outer_task = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_task,
        func: middle,
        args_base: outer_task,
        argc: 0,
    });
    let outer_result = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: outer_result,
        ra: outer_task,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: outer_result });
    program.functions[main] = main_proto;

    let program = bytecode;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let run = |program: std::sync::Arc<crate::interp::bytecode::BytecodeProgram>,
               library_path: String,
               recovery_path: String,
               barrier: std::sync::Arc<std::sync::Barrier>| {
        std::thread::spawn(move || {
            let stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let mut vm = BytecodeVM::new(program);
            vm.set_stdout_buf(stdout.clone());
            vm.set_canonical_ffi_library_path(library_path);
            barrier.wait();
            let first = match vm.run_value() {
                Ok(value) => Ok(value),
                Err(error) => Err((error.code().to_string(), error.to_string())),
            };
            barrier.wait();
            if !recovery_path.is_empty() {
                vm.set_canonical_ffi_library_path(recovery_path);
            }
            let second = match vm.run_value() {
                Ok(value) => Ok(value),
                Err(error) => Err((error.code().to_string(), error.to_string())),
            };
            let stdout_snapshot = stdout.lock().unwrap().clone();
            (
                first,
                second,
                stdout_snapshot,
                vm.debug_stack_state(),
                vm.debug_canonical_ffi_loaded_library_count(),
            )
        })
    };

    let good_path = good_fixture
        .dir
        .join("ffi.so")
        .to_string_lossy()
        .into_owned();
    let bad_path = bad_fixture
        .dir
        .join("ffi.so")
        .to_string_lossy()
        .into_owned();
    let good_handle = run(
        program.clone(),
        good_path.clone(),
        String::new(),
        barrier.clone(),
    );
    let bad_handle = run(program, bad_path, good_path, barrier);
    let (good_first, good_second, good_stdout, good_stack, good_cache) =
        good_handle.join().expect("good reentry VM thread");
    let (bad_first, bad_second, bad_stdout, bad_stack, bad_cache) =
        bad_handle.join().expect("bad reentry VM thread");

    assert_eq!(good_first.expect("good first nested result"), Value::Int(5));
    assert_eq!(
        good_second.expect("good second nested result"),
        Value::Int(5)
    );
    assert_eq!(good_stdout, "41\n41\n");
    assert_eq!(good_stack, (0, 0));
    assert_eq!(good_cache, 0);
    let (code, message) = bad_first.expect_err("bad first nested run must fail postcondition");
    assert_eq!(code, "E0808");
    assert!(message.contains("FFI postcondition failed"), "{message}");
    assert_eq!(bad_second.expect("recovered nested result"), Value::Int(5));
    assert_eq!(bad_stdout, "41\n41\n");
    assert_eq!(bad_stack, (0, 0));
    assert_eq!(bad_cache, 0);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_nested_fanout_failure_preserves_sibling_effects() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_fanout(int64_t value) { return value + 1; }
"#;
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_fanout(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_fanout(value: i64) -> i64 ensures: result == value; }
func plain_leaf() -> i64 {
    println(42)
    9
}
func ffi_leaf() -> i64 {
    println(41)
    mir_ffi_nested_fanout(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bad_fixture = library_fixture(counter, BAD_C_SOURCE);
    let good_fixture = library_fixture(counter + 1, GOOD_C_SOURCE);
    guard.set_path(&bad_fixture.dir.join("ffi.so"));

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("nested fanout sibling fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize nested fanout sibling MIR");
    let mut bytecode = compile_mir_program(&mir).expect("compile nested fanout sibling bytecode");
    assert!(bytecode.ast.is_none());
    let plain_leaf = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:plain_leaf")
        .expect("canonical plain leaf function") as u32;
    let ffi_leaf = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:ffi_leaf")
        .expect("canonical ffi leaf function") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical fanout main function");
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    let mut plain_middle =
        crate::interp::bytecode::instr::FunctionProto::new("function:plain_middle".into(), 0);
    let plain_task = plain_middle.alloc_reg();
    plain_middle.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: plain_task,
        func: plain_leaf,
        args_base: plain_task,
        argc: 0,
    });
    let plain_result = plain_middle.alloc_reg();
    plain_middle.emit(crate::interp::bytecode::instr::Op::Await {
        rd: plain_result,
        ra: plain_task,
    });
    plain_middle.emit(crate::interp::bytecode::instr::Op::Ret { ra: plain_result });
    let plain_middle_id = program.functions.len() as u32;
    program.functions.push(plain_middle);

    let mut ffi_middle =
        crate::interp::bytecode::instr::FunctionProto::new("function:ffi_middle".into(), 0);
    let ffi_task = ffi_middle.alloc_reg();
    ffi_middle.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: ffi_task,
        func: ffi_leaf,
        args_base: ffi_task,
        argc: 0,
    });
    let ffi_result = ffi_middle.alloc_reg();
    ffi_middle.emit(crate::interp::bytecode::instr::Op::Await {
        rd: ffi_result,
        ra: ffi_task,
    });
    ffi_middle.emit(crate::interp::bytecode::instr::Op::Ret { ra: ffi_result });
    let ffi_middle_id = program.functions.len() as u32;
    program.functions.push(ffi_middle);

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let plain_outer = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: plain_outer,
        func: plain_middle_id,
        args_base: plain_outer,
        argc: 0,
    });
    let ffi_outer = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: ffi_outer,
        func: ffi_middle_id,
        args_base: ffi_outer,
        argc: 0,
    });
    let plain_value = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: plain_value,
        ra: plain_outer,
    });
    let _ffi_value = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: _ffi_value,
        ra: ffi_outer,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: plain_value });
    program.functions[main] = main_proto;

    let sorted_lines = |stdout: &str| {
        // A concurrent println appends its payload and newline under separate
        // locks, so sibling workers may produce `4241\n\n`. Compact the
        // two digit fixtures before comparing the observable values.
        let compact: String = stdout.chars().filter(|ch| !ch.is_whitespace()).collect();
        let mut lines: Vec<_> = compact
            .as_bytes()
            .chunks(2)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect();
        lines.sort_unstable();
        lines
    };

    let good_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut good_vm = BytecodeVM::new(bytecode.clone());
    good_vm.set_stdout_buf(good_stdout.clone());
    good_vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    assert_eq!(
        good_vm.run_value().expect("fanout good path must complete"),
        Value::Int(9)
    );
    assert_eq!(sorted_lines(&good_stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(good_vm.debug_stack_state(), (0, 0));
    assert_eq!(good_vm.debug_canonical_ffi_loaded_library_count(), 0);

    let bad_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut bad_vm = BytecodeVM::new(bytecode);
    bad_vm.set_stdout_buf(bad_stdout.clone());
    bad_vm.set_canonical_ffi_library_path(
        bad_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    let error = bad_vm
        .run_value()
        .expect_err("fanout bad path must fail only at the FFI sibling");
    assert_eq!(error.code(), "E0808");
    assert!(
        error.to_string().contains("FFI postcondition failed"),
        "{error}"
    );
    assert_eq!(sorted_lines(&bad_stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(bad_vm.debug_stack_state(), (0, 0));
    assert_eq!(bad_vm.debug_canonical_ffi_loaded_library_count(), 0);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_nested_fanout_multi_call_short_circuit_and_reentry() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_fanout_multi(int64_t value) { return value == 5 ? value : value + 1; }
"#;
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_fanout_multi(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_fanout_multi(value: i64) -> i64 ensures: result == value; }
func leaf_a() -> i64 {
    println(41)
    mir_ffi_fanout_multi(5 as i64)
}
func leaf_b() -> i64 {
    println(42)
    mir_ffi_fanout_multi(6 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bad_fixture = library_fixture(counter, BAD_C_SOURCE);
    let good_fixture = library_fixture(counter + 1, GOOD_C_SOURCE);
    guard.set_path(&bad_fixture.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("nested fanout multi-call fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize nested fanout multi-call MIR");
    let mut bytecode =
        compile_mir_program(&mir).expect("compile nested fanout multi-call bytecode");
    assert!(bytecode.ast.is_none());
    let leaf_a = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:leaf_a")
        .expect("canonical fanout leaf_a function") as u32;
    let leaf_b = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:leaf_b")
        .expect("canonical fanout leaf_b function") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical fanout multi-call main function");
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    let mut middle_a =
        crate::interp::bytecode::instr::FunctionProto::new("function:middle_a".into(), 0);
    let task_a = middle_a.alloc_reg();
    middle_a.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: task_a,
        func: leaf_a,
        args_base: task_a,
        argc: 0,
    });
    let result_a = middle_a.alloc_reg();
    middle_a.emit(crate::interp::bytecode::instr::Op::Await {
        rd: result_a,
        ra: task_a,
    });
    middle_a.emit(crate::interp::bytecode::instr::Op::Ret { ra: result_a });
    let middle_a_id = program.functions.len() as u32;
    program.functions.push(middle_a);

    let mut middle_b =
        crate::interp::bytecode::instr::FunctionProto::new("function:middle_b".into(), 0);
    let task_b = middle_b.alloc_reg();
    middle_b.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: task_b,
        func: leaf_b,
        args_base: task_b,
        argc: 0,
    });
    let result_b = middle_b.alloc_reg();
    middle_b.emit(crate::interp::bytecode::instr::Op::Await {
        rd: result_b,
        ra: task_b,
    });
    middle_b.emit(crate::interp::bytecode::instr::Op::Ret { ra: result_b });
    let middle_b_id = program.functions.len() as u32;
    program.functions.push(middle_b);

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let outer_a = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_a,
        func: middle_a_id,
        args_base: outer_a,
        argc: 0,
    });
    let outer_b = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_b,
        func: middle_b_id,
        args_base: outer_b,
        argc: 0,
    });
    let first_value = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: first_value,
        ra: outer_a,
    });
    let _second_value = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: _second_value,
        ra: outer_b,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: first_value });
    program.functions[main] = main_proto;

    let sorted_lines = |stdout: &str| {
        // A concurrent println appends its payload and newline under separate
        // locks, so sibling workers may produce `4241\n\n`. Compact the
        // two digit fixtures before comparing the observable values.
        let compact: String = stdout.chars().filter(|ch| !ch.is_whitespace()).collect();
        let mut lines: Vec<_> = compact
            .as_bytes()
            .chunks(2)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect();
        lines.sort_unstable();
        lines
    };
    let expected_once = vec!["41".to_owned(), "42".to_owned()];
    let expected_twice = vec![
        "41".to_owned(),
        "41".to_owned(),
        "42".to_owned(),
        "42".to_owned(),
    ];
    let expected_thrice = vec![
        "41".to_owned(),
        "41".to_owned(),
        "41".to_owned(),
        "42".to_owned(),
        "42".to_owned(),
        "42".to_owned(),
    ];

    let stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut vm = BytecodeVM::new(bytecode);
    vm.set_stdout_buf(stdout.clone());
    vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    assert_eq!(
        vm.run_value()
            .expect("good multi-call fanout must complete"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), expected_once);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.set_canonical_ffi_library_path(
        bad_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    let error = vm
        .run_value()
        .expect_err("second fanout call must fail the bad postcondition");
    assert_eq!(error.code(), "E0808");
    assert!(
        error.to_string().contains("FFI postcondition failed"),
        "{error}"
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), expected_twice);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.set_canonical_ffi_library_path(
        good_fixture
            .dir
            .join("ffi.so")
            .to_string_lossy()
            .into_owned(),
    );
    assert_eq!(
        vm.run_value()
            .expect("multi-call fanout must recover after failure"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), expected_thrice);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_nested_child_rejects_forged_call_index_before_execution() {
    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_forged_index(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_forged_index(value: i64) -> i64; }
func leaf() -> i64 {
    println(41)
    mir_ffi_nested_forged_index(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("nested forged-index fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize nested forged-index MIR");
    let mut bytecode = compile_mir_program(&mir).expect("compile nested forged-index bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 1);
    let leaf = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:leaf")
        .expect("canonical forged-index leaf function") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical forged-index main function");
    let (ffi_pc, original_extern_idx) = bytecode.functions[leaf as usize]
        .code
        .iter()
        .enumerate()
        .find_map(|(pc, op)| match op {
            crate::interp::bytecode::instr::Op::CallCanonicalExtern { extern_idx, .. } => {
                Some((pc as u32, *extern_idx))
            }
            _ => None,
        })
        .expect("canonical forged-index call site");
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    let mut middle_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:middle".into(), 0);
    let task = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: task,
        func: leaf,
        args_base: task,
        argc: 0,
    });
    let result = middle_proto.alloc_reg();
    middle_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: result,
        ra: task,
    });
    middle_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: result });
    let middle = program.functions.len() as u32;
    program.functions.push(middle_proto);

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let outer_task = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_task,
        func: middle,
        args_base: outer_task,
        argc: 0,
    });
    let outer_result = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: outer_result,
        ra: outer_task,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: outer_result });
    program.functions[main] = main_proto;

    let stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut vm = BytecodeVM::new(bytecode);
    vm.set_stdout_buf(stdout.clone());
    vm.set_canonical_ffi_library_path(fixture.dir.join("ffi.so").to_string_lossy().into_owned());
    vm.replace_canonical_ffi_call_extern_index_for_test_only(leaf, ffi_pc, 17);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let error = vm
        .run_value()
        .expect_err("nested forged descriptor index must fail before spawning the child");
    assert_eq!(error.code(), "E0800");
    assert!(
        error
            .to_string()
            .contains("descriptor index 17 disagrees with compiler binding index 0"),
        "{error}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    vm.replace_canonical_ffi_call_extern_index_for_test_only(leaf, ffi_pc, original_extern_idx);
    assert_eq!(
        vm.run_value()
            .expect("restored nested descriptor index must recover"),
        Value::Int(5)
    );
    assert_eq!(&*stdout.lock().unwrap(), "41\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_nested_fanout_descriptor_forge_preserves_siblings_and_recovers() {
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_fanout_descriptor(int64_t value) { return value; }
"#;
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_fanout_descriptor(int64_t value) { return value + 1; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_fanout_descriptor(value: i64) -> i64 ensures: result == value; }
func plain_leaf() -> i64 {
    println(42)
    9
}
func ffi_leaf() -> i64 {
    println(41)
    mir_ffi_nested_fanout_descriptor(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let good_fixture = library_fixture(counter, GOOD_C_SOURCE);
    let bad_fixture = library_fixture(counter + 1, BAD_C_SOURCE);
    guard.set_path(&good_fixture.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("nested fanout descriptor fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize nested fanout descriptor MIR");
    let mut bytecode =
        compile_mir_program(&mir).expect("compile nested fanout descriptor bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 1);
    let plain_leaf = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:plain_leaf")
        .expect("canonical descriptor plain leaf") as u32;
    let ffi_leaf = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:ffi_leaf")
        .expect("canonical descriptor FFI leaf") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical descriptor main");
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    let mut plain_middle =
        crate::interp::bytecode::instr::FunctionProto::new("function:plain_middle".into(), 0);
    let plain_task = plain_middle.alloc_reg();
    plain_middle.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: plain_task,
        func: plain_leaf,
        args_base: plain_task,
        argc: 0,
    });
    let plain_result = plain_middle.alloc_reg();
    plain_middle.emit(crate::interp::bytecode::instr::Op::Await {
        rd: plain_result,
        ra: plain_task,
    });
    plain_middle.emit(crate::interp::bytecode::instr::Op::Ret { ra: plain_result });
    let plain_middle_id = program.functions.len() as u32;
    program.functions.push(plain_middle);

    let mut ffi_middle =
        crate::interp::bytecode::instr::FunctionProto::new("function:ffi_middle".into(), 0);
    let ffi_task = ffi_middle.alloc_reg();
    ffi_middle.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: ffi_task,
        func: ffi_leaf,
        args_base: ffi_task,
        argc: 0,
    });
    let ffi_result = ffi_middle.alloc_reg();
    ffi_middle.emit(crate::interp::bytecode::instr::Op::Await {
        rd: ffi_result,
        ra: ffi_task,
    });
    ffi_middle.emit(crate::interp::bytecode::instr::Op::Ret { ra: ffi_result });
    let ffi_middle_id = program.functions.len() as u32;
    program.functions.push(ffi_middle);

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let plain_outer = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: plain_outer,
        func: plain_middle_id,
        args_base: plain_outer,
        argc: 0,
    });
    let ffi_outer = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: ffi_outer,
        func: ffi_middle_id,
        args_base: ffi_outer,
        argc: 0,
    });
    let plain_value = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: plain_value,
        ra: plain_outer,
    });
    let _ffi_value = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: _ffi_value,
        ra: ffi_outer,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: plain_value });
    program.functions[main] = main_proto;

    let sorted_lines = |stdout: &str| {
        // A concurrent println appends its payload and newline under separate
        // locks, so sibling workers may produce `4241\n\n`. Compact the
        // two digit fixtures before comparing the observable values.
        let compact: String = stdout.chars().filter(|ch| !ch.is_whitespace()).collect();
        let mut lines: Vec<_> = compact
            .as_bytes()
            .chunks(2)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect();
        lines.sort_unstable();
        lines
    };
    let good_path = good_fixture
        .dir
        .join("ffi.so")
        .to_string_lossy()
        .into_owned();
    let bad_path = bad_fixture
        .dir
        .join("ffi.so")
        .to_string_lossy()
        .into_owned();
    let stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut vm = BytecodeVM::new(bytecode);
    vm.set_stdout_buf(stdout.clone());
    vm.set_canonical_ffi_library_path(good_path.clone());
    assert_eq!(
        vm.run_value().expect("nested fanout descriptor good run"),
        Value::Int(9)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    let original = descriptor_snapshot[0].clone();
    let mut forged = original.clone();
    forged.symbol = "mir_ffi_nested_fanout_forged_symbol".into();
    vm.replace_canonical_ffi_descriptor_for_test_only(0, forged);
    stdout.lock().unwrap().clear();
    let preflight_error = vm
        .run_value()
        .expect_err("forged nested descriptor must fail before either sibling starts");
    assert_eq!(preflight_error.code(), "E0800");
    assert!(
        preflight_error
            .to_string()
            .contains("differs from its compiler binding"),
        "{preflight_error}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_ne!(vm.program().canonical_ffi[0], original);

    vm.replace_canonical_ffi_descriptor_for_test_only(0, original.clone());
    vm.set_canonical_ffi_library_path(bad_path);
    stdout.lock().unwrap().clear();
    let runtime_error = vm
        .run_value()
        .expect_err("bad nested FFI sibling must fail after plain sibling completes");
    assert_eq!(runtime_error.code(), "E0808");
    assert!(
        runtime_error
            .to_string()
            .contains("FFI postcondition failed"),
        "{runtime_error}"
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.set_canonical_ffi_library_path(good_path);
    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.run_value()
            .expect("restored nested descriptor must recover after sibling failure"),
        Value::Int(9)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi[0], original);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_nested_fanout_binding_index_pair_forge_rejects_before_children_and_recovers() {
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_pair_forge(int64_t value) { return value; }
"#;
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_pair_forge(int64_t value) { return value == 5 ? value : value + 1; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_pair_forge(value: i64) -> i64 ensures: result == value; }
func leaf_a() -> i64 {
    println(41)
    mir_ffi_nested_pair_forge(5 as i64)
}
func leaf_b() -> i64 {
    println(42)
    mir_ffi_nested_pair_forge(6 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let good_fixture = library_fixture(counter, GOOD_C_SOURCE);
    let bad_fixture = library_fixture(counter + 1, BAD_C_SOURCE);
    guard.set_path(&good_fixture.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("nested fanout paired-forge fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize nested fanout paired-forge MIR");
    let mut bytecode =
        compile_mir_program(&mir).expect("compile nested fanout paired-forge bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    assert_eq!(bytecode.canonical_ffi_bindings.len(), 2);
    let leaf_a = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:leaf_a")
        .expect("paired-forge leaf_a function") as u32;
    let leaf_b = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:leaf_b")
        .expect("paired-forge leaf_b function") as u32;
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("paired-forge main function");
    let binding_snapshot = bytecode.canonical_ffi_bindings.clone();
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let original_b = binding_snapshot[1].clone();
    let first_binding = binding_snapshot[0].clone();
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    let mut middle_a =
        crate::interp::bytecode::instr::FunctionProto::new("function:middle_a".into(), 0);
    let task_a = middle_a.alloc_reg();
    middle_a.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: task_a,
        func: leaf_a,
        args_base: task_a,
        argc: 0,
    });
    let result_a = middle_a.alloc_reg();
    middle_a.emit(crate::interp::bytecode::instr::Op::Await {
        rd: result_a,
        ra: task_a,
    });
    middle_a.emit(crate::interp::bytecode::instr::Op::Ret { ra: result_a });
    let middle_a_id = program.functions.len() as u32;
    program.functions.push(middle_a);

    let mut middle_b =
        crate::interp::bytecode::instr::FunctionProto::new("function:middle_b".into(), 0);
    let task_b = middle_b.alloc_reg();
    middle_b.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: task_b,
        func: leaf_b,
        args_base: task_b,
        argc: 0,
    });
    let result_b = middle_b.alloc_reg();
    middle_b.emit(crate::interp::bytecode::instr::Op::Await {
        rd: result_b,
        ra: task_b,
    });
    middle_b.emit(crate::interp::bytecode::instr::Op::Ret { ra: result_b });
    let middle_b_id = program.functions.len() as u32;
    program.functions.push(middle_b);

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let outer_a = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_a,
        func: middle_a_id,
        args_base: outer_a,
        argc: 0,
    });
    let outer_b = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: outer_b,
        func: middle_b_id,
        args_base: outer_b,
        argc: 0,
    });
    let first_value = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: first_value,
        ra: outer_a,
    });
    let second_value = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: second_value,
        ra: outer_b,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: first_value });
    program.functions[main] = main_proto;

    let sorted_lines = |stdout: &str| {
        // A concurrent println appends its payload and newline under separate
        // locks, so sibling workers may produce `4241\n\n`. Compact the
        // two digit fixtures before comparing the observable values.
        let compact: String = stdout.chars().filter(|ch| !ch.is_whitespace()).collect();
        let mut lines: Vec<_> = compact
            .as_bytes()
            .chunks(2)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect();
        lines.sort_unstable();
        lines
    };
    let stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let good_library = good_fixture
        .dir
        .join("ffi.so")
        .to_string_lossy()
        .into_owned();
    let bad_library = bad_fixture
        .dir
        .join("ffi.so")
        .to_string_lossy()
        .into_owned();
    let mut vm = BytecodeVM::new(bytecode);
    vm.set_stdout_buf(stdout.clone());
    vm.set_canonical_ffi_library_path(good_library.clone());

    assert_eq!(
        vm.run_value().expect("initial paired-forge run"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    let mut forged_binding = original_b.clone();
    forged_binding.extern_idx = first_binding.extern_idx;
    vm.replace_canonical_ffi_call_extern_index_for_test_only(
        original_b.function,
        original_b.pc,
        first_binding.extern_idx,
    );
    vm.replace_canonical_ffi_binding_for_test_only(1, forged_binding);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    stdout.lock().unwrap().clear();
    let first = vm
        .run_value()
        .expect_err("paired binding/index forgery must fail before either child starts");
    assert_eq!(first.code(), "E0800");
    assert!(
        first
            .to_string()
            .contains("descriptor index 0 at function 'function:leaf_b' pc 6 differs from its compiler binding"),
        "{first}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    stdout.lock().unwrap().clear();
    let second = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must repeat paired binding/index rejection");
    assert_eq!(second.to_string(), first.to_string());
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.replace_canonical_ffi_call_extern_index_for_test_only(
        original_b.function,
        original_b.pc,
        original_b.extern_idx,
    );
    vm.replace_canonical_ffi_binding_for_test_only(1, original_b.clone());
    assert_eq!(vm.program().canonical_ffi_bindings, binding_snapshot);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.run_value()
            .expect("paired binding/index forgery must recover after restoration"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi_bindings, binding_snapshot);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    vm.set_canonical_ffi_library_path(bad_library.clone());
    stdout.lock().unwrap().clear();
    let direct_error = vm
        .call_named("function:main", Vec::new())
        .expect_err("direct nested fanout entry must surface the bad-library postcondition");
    assert_eq!(direct_error.code(), "E0808");
    assert!(
        direct_error
            .to_string()
            .contains("FFI postcondition failed"),
        "{direct_error}"
    );
    let direct_error_stdout = stdout.lock().unwrap().clone();
    assert_eq!(
        sorted_lines(&direct_error_stdout),
        vec!["41", "42"],
        "direct nested bad-library stdout: {direct_error_stdout:?}"
    );
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.set_canonical_ffi_library_path(good_library.clone());
    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.call_function(vm.program().entry, &[])
            .expect("direct entry must recover after the nested bad-library call"),
        Value::Int(5)
    );
    let direct_good_stdout = stdout.lock().unwrap().clone();
    assert_eq!(
        sorted_lines(&direct_good_stdout),
        vec!["41", "42"],
        "direct nested recovered stdout: {direct_good_stdout:?}"
    );
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi_bindings, binding_snapshot);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let mut forged_result_binding = binding_snapshot[1].clone();
    forged_result_binding.rd = forged_result_binding.rd.saturating_add(1);
    vm.replace_canonical_ffi_binding_for_test_only(1, forged_result_binding);
    stdout.lock().unwrap().clear();
    let result_error = vm
        .run_value()
        .expect_err("forged result register binding must fail before nested children start");
    assert_eq!(result_error.code(), "E0800");
    assert!(
        result_error.to_string().contains("result register"),
        "{result_error}"
    );
    assert!(
        result_error
            .to_string()
            .contains("disagrees with compiler binding register"),
        "{result_error}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    vm.replace_canonical_ffi_binding_for_test_only(1, binding_snapshot[1].clone());
    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("result register binding restoration must recover nested fanout"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi_bindings, binding_snapshot);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let forged_args_base = if original_b.args_base == u16::MAX {
        0
    } else {
        original_b.args_base + 1
    };
    let mut forged_args_binding = binding_snapshot[1].clone();
    forged_args_binding.args_base = forged_args_base;
    vm.replace_canonical_ffi_binding_for_test_only(1, forged_args_binding);
    stdout.lock().unwrap().clear();
    let args_base_error = vm
        .run_value()
        .expect_err("forged argument base binding must fail before nested children start");
    assert_eq!(args_base_error.code(), "E0800");
    assert!(
        args_base_error
            .to_string()
            .contains("argument base register"),
        "{args_base_error}"
    );
    assert!(
        args_base_error
            .to_string()
            .contains("disagrees with compiler binding register"),
        "{args_base_error}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    vm.replace_canonical_ffi_binding_for_test_only(1, binding_snapshot[1].clone());
    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("argument base binding restoration must recover nested fanout"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    let mut forged_window_binding = binding_snapshot[1].clone();
    forged_window_binding.args_base = u16::MAX;
    vm.replace_canonical_ffi_binding_for_test_only(1, forged_window_binding);
    vm.replace_canonical_ffi_call_args_for_test_only(
        original_b.function,
        original_b.pc,
        u16::MAX,
        original_b.argc,
    );
    stdout.lock().unwrap().clear();
    let window_error = vm
        .run_value()
        .expect_err("out-of-frame argument window must fail before nested children start");
    assert_eq!(window_error.code(), "E0800");
    assert!(
        window_error
            .to_string()
            .contains("argument register window base"),
        "{window_error}"
    );
    assert!(
        window_error.to_string().contains("exceeds function frame"),
        "{window_error}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    vm.replace_canonical_ffi_binding_for_test_only(1, binding_snapshot[1].clone());
    vm.replace_canonical_ffi_call_args_for_test_only(
        original_b.function,
        original_b.pc,
        original_b.args_base,
        original_b.argc,
    );
    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.run_value()
            .expect("argument window restoration must recover nested fanout"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    let forged_argc = if original_b.argc == u16::MAX {
        0
    } else {
        original_b.argc + 1
    };
    let mut forged_argc_binding = binding_snapshot[1].clone();
    forged_argc_binding.argc = forged_argc;
    vm.replace_canonical_ffi_binding_for_test_only(1, forged_argc_binding);
    stdout.lock().unwrap().clear();
    let argc_error = vm
        .run_value()
        .expect_err("forged argument count binding must fail before nested children start");
    assert_eq!(argc_error.code(), "E0800");
    assert!(
        argc_error.to_string().contains("argument count"),
        "{argc_error}"
    );
    assert!(
        argc_error
            .to_string()
            .contains("disagrees with compiler binding count"),
        "{argc_error}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    vm.replace_canonical_ffi_binding_for_test_only(1, binding_snapshot[1].clone());
    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("argument count binding restoration must recover nested fanout"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi_bindings, binding_snapshot);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let forged_instruction_argc = if original_b.argc == u16::MAX {
        0
    } else {
        original_b.argc + 1
    };
    vm.replace_canonical_ffi_call_args_for_test_only(
        original_b.function,
        original_b.pc,
        original_b.args_base,
        forged_instruction_argc,
    );
    stdout.lock().unwrap().clear();
    let instruction_argc_error = vm
        .run_value()
        .expect_err("forged instruction argument count must fail before nested children start");
    assert_eq!(instruction_argc_error.code(), "E0800");
    assert!(
        instruction_argc_error
            .to_string()
            .contains("argument count"),
        "{instruction_argc_error}"
    );
    assert!(
        instruction_argc_error
            .to_string()
            .contains("disagrees with compiler binding count"),
        "{instruction_argc_error}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi_bindings, binding_snapshot);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    vm.replace_canonical_ffi_call_args_for_test_only(
        original_b.function,
        original_b.pc,
        original_b.args_base,
        original_b.argc,
    );
    let forged_descriptor_argc = if original_b.argc == 0 { 1 } else { 0 };
    let mut forged_descriptor_binding = binding_snapshot[1].clone();
    forged_descriptor_binding.argc = forged_descriptor_argc;
    vm.replace_canonical_ffi_binding_for_test_only(1, forged_descriptor_binding);
    vm.replace_canonical_ffi_call_args_for_test_only(
        original_b.function,
        original_b.pc,
        original_b.args_base,
        forged_descriptor_argc,
    );
    stdout.lock().unwrap().clear();
    let descriptor_arity_error = vm
        .run_value()
        .expect_err("forged descriptor arity must fail before nested children start");
    assert_eq!(descriptor_arity_error.code(), "E0800");
    assert!(
        descriptor_arity_error
            .to_string()
            .contains("argument count"),
        "{descriptor_arity_error}"
    );
    assert!(
        descriptor_arity_error
            .to_string()
            .contains("descriptor arity"),
        "{descriptor_arity_error}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    vm.replace_canonical_ffi_binding_for_test_only(1, binding_snapshot[1].clone());
    vm.replace_canonical_ffi_call_args_for_test_only(
        original_b.function,
        original_b.pc,
        original_b.args_base,
        original_b.argc,
    );
    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.run_value()
            .expect("instruction argc and descriptor arity restoration must recover"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi_bindings, binding_snapshot);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let forged_combo_argc = if original_b.argc == u16::MAX {
        0
    } else {
        original_b.argc + 1
    };
    let forged_combo_args_base =
        vm.program().functions[original_b.function as usize].register_count;
    let mut forged_combo_binding = binding_snapshot[1].clone();
    forged_combo_binding.args_base = forged_combo_args_base;
    forged_combo_binding.argc = forged_combo_argc;
    vm.replace_canonical_ffi_binding_for_test_only(1, forged_combo_binding);
    vm.replace_canonical_ffi_call_args_for_test_only(
        original_b.function,
        original_b.pc,
        forged_combo_args_base,
        forged_combo_argc,
    );
    stdout.lock().unwrap().clear();
    let combo_error = vm
        .run_value()
        .expect_err("window and descriptor arity forgery must fail before nested children start");
    assert_eq!(combo_error.code(), "E0800");
    assert!(
        combo_error.to_string().contains("argument register window"),
        "{combo_error}"
    );
    assert!(
        combo_error.to_string().contains("exceeds function frame"),
        "{combo_error}"
    );
    assert!(
        !combo_error.to_string().contains("descriptor arity"),
        "window validation must precede descriptor arity: {combo_error}"
    );
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    stdout.lock().unwrap().clear();
    let wrapped_combo_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must preserve combined argument validation");
    assert_eq!(wrapped_combo_error.code(), "E0800");
    assert_eq!(wrapped_combo_error.to_string(), combo_error.to_string());
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    stdout.lock().unwrap().clear();
    let direct_combo_error = vm
        .call_function(vm.program().entry, &[])
        .expect_err("direct entry must preserve combined argument validation");
    assert_eq!(direct_combo_error.code(), "E0800");
    assert_eq!(direct_combo_error.to_string(), combo_error.to_string());
    assert_eq!(&*stdout.lock().unwrap(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.replace_canonical_ffi_binding_for_test_only(1, binding_snapshot[1].clone());
    vm.replace_canonical_ffi_call_args_for_test_only(
        original_b.function,
        original_b.pc,
        original_b.args_base,
        original_b.argc,
    );
    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.call_function(vm.program().entry, &[])
            .expect("combined argument validation restoration must recover"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi_bindings, binding_snapshot);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    vm.set_canonical_ffi_library_path(bad_library);
    stdout.lock().unwrap().clear();
    let bad_run_error = vm
        .run_value()
        .expect_err("bad library must remain observable after restored entry diagnostics");
    assert_eq!(bad_run_error.code(), "E0808");
    assert!(
        bad_run_error
            .to_string()
            .contains("FFI postcondition failed"),
        "{bad_run_error}"
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    stdout.lock().unwrap().clear();
    let bad_wrapped_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must preserve bad-library failure after preflight recovery");
    assert_eq!(bad_wrapped_error.code(), "E0808");
    assert_eq!(bad_wrapped_error.to_string(), bad_run_error.to_string());
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.set_canonical_ffi_library_path(good_library);
    stdout.lock().unwrap().clear();
    assert_eq!(
        vm.call_function(vm.program().entry, &[])
            .expect("good library must recover after repeated bad entry calls"),
        Value::Int(5)
    );
    assert_eq!(sorted_lines(&stdout.lock().unwrap()), vec!["41", "42"]);
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(vm.program().canonical_ffi_bindings, binding_snapshot);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn canonical_spawned_vm_inherits_ordinary_contract_mode() {
    const SOURCE: &str = r#"
func worker() -> i64 {
    println(41)
    6
}
func main() -> i64 { 0 }
"#;

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("spawned ordinary contract fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize spawned ordinary contract MIR");
    let mut bytecode =
        compile_mir_program(&mir).expect("compile spawned ordinary contract bytecode");
    assert!(bytecode.ast.is_none());
    let worker = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:worker")
        .expect("canonical worker function");
    let main = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:main")
        .expect("canonical main function");
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");

    // Canonical MIR currently carries ordinary contracts as typed predicates,
    // while the bytecode VM's legacy contract hook consumes mini-function
    // metadata.  Attach a tiny AST-free false predicate here so this test
    // isolates the child VM's inherited `verify_contracts` switch without
    // asking the MIR adapter to reopen the surface AST.
    let mut contract_proto =
        crate::interp::bytecode::instr::FunctionProto::new("__contract_false".into(), 1);
    contract_proto.emit(crate::interp::bytecode::instr::Op::LoadFalse { rd: 0 });
    contract_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: 0 });
    let contract_func = program.functions.len() as u32;
    program.functions.push(contract_proto);
    program.functions[worker].has_ensures = true;
    program.functions[worker].ensures_funcs = vec![contract_func];

    let mut main_proto =
        crate::interp::bytecode::instr::FunctionProto::new("function:main".into(), 0);
    let task = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Spawn {
        rd: task,
        func: worker as u32,
        args_base: task,
        argc: 0,
    });
    let result = main_proto.alloc_reg();
    main_proto.emit(crate::interp::bytecode::instr::Op::Await {
        rd: result,
        ra: task,
    });
    main_proto.emit(crate::interp::bytecode::instr::Op::Ret { ra: result });
    program.functions[main] = main_proto;
    program.entry = main as u32;

    let verified_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut verified = BytecodeVM::new(bytecode.clone());
    verified.set_stdout_buf(verified_stdout.clone());
    let error = verified
        .run_value()
        .expect_err("ordinary child contract must be enforced when enabled");
    assert_eq!(error.code(), "E0808");
    assert!(error.to_string().contains("ensures condition failed"));
    assert_eq!(&*verified_stdout.lock().unwrap(), "41\n");
    assert_eq!(verified.debug_stack_state(), (0, 0));

    let unchecked_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut unchecked = BytecodeVM::new(bytecode);
    unchecked.set_stdout_buf(unchecked_stdout.clone());
    unchecked.verify_contracts = false;
    assert_eq!(
        unchecked
            .run_value()
            .expect("ordinary child contract must follow the parent switch when disabled"),
        Value::Int(6)
    );
    assert_eq!(&*unchecked_stdout.lock().unwrap(), "41\n");
    assert_eq!(unchecked.debug_stack_state(), (0, 0));
}

#[test]
fn scalar_ffi_actor_worker_inherits_contract_mode() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_actor_contract(int64_t value) { return value + 1; }
"#;
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_actor_contract(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_actor_contract(value: i64) -> i64 ensures: result == value; }
func worker(self: i64) -> i64 {
    println(41)
    mir_ffi_actor_contract(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bad_fixture = library_fixture(counter, BAD_C_SOURCE);
    let good_fixture = library_fixture(counter + 1, GOOD_C_SOURCE);
    let isolated_fixture = library_fixture(counter + 2, GOOD_C_SOURCE);
    guard.set_path(&bad_fixture.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("actor canonical FFI contract fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize actor canonical FFI contract MIR");
    let mut bytecode = compile_mir_program(&mir).expect("compile actor canonical FFI bytecode");
    assert!(bytecode.ast.is_none());
    let worker = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:worker")
        .expect("canonical actor worker function") as u32;
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");
    program
        .actor_method_funcs
        .insert(("Worker".to_string(), "call".to_string()), worker);

    let actor_instance = || crate::interp::ActorInstance {
        actor_name: "Worker".to_string(),
        fields: std::collections::HashMap::new(),
        methods: Vec::new(),
        runs_flow: None,
        flow_state: None,
        faulted: false,
        peer_links: Vec::new(),
        parent_id: None,
        is_detached: false,
        producers: Vec::new(),
    };
    let program = bytecode;
    let empty_ast = std::sync::Arc::new(crate::ast::File {
        sources: crate::span::SourceRegistry::default(),
        imports: Vec::new(),
        items: Vec::new(),
        implicit_single: false,
    });

    let disabled = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast.clone(),
        program.clone(),
        None,
        true,
        false,
        None,
    );
    let response = disabled
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue disabled actor FFI call")
        .recv()
        .expect("disabled actor worker response")
        .expect("disabled actor FFI contract check");
    assert_eq!(response, Value::Int(6));

    let enabled = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast.clone(),
        program.clone(),
        None,
        true,
        true,
        None,
    );
    let error = enabled
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue enabled actor FFI call")
        .recv()
        .expect("enabled actor worker response")
        .expect_err("enabled actor worker must enforce FFI postcondition");
    assert_eq!(error.code(), "E0808");
    assert!(error.to_string().contains("FFI postcondition failed"));

    let good_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let explicitly_bound = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast.clone(),
        program.clone(),
        Some(good_stdout.clone()),
        true,
        true,
        Some(
            good_fixture
                .dir
                .join("ffi.so")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    let response = explicitly_bound
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue explicitly bound actor FFI call")
        .recv()
        .expect("explicitly bound actor worker response")
        .expect("explicitly bound actor FFI contract check");
    assert_eq!(response, Value::Int(5));
    assert_eq!(&*good_stdout.lock().unwrap(), "41\n");

    let failing_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let failing_actor = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast.clone(),
        program.clone(),
        Some(failing_stdout.clone()),
        true,
        true,
        None,
    );
    let failing_response = failing_actor
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue environment-bound failing actor FFI call")
        .recv()
        .expect("environment-bound failing actor worker response")
        .expect_err("environment-bound actor must enforce the bad FFI postcondition");
    assert_eq!(failing_response.code(), "E0808");
    assert!(failing_response
        .to_string()
        .contains("FFI postcondition failed"));
    assert_eq!(&*failing_stdout.lock().unwrap(), "41\n");
    assert!(!failing_actor.is_faulted());

    let recovered_response = explicitly_bound
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue repeated explicitly bound actor FFI call")
        .recv()
        .expect("repeated explicitly bound actor worker response")
        .expect("a neighboring failed actor must not poison the good binding");
    assert_eq!(recovered_response, Value::Int(5));
    assert_eq!(&*good_stdout.lock().unwrap(), "41\n41\n");
    assert!(!explicitly_bound.is_faulted());

    let isolated_stdout = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let isolated_actor = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast.clone(),
        program.clone(),
        Some(isolated_stdout.clone()),
        true,
        true,
        Some(
            isolated_fixture
                .dir
                .join("ffi.so")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    let isolated_initial = isolated_actor
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue isolated actor initial FFI call")
        .recv()
        .expect("isolated actor initial worker response")
        .expect("isolated actor initial FFI call");
    assert_eq!(isolated_initial, Value::Int(5));
    assert_eq!(&*isolated_stdout.lock().unwrap(), "41\n");

    let good_path = good_fixture.dir.join("ffi.so");
    let good_backup = good_fixture.dir.join("ffi-good-backup.so");
    std::fs::copy(&good_path, &good_backup).expect("backup explicitly bound actor library");
    std::fs::copy(bad_fixture.dir.join("ffi.so"), &good_path)
        .expect("replace explicitly bound actor library with bad implementation");
    let rebound_rx = explicitly_bound
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue actor call after library replacement");
    let isolated_rebound_rx = isolated_actor
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue isolated actor call after neighboring replacement");
    let rebound_error = rebound_rx
        .recv()
        .expect("actor worker response after library replacement")
        .expect_err("replaced actor library must trigger the FFI postcondition");
    assert_eq!(rebound_error.code(), "E0808");
    assert_eq!(&*good_stdout.lock().unwrap(), "41\n41\n41\n");
    assert!(!explicitly_bound.is_faulted());
    let isolated_rebound = isolated_rebound_rx
        .recv()
        .expect("isolated actor worker response after neighboring replacement")
        .expect("a replacement in one actor path must not affect another path");
    assert_eq!(isolated_rebound, Value::Int(5));
    assert_eq!(&*isolated_stdout.lock().unwrap(), "41\n41\n");
    assert!(!isolated_actor.is_faulted());
    std::fs::copy(&good_backup, &good_path).expect("restore explicitly bound actor library");
    let restored_response = explicitly_bound
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue actor call after library restoration")
        .recv()
        .expect("actor worker response after library restoration")
        .expect("restored actor library must satisfy the FFI postcondition");
    assert_eq!(restored_response, Value::Int(5));
    assert_eq!(&*good_stdout.lock().unwrap(), "41\n41\n41\n41\n");
    assert!(!explicitly_bound.is_faulted());
    let isolated_restored = isolated_actor
        .try_enqueue("call".to_string(), Vec::new())
        .expect("enqueue isolated actor call after restoration")
        .recv()
        .expect("isolated actor worker response after restoration")
        .expect("isolated actor remains healthy after neighboring replacement");
    assert_eq!(isolated_restored, Value::Int(5));
    assert_eq!(&*isolated_stdout.lock().unwrap(), "41\n41\n41\n");
    assert!(!isolated_actor.is_faulted());
}

#[test]
fn scalar_ffi_flow_actor_transition_inherits_contract_mode_and_binding() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_flow_actor_contract(int64_t value) { return value + 1; }
"#;
    const GOOD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_flow_actor_contract(int64_t value) { return value; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_flow_actor_contract(value: i64) -> i64 ensures: result == value; }
func advance_state(self: i64) -> i64 {
    mir_ffi_flow_actor_contract(5 as i64)
}
func main() -> i64 { 0 }
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bad_fixture = library_fixture(counter, BAD_C_SOURCE);
    let good_fixture = library_fixture(counter + 1, GOOD_C_SOURCE);
    guard.set_path(&bad_fixture.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("flow actor canonical FFI contract fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize flow actor canonical FFI contract MIR");
    let mut bytecode =
        compile_mir_program(&mir).expect("compile flow actor canonical FFI bytecode");
    assert!(bytecode.ast.is_none());
    let transition = bytecode
        .functions
        .iter()
        .position(|function| function.name == "function:advance_state")
        .expect("canonical flow transition function") as u32;
    let program = std::sync::Arc::get_mut(&mut bytecode).expect("test bytecode must be unique");
    program.flow_transition_funcs.insert(
        (
            "WorkerFlow".to_string(),
            "advance".to_string(),
            "Start".to_string(),
        ),
        transition,
    );

    let actor_instance = || crate::interp::ActorInstance {
        actor_name: "WorkerFlow".to_string(),
        fields: std::collections::HashMap::new(),
        methods: Vec::new(),
        runs_flow: Some("WorkerFlow".to_string()),
        flow_state: Some(Value::Record(
            Some("Start".to_string()),
            std::collections::HashMap::new(),
        )),
        faulted: false,
        peer_links: Vec::new(),
        parent_id: None,
        is_detached: false,
        producers: Vec::new(),
    };
    let program = bytecode;
    let empty_ast = std::sync::Arc::new(crate::ast::File {
        sources: crate::span::SourceRegistry::default(),
        imports: Vec::new(),
        items: Vec::new(),
        implicit_single: false,
    });

    let disabled = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast.clone(),
        program.clone(),
        None,
        true,
        false,
        None,
    );
    let response = disabled
        .try_enqueue("advance".to_string(), Vec::new())
        .expect("enqueue disabled flow transition")
        .recv()
        .expect("disabled flow actor worker response")
        .expect("disabled flow actor FFI contract check");
    assert_eq!(response, Value::Int(6));

    let enabled = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast.clone(),
        program.clone(),
        None,
        true,
        true,
        None,
    );
    let error = enabled
        .try_enqueue("advance".to_string(), Vec::new())
        .expect("enqueue enabled flow transition")
        .recv()
        .expect("enabled flow actor worker response")
        .expect_err("enabled flow actor worker must enforce FFI postcondition");
    assert_eq!(error.code(), "E0808");
    assert!(error.to_string().contains("FFI postcondition failed"));
    let repeated_error = enabled
        .try_enqueue("advance".to_string(), Vec::new())
        .expect("enqueue repeated enabled flow transition")
        .recv()
        .expect("repeated enabled flow actor worker response")
        .expect_err("a failed flow transition must leave the actor dispatchable");
    assert_eq!(repeated_error.code(), "E0808");
    assert!(repeated_error
        .to_string()
        .contains("FFI postcondition failed"));

    let explicitly_bound = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast.clone(),
        program.clone(),
        None,
        true,
        true,
        Some(
            good_fixture
                .dir
                .join("ffi.so")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    let response = explicitly_bound
        .try_enqueue("advance".to_string(), Vec::new())
        .expect("enqueue explicitly bound flow transition")
        .recv()
        .expect("explicitly bound flow actor worker response")
        .expect("explicitly bound flow actor FFI contract check");
    assert_eq!(response, Value::Int(5));

    let missing_path = bad_fixture.dir.join("missing.so");
    let missing_library = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast.clone(),
        program.clone(),
        None,
        true,
        true,
        Some(missing_path.to_string_lossy().into_owned()),
    );
    let isolated_good = crate::interp::ActorHandle::new_bytecode(
        actor_instance(),
        empty_ast,
        program,
        None,
        true,
        true,
        Some(
            good_fixture
                .dir
                .join("ffi.so")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    let missing_rx = missing_library
        .try_enqueue("advance".to_string(), Vec::new())
        .expect("enqueue missing-library flow transition");
    let good_rx = isolated_good
        .try_enqueue("advance".to_string(), Vec::new())
        .expect("enqueue isolated good-library flow transition");
    let missing_response = missing_rx
        .recv()
        .expect("missing-library flow actor worker response")
        .expect_err("missing-library flow actor must fail to load its binding");
    assert_eq!(missing_response.code(), "E0800");
    assert!(missing_response.to_string().contains("failed to load"));
    let good_response = good_rx
        .recv()
        .expect("isolated good-library flow actor worker response")
        .expect("a missing binding in one actor must not poison another actor");
    assert_eq!(good_response, Value::Int(5));
    let repeated_missing = missing_library
        .try_enqueue("advance".to_string(), Vec::new())
        .expect("enqueue repeated missing-library flow transition")
        .recv()
        .expect("repeated missing-library flow actor worker response")
        .expect_err("missing-library actor must remain dispatchable after failure");
    assert_eq!(repeated_missing.code(), "E0800");
    assert!(repeated_missing.to_string().contains("failed to load"));
}

#[test]
fn scalar_ffi_shared_program_cache_lifetime_is_local_after_vm_drop() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_library = first.dir.join("ffi.so");
    let second_library = second.dir.join("ffi.so");
    guard.set_path(&first_library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("shared-program cache-lifetime fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize shared-program cache-lifetime MIR");
    let bytecode = compile_mir_program(&mir).expect("shared-program cache-lifetime bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let shared_program = bytecode.clone();
    let mut transient_vm = BytecodeVM::new(shared_program.clone());
    let mut survivor_vm = BytecodeVM::new(shared_program.clone());

    assert_eq!(
        transient_vm
            .run_value()
            .expect("transient VM must load library A"),
        Value::Int(0)
    );
    assert_eq!(transient_vm.stdout(), "12\n");
    assert_eq!(transient_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(survivor_vm.stdout(), "");
    assert_eq!(survivor_vm.debug_canonical_ffi_loaded_library_count(), 0);

    assert_eq!(
        survivor_vm
            .run_value()
            .expect("survivor VM must independently load library A"),
        Value::Int(0)
    );
    assert_eq!(survivor_vm.stdout(), "12\n");
    assert_eq!(survivor_vm.debug_canonical_ffi_loaded_library_count(), 1);
    drop(transient_vm);

    guard.set_path(&second_library);
    assert_eq!(
        survivor_vm
            .run_value()
            .expect("survivor VM must rebind to library B after peer drop"),
        Value::Int(0)
    );
    assert_eq!(survivor_vm.stdout(), "23\n");
    assert_eq!(survivor_vm.debug_stack_state(), (0, 0));
    assert_eq!(survivor_vm.debug_canonical_ffi_loaded_library_count(), 2);

    let mut recreated_vm = BytecodeVM::new(shared_program);
    assert_eq!(recreated_vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert_eq!(
        recreated_vm
            .run_value()
            .expect("recreated VM must start a fresh library cache"),
        Value::Int(0)
    );
    assert_eq!(recreated_vm.stdout(), "23\n");
    assert_eq!(recreated_vm.debug_stack_state(), (0, 0));
    assert_eq!(recreated_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(survivor_vm.stdout(), "23\n");
    assert_eq!(survivor_vm.debug_canonical_ffi_loaded_library_count(), 2);

    guard.set_path(&first_library);
    assert_eq!(
        survivor_vm
            .run_value()
            .expect("survivor VM must retain its own A binding after recreation"),
        Value::Int(0)
    );
    assert_eq!(survivor_vm.stdout(), "12\n");
    assert_eq!(survivor_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(recreated_vm.stdout(), "23\n");
    assert_eq!(recreated_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(survivor_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(recreated_vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_shared_program_concurrent_vms_keep_thread_local_state() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("shared-program concurrent VM fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize shared-program concurrent VM MIR");
    let bytecode = compile_mir_program(&mir).expect("shared-program concurrent VM bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let first_program = bytecode.clone();
    let second_program = bytecode;

    let first = std::thread::spawn(move || {
        let mut vm = BytecodeVM::new(first_program);
        let value = vm.run_value();
        (
            value,
            vm.stdout().to_owned(),
            vm.debug_stack_state(),
            vm.debug_canonical_ffi_loaded_library_count(),
            vm.program().canonical_ffi.clone(),
        )
    });
    let second = std::thread::spawn(move || {
        let mut vm = BytecodeVM::new(second_program);
        let value = vm.call_named("function:main", Vec::new());
        (
            value,
            vm.stdout().to_owned(),
            vm.debug_stack_state(),
            vm.debug_canonical_ffi_loaded_library_count(),
            vm.program().canonical_ffi.clone(),
        )
    });

    let first = first
        .join()
        .expect("first canonical FFI VM thread must join");
    let second = second
        .join()
        .expect("second canonical FFI VM thread must join");
    assert_eq!(first.0.expect("first concurrent VM run"), Value::Int(0));
    assert_eq!(second.0.expect("second concurrent VM call"), Value::Int(0));
    assert_eq!(first.1, "12\n");
    assert_eq!(second.1, "12\n");
    assert_eq!(first.2, (0, 0));
    assert_eq!(second.2, (0, 0));
    assert_eq!(first.3, 1);
    assert_eq!(second.3, 1);
    assert_eq!(first.4, descriptor_snapshot);
    assert_eq!(second.4, descriptor_snapshot);
}

#[test]
fn scalar_ffi_shared_program_concurrent_failure_recovery_is_isolated() {
    const BAD_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_concurrent_failure(int64_t value) { return value + 1; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_concurrent_failure(value: i64) -> i64 ensures: result == value; }
func main() -> i64 {
    println(0 as i64)
    println(mir_ffi_concurrent_failure(5 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, BAD_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("shared-program concurrent failure fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize shared-program concurrent failure MIR");
    let bytecode = compile_mir_program(&mir).expect("shared-program concurrent failure bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let first_program = bytecode.clone();
    let second_program = bytecode;

    let first = std::thread::spawn(move || {
        let mut vm = BytecodeVM::new(first_program);
        let first_error = vm
            .run_value()
            .expect_err("first concurrent VM must reject the bad result");
        assert_eq!(first_error.code(), "E0808");
        assert!(first_error.to_string().contains("FFI postcondition failed"));
        assert_eq!(vm.stdout(), "0\n");
        assert_eq!(vm.debug_stack_state(), (0, 0));
        assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

        vm.set_verify_ffi(false);
        let recovered = vm
            .call_named("function:main", Vec::new())
            .expect("first concurrent VM must recover in unchecked mode");
        assert_eq!(recovered, Value::Int(0));
        assert_eq!(vm.stdout(), "0\n6\n");
        assert_eq!(vm.debug_stack_state(), (0, 0));
        assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

        vm.set_verify_ffi(true);
        let second_error = vm
            .run_value()
            .expect_err("first concurrent VM must fail again after re-enabling checks");
        assert_eq!(second_error.code(), "E0808");
        assert_eq!(vm.stdout(), "0\n");
        assert_eq!(vm.debug_stack_state(), (0, 0));
        assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
        (
            vm.stdout().to_owned(),
            vm.debug_canonical_ffi_loaded_library_count(),
            vm.program().canonical_ffi.clone(),
        )
    });
    let second = std::thread::spawn(move || {
        let mut vm = BytecodeVM::new(second_program);
        let first_error = vm
            .run_value()
            .expect_err("second concurrent VM must reject the bad result");
        assert_eq!(first_error.code(), "E0808");
        assert_eq!(vm.stdout(), "0\n");
        assert_eq!(vm.debug_stack_state(), (0, 0));
        assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

        vm.set_verify_ffi(false);
        assert_eq!(
            vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
                .expect("second concurrent VM must recover through wrapped entry"),
            Value::Variant("Ok".into(), vec![Value::Int(0)])
        );
        assert_eq!(vm.stdout(), "0\n6\n");
        assert_eq!(vm.debug_stack_state(), (0, 0));
        assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

        vm.set_verify_ffi(true);
        let second_error = vm
            .run_value()
            .expect_err("second concurrent VM must fail again after re-enabling checks");
        assert_eq!(second_error.code(), "E0808");
        assert_eq!(vm.stdout(), "0\n");
        assert_eq!(vm.debug_stack_state(), (0, 0));
        assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
        (
            vm.stdout().to_owned(),
            vm.debug_canonical_ffi_loaded_library_count(),
            vm.program().canonical_ffi.clone(),
        )
    });

    let first = first
        .join()
        .expect("first concurrent failure/recovery VM thread must join");
    let second = second
        .join()
        .expect("second concurrent failure/recovery VM thread must join");
    assert_eq!(first.0, "0\n");
    assert_eq!(second.0, "0\n");
    assert_eq!(first.1, 1);
    assert_eq!(second.1, 1);
    assert_eq!(first.2, descriptor_snapshot);
    assert_eq!(second.2, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_vm_library_bindings_are_thread_local() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_library = first.dir.join("ffi.so");
    let second_library = second.dir.join("ffi.so");
    guard.set_path(&first_library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("explicit VM library binding fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize explicit VM library binding MIR");
    let bytecode = compile_mir_program(&mir).expect("explicit VM library binding bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let first_program = bytecode.clone();
    let second_program = bytecode.clone();
    let fallback_program = bytecode;
    let first_path = first_library.to_string_lossy().into_owned();
    let second_path = second_library.to_string_lossy().into_owned();
    let second_path_for_fallback = second_path.clone();

    let first = std::thread::spawn(move || {
        let mut vm = BytecodeVM::new(first_program);
        vm.set_canonical_ffi_library_path(first_path);
        let value = vm.run_value();
        (
            value,
            vm.stdout().to_owned(),
            vm.debug_stack_state(),
            vm.debug_canonical_ffi_loaded_library_count(),
            vm.program().canonical_ffi.clone(),
        )
    });
    let second = std::thread::spawn(move || {
        let mut vm = BytecodeVM::new(second_program);
        vm.set_canonical_ffi_library_path(second_path);
        let value = vm.call_named("function:main", Vec::new());
        (
            value,
            vm.stdout().to_owned(),
            vm.debug_stack_state(),
            vm.debug_canonical_ffi_loaded_library_count(),
            vm.program().canonical_ffi.clone(),
        )
    });

    let first = first
        .join()
        .expect("first explicit-binding VM thread must join");
    let second = second
        .join()
        .expect("second explicit-binding VM thread must join");
    assert_eq!(first.0.expect("first explicit-binding run"), Value::Int(0));
    assert_eq!(
        second.0.expect("second explicit-binding call"),
        Value::Int(0)
    );
    assert_eq!(first.1, "12\n");
    assert_eq!(second.1, "23\n");
    assert_eq!(first.2, (0, 0));
    assert_eq!(second.2, (0, 0));
    assert_eq!(first.3, 1);
    assert_eq!(second.3, 1);
    assert_eq!(first.4, descriptor_snapshot);
    assert_eq!(second.4, descriptor_snapshot);

    let mut fallback_vm = BytecodeVM::new(fallback_program);
    fallback_vm.set_canonical_ffi_library_path(second_path_for_fallback);
    assert_eq!(
        fallback_vm
            .run_value()
            .expect("explicit binding must select library B after threaded calls"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "23\n");
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);
    fallback_vm.clear_canonical_ffi_library_path();
    assert_eq!(
        fallback_vm
            .run_value()
            .expect("clearing explicit binding must restore environment lookup"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "12\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(fallback_vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_vm_binding_failures_do_not_pollute_cache() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_library = first.dir.join("ffi.so");
    let second_library = second.dir.join("ffi.so");
    let missing_library = first.dir.join("missing-ffi.so");
    guard.set_path(&first_library);

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("explicit VM binding failure fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize explicit VM binding failure MIR");
    let bytecode = compile_mir_program(&mir).expect("explicit VM binding failure bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(first_library.to_string_lossy().into_owned());
    assert_eq!(vm.run_value().expect("explicit A binding"), Value::Int(0));
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.set_canonical_ffi_library_path(missing_library.to_string_lossy().into_owned());
    let missing_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("missing explicit library must fail through wrapped entry");
    assert_eq!(missing_error.code(), "E0800");
    assert!(missing_error.to_string().contains("failed to load"));
    assert!(missing_error.to_string().contains("missing-ffi.so"));
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        1,
        "failed explicit loads must not add a cache entry"
    );

    vm.set_canonical_ffi_library_path(second_library.to_string_lossy().into_owned());
    assert_eq!(vm.run_value().expect("explicit B binding"), Value::Int(0));
    assert_eq!(vm.stdout(), "23\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.clear_canonical_ffi_library_path();
    assert_eq!(
        vm.run_value()
            .expect("cleared binding must use environment A"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        2,
        "clearing to an already cached environment path must reuse it"
    );
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_never_bypasses_cached_descriptor_preflight() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    // Fixture allocation must share the process-wide guard with the other
    // explicit-binding tests; this test derives a second path from `counter`.
    let _guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_path = first.dir.join("ffi.so").to_string_lossy().into_owned();
    let second_path = second.dir.join("ffi.so").to_string_lossy().into_owned();

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("explicit preflight fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize explicit preflight MIR");
    let bytecode = compile_mir_program(&mir).expect("explicit preflight bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(first_path.clone());
    assert_eq!(
        vm.run_value().expect("initial explicit A run"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    let original = vm.program().canonical_ffi[0].clone();
    let mut forged = original.clone();
    forged.symbol = "forged_after_explicit_rebind".into();
    vm.replace_canonical_ffi_descriptor_for_test_only(0, forged);
    vm.set_canonical_ffi_library_path(second_path.clone());
    let forged_error = vm
        .run_value()
        .expect_err("explicit path changes must not bypass descriptor preflight");
    assert!(
        forged_error
            .to_string()
            .contains("differs from its compiler binding"),
        "{forged_error}"
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        1,
        "forged preflight must reject before loading the rebound library"
    );

    vm.replace_canonical_ffi_descriptor_for_test_only(0, original);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("restored descriptor must permit explicit B binding"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "23\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(first_path);
    assert_eq!(
        vm.run_value()
            .expect("explicit A rebinding after restoration"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_contract_failures_preserve_cross_entry_snapshots() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64 ensures: result == value; }
func main() -> i64 {
    println(0 as i64)
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_path = first.dir.join("ffi.so").to_string_lossy().into_owned();
    let second_path = second.dir.join("ffi.so").to_string_lossy().into_owned();
    guard.set_path(&first.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("explicit-binding contract fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize explicit-binding contract MIR");
    let bytecode = compile_mir_program(&mir).expect("explicit-binding contract bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(first_path.clone());
    let first_error = vm
        .run_value()
        .expect_err("checked explicit library A must reject its violating result");
    assert_eq!(first_error.code(), "E0808");
    assert!(first_error.to_string().contains("FFI postcondition failed"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.set_canonical_ffi_library_path(second_path.clone());
    let second_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped explicit library B must share the contract failure boundary");
    assert_eq!(second_error.code(), "E0808");
    assert!(second_error
        .to_string()
        .contains("FFI postcondition failed"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        2,
        "switching to B must load it before the host-result contract failure"
    );

    vm.set_verify_ffi(false);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("unchecked direct entry must recover on explicit B"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n23\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_verify_ffi(true);
    vm.set_canonical_ffi_library_path(first_path);
    let third_error = vm
        .run_value()
        .expect_err("re-enabled checks must reject after rebinding back to A");
    assert_eq!(third_error.code(), "E0808");
    assert!(third_error.to_string().contains("FFI postcondition failed"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_verify_ffi(false);
    assert_eq!(
        vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
            .expect("unchecked wrapped entry must recover on explicit A"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_contract_failure_clear_binding_falls_back_without_cache_duplication() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64 ensures: result == value; }
func main() -> i64 {
    println(0 as i64)
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let second_path = second.dir.join("ffi.so").to_string_lossy().into_owned();
    guard.set_path(&first.dir.join("ffi.so"));

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("clear-binding contract fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize clear-binding contract MIR");
    let bytecode = compile_mir_program(&mir).expect("clear-binding contract bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(second_path.clone());
    let explicit_error = vm
        .run_value()
        .expect_err("checked explicit B must reject its violating result");
    assert_eq!(explicit_error.code(), "E0808");
    assert!(explicit_error
        .to_string()
        .contains("FFI postcondition failed"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.clear_canonical_ffi_library_path();
    let fallback_error = vm
        .call_named("function:main", Vec::new())
        .expect_err("cleared binding must fail against violating environment A");
    assert_eq!(fallback_error.code(), "E0808");
    assert_eq!(fallback_error.to_string(), explicit_error.to_string());
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        2,
        "clearing after explicit B failure must load environment A exactly once"
    );

    vm.set_verify_ffi(false);
    assert_eq!(
        vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
            .expect("unchecked wrapped entry must recover through environment A"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(second_path);
    assert_eq!(
        vm.run_value()
            .expect("unchecked run must recover after explicit B rebinding"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n23\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_verify_ffi(true);
    let rebound_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("re-enabled wrapped entry must reject explicit B again");
    assert_eq!(rebound_error.code(), "E0808");
    assert!(rebound_error
        .to_string()
        .contains("FFI postcondition failed"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.clear_canonical_ffi_library_path();
    vm.set_verify_ffi(false);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("cleared binding must reuse environment A after explicit B failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_requires_failure_precedes_library_load() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64 requires: value >= 0; }
func invoke(value: i64) -> i64 {
    println(0 as i64)
    println(mir_ffi_rebindable(value))
    0
}
func main() -> i64 {
    invoke(-1 as i64)
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let second_path = second.dir.join("ffi.so").to_string_lossy().into_owned();
    guard.set_path(&first.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("explicit-binding requires fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize explicit-binding requires MIR");
    let bytecode = compile_mir_program(&mir).expect("explicit-binding requires bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(second_path.clone());
    let first_error = vm
        .call_named("function:invoke", vec![Value::Int(-1)])
        .expect_err("explicit B requires failure must precede its library load");
    assert_eq!(first_error.code(), "E0808");
    assert!(first_error.to_string().contains("precondition"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        0,
        "a failed requires predicate must not load the explicit library"
    );

    let wrapped_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must preserve the pre-load requires boundary");
    assert_eq!(wrapped_error.code(), "E0808");
    assert!(wrapped_error.to_string().contains("precondition"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.clear_canonical_ffi_library_path();
    vm.set_verify_ffi(false);
    assert_eq!(
        vm.call_named("function:invoke", vec![Value::Int(-1)])
            .expect("unchecked direct entry must execute through environment A"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n10\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.set_canonical_ffi_library_path(second_path);
    assert_eq!(
        vm.call_named("function:invoke", vec![Value::Int(1)])
            .expect("unchecked direct entry must execute through explicit B"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n23\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_verify_ffi(true);
    let second_requires_error = vm
        .call_named("function:invoke", vec![Value::Int(-1)])
        .expect_err("re-enabled requires must fail before a cached-library call");
    assert_eq!(second_requires_error.code(), "E0808");
    assert!(second_requires_error.to_string().contains("precondition"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.clear_canonical_ffi_library_path();
    vm.set_verify_ffi(false);
    assert_eq!(
        vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
            .expect("unchecked wrapped entry must recover through environment A"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(vm.stdout(), "0\n10\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_invalid_explicit_binding_failure_recovers_without_cache_pollution() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(0 as i64)
    println(mir_ffi_rebindable(1 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let first_path = first.dir.join("ffi.so").to_string_lossy().into_owned();
    let invalid_path = first.dir.to_string_lossy().into_owned();
    guard.set_path(&first.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("invalid explicit-binding fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize invalid explicit-binding MIR");
    let bytecode = compile_mir_program(&mir).expect("invalid explicit-binding bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(invalid_path.clone());
    let invalid_error = vm
        .run_value()
        .expect_err("an invalid explicit path must fail before loading a library");
    assert_eq!(invalid_error.code(), "E0800");
    assert!(
        invalid_error.to_string().contains("failed to load"),
        "{invalid_error}"
    );
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.set_canonical_ffi_library_path(first_path.clone());
    assert_eq!(
        vm.run_value()
            .expect("valid explicit path must recover after invalid-path failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.set_canonical_ffi_library_path(invalid_path);
    let wrapped_invalid_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must preserve the invalid-path load failure");
    assert_eq!(wrapped_invalid_error.code(), "E0800");
    assert!(wrapped_invalid_error.to_string().contains("failed to load"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.clear_canonical_ffi_library_path();
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("cleared invalid binding must recover through environment A"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_multisite_failure_recovery_preserves_receipts() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 {
    println(0 as i64)
    println(mir_ffi_rebindable(1 as i64))
    println(mir_ffi_rebindable(2 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let missing = library_fixture(counter + 2, MISSING_SYMBOL_C_SOURCE);
    let first_path = first.dir.join("ffi.so").to_string_lossy().into_owned();
    let second_path = second.dir.join("ffi.so").to_string_lossy().into_owned();
    let missing_path = missing.dir.join("ffi.so").to_string_lossy().into_owned();
    guard.set_path(&first.dir.join("ffi.so"));

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("explicit multisite fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize explicit multisite MIR");
    let bytecode = compile_mir_program(&mir).expect("explicit multisite bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(missing_path);
    let missing_error = vm
        .run_value()
        .expect_err("explicit missing library must fail at the first call site");
    assert_eq!(missing_error.code(), "E0800");
    assert!(missing_error
        .to_string()
        .contains("failed to find canonical MIR FFI symbol"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.set_canonical_ffi_library_path(second_path);
    assert_eq!(
        vm.run_value()
            .expect("explicit B must recover both call sites"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n23\n24\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(first_path);
    assert_eq!(
        vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
            .expect("wrapped entry must recover both call sites on explicit A"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(vm.stdout(), "0\n12\n13\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 3);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_second_site_failure_recovers_without_cache_drift() {
    const FIRST_ONLY_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_first(int64_t value) { return value + 30; }
"#;
    const BOTH_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_first(int64_t value) { return value + 40; }
int64_t mir_ffi_second(int64_t value) { return value + 50; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_first(value: i64) -> i64;
    func mir_ffi_second(value: i64) -> i64;
}
func main() -> i64 {
    println(0 as i64)
    println(mir_ffi_first(1 as i64))
    println(mir_ffi_second(2 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, FIRST_ONLY_C_SOURCE);
    let both = library_fixture(counter + 1, BOTH_C_SOURCE);
    let first_path = first.dir.join("ffi.so").to_string_lossy().into_owned();
    let both_path = both.dir.join("ffi.so").to_string_lossy().into_owned();
    guard.set_path(&first.dir.join("ffi.so"));

    let checked =
        crate::core::check_program(&super::parse(SOURCE)).expect("explicit second-site fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize explicit second-site MIR");
    let bytecode = compile_mir_program(&mir).expect("explicit second-site bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(first_path.clone());
    let first_error = vm
        .run_value()
        .expect_err("first-only library must fail at the second call site");
    assert_eq!(first_error.code(), "E0800");
    assert!(first_error.to_string().contains("mir_ffi_second"));
    assert_eq!(vm.stdout(), "0\n31\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.set_canonical_ffi_library_path(both_path.clone());
    assert_eq!(
        vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
            .expect("complete library must recover both explicit call sites"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(vm.stdout(), "0\n41\n52\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(first_path);
    let second_error = vm
        .run_value()
        .expect_err("switching back must reproduce the second-site failure");
    assert_eq!(second_error.code(), "E0800");
    assert_eq!(second_error.to_string(), first_error.to_string());
    assert_eq!(vm.stdout(), "0\n31\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(both_path);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("direct entry must recover after repeated second-site failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n41\n52\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_repeated_multisite_entry_alternation_preserves_snapshots() {
    const FIRST_ONLY_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_first(int64_t value) { return value + 60; }
"#;
    const BOTH_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_first(int64_t value) { return value + 70; }
int64_t mir_ffi_second(int64_t value) { return value + 80; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_first(value: i64) -> i64;
    func mir_ffi_second(value: i64) -> i64;
}
func main() -> i64 {
    println(0 as i64)
    println(mir_ffi_first(1 as i64))
    println(mir_ffi_second(2 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, FIRST_ONLY_C_SOURCE);
    let both = library_fixture(counter + 1, BOTH_C_SOURCE);
    let first_path = first.dir.join("ffi.so").to_string_lossy().into_owned();
    let both_path = both.dir.join("ffi.so").to_string_lossy().into_owned();
    guard.set_path(&first.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("explicit alternating multisite fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize alternating multisite MIR");
    let bytecode = compile_mir_program(&mir).expect("alternating multisite bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(first_path.clone());
    let first_error = vm
        .run_value()
        .expect_err("first-only library must fail at the second call site");
    assert_eq!(first_error.code(), "E0800");
    assert!(first_error.to_string().contains("mir_ffi_second"));
    assert_eq!(vm.stdout(), "0\n61\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.set_canonical_ffi_library_path(both_path.clone());
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("direct entry must recover both call sites"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n71\n82\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(first_path.clone());
    let wrapped_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must repeat the second-site failure");
    assert_eq!(wrapped_error.code(), "E0800");
    assert_eq!(wrapped_error.to_string(), first_error.to_string());
    assert_eq!(vm.stdout(), "0\n61\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(both_path.clone());
    assert_eq!(
        vm.run_value()
            .expect("run_value must recover after wrapped failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n71\n82\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(first_path);
    let direct_error = vm
        .call_named("function:main", Vec::new())
        .expect_err("direct entry must repeat the failure after recovery");
    assert_eq!(direct_error.code(), "E0800");
    assert_eq!(direct_error.to_string(), first_error.to_string());
    assert_eq!(vm.stdout(), "0\n61\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(both_path);
    assert_eq!(
        vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
            .expect("wrapped entry must recover after repeated direct failure"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(vm.stdout(), "0\n71\n82\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_clear_rebind_multisite_fallback_keeps_cache_identity() {
    const FIRST_ONLY_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_first(int64_t value) { return value + 90; }
"#;
    const BOTH_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_first(int64_t value) { return value + 100; }
int64_t mir_ffi_second(int64_t value) { return value + 110; }
"#;
    const ENVIRONMENT_C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_first(int64_t value) { return value + 120; }
int64_t mir_ffi_second(int64_t value) { return value + 130; }
"#;
    const SOURCE: &str = r#"
extern "C" {
    func mir_ffi_first(value: i64) -> i64;
    func mir_ffi_second(value: i64) -> i64;
}
func main() -> i64 {
    println(0 as i64)
    println(mir_ffi_first(1 as i64))
    println(mir_ffi_second(2 as i64))
    0
}
"#;

    let mut guard = super::FfiEnvGuard::lock();
    // Reserve a disjoint namespace because this test owns three fixture paths
    // and the parallel suite has older tests that derive `counter + 1`/`+2`.
    let counter = super::E2E_COUNTER
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .saturating_add(1_000_000_000);
    let first = library_fixture(counter, FIRST_ONLY_C_SOURCE);
    let both = library_fixture(counter + 1, BOTH_C_SOURCE);
    let environment = library_fixture(counter + 2, ENVIRONMENT_C_SOURCE);
    let first_path = first.dir.join("ffi.so").to_string_lossy().into_owned();
    let both_path = both.dir.join("ffi.so").to_string_lossy().into_owned();
    guard.set_path(&environment.dir.join("ffi.so"));

    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("explicit clear/rebind multisite fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize clear/rebind multisite MIR");
    let bytecode = compile_mir_program(&mir).expect("clear/rebind multisite bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    vm.set_canonical_ffi_library_path(first_path.clone());
    let first_error = vm
        .run_value()
        .expect_err("first-only explicit binding must fail at the second site");
    assert_eq!(first_error.code(), "E0800");
    assert!(first_error.to_string().contains("mir_ffi_second"));
    assert_eq!(vm.stdout(), "0\n91\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.set_canonical_ffi_library_path(both_path.clone());
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("complete explicit binding must recover both sites"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n101\n112\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.set_canonical_ffi_library_path(first_path.clone());
    let repeated_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("first-only binding must fail again through wrapped entry");
    assert_eq!(repeated_error.code(), "E0800");
    assert_eq!(repeated_error.to_string(), first_error.to_string());
    assert_eq!(vm.stdout(), "0\n91\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    vm.clear_canonical_ffi_library_path();
    assert_eq!(
        vm.run_value()
            .expect("clearing explicit binding must restore environment fallback"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n121\n132\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        3,
        "environment fallback must load its distinct library exactly once"
    );

    vm.set_canonical_ffi_library_path(first_path);
    let rebound_error = vm
        .call_named("function:main", Vec::new())
        .expect_err("rebinding first-only library must preserve second-site failure");
    assert_eq!(rebound_error.code(), "E0800");
    assert_eq!(rebound_error.to_string(), first_error.to_string());
    assert_eq!(vm.stdout(), "0\n91\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 3);

    vm.clear_canonical_ffi_library_path();
    assert_eq!(
        vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
            .expect("wrapped fallback must recover after repeated rebind failure"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(vm.stdout(), "0\n121\n132\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 3);

    vm.set_canonical_ffi_library_path(both_path);
    assert_eq!(
        vm.run_value()
            .expect("complete explicit binding must reuse its cached handle"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n101\n112\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 3);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_ignores_environment_changes_until_cleared() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_path = first.dir.join("ffi.so");
    let second_path = second.dir.join("ffi.so");
    guard.set_path(&second_path);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("environment-change isolation fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize environment-change isolation MIR");
    let bytecode = compile_mir_program(&mir).expect("environment-change isolation bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut explicit_vm = BytecodeVM::new(bytecode.clone());
    let mut environment_vm = BytecodeVM::new(bytecode);

    explicit_vm.set_canonical_ffi_library_path(first_path.to_string_lossy().into_owned());
    assert_eq!(
        explicit_vm.run_value().expect("explicit A run"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "12\n");
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    guard.set_path(&second_path);
    assert_eq!(
        explicit_vm
            .call_named("function:main", Vec::new())
            .expect("explicit A must ignore a changed environment path"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "12\n");
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(
        environment_vm
            .run_value()
            .expect("unbound VM must observe environment B"),
        Value::Int(0)
    );
    assert_eq!(environment_vm.stdout(), "23\n");
    assert_eq!(environment_vm.debug_canonical_ffi_loaded_library_count(), 1);

    explicit_vm.clear_canonical_ffi_library_path();
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("cleared explicit binding must observe environment B"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "23\n");
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);

    guard.set_path(&first_path);
    assert_eq!(
        environment_vm
            .call_named("function:main", Vec::new())
            .expect("unbound VM must observe environment A after the next change"),
        Value::Int(0)
    );
    assert_eq!(environment_vm.stdout(), "12\n");
    assert_eq!(environment_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(explicit_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(environment_vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_nul_path_fails_without_cache_pollution() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("NUL explicit-binding fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize NUL-path MIR");
    let bytecode = compile_mir_program(&mir).expect("NUL-path bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    let nul_path = format!("{}\0tail", library.to_string_lossy());
    vm.set_canonical_ffi_library_path(nul_path);
    let error = vm
        .run_value()
        .expect_err("an explicit path containing NUL must fail at library loading");
    assert_eq!(error.code(), "E0800");
    assert!(error.to_string().contains("failed to load"), "{error}");
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    vm.clear_canonical_ffi_library_path();
    assert_eq!(
        vm.run_value()
            .expect("clearing NUL binding must recover through environment A"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_nul_after_cached_library_preserves_cache_identity() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("cached NUL explicit-binding fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize cached NUL-path MIR");
    let bytecode = compile_mir_program(&mir).expect("cached NUL-path bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    assert_eq!(
        vm.run_value()
            .expect("initial environment binding must load"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.set_canonical_ffi_library_path(format!("{}\0tail", library.to_string_lossy()));
    let error = vm
        .run_value()
        .expect_err("NUL rebinding must fail without disturbing a cached library");
    assert_eq!(error.code(), "E0800");
    assert!(error.to_string().contains("failed to load"), "{error}");
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.clear_canonical_ffi_library_path();
    assert_eq!(
        vm.run_value()
            .expect("clearing NUL rebinding must reuse environment A"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[test]
fn scalar_ffi_explicit_binding_empty_and_whitespace_paths_preserve_cache_identity() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("empty-path explicit-binding fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize empty-path explicit-binding MIR");
    let bytecode = compile_mir_program(&mir).expect("empty-path explicit-binding bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    assert_eq!(
        vm.run_value()
            .expect("initial environment binding must load"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    for malformed_path in ["", "   "] {
        vm.set_canonical_ffi_library_path(malformed_path);
        let error = vm
            .run_value()
            .expect_err("empty or whitespace binding must fail before a load");
        assert_eq!(error.code(), "E0800");
        assert!(error.to_string().contains("failed to load"), "{error}");
        assert_eq!(vm.stdout(), "0\n");
        assert_eq!(vm.debug_stack_state(), (0, 0));
        assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

        vm.clear_canonical_ffi_library_path();
        assert_eq!(
            vm.run_value()
                .expect("clearing malformed binding must reuse environment A"),
            Value::Int(0)
        );
        assert_eq!(vm.stdout(), "0\n12\n");
        assert_eq!(vm.debug_stack_state(), (0, 0));
        assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
        assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
    }
}

#[test]
fn scalar_ffi_environment_empty_and_whitespace_paths_preserve_cache_identity() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("empty environment-path fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize empty environment-path MIR");
    let bytecode = compile_mir_program(&mir).expect("empty environment-path bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    assert_eq!(
        vm.run_value()
            .expect("initial environment binding must load"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    for malformed_path in [std::path::Path::new(""), std::path::Path::new("   ")] {
        guard.set_path(malformed_path);
        let error = vm
            .run_value()
            .expect_err("empty or whitespace environment path must fail before a load");
        assert_eq!(error.code(), "E0800");
        assert!(error.to_string().contains("failed to load"), "{error}");
        assert_eq!(vm.stdout(), "0\n");
        assert_eq!(vm.debug_stack_state(), (0, 0));
        assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    }

    guard.set_path(&library);
    assert_eq!(
        vm.run_value()
            .expect("restoring environment A must reuse cached handle"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_environment_non_utf8_path_fails_without_cache_pollution() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("non-UTF-8 environment-path fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize non-UTF-8 environment-path MIR");
    let bytecode = compile_mir_program(&mir).expect("non-UTF-8 environment-path bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    assert_eq!(
        vm.run_value()
            .expect("initial environment binding must load"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::env::set_var(
        "MIMI_FFI_LIB",
        OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff, b'.', b's', b'o']),
    );
    let error = vm
        .run_value()
        .expect_err("non-UTF-8 environment path must fail closed");
    assert_eq!(error.code(), "E0800");
    assert!(error.to_string().contains("not valid UTF-8"), "{error}");
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    guard.set_path(&library);
    assert_eq!(
        vm.run_value()
            .expect("restoring environment A must reuse cached handle"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_non_utf8_environment_does_not_cross_vm_explicit_binding() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let second = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_path = first.dir.join("ffi.so");
    let second_path = second.dir.join("ffi.so");
    guard.set_path(&first_path);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("cross-VM non-UTF-8 environment fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize cross-VM non-UTF-8 environment MIR");
    let bytecode = compile_mir_program(&mir).expect("cross-VM non-UTF-8 environment bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut explicit_vm = BytecodeVM::new(bytecode.clone());
    let mut fallback_vm = BytecodeVM::new(bytecode);

    explicit_vm.set_canonical_ffi_library_path(first_path.to_string_lossy().into_owned());
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit VM must load library A"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "12\n");
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::env::set_var(
        "MIMI_FFI_LIB",
        OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xfe, b'.', b's', b'o']),
    );
    assert_eq!(
        explicit_vm
            .call_named("function:main", Vec::new())
            .expect("explicit VM must ignore malformed global environment"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "12\n");
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    let fallback_error = fallback_vm
        .run_value()
        .expect_err("unbound VM must reject malformed environment bytes");
    assert_eq!(fallback_error.code(), "E0800");
    assert!(fallback_error.to_string().contains("not valid UTF-8"));
    assert_eq!(fallback_vm.stdout(), "");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 0);

    guard.set_path(&second_path);
    assert_eq!(
        fallback_vm
            .run_value()
            .expect("restored environment B must load for the unbound VM"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "23\n");
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit VM must remain bound to A"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "12\n");
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(explicit_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(fallback_vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_cached_binding_survives_same_path_replacement() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let replacement = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_path = first.dir.join("ffi.so");
    let replacement_path = replacement.dir.join("ffi.so");
    guard.set_path(&first_path);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("same-path replacement fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize same-path replacement MIR");
    let bytecode = compile_mir_program(&mir).expect("same-path replacement bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut cached_vm = BytecodeVM::new(bytecode.clone());

    assert_eq!(
        cached_vm
            .run_value()
            .expect("initial VM must load library A"),
        Value::Int(0)
    );
    assert_eq!(cached_vm.stdout(), "12\n");
    assert_eq!(cached_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&replacement_path, &first_path)
        .expect("replacement library must atomically take over the binding path");
    assert_eq!(
        cached_vm
            .run_value()
            .expect("cached VM must keep the original loaded handle"),
        Value::Int(0)
    );
    assert_eq!(cached_vm.stdout(), "12\n");
    assert_eq!(cached_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(cached_vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_failed_load_reopens_same_path_after_library_appears() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    let pending = fixture.dir.join("ffi.pending.so");
    guard.set_path(&library);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("failed-load reopen fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize failed-load reopen MIR");
    let bytecode = compile_mir_program(&mir).expect("failed-load reopen bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    std::fs::rename(&library, &pending).expect("fixture library must be temporarily hidden");
    let missing = vm
        .run_value()
        .expect_err("missing binding path must fail before caching");
    assert_eq!(missing.code(), "E0800");
    assert!(missing.to_string().contains("failed to load"), "{missing}");
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    std::fs::rename(&pending, &library).expect("fixture library must reappear at its binding path");
    assert_eq!(
        vm.run_value()
            .expect("same VM must reopen a path that appears after failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_failed_load_entrypoints_reopen_without_cache_poison() {
    let _guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    let pending = fixture.dir.join("ffi.pending.so");

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("entrypoint reopen fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize entrypoint reopen MIR");
    let bytecode = compile_mir_program(&mir).expect("entrypoint reopen bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);
    vm.set_canonical_ffi_library_path(library.to_string_lossy().into_owned());

    std::fs::rename(&library, &pending).expect("fixture library must be temporarily hidden");
    let direct_error = vm
        .call_named("function:main", Vec::new())
        .expect_err("direct entry must fail while its binding path is absent");
    assert_eq!(direct_error.code(), "E0800");
    assert!(
        direct_error.to_string().contains("failed to load"),
        "{direct_error}"
    );
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    let wrapped_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must fail while its binding path is absent");
    assert_eq!(wrapped_error.code(), "E0800");
    assert!(
        wrapped_error.to_string().contains("failed to load"),
        "{wrapped_error}"
    );
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    let run_error = vm
        .run_value()
        .expect_err("run entry must fail while its binding path is absent");
    assert_eq!(run_error.code(), "E0800");
    assert!(
        run_error.to_string().contains("failed to load"),
        "{run_error}"
    );
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);

    std::fs::rename(&pending, &library).expect("fixture library must reappear at its binding path");
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("direct entry must reopen the restored path"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&library, &pending).expect("cached fixture must be temporarily hidden");
    assert_eq!(
        vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
            .expect("cached handle must survive a removed path"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&pending, &library).expect("fixture library must be restored for final run");
    assert_eq!(
        vm.run_value()
            .expect("final run must reuse restored handle"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_failed_path_reopens_after_new_inode_repair() {
    let _guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let anchor = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let repair = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let target = anchor.dir.join("late.so");
    let repair_path = repair.dir.join("ffi.so");

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("new-inode repair fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize new-inode repair MIR");
    let bytecode = compile_mir_program(&mir).expect("new-inode repair bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);
    vm.set_canonical_ffi_library_path(target.to_string_lossy().into_owned());

    let first_error = vm
        .run_value()
        .expect_err("an absent path must fail before any cache entry exists");
    assert_eq!(first_error.code(), "E0800");
    assert!(
        first_error.to_string().contains("failed to load"),
        "{first_error}"
    );
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert!(!target.exists());

    std::fs::rename(&repair_path, &target).expect("repair library must appear at the failed path");
    assert!(target.is_file());
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("same VM must load a repaired path with a new inode"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n23\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_environment_failed_path_reopens_after_new_inode_repair() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let anchor = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let repair = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let target = anchor.dir.join("environment-late.so");
    let repair_path = repair.dir.join("ffi.so");
    guard.set_path(&target);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("environment new-inode repair fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize environment new-inode repair MIR");
    let bytecode = compile_mir_program(&mir).expect("environment new-inode repair bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);

    let first_error = vm
        .run_value()
        .expect_err("an absent environment path must fail before caching");
    assert_eq!(first_error.code(), "E0800");
    assert!(
        first_error.to_string().contains("failed to load"),
        "{first_error}"
    );
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 0);
    assert!(!target.exists());

    std::fs::rename(&repair_path, &target)
        .expect("repair library must appear at the failed environment path");
    guard.set_path(&target);
    assert_eq!(
        vm.run_value()
            .expect("environment fallback must load the repaired new inode"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n23\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_environment_repair_does_not_cross_explicit_vm_binding() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let explicit = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let repair = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let explicit_path = explicit.dir.join("ffi.so");
    let target = explicit.dir.join("environment-late.so");
    let repair_path = repair.dir.join("ffi.so");
    guard.set_path(&target);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("environment/explicit VM repair fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize environment/explicit VM repair MIR");
    let bytecode = compile_mir_program(&mir).expect("environment/explicit VM repair bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut explicit_vm = BytecodeVM::new(bytecode.clone());
    let mut fallback_vm = BytecodeVM::new(bytecode);
    explicit_vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());

    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit VM must use library A despite the missing environment path"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n12\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    let missing = fallback_vm
        .run_value()
        .expect_err("unbound VM must fail while its environment path is absent");
    assert_eq!(missing.code(), "E0800");
    assert!(missing.to_string().contains("failed to load"), "{missing}");
    assert_eq!(fallback_vm.stdout(), "0\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 0);

    std::fs::rename(&repair_path, &target)
        .expect("repair library must appear at the environment binding path");
    guard.set_path(&target);
    assert_eq!(
        fallback_vm
            .call_function_wrap_ok(fallback_vm.program().entry, &[], Value::Unit)
            .expect("unbound VM must recover through the repaired environment path"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);

    assert_eq!(
        explicit_vm
            .call_named("function:main", Vec::new())
            .expect("explicit VM must remain isolated from the repaired environment path"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n12\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(explicit_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(fallback_vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_cached_environment_binding_stays_vm_local_across_clear_rebind() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .saturating_add(1_000_000_000);
    let explicit = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let environment = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let explicit_path = explicit.dir.join("ffi.so");
    let environment_path = environment.dir.join("ffi.so");
    let pending_path = environment.dir.join("environment-cached.pending.so");
    guard.set_path(&environment_path);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("cached environment clear/rebind fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize cached environment clear/rebind MIR");
    let bytecode = compile_mir_program(&mir).expect("cached environment clear/rebind bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut explicit_vm = BytecodeVM::new(bytecode.clone());
    let mut fallback_vm = BytecodeVM::new(bytecode);

    explicit_vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit VM must initially load library A"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n12\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    assert_eq!(
        fallback_vm
            .run_value()
            .expect("fallback VM must cache environment library B"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&environment_path, &pending_path)
        .expect("cached environment library must be temporarily hidden");
    guard.set_path(&environment_path);
    assert_eq!(
        fallback_vm
            .call_function_wrap_ok(fallback_vm.program().entry, &[], Value::Unit)
            .expect("fallback VM must retain its cached B handle after removal"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);
    let fallback_descriptor_snapshot = fallback_vm.program().canonical_ffi.clone();

    // Release the fallback VM's host handle before probing the explicit VM.
    // Linux may let a second dlopen("path") resurrect an already loaded
    // object even after the directory entry is renamed away; dropping the
    // first VM keeps this assertion about Mimi's VM-local cache rather than
    // the platform loader's process-wide handle table.
    drop(fallback_vm);

    explicit_vm.clear_canonical_ffi_library_path();
    let clear_error = explicit_vm
        .run_value()
        .expect_err("cleared explicit VM must not borrow fallback VM's cached handle");
    assert_eq!(clear_error.code(), "E0800");
    assert!(
        clear_error.to_string().contains("failed to load"),
        "{clear_error}"
    );
    assert_eq!(explicit_vm.stdout(), "0\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    explicit_vm.set_canonical_ffi_library_path(environment_path.to_string_lossy().into_owned());
    let rebind_error = explicit_vm
        .call_named("function:main", Vec::new())
        .expect_err("explicit rebind must still fail while its path is absent");
    assert_eq!(rebind_error.code(), "E0800");
    assert_eq!(rebind_error.to_string(), clear_error.to_string());
    assert_eq!(explicit_vm.stdout(), "0\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&pending_path, &environment_path)
        .expect("environment library must be restored for explicit rebind");
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit VM must load B after the repaired rebind path appears"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n23\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);

    explicit_vm.clear_canonical_ffi_library_path();
    assert_eq!(
        explicit_vm
            .call_function_wrap_ok(explicit_vm.program().entry, &[], Value::Unit)
            .expect("cleared binding must reuse its own cached environment handle"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(explicit_vm.stdout(), "0\n23\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(explicit_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(fallback_descriptor_snapshot, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_cached_fallback_keeps_replaced_path_until_clear_rebind() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .saturating_add(1_000_000_000);
    let explicit = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let environment = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let replacement = library_fixture(counter + 2, REBINDABLE_SYMBOL_C_C_SOURCE);
    let explicit_path = explicit.dir.join("ffi.so");
    let environment_path = environment.dir.join("ffi.so");
    let replacement_path = replacement.dir.join("ffi.so");
    let pending_path = environment.dir.join("environment-replaced.pending.so");
    guard.set_path(&environment_path);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("cached fallback replacement fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize cached fallback replacement MIR");
    let bytecode = compile_mir_program(&mir).expect("cached fallback replacement bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut fallback_vm = BytecodeVM::new(bytecode.clone());
    let mut explicit_vm = BytecodeVM::new(bytecode);

    assert_eq!(
        fallback_vm
            .run_value()
            .expect("fallback VM must initially cache environment library B"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);

    explicit_vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit VM must initially cache library A"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n12\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&environment_path, &pending_path)
        .expect("cached environment library must be moved before replacement");
    std::fs::rename(&replacement_path, &environment_path)
        .expect("replacement library must occupy the cached environment path");
    guard.set_path(&environment_path);
    assert_eq!(
        fallback_vm
            .call_named("function:main", Vec::new())
            .expect("cached fallback must keep library B after path replacement"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);
    let fallback_descriptor_snapshot = fallback_vm.program().canonical_ffi.clone();

    // Drop the B handle before clearing the other VM.  This ensures the
    // subsequent load observes the replacement C object at the same path,
    // rather than a process-wide loader handle retained by the fallback VM.
    drop(fallback_vm);

    explicit_vm.clear_canonical_ffi_library_path();
    assert_eq!(
        explicit_vm
            .call_function_wrap_ok(explicit_vm.program().entry, &[], Value::Unit)
            .expect("clear must load replacement library C from the environment path"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(explicit_vm.stdout(), "0\n34\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);

    explicit_vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit rebind must reuse its cached A handle"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n12\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);

    explicit_vm.clear_canonical_ffi_library_path();
    assert_eq!(
        explicit_vm
            .call_named("function:main", Vec::new())
            .expect("cleared binding must reuse its cached replacement C handle"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n34\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(explicit_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(fallback_descriptor_snapshot, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_repeated_path_replacements_preserve_cached_entrypoint_identity() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .saturating_add(1_000_000_000);
    let explicit = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let environment = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let first_replacement = library_fixture(counter + 2, REBINDABLE_SYMBOL_C_C_SOURCE);
    let second_replacement = library_fixture(counter + 3, REBINDABLE_SYMBOL_D_C_SOURCE);
    let explicit_path = explicit.dir.join("ffi.so");
    let environment_path = environment.dir.join("ffi.so");
    let first_replacement_path = first_replacement.dir.join("ffi.so");
    let second_replacement_path = second_replacement.dir.join("ffi.so");
    let first_pending_path = environment.dir.join("environment-repeated-one.pending.so");
    let second_pending_path = environment.dir.join("environment-repeated-two.pending.so");
    let third_pending_path = environment
        .dir
        .join("environment-repeated-three.pending.so");
    guard.set_path(&environment_path);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("repeated replacement fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize repeated replacement MIR");
    let bytecode = compile_mir_program(&mir).expect("repeated replacement bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut fallback_vm = BytecodeVM::new(bytecode.clone());
    let mut explicit_vm = BytecodeVM::new(bytecode);

    assert_eq!(
        fallback_vm
            .run_value()
            .expect("fallback VM must initially cache library B"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&environment_path, &first_pending_path)
        .expect("library B must move before the first replacement");
    std::fs::rename(&first_replacement_path, &environment_path)
        .expect("library C must occupy the environment path");
    guard.set_path(&environment_path);
    assert_eq!(
        fallback_vm
            .call_function_wrap_ok(fallback_vm.program().entry, &[], Value::Unit)
            .expect("first replacement must not change the cached fallback handle"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&environment_path, &second_pending_path)
        .expect("library C must move before the second replacement");
    std::fs::rename(&second_replacement_path, &environment_path)
        .expect("library D must occupy the environment path");
    guard.set_path(&environment_path);
    assert_eq!(
        fallback_vm
            .call_named("function:main", Vec::new())
            .expect("second replacement must still use cached library B"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);
    let fallback_descriptor_snapshot = fallback_vm.program().canonical_ffi.clone();
    drop(fallback_vm);

    explicit_vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit VM must cache library A"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n12\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    explicit_vm.clear_canonical_ffi_library_path();
    assert_eq!(
        explicit_vm
            .call_named("function:main", Vec::new())
            .expect("clear must load the second replacement library D"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n45\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);

    std::fs::rename(&environment_path, &third_pending_path)
        .expect("library D must move before the cached replacement check");
    std::fs::rename(&first_pending_path, &environment_path)
        .expect("library B must be restored at the environment path");
    guard.set_path(&environment_path);
    assert_eq!(
        explicit_vm
            .call_function_wrap_ok(explicit_vm.program().entry, &[], Value::Unit)
            .expect("cached replacement handle must survive a later path replacement"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(explicit_vm.stdout(), "0\n45\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);

    explicit_vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit rebinding must reuse cached library A"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n12\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);

    explicit_vm.clear_canonical_ffi_library_path();
    assert_eq!(
        explicit_vm
            .call_named("function:main", Vec::new())
            .expect("cleared binding must reuse cached library D after repeated replacements"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n45\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(explicit_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(fallback_descriptor_snapshot, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_malformed_replacement_reopens_after_failure_without_cache_drift() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .saturating_add(1_000_000_000);
    let explicit = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let environment = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let repair = library_fixture(counter + 2, REBINDABLE_SYMBOL_C_C_SOURCE);
    let explicit_path = explicit.dir.join("ffi.so");
    let environment_path = environment.dir.join("ffi.so");
    let repair_path = repair.dir.join("ffi.so");
    let pending_path = environment.dir.join("environment-malformed.pending.so");
    guard.set_path(&environment_path);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("malformed replacement fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize malformed replacement MIR");
    let bytecode = compile_mir_program(&mir).expect("malformed replacement bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut fallback_vm = BytecodeVM::new(bytecode.clone());
    let mut explicit_vm = BytecodeVM::new(bytecode);

    assert_eq!(
        fallback_vm
            .run_value()
            .expect("fallback VM must initially cache library B"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&environment_path, &pending_path)
        .expect("library B must move before malformed replacement");
    std::fs::write(&environment_path, b"this is not a shared library")
        .expect("malformed library payload must be written");
    guard.set_path(&environment_path);
    assert_eq!(
        fallback_vm
            .call_named("function:main", Vec::new())
            .expect("cached fallback must ignore malformed replacement bytes"),
        Value::Int(0)
    );
    assert_eq!(fallback_vm.stdout(), "0\n23\n");
    assert_eq!(fallback_vm.debug_stack_state(), (0, 0));
    assert_eq!(fallback_vm.debug_canonical_ffi_loaded_library_count(), 1);
    let fallback_descriptor_snapshot = fallback_vm.program().canonical_ffi.clone();
    drop(fallback_vm);

    explicit_vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("explicit VM must cache library A"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n12\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    explicit_vm.clear_canonical_ffi_library_path();
    let malformed_error = explicit_vm
        .call_function_wrap_ok(explicit_vm.program().entry, &[], Value::Unit)
        .expect_err("malformed replacement must fail through the wrapped entry");
    assert_eq!(malformed_error.code(), "E0800");
    assert!(
        malformed_error.to_string().contains("failed to load"),
        "{malformed_error}"
    );
    assert_eq!(explicit_vm.stdout(), "0\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::remove_file(&environment_path).expect("malformed replacement must be removed");
    std::fs::rename(&repair_path, &environment_path)
        .expect("repair library must replace malformed bytes at a new inode");
    guard.set_path(&environment_path);
    assert_eq!(
        explicit_vm
            .run_value()
            .expect("same VM must reopen the repaired library after malformed failure"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n34\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);

    explicit_vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());
    assert_eq!(
        explicit_vm
            .call_named("function:main", Vec::new())
            .expect("explicit A rebinding must reuse its cached handle"),
        Value::Int(0)
    );
    assert_eq!(explicit_vm.stdout(), "0\n12\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);

    explicit_vm.clear_canonical_ffi_library_path();
    assert_eq!(
        explicit_vm
            .call_function_wrap_ok(explicit_vm.program().entry, &[], Value::Unit)
            .expect("cleared binding must reuse repaired library C"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(explicit_vm.stdout(), "0\n34\n");
    assert_eq!(explicit_vm.debug_stack_state(), (0, 0));
    assert_eq!(explicit_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(explicit_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(fallback_descriptor_snapshot, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_repeated_failed_generations_preserve_cache_and_recover_after_vm_rebuild() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .saturating_add(1_000_000_000);
    let explicit = library_fixture(counter, REBINDABLE_SYMBOL_A_C_SOURCE);
    let environment = library_fixture(counter + 1, REBINDABLE_SYMBOL_B_C_SOURCE);
    let missing = library_fixture(counter + 2, MISSING_SYMBOL_C_SOURCE);
    let repair = library_fixture(counter + 3, REBINDABLE_SYMBOL_C_C_SOURCE);
    let explicit_path = explicit.dir.join("ffi.so");
    let environment_path = environment.dir.join("ffi.so");
    let missing_path = missing.dir.join("ffi.so");
    let repair_path = repair.dir.join("ffi.so");
    let malformed_pending = environment.dir.join("environment-failed-one.pending.so");
    let missing_pending = environment.dir.join("environment-failed-two.pending.so");
    let repair_pending = environment.dir.join("environment-failed-three.pending.so");
    guard.set_path(&environment_path);

    let source = r#"
extern "C" { func mir_ffi_rebindable(value: i64) -> i64; }
func main() -> i64 { println(0 as i64); println(mir_ffi_rebindable(1 as i64)); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("repeated failed-generation fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize repeated failed-generation MIR");
    let bytecode = compile_mir_program(&mir).expect("repeated failed-generation bytecode");
    assert!(bytecode.ast.is_none());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut cached_vm = BytecodeVM::new(bytecode.clone());

    assert_eq!(
        cached_vm
            .run_value()
            .expect("fallback VM must initially cache library B"),
        Value::Int(0)
    );
    assert_eq!(cached_vm.stdout(), "0\n23\n");
    assert_eq!(cached_vm.debug_stack_state(), (0, 0));
    assert_eq!(cached_vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::rename(&environment_path, &malformed_pending)
        .expect("library B must move before malformed generation");
    std::fs::write(&environment_path, b"this is not a shared library")
        .expect("malformed generation payload must be written");
    guard.set_path(&environment_path);
    assert_eq!(
        cached_vm
            .call_function_wrap_ok(cached_vm.program().entry, &[], Value::Unit)
            .expect("cached fallback must survive the malformed generation"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(cached_vm.stdout(), "0\n23\n");
    assert_eq!(cached_vm.debug_stack_state(), (0, 0));
    assert_eq!(cached_vm.debug_canonical_ffi_loaded_library_count(), 1);
    let cached_descriptor_snapshot = cached_vm.program().canonical_ffi.clone();
    drop(cached_vm);

    let mut vm = BytecodeVM::new(bytecode);
    vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());
    assert_eq!(
        vm.run_value()
            .expect("explicit VM must initially cache library A"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n12\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    vm.clear_canonical_ffi_library_path();
    let malformed_error = vm
        .call_named("function:main", Vec::new())
        .expect_err("malformed generation must fail through the named entry");
    assert_eq!(malformed_error.code(), "E0800");
    assert!(malformed_error.to_string().contains("failed to load"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);

    std::fs::remove_file(&environment_path).expect("malformed generation must be removed");
    std::fs::rename(&missing_path, &environment_path)
        .expect("missing-symbol generation must occupy the same path");
    guard.set_path(&environment_path);
    let missing_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("missing-symbol generation must fail through the wrapped entry");
    assert_eq!(missing_error.code(), "E0800");
    assert!(missing_error
        .to_string()
        .contains("failed to find canonical MIR FFI symbol"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    std::fs::rename(&environment_path, &missing_pending)
        .expect("missing-symbol generation must move before repair");
    std::fs::rename(&repair_path, &environment_path)
        .expect("repair generation must occupy the same path");
    guard.set_path(&environment_path);
    let cached_missing_error = vm
        .run_value()
        .expect_err("same VM must retain the cached missing-symbol generation");
    assert_eq!(cached_missing_error.code(), "E0800");
    assert!(cached_missing_error
        .to_string()
        .contains("failed to find canonical MIR FFI symbol"));
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 2);

    drop(vm);
    let mut recovered_vm = BytecodeVM::new(
        crate::interp::bytecode::compile_mir_program(&mir)
            .expect("rebuild AST-free bytecode after cached missing-symbol generation"),
    );
    assert_eq!(
        recovered_vm
            .call_named("function:main", Vec::new())
            .expect("rebuilt VM must load the repaired library C"),
        Value::Int(0)
    );
    assert_eq!(recovered_vm.stdout(), "0\n34\n");
    assert_eq!(recovered_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovered_vm.debug_canonical_ffi_loaded_library_count(), 1);

    recovered_vm.set_canonical_ffi_library_path(explicit_path.to_string_lossy().into_owned());
    assert_eq!(
        recovered_vm
            .call_function_wrap_ok(recovered_vm.program().entry, &[], Value::Unit)
            .expect("rebuilt VM must rebind explicit library A"),
        Value::Variant("Ok".into(), vec![Value::Int(0)])
    );
    assert_eq!(recovered_vm.stdout(), "0\n12\n");
    assert_eq!(recovered_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovered_vm.debug_canonical_ffi_loaded_library_count(), 2);

    recovered_vm.clear_canonical_ffi_library_path();
    assert_eq!(
        recovered_vm
            .run_value()
            .expect("cleared binding must reuse repaired library C"),
        Value::Int(0)
    );
    assert_eq!(recovered_vm.stdout(), "0\n34\n");
    assert_eq!(recovered_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovered_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(recovered_vm.program().canonical_ffi, descriptor_snapshot);
    assert_eq!(cached_descriptor_snapshot, descriptor_snapshot);

    std::fs::rename(&environment_path, &repair_pending)
        .expect("repaired generation must move during final cache check");
    std::fs::rename(&missing_pending, &environment_path)
        .expect("missing-symbol generation must be restored for final cache check");
    guard.set_path(&environment_path);
    assert_eq!(
        recovered_vm
            .call_named("function:main", Vec::new())
            .expect("cached repaired handle must survive later failed-generation replacement"),
        Value::Int(0)
    );
    assert_eq!(recovered_vm.stdout(), "0\n34\n");
    assert_eq!(recovered_vm.debug_stack_state(), (0, 0));
    assert_eq!(recovered_vm.debug_canonical_ffi_loaded_library_count(), 2);
    assert_eq!(recovered_vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_default_libc_fallback_matches_reference_bytecode_and_native() {
    struct Oracle;
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "labs" {
                return Err(format!("unexpected symbol {}", receipt.symbol));
            }
            match arguments {
                [MirRuntimeValue::Int(value)] if *value == -41 => Ok(MirRuntimeValue::Int(41)),
                _ => Err(format!("unexpected labs arguments {arguments:?}")),
            }
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    std::env::remove_var("MIMI_FFI_LIB");
    let source = r#"
extern "C" { func labs(value: i64) -> i64; }
func main() -> i64 { println(labs(-41 as i64)); 0 }
"#;
    let checked =
        crate::core::check_program(&super::parse(source)).expect("default libc scalar FFI fixture");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
        "default libc scalar FFI must stay on canonical admission"
    );
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize default libc scalar FFI MIR");
    assert_eq!(mir.ffi_calls().len(), 1);
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    for results in [
        crate::verifier::verify_checked(&checked, "scalar-ffi-default-libc".into()),
        crate::verifier::verify_checked_dual(&checked, "scalar-ffi-default-libc".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("default libc scalar FFI verification");
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

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&Oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference default libc scalar FFI execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "41\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free default libc scalar FFI bytecode");
    assert!(bytecode.ast.is_none());
    assert!(bytecode.extern_names.is_empty());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .expect("bytecode default libc scalar FFI execution"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "41\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("bytecode default libc cache reuse"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "41\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_default_libc");
    generator
        .compile_mir_native(&mir)
        .expect("same MIR native default libc scalar FFI lowering");
    generator
        .module
        .verify()
        .expect("valid native default libc scalar FFI module");
    let config = super::E2EConfig::default();
    let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native = super::link_and_observe_module(&generator, &config, native_counter)
        .expect("native default libc scalar FFI execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "41\n");
    assert_eq!(native.stderr, "");

    guard.set_path(std::path::Path::new(
        "/definitely/missing/mimi-default-libc-after-check.so",
    ));
    assert_eq!(
        vm.run_value()
            .expect_err("explicit missing path must fail after default fallback cache")
            .code(),
        "E0800"
    );
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    std::env::remove_var("MIMI_FFI_LIB");
    assert_eq!(
        vm.run_value()
            .expect("default libc fallback must recover after missing override"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "41\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_default_libc_zero_argument_matches_reference_bytecode_and_native() {
    struct Oracle;
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol == "sched_yield" && arguments.is_empty() {
                Ok(MirRuntimeValue::Int(0))
            } else {
                Err(format!(
                    "unexpected sched_yield receipt/arguments: {receipt:?} {arguments:?}"
                ))
            }
        }
    }

    let guard = super::FfiEnvGuard::lock();
    std::env::remove_var("MIMI_FFI_LIB");
    let source = r#"
extern "C" { func sched_yield() -> i32; }
func main() -> i64 { println(sched_yield()); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("default libc zero-argument scalar FFI fixture");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
        "default libc zero-argument scalar FFI must stay on canonical admission"
    );
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize default libc zero-argument scalar FFI MIR");
    let receipt = mir
        .ffi_calls()
        .values()
        .next()
        .expect("zero-argument default libc receipt");
    assert_eq!(receipt.symbol, "sched_yield");
    assert!(receipt.parameter_types.is_empty());
    assert!(receipt.parameter_conversions.is_empty());
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    for results in [
        crate::verifier::verify_checked(&checked, "scalar-ffi-default-libc-zero-arg".into()),
        crate::verifier::verify_checked_dual(&checked, "scalar-ffi-default-libc-zero-arg".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("default libc zero-argument scalar FFI verification");
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

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&Oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference default libc zero-argument scalar FFI execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "0\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free zero-argument scalar FFI bytecode");
    assert!(bytecode.ast.is_none());
    assert!(bytecode.extern_names.is_empty());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("bytecode zero-argument scalar FFI"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_default_libc_zero_arg");
    generator
        .compile_mir_native(&mir)
        .expect("same MIR native zero-argument scalar FFI lowering");
    generator
        .module
        .verify()
        .expect("valid native zero-argument scalar FFI module");
    let config = super::E2EConfig::default();
    let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native = super::link_and_observe_module(&generator, &config, native_counter)
        .expect("native zero-argument scalar FFI execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "0\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    drop(guard);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_default_libc_multiple_symbols_reuse_one_handle_across_consumers() {
    struct Oracle;
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), arguments) {
                ("labs", [MirRuntimeValue::Int(value)]) if *value == -41 => {
                    Ok(MirRuntimeValue::Int(41))
                }
                ("sched_yield", []) => Ok(MirRuntimeValue::Int(0)),
                _ => Err(format!(
                    "unexpected default-libc receipt/arguments: {receipt:?} {arguments:?}"
                )),
            }
        }
    }

    let guard = super::FfiEnvGuard::lock();
    std::env::remove_var("MIMI_FFI_LIB");
    let source = r#"
extern "C" {
    func labs(value: i64) -> i64;
    func sched_yield() -> i32;
}
func main() -> i64 {
    println(labs(-41 as i64))
    println(sched_yield())
    0
}
"#;
    let checked = crate::core::check_program(&super::parse(source))
        .expect("default libc multi-symbol scalar FFI fixture");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
        "default libc multi-symbol FFI must stay on canonical admission"
    );
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize default libc multi-symbol scalar FFI MIR");
    assert_eq!(mir.ffi_calls().len(), 2);
    let ordered = mir.ffi_call_entries_in_source_order();
    assert_eq!(
        ordered
            .iter()
            .map(|(_, receipt)| receipt.symbol.as_str())
            .collect::<Vec<_>>(),
        vec!["labs", "sched_yield"]
    );

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    for results in [
        crate::verifier::verify_checked(&checked, "scalar-ffi-default-libc-multi".into()),
        crate::verifier::verify_checked_dual(&checked, "scalar-ffi-default-libc-multi".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("default libc multi-symbol scalar FFI verification");
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

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&Oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference default libc multi-symbol execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "41\n0\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free default libc multi-symbol bytecode");
    assert!(bytecode.ast.is_none());
    assert!(bytecode.extern_names.is_empty());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .expect("bytecode default libc multi-symbol run"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "41\n0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(
        vm.debug_canonical_ffi_loaded_library_count(),
        1,
        "all symbols from the default libc must reuse one VM-local handle"
    );
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("bytecode default libc multi-symbol re-entry"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "41\n0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_default_libc_multi");
    generator
        .compile_mir_native(&mir)
        .expect("same MIR native default libc multi-symbol lowering");
    generator
        .module
        .verify()
        .expect("valid native default libc multi-symbol module");
    let config = super::E2EConfig::default();
    let native_counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native = super::link_and_observe_module(&generator, &config, native_counter)
        .expect("native default libc multi-symbol execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "41\n0\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
    drop(guard);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_imported_default_libc_module_preserves_receipts_and_binding() {
    use std::fs;

    struct Oracle;
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), arguments) {
                ("labs", [MirRuntimeValue::Int(value)]) if *value == -41 => {
                    Ok(MirRuntimeValue::Int(41))
                }
                ("sched_yield", []) => Ok(MirRuntimeValue::Int(0)),
                _ => Err(format!(
                    "unexpected imported default-libc receipt/arguments: {receipt:?} {arguments:?}"
                )),
            }
        }
    }

    let guard = super::FfiEnvGuard::lock();
    std::env::remove_var("MIMI_FFI_LIB");
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let project = std::env::temp_dir().join(format!(
        "mimi-canonical-ffi-import-default-libc-{}-{counter}",
        std::process::id()
    ));
    fs::create_dir_all(&project).expect("create imported default-libc project");
    let main_path = project.join("main.mimi");
    fs::write(
        &main_path,
        r#"
use libc_bindings
func main() -> i64 {
    println(call_labs(-41 as i64))
    println(call_sched())
    0
}
"#,
    )
    .expect("write imported default-libc main");
    fs::write(
        project.join("libc_bindings.mimi"),
        r#"
extern "C" {
    func labs(value: i64) -> i64;
    func sched_yield() -> i32;
}
pub func call_labs(value: i64) -> i64 { labs(value) }
pub func call_sched() -> i32 { sched_yield() }
"#,
    )
    .expect("write imported default-libc module");

    let source = fs::read_to_string(&main_path).expect("read imported default-libc main");
    let tokens = crate::lexer::Lexer::new(&source)
        .tokenize()
        .expect("lex imported default-libc main");
    let file = crate::loader::parser_for_path(tokens, &main_path)
        .expect("select imported default-libc parser")
        .parse_file()
        .expect("parse imported default-libc main");
    let mut loader = crate::loader::ModuleLoader::new(project.clone());
    loader
        .load_main_with_file(&main_path, file)
        .expect("load imported default-libc graph");
    let mut merged = loader
        .merge_all()
        .expect("merge imported default-libc graph");
    crate::loader::merge_prelude_into(&mut merged);
    let checked = crate::core::check_program(&merged).expect("check imported default-libc graph");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
        "imported default-libc declarations must stay on canonical scalar FFI admission"
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
            .expect("materialize imported default-libc route");
    assert!(
        crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route),
        "imported default-libc declarations must materialize scalar FFI"
    );
    let mir = MirProgram::from_checked_program_excluding_sources(&checked, &excluded_sources)
        .expect("materialize imported default-libc MIR");
    let ordered = mir.ffi_call_entries_in_source_order();
    assert_eq!(ordered.len(), 2);
    assert_eq!(
        ordered
            .iter()
            .map(|(_, receipt)| receipt.symbol.as_str())
            .collect::<Vec<_>>(),
        vec!["labs", "sched_yield"]
    );
    assert!(ordered.iter().all(|(_, receipt)| {
        receipt.caller.0.contains("call_labs") || receipt.caller.0.contains("call_sched")
    }));

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    for results in [
        crate::verifier::verify_checked(&checked, "scalar-ffi-imported-default-libc".into()),
        crate::verifier::verify_checked_dual(&checked, "scalar-ffi-imported-default-libc".into()),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("imported default-libc scalar FFI verification");
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

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&Oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference imported default-libc execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "41\n0\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free imported default-libc bytecode");
    assert!(bytecode.ast.is_none());
    assert!(bytecode.extern_names.is_empty());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .expect("bytecode imported default-libc execution"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "41\n0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(
        vm.call_named("function:main", Vec::new())
            .expect("bytecode imported default-libc re-entry"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "41\n0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_imported_default_libc");
    generator
        .compile_mir_native(&mir)
        .expect("native imported default-libc lowering");
    generator
        .module
        .verify()
        .expect("valid imported default-libc LLVM module");
    let config = super::E2EConfig::default();
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native imported default-libc execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "41\n0\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    fs::remove_dir_all(project).expect("remove imported default-libc project");
    drop(guard);
}

#[cfg(unix)]
#[test]
fn scalar_ffi_imported_default_libc_contract_preserves_verifier_and_consumers() {
    use crate::verifier::{ProofArtifact, VerifStatus};
    use std::fs;

    struct Oracle;
    impl MirReferenceFfiResolver for Oracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            match (receipt.symbol.as_str(), arguments) {
                ("labs", [MirRuntimeValue::Int(value)]) if *value == -41 => {
                    Ok(MirRuntimeValue::Int(41))
                }
                ("sched_yield", []) => Ok(MirRuntimeValue::Int(0)),
                _ => Err(format!(
                    "unexpected imported default-libc contract receipt/arguments: {receipt:?} {arguments:?}"
                )),
            }
        }
    }

    let guard = super::FfiEnvGuard::lock();
    std::env::remove_var("MIMI_FFI_LIB");
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let project = std::env::temp_dir().join(format!(
        "mimi-canonical-ffi-import-default-libc-contract-{}-{counter}",
        std::process::id()
    ));
    fs::create_dir_all(&project).expect("create imported default-libc contract project");
    let main_path = project.join("main.mimi");
    fs::write(
        &main_path,
        r#"
use libc_guarded
func main() -> i64 {
    println(call_labs(-41 as i64))
    println(call_sched())
    0
}
"#,
    )
    .expect("write imported default-libc contract main");
    fs::write(
        project.join("libc_guarded.mimi"),
        r#"
extern "C" {
    func labs(value: i64) -> i64 requires: value <= 0 ensures: value <= 0;
    func sched_yield() -> i32;
}
pub func call_labs(value: i64) -> i64 {
    requires: value <= 0
    labs(value)
}
pub func call_sched() -> i32 { sched_yield() }
"#,
    )
    .expect("write imported default-libc contract module");

    let source = fs::read_to_string(&main_path).expect("read imported default-libc contract main");
    let tokens = crate::lexer::Lexer::new(&source)
        .tokenize()
        .expect("lex imported default-libc contract main");
    let file = crate::loader::parser_for_path(tokens, &main_path)
        .expect("select imported default-libc contract parser")
        .parse_file()
        .expect("parse imported default-libc contract main");
    let mut loader = crate::loader::ModuleLoader::new(project.clone());
    loader
        .load_main_with_file(&main_path, file)
        .expect("load imported default-libc contract graph");
    let mut merged = loader
        .merge_all()
        .expect("merge imported default-libc contract graph");
    crate::loader::merge_prelude_into(&mut merged);
    let checked =
        crate::core::check_program(&merged).expect("check imported default-libc contract graph");
    assert!(
        crate::core::mir::classify_canonical_mir_route_admission(&checked).scalar_ffi,
        "imported default-libc contracts must stay on canonical scalar FFI admission"
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
            .expect("materialize imported default-libc contract route");
    assert!(
        crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route),
        "imported default-libc contracts must materialize scalar FFI"
    );
    let mir = MirProgram::from_checked_program_excluding_sources(&checked, &excluded_sources)
        .expect("materialize imported default-libc contract MIR");
    let ordered = mir.ffi_call_entries_in_source_order();
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].1.symbol, "labs");
    assert_eq!(ordered[1].1.symbol, "sched_yield");
    let labs_receipt = ordered[0].1;
    assert_eq!(labs_receipt.caller.0, "function:call_labs");
    assert!(
        labs_receipt.callee.0.contains("labs"),
        "merged declaration identity must retain the imported labs symbol: {}",
        labs_receipt.callee.0
    );
    assert!(labs_receipt
        .requires
        .as_ref()
        .is_some_and(|requires| requires.canonical_text().contains("le(")));
    assert!(labs_receipt
        .ensures
        .as_ref()
        .is_some_and(|ensures| ensures.canonical_text().contains("le(")));

    let receipt = mir.route_receipt("scalar-ffi-v1");
    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let mir_results = crate::verifier::verify_mir(&mir, "imported-default-libc-contract".into())
        .expect("verify imported default-libc contract MIR");
    assert!(
        mir_results.iter().all(|result| matches!(
            result.status,
            VerifStatus::Proven | VerifStatus::NoObligations
        )),
        "{mir_results:?}"
    );
    assert!(mir_results.iter().all(|result| {
        result.artifact.as_ref().is_some_and(|artifact| {
            artifact.engine == ProofArtifact::ENGINE_MIR && artifact.mir_hash == receipt.mir_digest
        })
    }));
    for results in [
        crate::verifier::verify_checked(&checked, "imported-default-libc-contract-public".into()),
        crate::verifier::verify_checked_dual(
            &checked,
            "imported-default-libc-contract-dual".into(),
        ),
        crate::verifier::verify_ffi_checked(&checked),
    ] {
        let results = results.expect("verify imported default-libc contract checked program");
        assert!(
            results.iter().all(|result| matches!(
                result.status,
                VerifStatus::Proven | VerifStatus::NoObligations
            )),
            "{results:?}"
        );
        assert!(results.iter().all(|result| {
            result.artifact.as_ref().is_some_and(|artifact| {
                artifact.engine == ProofArtifact::ENGINE_MIR
                    && artifact.mir_hash == receipt.mir_digest
            })
        }));
    }
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&Oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference imported default-libc contract execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "41\n0\n");

    let bytecode =
        compile_mir_program(&mir).expect("AST-free imported default-libc contract bytecode");
    assert!(bytecode.ast.is_none());
    assert!(bytecode.extern_names.is_empty());
    let descriptor_snapshot = bytecode.canonical_ffi.clone();
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value()
            .expect("bytecode imported default-libc contract"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "41\n0\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
    assert_eq!(vm.debug_canonical_ffi_loaded_library_count(), 1);
    assert_eq!(vm.program().canonical_ffi, descriptor_snapshot);

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_imported_default_libc_contract");
    generator
        .compile_mir_native(&mir)
        .expect("native imported default-libc contract lowering");
    generator
        .module
        .verify()
        .expect("valid imported default-libc contract LLVM module");
    let native = super::link_and_observe_module(&generator, &super::E2EConfig::default(), counter)
        .expect("native imported default-libc contract execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "41\n0\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    fs::remove_dir_all(project).expect("remove imported default-libc contract project");
    drop(guard);
}

#[test]
fn scalar_ffi_reference_applies_integer_to_float_argument_conversion() {
    use crate::core::mir::types::MirAbiClass;

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MIXED_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, ALIASED_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, ALIASED_SCALAR_MATRIX_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, IMPORTED_ALIAS_F64_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);

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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

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
extern "C" { func mir_ffi_import_alias_sequence(value: Real) -> ResultId requires: value >= 0 ensures: value >= 0; }
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
extern "C" { func mir_ffi_import_alias_sequence_extra(value: ExtraReal) -> ExtraResultId requires: value >= 0 ensures: value >= 0; }
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
    assert!(receipts.iter().all(|receipt| {
        receipt
            .ensures
            .as_ref()
            .is_some_and(|ensures| ensures.canonical_text().contains("ge("))
    }));
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
    let ffi_verified = verified
        .iter()
        .filter(|result| result.message.contains("canonical MIR extern"))
        .collect::<Vec<_>>();
    assert_eq!(ffi_verified.len(), receipts.len());
    assert_eq!(
        ffi_verified
            .iter()
            .map(|result| result.func_name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "function:call_imported_alias",
            "function:call_imported_alias",
            "function:call_imported_alias_extra",
        ],
        "verifier FFI results must preserve canonical receipt caller order"
    );
    assert!(ffi_verified.iter().all(|result| {
        result.artifact.as_ref().is_some_and(|artifact| {
            artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
                && artifact.mir_hash == receipt.mir_digest
        })
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
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);
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
fn scalar_ffi_requires_and_ensures_share_one_result_with_ordered_summary() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);
    let source = r#"
extern "C" {
    func mir_ffi_i64(x: i64) -> i64 requires: x >= 0 ensures: result == x;
}
func main() -> i64 {
    println(mir_ffi_i64(42 as i64));
    0
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex scalar FFI requires-and-ensures fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse scalar FFI requires-and-ensures fixture");
    let checked =
        crate::core::check_program(&file).expect("check scalar FFI requires-and-ensures fixture");
    let mir = MirProgram::from_checked_program(&checked)
        .expect("materialize scalar FFI requires-and-ensures fixture");
    let receipt = mir.ffi_calls().values().next().expect("FFI receipt");
    assert!(receipt.requires.is_some());
    assert!(receipt.ensures.is_some());
    let digest = mir.canonical_digest();

    crate::core::CheckedProgram::reset_test_legacy_body_access();
    let results = crate::verifier::verify_mir(&mir, "scalar-ffi-requires-ensures".into())
        .expect("MIR FFI requires-and-ensures verifier");
    assert_eq!(results.len(), 1, "one receipt produces one combined result");
    assert_eq!(results[0].status, crate::verifier::VerifStatus::Disproven);
    assert!(results[0]
        .message
        .contains("extern ensures contract disproven"));
    assert_eq!(
        results[0].constraint_count, 3,
        "combined requires/ensures proof must retain the canonical path and result-definedness constraints"
    );
    assert_eq!(
        results[0]
            .diagnostic
            .as_ref()
            .expect("combined result diagnostic")
            .span,
        receipt.span
    );
    assert!(results[0].artifact.as_ref().is_some_and(|artifact| {
        artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR && artifact.mir_hash == digest
    }));
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());

    let oracle = Oracle(Cell::new(0));
    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&oracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference scalar FFI requires-and-ensures execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "42\n");

    let bytecode =
        compile_mir_program(&mir).expect("AST-free scalar FFI requires-and-ensures bytecode");
    assert!(bytecode.ast.is_none());
    assert_eq!(bytecode.canonical_ffi.len(), 1);
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value()
            .expect("bytecode scalar FFI requires-and-ensures"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "42\n");

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "scalar_ffi_requires_ensures");
    generator
        .compile_mir_native(&mir)
        .expect("native scalar FFI requires-and-ensures");
    generator
        .module
        .verify()
        .expect("valid native scalar FFI requires-and-ensures module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(
        &generator,
        &config,
        super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
    .expect("native scalar FFI requires-and-ensures execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "42\n");
    assert_eq!(native.stderr, "");
    assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
}

#[test]
fn scalar_ffi_unit_ensures_runs_after_void_call_across_three_consumers() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);
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
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);
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
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);
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

    let mut guard = super::FfiEnvGuard::lock();
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
        guard.set_path(&library);
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));
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
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));
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
fn scalar_ffi_forged_descriptor_fails_before_stdout_in_all_consumers() {
    const SOURCE: &str = r#"
extern "C" { func preflight_guard(value: i64) -> i64; }
func main() -> i64 { println(17); preflight_guard(1 as i64); 0 }
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("descriptor-preflight FFI fixture check");
    let canonical = MirProgram::from_checked_program(&checked)
        .expect("descriptor-preflight FFI fixture materialization");

    let mut forged_receipts = canonical.ffi_calls().clone();
    forged_receipts
        .values_mut()
        .next()
        .expect("descriptor-preflight call-site receipt")
        .symbol = "forged_preflight_symbol".into();
    let mut forged_mir = canonical.clone();
    forged_mir.replace_ffi_calls_for_test_only(forged_receipts);

    let reference_interpreter = MirReferenceInterpreter::new(&forged_mir);
    let reference_error = reference_interpreter
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged descriptor before execution");
    assert!(
        reference_error
            .to_string()
            .contains("symbol disagrees with canonical extern callee"),
        "{reference_error}"
    );
    assert_eq!(
        reference_interpreter.captured_output(),
        "",
        "reference preflight failure must not execute the leading println"
    );

    let bytecode = compile_mir_program(&canonical).expect("descriptor-preflight bytecode");
    let mut forged_bytecode = (*bytecode).clone();
    forged_bytecode.canonical_ffi[0].symbol = "forged_preflight_symbol".into();
    let mut vm = BytecodeVM::new(std::sync::Arc::new(forged_bytecode));
    let bytecode_error = vm
        .run_value()
        .expect_err("bytecode must reject a forged descriptor before execution");
    assert!(
        bytecode_error
            .to_string()
            .contains("differs from its compiler binding"),
        "{bytecode_error}"
    );
    assert_eq!(
        vm.stdout(),
        "",
        "bytecode preflight failure must not execute the leading println"
    );
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let native_errors = crate::codegen::mir::validate_mir_native(&forged_mir)
        .expect_err("native must reject a forged descriptor before LLVM emission");
    assert!(
        native_errors.iter().any(|error| {
            error
                .message
                .contains("symbol disagrees with canonical extern callee")
        }),
        "{native_errors:?}"
    );
}

#[test]
fn scalar_ffi_repeated_forged_descriptor_clears_vm_state_and_stabilizes_error() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MISSING_LIBRARY_C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(MISSING_LIBRARY_SOURCE)
            .tokenize()
            .expect("lex repeated descriptor fixture"),
    )
    .parse_file()
    .expect("parse repeated descriptor fixture");
    let checked = crate::core::check_program(&file).expect("check repeated descriptor fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("canonical repeated descriptor MIR");
    let bytecode = compile_mir_program(&mir).expect("AST-free repeated descriptor bytecode");
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("initial valid FFI run"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "13\n8\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let mut forged = vm.program().canonical_ffi[0].clone();
    forged.symbol = "forged_repeated_symbol".into();
    vm.replace_canonical_ffi_descriptor_for_test_only(0, forged);

    let first = vm
        .run_value()
        .expect_err("forged descriptor must fail on the next VM entry");
    assert!(first
        .to_string()
        .contains("differs from its compiler binding"));
    assert_eq!(
        vm.stdout(),
        "",
        "preflight failure must clear stdout from the previous invocation"
    );
    assert_eq!(
        vm.debug_stack_state(),
        (0, 0),
        "preflight failure must not leave a frame or depth residue"
    );

    let second = vm
        .run()
        .expect_err("repeated forged descriptor must fail deterministically");
    assert_eq!(second.to_string(), first.to_string());
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
}

#[test]
fn scalar_ffi_direct_entrypoints_reset_stdout_before_preflight() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MISSING_LIBRARY_C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(MISSING_LIBRARY_SOURCE)
            .tokenize()
            .expect("lex direct-entry fixture"),
    )
    .parse_file()
    .expect("parse direct-entry fixture");
    let checked = crate::core::check_program(&file).expect("check direct-entry fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("canonical direct-entry MIR");
    let bytecode = compile_mir_program(&mir).expect("AST-free direct-entry bytecode");
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.call_named("function:main", Vec::new())
            .expect("initial call_named FFI run"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "13\n8\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let mut forged = vm.program().canonical_ffi[0].clone();
    forged.symbol = "forged_direct_entry_symbol".into();
    vm.replace_canonical_ffi_descriptor_for_test_only(0, forged);

    let first = vm
        .call_named("function:main", Vec::new())
        .expect_err("call_named must reject a forged descriptor before execution");
    assert!(first
        .to_string()
        .contains("differs from its compiler binding"));
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let second = vm
        .call_function(vm.program().entry, &[])
        .expect_err("call_function must reject the same forged descriptor deterministically");
    assert_eq!(second.to_string(), first.to_string());
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));
}

#[test]
fn scalar_ffi_wrapped_entrypoint_reuses_vm_after_malformed_preflight() {
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MISSING_LIBRARY_C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(MISSING_LIBRARY_SOURCE)
            .tokenize()
            .expect("lex wrapped-entry fixture"),
    )
    .parse_file()
    .expect("parse wrapped-entry fixture");
    let checked = crate::core::check_program(&file).expect("check wrapped-entry fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("canonical wrapped-entry MIR");
    let bytecode = compile_mir_program(&mir).expect("AST-free wrapped-entry bytecode");
    let mut vm = BytecodeVM::new(bytecode);
    let original = vm.program().canonical_ffi[0].clone();
    let mut forged = original.clone();
    forged.symbol = "forged_wrapped_entry_symbol".into();
    vm.replace_canonical_ffi_descriptor_for_test_only(0, forged);

    let first = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must reject a forged descriptor before frame creation");
    assert!(first
        .to_string()
        .contains("differs from its compiler binding"));
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let second = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("repeated wrapped preflight must be deterministic");
    assert_eq!(second.to_string(), first.to_string());
    assert_eq!(vm.stdout(), "");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    vm.replace_canonical_ffi_descriptor_for_test_only(0, original);
    assert!(matches!(
        vm.call_function(vm.program().entry, &[])
            .expect("valid call after wrapped preflight failure"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), "13\n8\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));
}

#[test]
fn scalar_ffi_nested_call_preserves_parent_stdout_and_direct_entry_snapshot() {
    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_entry(int64_t value) { return value + 100; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_entry(value: i64) -> i64; }
func helper(value: i64) -> i64 {
    println(2);
    let result = mir_ffi_nested_entry(value);
    println(result);
    result
}
func main() -> i64 {
    println(1);
    let value = helper(7 as i64);
    println(value);
    0
}
"#;

    struct NestedOracle;
    impl MirReferenceFfiResolver for NestedOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_nested_entry" {
                return Err(format!("unexpected nested symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("nested FFI arguments {arguments:?}"));
            };
            Ok(MirRuntimeValue::Int(value + 100))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(SOURCE)
            .tokenize()
            .expect("lex nested FFI fixture"),
    )
    .parse_file()
    .expect("parse nested FFI fixture");
    let checked = crate::core::check_program(&file).expect("check nested FFI fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize nested FFI MIR");
    assert_eq!(mir.ffi_calls().len(), 1);

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&NestedOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested FFI execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "1\n2\n107\n107\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free nested FFI bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("bytecode nested FFI execution"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "1\n2\n107\n107\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    assert_eq!(
        vm.call_named("function:helper", vec![Value::Int(7)])
            .expect("direct nested helper entry"),
        Value::Int(107)
    );
    assert_eq!(
        vm.stdout(),
        "2\n107\n",
        "empty-stack direct entry resets only its own snapshot; nested FFI keeps helper output"
    );
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_nested");
    generator
        .compile_mir_native(&mir)
        .expect("native nested FFI lowering");
    generator
        .module
        .verify()
        .expect("valid nested FFI LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native nested FFI execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "1\n2\n107\n107\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_nested_failure_preserves_prefix_and_recovers_same_vm() {
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_missing_library(value: i64) -> i64; }
func helper(value: i64) -> i64 {
    println(2);
    let result = mir_ffi_missing_library(value);
    println(result);
    result
}
func main() -> i64 {
    println(1);
    helper(7 as i64);
    0
}
"#;

    struct NestedRecoveryOracle;
    impl MirReferenceFfiResolver for NestedRecoveryOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_missing_library" {
                return Err(format!("unexpected recovery symbol {}", receipt.symbol));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("recovery FFI arguments {arguments:?}"));
            };
            Ok(MirRuntimeValue::Int(value + 1))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, MISSING_LIBRARY_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    let missing = fixture.dir.join("missing.so");

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(SOURCE)
            .tokenize()
            .expect("lex nested recovery fixture"),
    )
    .parse_file()
    .expect("parse nested recovery fixture");
    let checked = crate::core::check_program(&file).expect("check nested recovery fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize nested recovery MIR");
    assert_eq!(mir.ffi_calls().len(), 1);

    let reference = MirReferenceInterpreter::new(&mir)
        .with_ffi_resolver(&NestedRecoveryOracle)
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested recovery execution");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "1\n2\n8\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free nested recovery bytecode");
    assert!(bytecode.ast.is_none());
    guard.set_path(&missing);
    let mut vm = BytecodeVM::new(bytecode);
    let missing_error = vm
        .run_value()
        .expect_err("nested missing-library call must fail closed");
    assert_eq!(missing_error.code(), "E0800");
    assert!(
        missing_error.to_string().contains("failed to load"),
        "{missing_error}"
    );
    assert_eq!(
        vm.stdout(),
        "1\n2\n",
        "nested load failure must preserve output from parent and helper frames"
    );
    assert_eq!(vm.debug_stack_state(), (0, 0));

    guard.set_path(&library);
    assert_eq!(
        vm.run_value()
            .expect("same VM must recover after nested load failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "1\n2\n8\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    assert_eq!(
        vm.call_named("function:helper", vec![Value::Int(7)])
            .expect("direct helper entry after nested recovery"),
        Value::Int(8)
    );
    assert_eq!(vm.stdout(), "2\n8\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_nested_recovery");
    generator
        .compile_mir_native(&mir)
        .expect("native nested recovery lowering");
    generator
        .module
        .verify()
        .expect("valid nested recovery LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(MISSING_LIBRARY_C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native nested recovery execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "1\n2\n8\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_nested_requires_failure_recovers_reference_and_same_vm() {
    const C_SOURCE: &str = r#"
#include <stdint.h>
int64_t mir_ffi_nested_requires(int64_t value) { return value + 1; }
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_requires(value: i64) -> i64 requires: value >= 0; }
func helper(value: i64) -> i64 {
    println(2);
    let result = mir_ffi_nested_requires(value);
    println(result);
    result
}
func main() -> i64 {
    println(1);
    let value = helper(7 as i64);
    println(value);
    0
}
"#;

    struct NestedRequiresOracle;
    impl MirReferenceFfiResolver for NestedRequiresOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_nested_requires" {
                return Err(format!(
                    "unexpected nested requires symbol {}",
                    receipt.symbol
                ));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("nested requires arguments {arguments:?}"));
            };
            Ok(MirRuntimeValue::Int(value + 1))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(SOURCE)
            .tokenize()
            .expect("lex nested requires fixture"),
    )
    .parse_file()
    .expect("parse nested requires fixture");
    let checked = crate::core::check_program(&file).expect("check nested requires fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize nested requires MIR");
    assert_eq!(mir.ffi_calls().len(), 1);

    let reference = MirReferenceInterpreter::new(&mir).with_ffi_resolver(&NestedRequiresOracle);
    let main_observation = reference
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested requires main execution");
    assert_eq!(main_observation.value, MirRuntimeValue::Int(0));
    assert_eq!(main_observation.output, "1\n2\n8\n8\n");

    let reference_error = reference
        .execute_with_output(
            &crate::core::NodeId("function:helper".into()),
            &[MirRuntimeValue::Int(-7)],
        )
        .expect_err("reference must reject nested FFI requires before host call");
    assert!(
        reference_error.to_string().contains("precondition")
            || reference_error.to_string().contains("requires"),
        "{reference_error}"
    );
    assert_eq!(
        reference.captured_output(),
        "2\n",
        "reference must retain helper output before the failed nested precondition"
    );

    let recovered_reference = reference
        .execute_with_output(
            &crate::core::NodeId("function:helper".into()),
            &[MirRuntimeValue::Int(7)],
        )
        .expect("reference must recover after nested precondition failure");
    assert_eq!(recovered_reference.value, MirRuntimeValue::Int(8));
    assert_eq!(recovered_reference.output, "2\n8\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free nested requires bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("bytecode nested requires main"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "1\n2\n8\n8\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let bytecode_error = vm
        .call_named("function:helper", vec![Value::Int(-7)])
        .expect_err("bytecode must reject nested FFI requires before host call");
    assert_eq!(bytecode_error.code(), "E0808");
    assert!(bytecode_error.to_string().contains("precondition"));
    assert_eq!(vm.stdout(), "2\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    assert_eq!(
        vm.call_named("function:helper", vec![Value::Int(7)])
            .expect("same bytecode VM must recover after nested precondition failure"),
        Value::Int(8)
    );
    assert_eq!(vm.stdout(), "2\n8\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_nested_requires");
    generator
        .compile_mir_native(&mir)
        .expect("native nested requires lowering");
    generator
        .module
        .verify()
        .expect("valid nested requires LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native nested requires execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "1\n2\n8\n8\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_nested_ensures_failure_recovers_reference_and_same_vm() {
    const C_SOURCE: &str = r#"
#include <stdint.h>
static int64_t call_count;
int64_t mir_ffi_nested_ensures(int64_t value) {
    ++call_count;
    return call_count == 2 ? value + 1 : value;
}
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_ensures(value: i64) -> i64 ensures: result == value; }
func helper(value: i64) -> i64 {
    println(2);
    let result = mir_ffi_nested_ensures(value);
    println(result);
    result
}
func main() -> i64 {
    println(1);
    let value = helper(7 as i64);
    println(value);
    0
}
"#;

    struct NestedEnsuresOracle {
        call_count: Cell<i64>,
    }
    impl MirReferenceFfiResolver for NestedEnsuresOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_nested_ensures" {
                return Err(format!(
                    "unexpected nested ensures symbol {}",
                    receipt.symbol
                ));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("nested ensures arguments {arguments:?}"));
            };
            let count = self.call_count.get() + 1;
            self.call_count.set(count);
            Ok(MirRuntimeValue::Int(if count == 2 {
                value + 1
            } else {
                *value
            }))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(SOURCE)
            .tokenize()
            .expect("lex nested ensures fixture"),
    )
    .parse_file()
    .expect("parse nested ensures fixture");
    let checked = crate::core::check_program(&file).expect("check nested ensures fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize nested ensures MIR");
    assert_eq!(mir.ffi_calls().len(), 1);

    let oracle = NestedEnsuresOracle {
        call_count: Cell::new(0),
    };
    let reference = MirReferenceInterpreter::new(&mir).with_ffi_resolver(&oracle);
    let main_observation = reference
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested ensures main execution");
    assert_eq!(main_observation.value, MirRuntimeValue::Int(0));
    assert_eq!(main_observation.output, "1\n2\n7\n7\n");

    let reference_error = reference
        .execute_with_output(
            &crate::core::NodeId("function:helper".into()),
            &[MirRuntimeValue::Int(7)],
        )
        .expect_err("reference must reject nested FFI postcondition after host return");
    assert!(reference_error.to_string().contains("postcondition"));
    assert_eq!(
        reference.captured_output(),
        "2\n",
        "reference must retain helper output before the failed nested postcondition"
    );

    let recovered_reference = reference
        .execute_with_output(
            &crate::core::NodeId("function:helper".into()),
            &[MirRuntimeValue::Int(7)],
        )
        .expect("reference must recover after nested postcondition failure");
    assert_eq!(recovered_reference.value, MirRuntimeValue::Int(7));
    assert_eq!(recovered_reference.output, "2\n7\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free nested ensures bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert_eq!(
        vm.run_value().expect("bytecode nested ensures main"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "1\n2\n7\n7\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let bytecode_error = vm
        .call_named("function:helper", vec![Value::Int(7)])
        .expect_err("bytecode must reject nested FFI postcondition after host return");
    assert_eq!(bytecode_error.code(), "E0808");
    assert!(bytecode_error.to_string().contains("postcondition"));
    assert_eq!(vm.stdout(), "2\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    assert_eq!(
        vm.call_named("function:helper", vec![Value::Int(7)])
            .expect("same bytecode VM must recover after nested postcondition failure"),
        Value::Int(7)
    );
    assert_eq!(vm.stdout(), "2\n7\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_nested_ensures");
    generator
        .compile_mir_native(&mir)
        .expect("native nested ensures lowering");
    generator
        .module
        .verify()
        .expect("valid nested ensures LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native nested ensures execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "1\n2\n7\n7\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn scalar_ffi_nested_ensures_wrapped_entry_failure_recovers_same_vm() {
    const C_SOURCE: &str = r#"
#include <stdint.h>
static int64_t call_count;
int64_t mir_ffi_nested_wrapped_ensures(int64_t value) {
    ++call_count;
    return call_count == 2 ? value + 1 : value;
}
"#;
    const SOURCE: &str = r#"
extern "C" { func mir_ffi_nested_wrapped_ensures(value: i64) -> i64 ensures: result == value; }
func helper(value: i64) -> i64 {
    println(2);
    let result = mir_ffi_nested_wrapped_ensures(value);
    println(result);
    result
}
func main() -> i64 {
    println(1);
    let value = helper(7 as i64);
    println(value);
    0
}
"#;

    struct WrappedEnsuresOracle {
        call_count: Cell<i64>,
    }
    impl MirReferenceFfiResolver for WrappedEnsuresOracle {
        fn call(
            &self,
            receipt: &MirFfiCallContract,
            arguments: &[MirRuntimeValue],
        ) -> Result<MirRuntimeValue, String> {
            if receipt.symbol != "mir_ffi_nested_wrapped_ensures" {
                return Err(format!(
                    "unexpected wrapped ensures symbol {}",
                    receipt.symbol
                ));
            }
            let [MirRuntimeValue::Int(value)] = arguments else {
                return Err(format!("wrapped ensures arguments {arguments:?}"));
            };
            let count = self.call_count.get() + 1;
            self.call_count.set(count);
            Ok(MirRuntimeValue::Int(if count == 2 {
                value + 1
            } else {
                *value
            }))
        }
    }

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));

    let file = crate::parser::Parser::new(
        crate::lexer::Lexer::new(SOURCE)
            .tokenize()
            .expect("lex wrapped nested ensures fixture"),
    )
    .parse_file()
    .expect("parse wrapped nested ensures fixture");
    let checked = crate::core::check_program(&file).expect("check wrapped nested ensures fixture");
    let mir =
        MirProgram::from_checked_program(&checked).expect("materialize wrapped nested ensures MIR");
    assert_eq!(mir.ffi_calls().len(), 1);

    let oracle = WrappedEnsuresOracle {
        call_count: Cell::new(0),
    };
    let reference = MirReferenceInterpreter::new(&mir).with_ffi_resolver(&oracle);
    let first = reference
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference wrapped entry first main");
    assert_eq!(first.value, MirRuntimeValue::Int(0));
    assert_eq!(first.output, "1\n2\n7\n7\n");

    let reference_error = reference
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference wrapped entry must reject the second postcondition");
    assert!(reference_error.to_string().contains("postcondition"));
    assert_eq!(reference.captured_output(), "1\n2\n");

    let recovered_reference = reference
        .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference wrapped entry must recover on the third call");
    assert_eq!(recovered_reference.value, MirRuntimeValue::Int(0));
    assert_eq!(recovered_reference.output, "1\n2\n7\n7\n");

    let bytecode = compile_mir_program(&mir).expect("AST-free wrapped nested ensures bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    vm.call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect("bytecode wrapped entry first main");
    assert_eq!(vm.stdout(), "1\n2\n7\n7\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let bytecode_error = vm
        .call_function_wrap_ok(vm.program().entry, &[], Value::Unit)
        .expect_err("wrapped entry must reject nested postcondition after host return");
    assert_eq!(bytecode_error.code(), "E0808");
    assert!(bytecode_error.to_string().contains("postcondition"));
    assert_eq!(vm.stdout(), "1\n2\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    assert_eq!(
        vm.call_function(vm.program().entry, &[])
            .expect("ordinary entry must recover after wrapped postcondition failure"),
        Value::Int(0)
    );
    assert_eq!(vm.stdout(), "1\n2\n7\n7\n");
    assert_eq!(vm.debug_stack_state(), (0, 0));

    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_scalar_ffi_nested_wrapped_ensures");
    generator
        .compile_mir_native(&mir)
        .expect("native wrapped nested ensures lowering");
    generator
        .module
        .verify()
        .expect("valid wrapped nested ensures LLVM module");
    let config = super::E2EConfig {
        extra_c_src: Some(C_SOURCE.into()),
        ..Default::default()
    };
    let native = super::link_and_observe_module(&generator, &config, counter)
        .expect("native wrapped nested ensures execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "1\n2\n7\n7\n");
    assert_eq!(native.stderr, "");
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
fn scalar_ffi_native_declaration_span_uses_canonical_source_order() {
    const SOURCE: &str = r#"
extern "C" { func mimi_session_pair(value: i64) -> i64; }
func main() -> i64 {
    let first = mimi_session_pair(7 as i64);
    let second = mimi_session_pair(8 as i64);
    first + second
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("reserved-symbol source-order fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("reserved-symbol source-order fixture materialization");
    let ordered = program.ffi_call_entries_in_source_order();
    assert_eq!(ordered.len(), 2);
    let instruction_order = program
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::Call {
                    callee: crate::core::ir::ResolvedCallee::Extern(_),
                    ..
                }
            )
        })
        .map(|instruction| instruction.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(instruction_order.len(), 2);

    let mut swapped_receipts = program.ffi_calls().clone();
    let first_span = swapped_receipts
        .get(&instruction_order[0])
        .expect("first FFI receipt")
        .span;
    let second_span = swapped_receipts
        .get(&instruction_order[1])
        .expect("second FFI receipt")
        .span;
    swapped_receipts
        .get_mut(&instruction_order[0])
        .expect("first mutable FFI receipt")
        .span = second_span;
    swapped_receipts
        .get_mut(&instruction_order[1])
        .expect("second mutable FFI receipt")
        .span = first_span;
    let mut forged = program.clone();
    forged.replace_ffi_calls_for_test_only(swapped_receipts);

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "ffi_source_order_span");
    let diagnostics = generator
        .compile_mir_native(&forged)
        .expect_err("reserved FFI symbol must fail native declaration admission");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].span, ordered[0].1.span,
        "native declaration diagnostics must use canonical source-order span"
    );
}

#[test]
fn scalar_ffi_native_multi_symbol_declaration_span_uses_global_source_order() {
    const SOURCE: &str = r#"
extern "C" {
    func mimi_session_pair(value: i64) -> i64;
    func mimi_channel_drop(value: i64) -> i64;
}
func main() -> i64 {
    let first = mimi_session_pair(7 as i64);
    let second = mimi_channel_drop(8 as i64);
    first + second
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("multi-symbol reserved FFI source-order fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("multi-symbol reserved FFI source-order fixture materialization");
    let ordered = program.ffi_call_entries_in_source_order();
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].1.symbol, "mimi_session_pair");
    assert_eq!(ordered[1].1.symbol, "mimi_channel_drop");
    assert!(
        (ordered[0].1.span.start_line, ordered[0].1.span.start_col)
            < (ordered[1].1.span.start_line, ordered[1].1.span.start_col)
    );

    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "ffi_multi_symbol_span");
    let diagnostics = generator
        .compile_mir_native(&program)
        .expect_err("reserved FFI symbols must fail native declaration admission");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].span, ordered[0].1.span,
        "the first native declaration diagnostic must use the first canonical receipt span"
    );
    assert!(diagnostics[0]
        .message
        .contains("FFI symbol collides with an already-declared native MIR function"));
}

#[test]
fn scalar_ffi_multi_symbol_verifier_artifacts_follow_receipt_source_order() {
    if !crate::verifier::is_z3_available() {
        eprintln!("SKIP: Z3 unavailable");
        return;
    }
    // Keep the calls on different source lines whose numeric order differs
    // from their instruction-id lexical order (`:10:` sorts before `:7:`).
    // The receipt/source order must remain the one observable order shared by
    // bytecode descriptors and verifier proof results.
    const SOURCE: &str = r#"
extern "C" {
    func first(value: i64) -> i64 requires: value < 0;
    func second(value: i64) -> i64 requires: value != 0;
}
func main() -> i32 {
    let first_value = first(7 as i64);









    let second_value = second(8 as i64);
    println(first_value);
    println(second_value);
    0
}
"#;
    let checked = crate::core::check_program(&super::parse(SOURCE))
        .expect("multi-symbol verifier source-order fixture check");
    let program = MirProgram::from_checked_program(&checked)
        .expect("multi-symbol verifier source-order fixture materialization");
    let ordered = program.ffi_call_entries_in_source_order();
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].1.symbol, "first");
    assert_eq!(ordered[1].1.symbol, "second");
    assert!(ordered[0].1.span.start_line < ordered[1].1.span.start_line);
    assert!(ordered[0].1.instruction > ordered[1].1.instruction);

    crate::verifier::validate_mir_capabilities(&program)
        .expect("multi-symbol scalar FFI capability gate");
    let receipt = program.route_receipt("scalar-ffi-multi-symbol-v1");
    let bytecode = compile_mir_program(&program).expect("multi-symbol scalar FFI bytecode");
    assert_eq!(bytecode.canonical_ffi.len(), 2);
    assert_eq!(bytecode.canonical_ffi[0].symbol, "first");
    assert_eq!(bytecode.canonical_ffi[1].symbol, "second");

    let results = crate::verifier::verify_mir(&program, "multi-symbol-proof".into())
        .expect("multi-symbol scalar FFI verifier");
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].status,
        crate::verifier::VerifStatus::Disproven,
        "the first source-order call must be the first verifier result"
    );
    assert_eq!(results[1].status, crate::verifier::VerifStatus::Proven);
    assert_eq!(
        results[0]
            .diagnostic
            .as_ref()
            .expect("disproven first call diagnostic")
            .span,
        ordered[0].1.span,
        "verifier diagnostic must retain the first receipt source span"
    );
    assert!(results.iter().all(|result| {
        result.artifact.as_ref().is_some_and(|artifact| {
            artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
                && artifact.mir_hash == receipt.mir_digest
        })
    }));
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
fn scalar_ffi_multiple_duplicate_imports_have_deterministic_diagnostic_order() {
    use std::fs;

    let project = std::env::temp_dir().join(format!(
        "mimi-canonical-ffi-duplicate-order-{}",
        std::process::id()
    ));
    fs::create_dir_all(&project).expect("create duplicate order project");
    let main_path = project.join("main.mimi");
    let left_path = project.join("left.mimi");
    let right_path = project.join("right.mimi");
    fs::write(
        &main_path,
        "use left;\nuse right;\nfunc main() -> i64 { 0 }\n",
    )
    .expect("write duplicate order main");
    let left_source = "extern \"C\" {\n    func zeta(value: i64) -> i64;\n    func alpha(value: i64) -> i64;\n}\npub func call_left(value: i64) -> i64 { zeta(value) + alpha(value) }\n";
    let right_source = "extern \"C\" {\n    func zeta(value: i64) -> i64;\n    func alpha(value: i64) -> i64;\n}\npub func call_right(value: i64) -> i64 { zeta(value) + alpha(value) }\n";
    fs::write(&left_path, left_source).expect("write left duplicate order declarations");
    fs::write(&right_path, right_source).expect("write right duplicate order declarations");

    let load = || {
        let source = fs::read_to_string(&main_path).expect("read duplicate order main");
        let tokens = crate::lexer::Lexer::new(&source)
            .tokenize()
            .expect("lex duplicate order main");
        let file = crate::loader::parser_for_path(tokens, &main_path)
            .expect("select duplicate order parser")
            .parse_file()
            .expect("parse duplicate order main");
        let mut loader = crate::loader::ModuleLoader::new(project.clone());
        loader
            .load_main_with_file(&main_path, file)
            .expect("load duplicate order graph");
        let mut merged = loader.merge_all().expect("merge duplicate order graph");
        crate::loader::merge_prelude_into(&mut merged);
        merged
    };

    let first = load();
    let second = load();
    let first_errors = crate::core::check_program(&first)
        .expect_err("multiple imported duplicate externs must fail checker");
    let second_errors = crate::core::check_program(&second)
        .expect_err("repeated duplicate order graph must fail checker");
    let duplicate_summary =
        |file: &crate::ast::File, diagnostics: &[crate::diagnostic::Diagnostic]| {
            diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0402)
                })
                .map(|diagnostic| {
                    let primary = file
                        .sources
                        .record(diagnostic.span.source_id)
                        .expect("duplicate primary source record")
                        .disk_path
                        .clone();
                    let note = diagnostic
                        .notes
                        .first()
                        .expect("duplicate declaration prior note");
                    let previous = file
                        .sources
                        .record(note.span.source_id)
                        .expect("duplicate prior source record")
                        .disk_path
                        .clone();
                    (
                        diagnostic.message.clone(),
                        primary,
                        note.message.clone(),
                        previous,
                        diagnostic.span.start_line,
                        diagnostic.span.start_col,
                    )
                })
                .collect::<Vec<_>>()
        };
    let first_summary = duplicate_summary(&first, &first_errors);
    let second_summary = duplicate_summary(&second, &second_errors);
    assert_eq!(first_summary, second_summary);
    assert_eq!(
        first_summary.len(),
        2,
        "both duplicate symbols must be reported"
    );
    assert_eq!(
        first_summary
            .iter()
            .map(|(message, _, _, _, _, _)| message.clone())
            .collect::<Vec<_>>(),
        vec![
            "duplicate extern function 'zeta' (conflicting declarations across extern blocks)"
                .to_string(),
            "duplicate extern function 'alpha' (conflicting declarations across extern blocks)"
                .to_string(),
        ]
    );
    for (_, primary, note, previous, _, _) in &first_summary {
        assert_eq!(
            primary.as_deref(),
            right_path.canonicalize().ok().as_deref()
        );
        assert_eq!(note, "previous extern declaration is here");
        assert_eq!(
            previous.as_deref(),
            left_path.canonicalize().ok().as_deref()
        );
    }
    assert!(first_errors
        .iter()
        .filter(|diagnostic| diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0402))
        .all(|diagnostic| diagnostic.notes.len() == 1));
    fs::remove_dir_all(project).expect("remove duplicate order project");
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));
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
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, BAD_C_SOURCE);
    let library = fixture.dir.join("ffi.so");
    guard.set_path(&library);
    let source = r#"
extern "C" {
    func mir_ffi_bad(x: i64) -> i64 ensures: result == x;
}
func main() -> i64 { println(5); mir_ffi_bad(41 as i64); 0 }
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex bad FFI");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse bad FFI");
    let checked = crate::core::check_program(&file).expect("check bad FFI");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize bad FFI");
    let reference_interpreter = MirReferenceInterpreter::new(&mir).with_ffi_resolver(&BadOracle);
    let reference = reference_interpreter
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("reference must enforce the FFI postcondition");
    assert!(
        reference.message.contains("FFI postcondition failed"),
        "{reference}"
    );
    assert_eq!(reference_interpreter.captured_output(), "5\n");

    let mut vm = BytecodeVM::new(compile_mir_program(&mir).expect("bad FFI bytecode"));
    let vm_error = vm
        .run_value()
        .expect_err("bytecode must enforce the FFI postcondition");
    assert_eq!(vm_error.code(), "E0808");
    assert!(vm_error.to_string().contains("FFI postcondition failed"));
    assert_eq!(vm.stdout(), "5\n");

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
    assert_eq!(native.stdout, "5\n");
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
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, &c_source);
    let trace_path = fixture.dir.join("trace.txt");
    guard.set_path(&fixture.dir.join("ffi.so"));
    guard.set_trace_path(&trace_path);

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
    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));
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
                "func observe(value: i64) -> i64 { println(value); value }",
                format!("observe(generated_foreign({value} as i64))"),
                value,
            ),
            1 => (
                "func observe(value: i64) -> i64 { println(value); value }",
                format!("let x = generated_foreign({value} as i64); observe(x)"),
                value,
            ),
            2 => (
                "func observe(value: i64) -> i64 { println(value); value }",
                format!(
                    "let x = if {condition} {{ generated_foreign({value} as i64) }} else {{ generated_foreign({alternate} as i64) }}; observe(x)"
                ),
                if condition { value } else { alternate },
            ),
            3 => (
                "func relay(x: i64) -> i64 { generated_foreign(x) }\nfunc observe(value: i64) -> i64 { println(value); value }",
                format!("observe(relay({value} as i64))"),
                value,
            ),
            4 => (
                "func relay(x: i64) -> i64 { generated_foreign(x) }\nfunc observe(value: i64) -> i64 { println(value); value }",
                format!(
                    "let x = relay({value} as i64); let y = generated_foreign(x); observe(y)"
                ),
                value,
            ),
            _ => (
                "func relay(x: i64) -> i64 { generated_foreign(x) }\nfunc observe(value: i64) -> i64 { println(value); value }",
                format!(
                    "let result = if {condition} {{ let x = relay({value} as i64); generated_foreign(x) }} else {{ generated_foreign({alternate} as i64) }}; observe(result)"
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
            .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
            .unwrap_or_else(|error| panic!("seeded case {case_index} reference: {error}"));
        assert_eq!(reference.value, MirRuntimeValue::Int(expected));
        assert_eq!(reference.output, format!("{expected}\n"));

        let bytecode = compile_mir_program(&mir)
            .unwrap_or_else(|error| panic!("seeded case {case_index} bytecode: {error:?}"));
        assert!(bytecode.ast.is_none());
        let mut vm = BytecodeVM::new(bytecode);
        assert!(matches!(
            vm.run_value()
                .unwrap_or_else(|error| panic!("seeded case {case_index} VM: {error}")),
            Value::Int(value) if value == expected
        ));
        assert_eq!(vm.stdout(), format!("{expected}\n"));

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
        assert_eq!(native.stdout, format!("{expected}\n"));
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

    let mut guard = super::FfiEnvGuard::lock();
    let counter = super::E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fixture = library_fixture(counter, C_SOURCE);
    guard.set_path(&fixture.dir.join("ffi.so"));
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
