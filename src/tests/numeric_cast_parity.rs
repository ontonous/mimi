use super::*;

const EXPLICIT_NUMERIC_CASTS: &str = r#"
func main() -> i32 {
    println(3000000000 as i32)
    println(1.9 as i32)
    println((-1.9) as i32)
    println(1e300 as i32)
    println((-1e300) as i32)
    println(1e300 as i64)
    println((-1e300) as i64)

    ieee_float {
        let nan = sqrt(-1.0)
        println(nan as i32)
        println(nan as i64)
    }

    ieee_float {
        let infinity = 1e308 * 10.0
        let negative_infinity = -1e308 * 10.0
        println(infinity as i32)
        println(infinity as i64)
        println(negative_infinity as i32)
        println(negative_infinity as i64)
    }

    // This value is one integer above an f32 midpoint at a magnitude where
    // i64 -> f64 -> f32 would double-round down to the lower binary32 value.
    println((9007199791611905 as f32) as i64)
    println((16777217.0 as f32) as i64)
    println(((16777217.0 as f32) as f64) as i64)
    println((9007199254740993 as f64) as i64)
    0
}
"#;

const EXPECTED_CAST_OUTPUT: &str = "-1294967296\n1\n-1\n2147483647\n-2147483648\n9223372036854775807\n-9223372036854775808\n0\n0\n2147483647\n9223372036854775807\n-2147483648\n-9223372036854775808\n9007200328482816\n16777216\n16777216\n9007199254740992";

const TOP_LEVEL_CONST_CAST: &str = r#"
const WRAPPED: i32 = 3000000000 as i32

func main() -> i32 {
    println(WRAPPED)
    0
}
"#;

const INVALID_TOP_LEVEL_CONST_CAST: &str = r#"
const WIDE: i128 = 1 as i128

func main() -> i32 {
    0
}
"#;

const NON_CONTRACT_NUMERIC_IDENTITY_CAST: &str = r#"
func identity_i8(value: i8) -> i8 {
    value as i8
}

func identity_bool(value: bool) -> bool {
    value as bool
}

func main() -> i32 {
    let value: i8 = 7
    println(identity_i8(value) == value)
    println(identity_bool(true))
    0
}
"#;

const NON_CONTRACT_PRIMITIVE_CASTS: &str = r#"
func i32_to_bool(value: i32) -> bool {
    value as bool
}

func bool_to_i32(value: bool) -> i32 {
    value as i32
}

func main() -> i32 {
    0
}
"#;

#[test]
fn explicit_numeric_casts_match_checked_bytecode_and_legacy_native() {
    if !can_link() {
        eprintln!("SKIP: cc is unavailable");
        return;
    }

    let (_value, vm_stdout) = checked_run_source_with_stdout(EXPLICIT_NUMERIC_CASTS);
    let native_stdout = checked_codegen_compile_and_run(EXPLICIT_NUMERIC_CASTS)
        .expect("legacy native explicit numeric cast program failed");

    assert_eq!(
        vm_stdout.trim(),
        EXPECTED_CAST_OUTPUT,
        "checked bytecode explicit cast output"
    );
    assert_eq!(
        native_stdout.trim(),
        EXPECTED_CAST_OUTPUT,
        "legacy native explicit cast output"
    );
}

#[test]
fn explicit_numeric_cast_pair_matrix_matches_checked_bytecode_and_legacy_native() {
    if !can_link() {
        eprintln!("SKIP: cc is unavailable");
        return;
    }

    let primitive_types = ["i32", "i64", "f32", "f64"];
    let mut source = String::new();
    for from in primitive_types {
        for to in primitive_types {
            source.push_str(&format!(
                "func cast_{from}_to_{to}(value: {from}) -> {to} {{ value as {to} }}\n"
            ));
        }
    }

    source.push_str("func main() -> i32 {\n");
    let mut expected = Vec::new();
    for from in primitive_types {
        let argument = match from {
            "i32" => "7",
            "i64" => "3000000000",
            "f32" => "(1.75 as f32)",
            "f64" => "-1.75",
            _ => unreachable!("the matrix uses only contract numeric primitives"),
        };
        for to in primitive_types {
            source.push_str(&format!(
                "    println(cast_{from}_to_{to}({argument}) as i64)\n"
            ));
            expected.push(match (from, to) {
                ("i64", "i32") => "-1294967296",
                ("i64", _) => "3000000000",
                ("f32", _) => "1",
                ("f64", _) => "-1",
                _ => "7",
            });
        }
    }
    source.push_str("    0\n}\n");
    let expected = expected.join("\n");

    let (_value, vm_stdout) = checked_run_source_with_stdout(&source);
    let native_stdout = checked_codegen_compile_and_run(&source)
        .expect("legacy native explicit cast-pair matrix failed");

    assert_eq!(vm_stdout.trim(), expected, "checked bytecode pair matrix");
    assert_eq!(native_stdout.trim(), expected, "legacy native pair matrix");
}

