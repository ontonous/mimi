use mimi::lexer::Lexer;
use mimi::parser::Parser;
use mimi::{core::check_program_strict, diagnostic::Diagnostic};

fn check(source: &str) -> Result<(), Vec<Diagnostic>> {
    let tokens = Lexer::new(source)
        .tokenize()
        .expect("tokenize Map/Any checked-unpack source");
    let file = Parser::new(tokens)
        .parse_file()
        .expect("parse Map/Any checked-unpack source");
    check_program_strict(&file).map(|_| ())
}

fn map_get_source_with_keys(
    payload: &str,
    set_key: &str,
    get_key: &str,
    expression: &str,
) -> String {
    if expression == "unpack_i32(pair.1)" {
        return include_str!("fixtures/mir_ordinary_map_owned_any_v2_set_get_any_i32_lineage.mimi")
            .replace(
                "map_set(root, \"answer\", 42)",
                &format!("map_set(root, \"{set_key}\", {payload})"),
            )
            .replace(
                "map_get(updated, \"answer\")",
                &format!("map_get(updated, \"{get_key}\")"),
            );
    }
    if expression == "unpack_string(pair.1)" {
        return format!(
            r#"
func main() -> i32 {{
    let created = map_new()
    match created {{
        Ok(empty) => {{
            let changed = map_set(empty, "{set_key}", {payload})
            match changed {{
                Ok(updated) => {{
                    drop(empty)
                    let pair = map_get(updated, "{get_key}")
                    drop(updated)
                    let converted = unpack_string(pair.1)
                    drop(pair)
                    drop(converted)
                    0
                }},
                Err(status) => {{ drop(empty); status }},
            }}
        }},
        Err(status) => status,
    }}
}}
"#
        );
    }
    direct_match_map_get_source_with_keys(payload, set_key, get_key, expression)
}

fn direct_match_map_get_source_with_keys(
    payload: &str,
    set_key: &str,
    get_key: &str,
    expression: &str,
) -> String {
    format!(
        r#"
func main() -> i32 {{
    match map_new() {{
        Ok(empty) => match map_set(empty, "{set_key}", {payload}) {{
            Ok(root) => {{
                drop(empty)
                let pair = map_get(root, "{get_key}")
                drop(root)
                let converted = {expression}
                drop(pair)
                drop(converted)
                0
            }},
            Err(status) => {{ drop(empty); status }},
        }},
        Err(status) => {{ status }},
    }}
}}
"#
    )
}

fn map_get_source(payload: &str, expression: &str) -> String {
    map_get_source_with_keys(payload, "key", "key", expression)
}

fn multi_set_map_get_source(expression: &str) -> String {
    format!(
        r#"
func main() -> i32 {{
    match map_new() {{
        Ok(empty) => match map_set(empty, "old", 11) {{
            Ok(first) => {{
                drop(empty)
                match map_set(first, "target", "overwritten text") {{
                    Ok(second) => {{
                        drop(first)
                        match map_set(second, "target", 73) {{
                            Ok(root) => {{
                                drop(second)
                                match map_set(root, "text", "owned text") {{
                                    Ok(other) => {{
                                        drop(root)
                                        let pair = map_get(other, "target")
                                        drop(other)
                                        let decoded = {expression}
                                        drop(pair)
                                        drop(decoded)
                                        0
                                    }},
                                    Err(status) => {{ drop(root); status }},
                                }}
                            }},
                            Err(status) => {{ drop(second); status }},
                        }}
                    }},
                    Err(status) => {{ drop(first); status }},
                }}
            }},
            Err(status) => {{ drop(empty); status }},
        }},
        Err(status) => {{ status }},
    }}
}}
"#
    )
}

