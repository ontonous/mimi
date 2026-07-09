use super::*;

fn has_code(errors: &[crate::diagnostic::Diagnostic], code: &str) -> bool {
    errors.iter().any(|e| e.code.as_deref() == Some(code))
}

fn assert_err_code(src: &str, expected: &str) {
    let errors = match check_source(src) {
        Err(errors) => errors,
        Ok(()) => panic!("expected error {expected}, but check succeeded\nsrc: {src}"),
    };
    assert!(
        has_code(&errors, expected),
        "expected {expected}, got codes: {:?}\nsrc: {src}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

// ── E0204: cannot dereference type ────────────────────────────────────
#[test]
fn e0204_deref_non_pointer() {
    assert_err_code(
        r"func main() -> i32 { let x = 42; *x }",
        crate::diagnostic::codes::E0204,
    );
}

// ── E0208: cannot assign to immutable ────────────────────────────────
#[test]
fn e0208_assign_immutable() {
    assert_err_code(
        r"func main() -> i32 { let x = 1; x = 2; x }",
        crate::diagnostic::codes::E0208,
    );
}

// ── E0211: argument type mismatch ─────────────────────────────────────
#[test]
fn e0211_arg_type_mismatch() {
    assert_err_code(
        "func greet(n: string) -> string { n }\nfunc main() -> string { greet(42) }",
        crate::diagnostic::codes::E0211,
    );
}

// ── E0213: match must have at least one arm ───────────────────────────
#[test]
fn e0213_match_no_arms() {
    assert_err_code(
        r"func main() -> i32 { match 1 {} }",
        crate::diagnostic::codes::E0213,
    );
}

// ── E0215: match not exhaustive ───────────────────────────────────────
#[test]
fn e0215_match_not_exhaustive() {
    assert_err_code(
        r"func main() -> i32 { match 5 { 0 => 1, 1 => 2 } }",
        crate::diagnostic::codes::E0215,
    );
}

// ── E0217: index must be integer ──────────────────────────────────────
#[test]
fn e0217_index_not_integer() {
    assert_err_code(
        "func main() -> i32 { let xs = [1, 2, 3]; xs[\"hello\"] }",
        crate::diagnostic::codes::E0217,
    );
}

// ── E0218: cannot index type ──────────────────────────────────────────
#[test]
fn e0218_index_non_list() {
    assert_err_code(
        r"func main() -> i32 { let x = 42; x[0] }",
        crate::diagnostic::codes::E0218,
    );
}

// ── E0221: type has no method ─────────────────────────────────────────
#[test]
fn e0221_no_method() {
    assert_err_code(
        r"func main() -> i32 { let x = 42; x.foobar() }",
        crate::diagnostic::codes::E0221,
    );
}

// ── E0224: cannot apply ? to type ─────────────────────────────────────
#[test]
fn e0224_question_on_i32() {
    assert_err_code(
        r"func main() -> i32 { let x = 42; x? }",
        crate::diagnostic::codes::E0224,
    );
}

// ── E0225: pattern type does not match subject ────────────────────────
#[test]
fn e0225_pattern_type_mismatch() {
    assert_err_code(
        "func main() -> i32 { match 42 { \"hello\" => 1, _ => 0 } }",
        crate::diagnostic::codes::E0225,
    );
}

// ── E0242: list element type mismatch via annotation ──────────────────
#[test]
fn e0242_list_element_type_mismatch() {
    assert_err_code(
        "func main() -> i32 { let xs: List<i32> = [1, 2, \"hello\"]; 0 }",
        crate::diagnostic::codes::E0242,
    );
}

// ── E0207: return type mismatch ───────────────────────────────────────
#[test]
fn e0207_return_type_mismatch() {
    assert_err_code(
        r"func main() -> i32 { return; }",
        crate::diagnostic::codes::E0207,
    );
}

// ── E0255: missing return on all paths ────────────────────────────────
#[test]
fn e0255_missing_return_in_if() {
    assert_err_code(
        r"func main() -> i32 { if true { 1 } }",
        crate::diagnostic::codes::E0255,
    );
}

// ── E0237: division by zero literal ───────────────────────────────────
#[test]
fn e0237_div_by_zero_literal() {
    assert_err_code(
        r"func main() -> i32 { 5 / 0 }",
        crate::diagnostic::codes::E0237,
    );
}

// ── E0238: modulo by zero literal ─────────────────────────────────────
#[test]
fn e0238_mod_by_zero_literal() {
    assert_err_code(
        r"func main() -> i32 { 5 % 0 }",
        crate::diagnostic::codes::E0238,
    );
}

// ── E0244: cannot index non-tuple type ────────────────────────────────
#[test]
fn e0244_index_non_tuple() {
    assert_err_code(
        r"func main() -> i32 { let x = 42; x.0 }",
        crate::diagnostic::codes::E0244,
    );
}

// ── E0245: await requires Future type ─────────────────────────────────
#[test]
fn e0245_await_non_future() {
    assert_err_code(
        r"func main() -> i32 { await 42 }",
        crate::diagnostic::codes::E0245,
    );
}

// ── E0246: type has no variant ────────────────────────────────────────
#[test]
fn e0246_no_variant() {
    assert_err_code(
        "type Color { Red Green }\nfunc main() -> i32 {\n    let c = Red;\n    c.Blue\n}",
        crate::diagnostic::codes::E0246,
    );
}

// ── E0247: record field type mismatch ─────────────────────────────────
#[test]
fn e0247_record_field_type_mismatch() {
    let src = "type Point { x: i32, y: i32 }\nfunc main() -> i32 {\n    let p = Point { x: 1, y: \"hello\" };\n    0\n}";
    assert_err_code(src, crate::diagnostic::codes::E0247);
}

// ── E0248: missing field in record literal ────────────────────────────
#[test]
fn e0248_missing_record_field() {
    assert_err_code(
        "type Point { x: i32, y: i32 }\nfunc main() -> i32 { let p = Point { x: 1 }; 0 }",
        crate::diagnostic::codes::E0248,
    );
}

// ── E0249 → E0410: undefined record construction ──────────────────────
#[test]
fn e0410_undefined_record_construction() {
    assert_err_code(
        r"func main() -> i32 { NotARecord { x: 1 }; 0 }",
        crate::diagnostic::codes::E0410,
    );
}

// ── E0400: undefined variable ─────────────────────────────────────────
#[test]
fn e0400_undefined_variable() {
    assert_err_code(
        r"func main() -> i32 { undefined_var }",
        crate::diagnostic::codes::E0400,
    );
}

// ── E0401: undefined function ─────────────────────────────────────────
#[test]
fn e0401_undefined_function() {
    assert_err_code(
        r"func main() -> i32 { undefined_func() }",
        crate::diagnostic::codes::E0401,
    );
}

// ── E0406: undefined trait ────────────────────────────────────────────
#[test]
fn e0406_undefined_trait() {
    assert_err_code(
        "func main() -> i32 { 0 }\nimpl NonExistentTrait for i32 {}",
        crate::diagnostic::codes::E0406,
    );
}

// ── E0407: undefined type ─────────────────────────────────────────────
#[test]
fn e0407_undefined_type() {
    assert_err_code(
        "type Foo = UndefinedType;\nfunc main() -> i32 { 0 }",
        crate::diagnostic::codes::E0407,
    );
}

// ── E0257: function argument count mismatch ──────────────────────────
#[test]
fn e0257_too_few_args() {
    assert_err_code(
        "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1) }",
        crate::diagnostic::codes::E0257,
    );
}

#[test]
fn e0257_too_many_args() {
    assert_err_code(
        "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1, 2, 3) }",
        crate::diagnostic::codes::E0257,
    );
}