#[test]
fn top_level_const_initializer_cast_receipt_matches_checked_backends() {
    if !can_link() {
        eprintln!("SKIP: cc is unavailable");
        return;
    }

    let (_value, vm_stdout) = checked_run_source_with_stdout(TOP_LEVEL_CONST_CAST);
    let native_stdout = checked_codegen_compile_and_run(TOP_LEVEL_CONST_CAST)
        .expect("legacy native top-level const cast failed");

    assert_eq!(
        vm_stdout.trim(),
        "-1294967296",
        "checked bytecode const cast"
    );
    assert_eq!(
        native_stdout.trim(),
        "-1294967296",
        "legacy native const cast"
    );
}

#[test]
fn unsupported_numeric_cast_pairs_are_rejected_during_source_check() {
    let primitive_types = [
        "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "isize", "usize",
        "f32", "f64",
    ];
    let mut source = String::new();
    for from in primitive_types {
        for to in primitive_types {
            source.push_str(&format!(
                "func cast_{from}_to_{to}(value: {from}) -> {to} {{ value as {to} }}\n"
            ));
        }
    }
    source.push_str("func main() -> i32 { 0 }\n");

    let diagnostics = check_source(&source)
        .expect_err("out-of-contract numeric casts must fail during source check");
    let numeric_cast_errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0200))
        .count();
    // The complete 14 × 14 checker surface accepts all fourteen identity
    // casts and all sixteen pairs among i32/i64/f32/f64. The latter includes
    // the four identity pairs already counted above.
    assert_eq!(
        numeric_cast_errors,
        primitive_types.len() * primitive_types.len() - (primitive_types.len() + 12),
        "every non-identity pair outside i32/i64/f32/f64 receives E0200"
    );
}

#[test]
fn non_contract_primitive_identity_cast_remains_accepted() {
    if !can_link() {
        check_source(NON_CONTRACT_NUMERIC_IDENTITY_CAST)
            .expect("identity casts remain valid for primitive types outside the numeric matrix");
        return;
    }

    let (_value, vm_stdout) = checked_run_source_with_stdout(NON_CONTRACT_NUMERIC_IDENTITY_CAST);
    let native_stdout = checked_codegen_compile_and_run(NON_CONTRACT_NUMERIC_IDENTITY_CAST)
        .expect("legacy native identity casts for i8 and bool must compile");
    assert_eq!(vm_stdout.trim(), "true\ntrue");
    assert_eq!(native_stdout.trim(), "true\ntrue");
    check_source(NON_CONTRACT_NUMERIC_IDENTITY_CAST)
        .expect("identity casts remain valid for primitive types outside the numeric matrix");
}

#[test]
fn non_contract_primitive_conversions_are_rejected_during_source_check() {
    let diagnostics = check_source(NON_CONTRACT_PRIMITIVE_CASTS)
        .expect_err("non-identity casts involving other primitives must fail during check");
    let cast_errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0200))
        .count();
    assert_eq!(
        cast_errors, 2,
        "both directions involving bool receive E0200"
    );
}

#[test]
fn unsupported_top_level_const_cast_is_rejected_during_source_check() {
    let diagnostics = check_source(INVALID_TOP_LEVEL_CONST_CAST)
        .expect_err("a top-level const cannot hide an out-of-contract numeric cast");
    assert!(diagnostics
        .iter()
        .any(|diagnostic| { diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0200) }));
}