#[test]
fn checked_get_unpack_intrinsics_require_exact_static_key_and_selected_type_receipts() {
    for (payload, expression) in [
        ("41", "unpack_i32(pair.1)"),
        (r#""owned text""#, "unpack_string(pair.1)"),
    ] {
        let valid = map_get_source(payload, expression);
        assert!(
            check(&valid).is_ok(),
            "the exact static-key checked unpack must typecheck: {expression}"
        );
    }

    let canonical_miss = map_get_source_with_keys("41", "stored", "missing", "unpack_i32(pair.1)");
    assert!(
        check(&canonical_miss).is_ok(),
        "a distinct static Get key retains the canonical i32 miss path"
    );

    for (source, reason) in [
        (
            map_get_source(r#""owned text""#, "unpack_i32(pair.1)"),
            "a matching key cannot reinterpret the selected String payload as i32",
        ),
        (
            map_get_source_with_keys(
                r#""owned text""#,
                "stored",
                "missing",
                "unpack_string(pair.1)",
            ),
            "a missing key cannot use a String unpack instead of the canonical i32 miss",
        ),
    ] {
        let diagnostics = check(&source).expect_err(reason);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
            "rejected checked-unpack shape must carry E0200: {diagnostics:?}"
        );
    }
}

#[test]
fn checked_tuple_get_unpack_accepts_direct_result_match_new_roots() {
    for (payload, expression) in [
        ("41", "unpack_i32(pair.1)"),
        (r#""owned text""#, "unpack_string(pair.1)"),
    ] {
        let source = direct_match_map_get_source_with_keys(payload, "key", "key", expression);
        assert!(
            check(&source).is_ok(),
            "direct match map_new Result path must retain the public tuple Get/unpack source contract: {expression}"
        );
    }

    let missing_string = direct_match_map_get_source_with_keys(
        r#""owned text""#,
        "stored",
        "missing",
        "unpack_string(pair.1)",
    );
    let diagnostics = check(&missing_string)
        .expect_err("a direct Result match cannot authorize String unpack at a static miss");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
        "direct-match miss mismatch must carry E0200: {diagnostics:?}"
    );
}

#[test]
fn checked_get_unpack_uses_the_newest_set_for_the_exact_queried_key() {
    let newest_same_key = multi_set_map_get_source("unpack_i32(pair.1)");
    let newest_same_key_check = check(&newest_same_key);
    assert!(
        newest_same_key_check.is_ok(),
        "the newest same-key i32 Set must determine the queried payload type: {:?}",
        newest_same_key_check.err()
    );

    let newest_string = multi_set_map_get_source("unpack_string(pair.1)")
        .replace("map_get(other, \"target\")", "map_get(other, \"text\")");
    assert!(
        check(&newest_string).is_ok(),
        "a multi-Set Get must select the exact winning String TypeDesc for its queried key"
    );

    let different_key_string = multi_set_map_get_source("unpack_string(pair.1)");
    let diagnostics = check(&different_key_string)
        .expect_err("a String selected at another key cannot authorize unpacking target");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
        "a non-winning Set key must not authorize the unpack: {diagnostics:?}"
    );

    let missing_key = multi_set_map_get_source("unpack_i32(pair.1)")
        .replace("map_get(other, \"target\")", "map_get(other, \"missing\")");
    assert!(
        check(&missing_key).is_ok(),
        "a statically proven miss keeps only the canonical i32 unpack"
    );

    let string_miss = multi_set_map_get_source("unpack_string(pair.1)")
        .replace("map_get(other, \"target\")", "map_get(other, \"missing\")");
    let diagnostics = check(&string_miss)
        .expect_err("a static miss cannot reinterpret the canonical i32 token as String");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
        "a String unpack at a static miss must remain fail-closed: {diagnostics:?}"
    );
}

#[test]
fn checked_get_unpack_intrinsics_reject_unrelated_any_and_wrong_arity() {
    let unrelated_any = r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => match map_set(empty, "key", 41) {
            Ok(root) => {
                drop(empty)
                let snapshot = values(root)
                let value = snapshot[0]
                let decoded = unpack_i32(value)
                drop(decoded)
                drop(snapshot)
                drop(root)
                0
            },
            Err(status) => { drop(empty); status },
        },
        Err(status) => { status },
    }
}
"#;
    let diagnostics =
        check(unrelated_any).expect_err("an unrelated Any value has no checked Map.GetAny receipt");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
        "an unrelated Any unpack must carry E0200: {diagnostics:?}"
    );

    for expression in ["unpack_i32()", "unpack_i32(pair.1, 7)"] {
        let source = map_get_source("41", expression);
        let diagnostics = check(&source).expect_err("checked-unpack arity is exactly one");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("E0242")),
            "wrong checked-unpack arity must carry E0242: {diagnostics:?}"
        );
    }
}

#[test]
fn checked_get_unpack_does_not_authorize_casts_values_or_shadowed_functions() {
    let arbitrary_cast = map_get_source("41", "pair.1 as i32");
    let diagnostics = check(&arbitrary_cast)
        .expect_err("the dedicated GetAny unpack form must not loosen ordinary Any cast semantics");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
        "an ordinary Any cast must retain E0200: {diagnostics:?}"
    );
    let arbitrary_string_cast = map_get_source(r#""owned text""#, "pair.1 as string");
    let diagnostics = check(&arbitrary_string_cast)
        .expect_err("the dedicated GetAny unpack form must not loosen String casts either");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
        "an ordinary Any-to-String cast must retain E0200: {diagnostics:?}"
    );

    let values_cast = r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => match map_set(empty, "key", 41) {
            Ok(root) => {
                drop(empty)
                let snapshot = values(root)
                let payload = unpack_i32(snapshot[0])
                drop(snapshot)
                drop(root)
                payload
            },
            Err(status) => { drop(empty); status },
        },
        Err(status) => { status },
    }
}
"#;
    let values_diagnostics =
        check(values_cast).expect_err("Values extraction has no CheckedGet unpack authority");
    assert!(
        values_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
        "Values extraction must fail with E0200: {values_diagnostics:?}"
    );

    let shadowed_builtin = r#"
