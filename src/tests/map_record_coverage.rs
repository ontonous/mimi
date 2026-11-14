//! Ordinary Map/Any record payload boundary.
//!
//! The current canonical owner profiles are deliberately typed to i32/String
//! entries.  A user record must therefore stop at the checker-owned coverage
//! boundary until a dynamic record descriptor and matching owner ABI exist;
//! it must never be reinterpreted by the erased legacy Map path.

use inkwell::context::Context;

fn checked(source: &str) -> crate::core::CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("record Map fixture lexes");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("record Map fixture parses");
    crate::core::check_program(&file).expect("record Map fixture checks")
}

#[test]
fn record_valued_map_set_is_checker_owned_coverage_boundary() {
    let program = checked(
        r#"
type Point { x: i32, y: i32 }

func main() -> i32 {
    let point = Point { x: 7, y: 9 }
    let root = match map_new() {
        Ok(value) => value,
        Err(status) => return 1000000 + status,
    }
    let updated = match map_set(root, "point", point) {
        Ok(value) => {
            drop(root)
            value
        },
        Err(status) => {
            drop(root)
            return 1000000 + status
        },
    }
    let size = map_size(updated)
    drop(updated)
    size
}
"#,
    );

    let admission = crate::core::mir::classify_canonical_mir_route_admission(&program);
    assert!(
        admission
            .ordinary_map_owner_lifecycle_i32_string_v1
            .has_profile_signal(),
        "record map_set must remain visible as an ordinary Map candidate"
    );
    assert!(!admission
        .ordinary_map_owner_lifecycle_i32_string_v1
        .is_complete());

    let context = Context::create();
    let errors = crate::codegen::CodeGenerator::new(&context, "record_map_coverage")
        .compile_checked(&program)
        .expect_err("record-valued Map Set has no canonical owner ABI yet");
    assert!(errors.iter().any(|diagnostic| {
        diagnostic.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
    }));
}
