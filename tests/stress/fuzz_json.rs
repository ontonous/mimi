// JSON fuzz smoke: a batch of malformed JSON inputs must be rejected safely.
use super::run_program;

/// Escape a Rust string so it can appear inside a Mimi string literal.
fn mimi_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[test]
fn stress_json_fuzz_malformed_no_panic() {
    let cases = [
        "{",
        "}",
        "[",
        "]",
        "[1,",
        "{\"a\":",
        "{\"a\":1,}",
        "{\"a\":1]]",
        "[1 2]",
        "{\"a\":01}",
        "{\"a\":+1}",
        "nul",
        "true false",
        "\"\\x\"",
        "{\"a\":\"unterminated}",
        "[{\"a\":1},]",
    ];

    let mut src = String::from("func main() -> i32 {\n    let cases = [\n");
    for (i, case) in cases.iter().enumerate() {
        src.push_str("        ");
        src.push_str(&mimi_string_literal(case));
        if i + 1 < cases.len() {
            src.push(',');
        }
        src.push('\n');
    }
    src.push_str("    ]\n");
    src.push_str("    let mut invalid = 0\n");
    src.push_str("    for s in cases {\n");
    src.push_str("        if !json_is_valid(s) { invalid += 1 }\n");
    src.push_str("    }\n");
    src.push_str("    println(invalid)\n");
    src.push_str("    0\n}\n");

    let out = run_program(&src).expect("JSON fuzz smoke failed");
    assert_eq!(
        out.trim(),
        cases.len().to_string(),
        "all supplied malformed JSON inputs should be rejected"
    );
}
