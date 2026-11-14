use inkwell::OptimizationLevel;

use crate::codegen::CodeGenerator;
use crate::core::CheckedProgram;
use crate::interp::Value;

const FLOAT_TO_INT_CASTS: &str = r#"
func cast_f64_to_i32(value: f64) -> i32 { value as i32 }
func cast_f64_to_i64(value: f64) -> i64 { value as i64 }
func cast_f32_to_i32(value: f32) -> i32 { value as i32 }
func cast_f32_to_i64(value: f32) -> i64 { value as i64 }

func cast_checks() -> i32 {
    let mut nan = 0.0
    let mut positive_infinity = 0.0
    let mut negative_infinity = 0.0
    ieee_float {
        nan = 0.0 / 0.0
        positive_infinity = 1.0 / 0.0
        negative_infinity = -1.0 / 0.0
    }

    let positive_overflow = 1.0e100
    let negative_overflow = -1.0e100
    let positive_fraction = 12.875
    let negative_fraction = -12.875
    let f32_nan = nan as f32
    let f32_positive_infinity = positive_infinity as f32
    let f32_negative_infinity = negative_infinity as f32
    let f32_overflow = 1.0e38 as f32
    let f32_negative_overflow = -1.0e38 as f32
    let f32_fraction = 12.875 as f32
    let f32_negative_fraction = -12.875 as f32

    if cast_f64_to_i32(nan) == 0
        && cast_f64_to_i64(nan) == 0
        && cast_f64_to_i32(positive_infinity) == 2147483647
        && cast_f64_to_i32(negative_infinity) == -2147483648
        && cast_f64_to_i64(positive_infinity) == 9223372036854775807
        && cast_f64_to_i64(negative_infinity) == -9223372036854775808
        && cast_f64_to_i32(positive_overflow) == 2147483647
        && cast_f64_to_i32(negative_overflow) == -2147483648
        && cast_f64_to_i64(positive_overflow) == 9223372036854775807
        && cast_f64_to_i64(negative_overflow) == -9223372036854775808
        && cast_f64_to_i32(positive_fraction) == 12
        && cast_f64_to_i32(negative_fraction) == -12
        && cast_f64_to_i64(positive_fraction) == 12
        && cast_f64_to_i64(negative_fraction) == -12
        && cast_f32_to_i32(f32_nan) == 0
        && cast_f32_to_i64(f32_nan) == 0
        && cast_f32_to_i32(f32_positive_infinity) == 2147483647
        && cast_f32_to_i32(f32_negative_infinity) == -2147483648
        && cast_f32_to_i64(f32_positive_infinity) == 9223372036854775807
        && cast_f32_to_i64(f32_negative_infinity) == -9223372036854775808
        && cast_f32_to_i32(f32_overflow) == 2147483647
        && cast_f32_to_i32(f32_negative_overflow) == -2147483648
        && cast_f32_to_i64(f32_overflow) == 9223372036854775807
        && cast_f32_to_i64(f32_negative_overflow) == -9223372036854775808
        && cast_f32_to_i32(f32_fraction) == 12
        && cast_f32_to_i32(f32_negative_fraction) == -12
        && cast_f32_to_i64(f32_fraction) == 12
        && cast_f32_to_i64(f32_negative_fraction) == -12
    {
        0
    } else {
        1
    }
}

func main() -> i32 { cast_checks() }
"#;

fn checked_program(source: &str) -> CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("cast source lexes");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("cast source parses");
    crate::core::check_program(&file).expect("cast source type-checks")
}

