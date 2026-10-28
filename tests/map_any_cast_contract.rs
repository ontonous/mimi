use mimi::lexer::Lexer;
use mimi::parser::Parser;
use mimi::{core::check_program_strict, diagnostic::Diagnostic};

fn check(source: &str) -> Result<(), Vec<Diagnostic>> {
    let tokens = Lexer::new(source)
        .tokenize()
        .expect("tokenize Map/Any cast source");
    let file = Parser::new(tokens)
        .parse_file()
        .expect("parse Map/Any cast source");
    check_program_strict(&file).map(|_| ())
}

fn map_get_source(payload: &str, expression: &str) -> String {
    format!(
        r#"
func main() -> i32 {{
    let empty = map_new()
    let root = map_set(empty, "key", {payload})
    let pair = map_get(root, "key")
    let converted = {expression}
    drop(pair)
    drop(root)
    drop(empty)
    drop(converted)
    0
}}
"#
    )
}

#[test]
fn ordinary_map_any_conversion_builtins_are_checkable_but_any_casts_are_rejected() {
    for (target, source_value, conversion) in [
        ("i32", "41", "to_int(pair.1)"),
        ("string", r#""owned text""#, "to_string(pair.1)"),
    ] {
        let valid = map_get_source(source_value, conversion);
        assert!(
            check(&valid).is_ok(),
            "the supported {conversion} Any conversion must typecheck"
        );

        let invalid = map_get_source(source_value, &format!("pair.1 as {target}"));
        let diagnostics = check(&invalid)
            .expect_err("an Any payload must not be cast outside the defined cast contract");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_deref() == Some("E0200")),
            "the rejected Any->{target} cast must carry E0200: {diagnostics:?}"
        );
    }
}
