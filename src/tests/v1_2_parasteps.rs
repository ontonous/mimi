use super::*;

#[test]
fn parasteps_local_shared_keyword_removed() {
    // 0.35.39: `local_shared` is a culled keyword (no longer in the language).
    // `shared` is the only supported ownership keyword; `local_shared` is now
    // parsed as a plain identifier, so this program is rejected at parse time.
    let src = r#"
func main() -> i32 {
    local_shared x = 42;
    parasteps {
        println(x);
    }
    42
}
"#;
    let result = check_source(src);
    assert!(
        result.is_err(),
        "local_shared is no longer a keyword and must be rejected"
    );
}

#[test]
fn parasteps_shared_allowed() {
    let src = r#"
func main() -> i32 {
    shared x = 42;
    parasteps {
        println(x);
    }
    42
}
"#;
    // shared can be captured in parasteps
    let result = check_source(src);
    assert!(result.is_ok());
}
