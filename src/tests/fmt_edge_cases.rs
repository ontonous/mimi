/// Edge case tests for the Mimi formatter.
///
/// Verifies that the formatter handles all syntax constructs correctly,
/// including mms{}, rule{}, desc{}, use as, named args, default params, while let.
use crate::fmt::Formatter;

fn check_format(source: &str) -> String {
    Formatter::new().format(source)
}

#[test]
fn fmt_mms_block_string() {
    let input = "func main() -> i32 {
mms {
desc \"hello\"
rule \"world\"
}
0
}";
    let expected = "func main() -> i32 {
    mms {
        desc \"hello\"
        rule \"world\"
    }
    0
}
";
    assert_eq!(check_format(input), expected);
}

#[test]
fn fmt_mms_block_raw() {
    let input = "func f() {
mms {
...
}
}";
    let expected = "func f() {
    mms {
        ...
    }
}
";
    assert_eq!(check_format(input), expected);
}

#[test]
fn fmt_desc_block() {
    let input = "func f() {
desc {
this is a description
}
}";
    let expected = "func f() {
    desc {
        this is a description
    }
}
";
    assert_eq!(check_format(input), expected);
}

#[test]
fn fmt_rule_block() {
    let input = "func f() {
rule {
result > 0
}
}";
    let expected = "func f() {
    rule {
        result > 0
    }
}
";
    assert_eq!(check_format(input), expected);
}

#[test]
fn fmt_use_as() {
    let input = "use path::to::module as alias
func main() -> i32 { 42 }";
    let expected = "use path::to::module as alias
func main() -> i32 { 42 }
";
    assert_eq!(check_format(input), expected);
}

#[test]
fn fmt_named_args() {
    let input = "func f(x: i32, y: i32) -> i32 { x + y }
func main() -> i32 { f(x = 1, y = 2) }";
    let expected = "func f(x: i32, y: i32) -> i32 { x + y }
func main() -> i32 { f(x = 1, y = 2) }
";
    assert_eq!(check_format(input), expected);
}

#[test]
fn fmt_default_params() {
    let input = "func f(x: i32 = 5, y: i32 = 10) -> i32 { x + y }
func main() -> i32 { f() }";
    let expected = "func f(x: i32 = 5, y: i32 = 10) -> i32 { x + y }
func main() -> i32 { f() }
";
    assert_eq!(check_format(input), expected);
}

#[test]
fn fmt_while_let() {
    let input = "func main() -> i32 {
let mut xs = [1, 2, 3];
while let x = pop(xs) {
println(x)
}
0
}";
    let expected = "func main() -> i32 {
    let mut xs = [1, 2, 3];
    while let x = pop(xs) {
        println(x)
    }
    0
}
";
    assert_eq!(check_format(input), expected);
}

#[test]
fn fmt_combined_constructs() {
    let input = "func f() {
let x = 42;
desc \"some description\"
mms {
rule \"must be true\"
}
while let y = maybe() {
println(y)
}
}";
    let expected = "func f() {
    let x = 42;
    desc \"some description\"
    mms {
        rule \"must be true\"
    }
    while let y = maybe() {
        println(y)
    }
}
";
    assert_eq!(check_format(input), expected);
}

#[test]
fn fmt_idempotent() {
    let input = "func main() -> i32 {
    let x = 42;
    x
}
";
    let mut formatted = input.to_string();
    assert!(!Formatter::new().format_in_place(&mut formatted));
}

#[test]
fn fmt_multi_line_named_args() {
    let input = "func f(x: i32, y: i32) -> i32 {
f(
x = 1,
y = 2
)
}";
    let result = check_format(input);
    // The multi-line case preserves content — verify indent is maintained
    assert!(!result.is_empty());
    assert!(result.contains("x = 1"));
    assert!(result.contains("y = 2"));
}

#[test]
fn fmt_string_literal_braces_preserved() {
    let input = r#"func main() {
    let s = "a{b";
    0
}"#;
    let result = check_format(input);
    assert!(
        result.contains("\"a{b\""),
        "formatter must not insert spaces inside string literal: got {}",
        result
    );
}

