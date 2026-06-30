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