func unpack_i32(value: i32) -> i32 { value }
func main() -> i32 { unpack_i32(7) }
"#;
    assert!(
        check(shadowed_builtin).is_ok(),
        "a user global with the intrinsic spelling must keep normal function resolution"
    );
}

#[test]
fn map_get_requires_a_root_after_the_map_new_result_is_matched() {
    for (source, expected_type) in [
        (
            r#"
func main() -> i32 {
    let pair = map_get(map_new(), "key")
    drop(pair)
    0
}

"#,
            "Result<Record, i32>",
        ),
        (
            r#"
func main() -> i32 {
    let unhandled = map_new()
    let pair = map_get(unhandled, "key")
    drop(pair)
    drop(unhandled)
    0
}
"#,
            "Result<Record, i32>",
        ),
        (
            r#"
func main() -> i32 {
    let changed = map_set(map_new(), "key", 41)
    drop(changed)
    0
}
"#,
            "Result<Record, i32>",
        ),
        (
            r#"
func main() -> i32 {
    let unhandled = map_new()
    let changed = map_set(unhandled, "key", 41)
    drop(changed)
    drop(unhandled)
    0
}
"#,
            "Result<Record, i32>",
        ),
        (
            r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => {
            let pair = map_get(root, 7)
            drop(pair)
            drop(root)
            0
        },
        Err(status) => { status },
    }
}

"#,
            "String key",
        ),
    ] {
        let diagnostics = check(source).expect_err("invalid map_get operands must fail in Checker");
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_deref() == Some("E0242"))
            .expect("map_get operand rejection uses E0242");
        assert!(
            diagnostic.message.contains(expected_type),
            "diagnostic must identify the rejected operand ({expected_type}): {diagnostic:?}"
        );
    }

    let properly_matched = r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => {
            let pair = map_get(root, "key")
            drop(pair)
            drop(root)
            0
        },
        Err(status) => { status },
    }
}
"#;
    assert!(
        check(properly_matched).is_ok(),
        "a map_get over the exact Ok root remains source-compatible"
    );
}

#[test]
fn map_get_accepts_homogeneous_string_keyed_map_literals() {
    let valid_map_literal = r#"
func main() -> i32 {
    let values = {"answer": 42}
    let pair = map_get(values, "answer")
    drop(pair)
    drop(values)
    0
}
"#;
    assert!(
        check(valid_map_literal).is_ok(),
        "map_get accepts Map<string, T> produced by a homogeneous literal"
    );

    let non_string_key_map = r#"
func get_from_non_string_map(values: Map<i32, i32>) -> i32 {
    let pair = map_get(values, "answer")
    drop(pair)
    0
}
func main() -> i32 { 0 }
"#;
    let diagnostics =
        check(non_string_key_map).expect_err("map_get must reject Map<K, T> when K is not string");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("E0242")
                && diagnostic.message.contains("Map<string, T>")
        }),
        "non-string-key Map receiver rejection must be explicit: {diagnostics:?}"
    );
}

#[test]
fn map_set_requires_string_keys_and_rejects_linear_any_payloads() {
    let non_string_key = r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => match map_set(root, 7, 41) {
            Ok(updated) => { drop(root); drop(updated); 0 },
            Err(status) => { drop(root); status },
        },
        Err(status) => { status },
    }
}
"#;
    let diagnostics =
        check(non_string_key).expect_err("map_set must reject non-string keys at Checker time");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("E0242") && diagnostic.message.contains("String key")
        }),
        "non-string map_set key must be diagnosed: {diagnostics:?}"
    );

    let linear_payload = r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => {
            let token = make_token()
            let changed = map_set(root, "token", token)
            drop(changed)
            0
        },
        Err(status) => { status },
    }
}
"#;
    let diagnostics =
        check(linear_payload).expect_err("a linear token cannot be erased into a Map Any entry");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_deref(),
                Some("E0256" | "E0304" | "E0432")
            )
        }),
        "linear Map payload must retain its ownership diagnostic: {diagnostics:?}"
    );
}