#[test]
fn compile_checked_saturates_runtime_float_to_integer_casts() {
    let program = checked_program(FLOAT_TO_INT_CASTS);
    assert!(
        super::supports_resolved_native(&program),
        "the cast fixture must enter the canonical Resolved native emitter"
    );

    let (bytecode_value, _) = crate::tests::checked_run_source_with_stdout(FLOAT_TO_INT_CASTS);
    assert_eq!(
        bytecode_value,
        Value::Int(0),
        "Bytecode cast reference must produce the expected saturated results"
    );

    CheckedProgram::reset_test_legacy_codegen_function_calls();
    let context = inkwell::context::Context::create();
    let mut codegen = CodeGenerator::new(&context, "resolved_float_to_int_casts");
    codegen
        .compile_checked(&program)
        .expect("compile_checked emits the resolved float-cast fixture");
    codegen.module.verify().expect("valid native LLVM module");
    assert!(
        CheckedProgram::test_legacy_codegen_function_calls().is_empty(),
        "the cast regression must not invoke legacy codegen"
    );

    let native = codegen
        .module
        .create_jit_execution_engine(OptimizationLevel::None)
        .expect("native JIT engine");
    // SAFETY: every JIT entry below is compiled with the matching C scalar ABI.
    type CastChecks = unsafe extern "C" fn() -> i32;
    type CastF64ToI32 = unsafe extern "C" fn(f64) -> i32;
    type CastF64ToI64 = unsafe extern "C" fn(f64) -> i64;
    type CastF32ToI32 = unsafe extern "C" fn(f32) -> i32;
    type CastF32ToI64 = unsafe extern "C" fn(f32) -> i64;
    let cast_checks = unsafe {
        // SAFETY: the fixture functions have these checked scalar ABIs.
        native
            .get_function::<CastChecks>("cast_checks")
            .expect("native cast_checks function")
    };
    let f64_to_i32 = unsafe {
        // SAFETY: native JIT module defines `cast_f64_to_i32` with ABI `fn(f64) -> i32`.
        native
            .get_function::<CastF64ToI32>("cast_f64_to_i32")
            .expect("native f64→i32 cast function")
    };
    let f64_to_i64 = unsafe {
        // SAFETY: native JIT module defines `cast_f64_to_i64` with ABI `fn(f64) -> i64`.
        native
            .get_function::<CastF64ToI64>("cast_f64_to_i64")
            .expect("native f64→i64 cast function")
    };
    let f32_to_i32 = unsafe {
        // SAFETY: native JIT module defines `cast_f32_to_i32` with ABI `fn(f32) -> i32`.
        native
            .get_function::<CastF32ToI32>("cast_f32_to_i32")
            .expect("native f32→i32 cast function")
    };
    let f32_to_i64 = unsafe {
        // SAFETY: native JIT module defines `cast_f32_to_i64` with ABI `fn(f32) -> i64`.
        native
            .get_function::<CastF32ToI64>("cast_f32_to_i64")
            .expect("native f32→i64 cast function")
    };
    assert_eq!(
        // SAFETY: `cast_checks` takes no arguments and matches the `CastChecks` JIT signature.
        unsafe { cast_checks.call() },
        0,
        "source-level cast checks"
    );

    for (value, expected) in [
        (f64::NAN, 0),
        (f64::INFINITY, i32::MAX),
        (f64::NEG_INFINITY, i32::MIN),
        (1.0e100, i32::MAX),
        (-1.0e100, i32::MIN),
        (12.875, 12),
        (-12.875, -12),
    ] {
        assert_eq!(
            unsafe {
                // SAFETY: `f64_to_i32` was resolved with `CastF64ToI32` ABI and engine stays alive.
                f64_to_i32.call(value)
            },
            expected,
            "f64→i32: {value}"
        );
    }
    for (value, expected) in [
        (f64::NAN, 0),
        (f64::INFINITY, i64::MAX),
        (f64::NEG_INFINITY, i64::MIN),
        (1.0e100, i64::MAX),
        (-1.0e100, i64::MIN),
        (12.875, 12),
        (-12.875, -12),
    ] {
        assert_eq!(
            unsafe {
                // SAFETY: `f64_to_i64` was resolved with `CastF64ToI64` ABI and engine stays alive.
                f64_to_i64.call(value)
            },
            expected,
            "f64→i64: {value}"
        );
    }
    for (value, expected) in [
        (f32::NAN, 0),
        (f32::INFINITY, i32::MAX),
        (f32::NEG_INFINITY, i32::MIN),
        (f32::MAX, i32::MAX),
        (-f32::MAX, i32::MIN),
        (12.875, 12),
        (-12.875, -12),
    ] {
        assert_eq!(
            unsafe {
                // SAFETY: `f32_to_i32` was resolved with `CastF32ToI32` ABI and engine stays alive.
                f32_to_i32.call(value)
            },
            expected,
            "f32→i32: {value}"
        );
    }
    for (value, expected) in [
        (f32::NAN, 0),
        (f32::INFINITY, i64::MAX),
        (f32::NEG_INFINITY, i64::MIN),
        (f32::MAX, i64::MAX),
        (-f32::MAX, i64::MIN),
        (12.875, 12),
        (-12.875, -12),
    ] {
        assert_eq!(
            unsafe {
                // SAFETY: `f32_to_i64` was resolved with `CastF32ToI64` ABI and engine stays alive.
                f32_to_i64.call(value)
            },
            expected,
            "f32→i64: {value}"
        );
    }
}