#[test]
fn fmt_string_literal_colon_preserved() {
    let input = r#"func main() {
    let s = "a:b";
    0
}"#;
    let result = check_format(input);
    assert!(
        result.contains("\"a:b\""),
        "formatter must not insert spaces inside string literal: got {}",
        result
    );
}

#[test]
fn fmt_string_literal_equals_preserved() {
    let input = r#"func main() {
    let s = "a=b";
    0
}"#;
    let result = check_format(input);
    assert!(
        result.contains("\"a=b\""),
        "formatter must not insert spaces inside string literal: got {}",
        result
    );
}

#[test]
fn fmt_string_literal_combined_operators_preserved() {
    let input = r#"func main() {
    let s = "a:{b}=c,d";
    0
}"#;
    let result = check_format(input);
    assert!(
        result.contains("\"a:{b}=c,d\""),
        "formatter must not insert spaces inside string literal: got {}",
        result
    );
}

#[test]
fn fmt_string_literal_escaped_quote_preserved() {
    let input = r#"func main() {
    let s = "a\"b:c";
    0
}"#;
    let result = check_format(input);
    // The escaped quote should keep the literal open, so the colon stays inside.
    assert!(
        result.contains("\"a\\\"b:c\""),
        "formatter must handle escaped quotes: got {}",
        result
    );
}

#[test]
fn fmt_char_literal_operators_preserved() {
    let input = r#"func main() {
    let c = '{';
    0
}"#;
    let result = check_format(input);
    assert!(
        result.contains("'{'"),
        "formatter must not alter char literal: got {}",
        result
    );
}

#[test]
fn fmt_non_string_operators_still_normalized() {
    let input = "func main(){let x=1;if x>0{x=x+1}}";
    let result = check_format(input);
    assert!(
        result.contains("let x = 1"),
        "formatter should still normalize assignments: got {}",
        result
    );
    assert!(
        result.contains("if x > 0"),
        "formatter should still normalize comparisons: got {}",
        result
    );
    assert!(
        result.contains("x = x + 1"),
        "formatter should still normalize operators: got {}",
        result
    );
}

// ── FMT-OP1: multi-char operator adjacency (full audit 2026-08-05 §13.1) ──
//
// The formatter used to split multi-char operators (`==` → `= =`,
// `&&` → `& &`, ...), producing output that no longer re-parses. These
// tests pin every lexer multi-char operator to stay glued.

/// Assert `line` formats to text that contains `op` glued (no space inside).
fn assert_operator_glued(line: &str, op: &str) {
    let result = check_format(line);
    assert!(
        result.contains(op),
        "operator `{}` must survive formatting glued; got: {}",
        op,
        result
    );
    // All Mimi multi-char operators are 2 chars; the corruption form is
    // the two chars separated by a space.
    let mut chars = op.chars();
    let split = format!("{} {}", chars.next().unwrap(), chars.next().unwrap());
    assert!(
        !result.contains(&split),
        "operator `{}` was split into `{}` by formatting: {}",
        op,
        split,
        result
    );
}

#[test]
fn fmt_multi_char_comparison_operators_glued() {
    assert_operator_glued("func f(a: i32, b: i32) -> bool { a==b }", "==");
    assert_operator_glued("func f(a: i32, b: i32) -> bool { a!=b }", "!=");
    assert_operator_glued("func f(a: i32, b: i32) -> bool { a<=b }", "<=");
    assert_operator_glued("func f(a: i32, b: i32) -> bool { a>=b }", ">=");
}

#[test]
fn fmt_multi_char_assignment_operators_glued() {
    assert_operator_glued("func f() { let mut x = 1; x+=1 }", "+=");
    assert_operator_glued("func f() { let mut x = 1; x-=1 }", "-=");
    assert_operator_glued("func f() { let mut x = 1; x*=2 }", "*=");
    assert_operator_glued("func f() { let mut x = 8; x/=2 }", "/=");
}

#[test]
fn fmt_bitwise_compound_assignment_operators_glued() {
    // BitAndEq / BitOrEq / BitXorEq exist in the lexer and parser
    // (parse_stmt.rs compound assignment); they must stay glued too.
    assert_operator_glued("func f() { let mut x = 7; x&=3 }", "&=");
    assert_operator_glued("func f() { let mut x = 5; x|=2 }", "|=");
    assert_operator_glued("func f() { let mut x = 6; x^=3 }", "^=");
}

