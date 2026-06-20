use proptest::prelude::*;

/// Generate a random Mimi expression as a string.
pub fn arb_expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        any::<i64>().prop_map(|n| n.to_string()),
        any::<i64>().prop_map(|n| if n % 2 == 0 { "true".into() } else { "false".into() }),
        any::<i64>().prop_map(|n| format!("\"val_{}\"", n.abs() % 100)),
        Just("x".into()),
        Just("y".into()),
    ];
    leaf.prop_recursive(3, 8, 3, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} + {})", a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} - {})", a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} * {})", a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} / {})", a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} > {})", a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} < {})", a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({} == {})", a, b)),
            inner.clone().prop_map(|e| format!("-{}", e)),
        ]
    })
}

/// Generate a random Mimi program source string with a `main` function.
pub fn arb_mimi_program() -> impl Strategy<Value = String> {
    let stmts = proptest::collection::vec(prop_oneof![
        arb_expr().prop_map(|e| format!("println({});", e)),
        arb_expr().prop_map(|e| format!("let x = {};", e)),
        arb_expr().prop_map(|e| format!("let y = {};", e)),
    ], 0..5);
    stmts.prop_map(|stmts| {
        let body = if stmts.is_empty() {
            "0".to_string()
        } else {
            stmts.join("\n    ") + "\n    0"
        };
        format!("func main() -> i32 {{\n    {}\n}}", body)
    })
}

/// Generate random byte strings for parser fuzzing (no regex dependency).
pub fn arb_random_source() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<u8>(), 0..200)
        .prop_map(|bytes| {
            bytes.into_iter()
                .map(|b| {
                    let printable = b.wrapping_add(32) % 95 + 32;
                    printable as char
                })
                .collect()
        })
        .prop_map(|s: String| {
            // Replace NUL bytes that may slip through
            s.chars().filter(|&c| c != '\0').collect()
        })
}