#[test]
fn fmt_multi_char_logic_operators_glued() {
    assert_operator_glued("func f(a: bool, b: bool) -> bool { a&&b }", "&&");
    assert_operator_glued("func f(a: bool, b: bool) -> bool { a||b }", "||");
}

#[test]
fn fmt_fat_arrow_and_pipe_arrow_glued() {
    assert_operator_glued("func f(x: i32) -> i32 { match x { 1=>10  _=>0 } }", "=>");
    assert_operator_glued(
        "func double(x: i32) -> i32 { x * 2 }
func f() -> i32 { 5|>double() }",
        "|>",
    );
}

#[test]
fn fmt_arrow_pow_shift_glued() {
    // `->` in signatures must survive intact.
    assert_operator_glued("func f()->i32 { 0 }", "->");
    // Pow `**` and shifts must not be split into binary */< /> pairs.
    assert_operator_glued("func f() -> i32 { 2**3 }", "**");
    assert_operator_glued("func f() -> i32 { 1<<2 }", "<<");
    assert_operator_glued("func f() -> i32 { 8>>1 }", ">>");
}

#[test]
fn fmt_multi_char_operators_get_spacing() {
    // Glued, AND spaced like other binary operators.
    let result = check_format("func f(a: i32, b: i32) -> bool { a==b }");
    assert!(
        result.contains("a == b"),
        "expected `a == b`, got: {}",
        result
    );
    let result = check_format("func f(a: bool, b: bool) -> bool { a||b }");
    assert!(
        result.contains("a || b"),
        "expected `a || b`, got: {}",
        result
    );
}

#[test]
fn fmt_single_char_operators_behavior_preserved() {
    // Genuine single-char cases keep the pre-existing normalization.
    let result = check_format("func f(a: i32, b: i32) -> i32 { a+b }");
    assert!(
        result.contains("a + b"),
        "single-char `+` spacing must be preserved: {}",
        result
    );
    let result = check_format("func f(a: i32, b: i32) -> bool { a>b }");
    assert!(
        result.contains("a > b"),
        "single-char `>` spacing must be preserved: {}",
        result
    );
}

#[test]
fn fmt_multi_char_operators_in_strings_still_preserved() {
    // Operators inside string literals remain untouched.
    let input = r#"func main() {
    let s = "a == b && c != d";
    0
}"#;
    let result = check_format(input);
    assert!(
        result.contains("\"a == b && c != d\""),
        "string content must not be re-spaced: {}",
        result
    );
}

#[test]
fn fmt_formatted_program_reparses_and_is_idempotent() {
    // CRITICAL (full audit 2026-08-05 §13.1): formatted output must still be
    // valid Mimi (parse + type-check) and formatting must be idempotent.
    let input = "func double(x: i32) -> i32 { x * 2 }
func main() -> i32 {
let a = 6
let b = 3
let eq = a==b
let ne = a!=b
let le = a<=b
let ge = a>=b
let both = eq&&ne
let either = eq||ne
let mut acc = 0
acc+=a
acc-=b
acc*=2
acc/=4
let pw = 2**3
let sl = 8>>1
let sr = 1<<2
let piped = 5|>double()
let picked = match a { 6=>1  _=>0 }
if both||either { acc + pw + sl + sr + piped + picked } else { 0 }
}";
    let formatted = check_format(input);
    // Idempotent: formatting the formatted text changes nothing.
    assert_eq!(
        check_format(&formatted),
        formatted,
        "formatter is not idempotent"
    );
    // Re-parses and type-checks (check_source panics on parse failure).
    crate::tests::check_source(&formatted)
        .unwrap_or_else(|diags| panic!("formatted output fails type check: {:?}", diags));
    // Semantic preservation: both versions run to the same value.
    let (expected, _) = crate::tests::run_source_with_stdout(input);
    let (actual, _) = crate::tests::run_source_with_stdout(&formatted);
    assert_eq!(expected, actual, "formatting changed program semantics");
}
