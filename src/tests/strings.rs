use super::*;

#[test]
fn string_concat() {
    let src = r#"
func observed_string_concat() -> string {
    let s = "hello" + " " + "world";
    s
}
func main() -> i32 {
    if observed_string_concat() == "hello world" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_concat_empty() {
    let src = r#"
func observed_empty_string_concat() -> string {
    let s = "" + "abc" + "";
    s
}
func main() -> i32 {
    if observed_empty_string_concat() == "abc" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_trait_len() {
    let src = r#"
trait Str {
    func len() -> i32
    func trim() -> string
    func to_upper() -> string
    func to_lower() -> string
    func contains(sub: string) -> bool
    func starts_with(prefix: string) -> bool
    func ends_with(suffix: string) -> bool
    func split(delimiter: string) -> List<string>
    func replace(from: string, to: string) -> string
    func repeat(n: i32) -> string
    func char_at(index: i32) -> string
    func substring(start: i32, end: i32) -> string
    func index_of(sub: string) -> Option<i32>
}

impl Str for string {
    func len() -> i32 { len(self) }
    func trim() -> string { str_trim(self) }
    func to_upper() -> string { str_to_upper(self) }
    func to_lower() -> string { str_to_lower(self) }
    func contains(sub: string) -> bool { str_contains(self, sub) }
    func starts_with(prefix: string) -> bool { str_starts_with(self, prefix) }
    func ends_with(suffix: string) -> bool { str_ends_with(self, suffix) }
    func split(delimiter: string) -> List<string> { str_split(self, delimiter) }
    func replace(from: string, to: string) -> string { str_replace(self, from, to) }
    func repeat(n: i32) -> string { str_repeat(self, n) }
    func char_at(index: i32) -> string { str_char_at(self, index) }
    func substring(start: i32, end: i32) -> string { str_substring(self, start, end) }
    func index_of(sub: string) -> Option<i32> { str_index_of(self, sub) }
}

func main() -> i32 {
    let s = "hello world"
    s.len()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(11));
}

#[test]
fn string_trait_trim() {
    let src = r#"
trait Str { func trim() -> string }
impl Str for string { func trim() -> string { str_trim(self) } }

func observed_string_trait_trim() -> string {
    let s = "  hi  "
    s.trim()
}
func main() -> i32 {
    if observed_string_trait_trim() == "hi" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_trait_to_upper() {
    let src = r#"
trait Str { func to_upper() -> string }
impl Str for string { func to_upper() -> string { str_to_upper(self) } }

func observed_string_trait_to_upper() -> string {
    let s = "hello"
    s.to_upper()
}
func main() -> i32 {
    if observed_string_trait_to_upper() == "HELLO" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_trait_to_lower() {
    let src = r#"
trait Str { func to_lower() -> string }
impl Str for string { func to_lower() -> string { str_to_lower(self) } }

func observed_string_trait_to_lower() -> string {
    let s = "HELLO"
    s.to_lower()
}
func main() -> i32 {
    if observed_string_trait_to_lower() == "hello" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_trait_contains() {
    let src = r#"
trait Str { func contains(sub: string) -> bool }
impl Str for string { func contains(sub: string) -> bool { str_contains(self, sub) } }

func main() -> bool {
    let s = "hello world"
    s.contains("world")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn string_trait_char_at() {
    let src = r#"
trait Str { func char_at(index: i32) -> string }
impl Str for string { func char_at(index: i32) -> string { str_char_at(self, index) } }

func observed_string_trait_char_at() -> string {
    let s = "hello"
    s.char_at(1)
}
func main() -> i32 {
    if observed_string_trait_char_at() == "e" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn native_char_at_preserves_nul_scalar_length() {
    let src = r#"
func main() -> i32 {
    let first = char_at("\0a", 0)
    if first == "\0" { 0 } else { 1 }
}
"#;
    assert_eq!(
        checked_run_source_result(src).expect("bytecode char_at should preserve U+0000"),
        interp::Value::Int(0)
    );
    assert_eq!(
        checked_codegen_compile_and_run(src).expect("native char_at should preserve U+0000"),
        ""
    );
}

#[test]
fn native_char_at_preserves_supplementary_plane_scalar() {
    let src = r#"
func main() -> i32 {
    let first = char_at("\u{1F600}tail", 0)
    if first == "\u{1F600}" { 0 } else { 1 }
}
"#;
    assert_eq!(
        checked_run_source_result(src)
            .expect("bytecode char_at should preserve a supplementary scalar"),
        interp::Value::Int(0)
    );
    assert_eq!(
        checked_codegen_compile_and_run(src)
            .expect("native char_at should preserve a supplementary scalar"),
        ""
    );
}

#[test]
fn checked_native_char_at_route_reports_the_current_mir_gap() {
    let src = r#"
func main() -> i32 {
    let first = char_at("\0a", 0)
    if first == "\0" { 0 } else { 1 }
}
"#;
    let error = checked_codegen_compile_and_run(src)
        .expect_err("production native route currently rejects synthetic Main.run");
    assert!(
        error.contains("Flow 'Main' transition 'run' is outside the canonical MIR route"),
        "unexpected production char_at route diagnostic: {error}"
    );
}

#[test]
fn string_trait_substring() {
    let src = r#"
trait Str { func substring(start: i32, end: i32) -> string }
impl Str for string { func substring(start: i32, end: i32) -> string { str_substring(self, start, end) } }

func observed_string_trait_substring() -> string {
    let s = "hello world"
    s.substring(0, 5)
}
func main() -> i32 {
    if observed_string_trait_substring() == "hello" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_trait_replace() {
    let src = r#"
trait Str { func replace(from: string, to: string) -> string }
impl Str for string { func replace(from: string, to: string) -> string { str_replace(self, from, to) } }

func observed_string_trait_replace() -> string {
    let s = "hello world"
    s.replace("world", "there")
}
func main() -> i32 {
    if observed_string_trait_replace() == "hello there" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_trait_split() {
    let src = r#"
trait Str { func split(delimiter: string) -> List<string> }
impl Str for string { func split(delimiter: string) -> List<string> { str_split(self, delimiter) } }

func main() -> i32 {
    let s = "a,b,c"
    let parts = s.split(",")
    len(parts)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn string_trait_index_of() {
    let src = r#"
trait Str { func index_of(sub: string) -> Option<i32> }
impl Str for string { func index_of(sub: string) -> Option<i32> { str_index_of(self, sub) } }

func main() -> i32 {
    let s = "hello world"
    let found = s.index_of("world")
    match found {
        Some(idx) => idx,
        None => -1,
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn fstring_basic() {
    let src = r#"
func observed_fstring_basic() -> string {
    let name = "World";
    f"Hello, {name}!"
}
func main() -> i32 {
    if observed_fstring_basic() == "Hello, World!" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn fstring_multiple_interpolations() {
    let src = r#"
func observed_fstring_multiple_interpolations() -> string {
    let a = 1;
    let b = 2;
    f"{a} + {b} = {a + b}"
}
func main() -> i32 {
    if observed_fstring_multiple_interpolations() == "1 + 2 = 3" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn fstring_no_interpolation() {
    let src = r#"
func observed_fstring_no_interpolation() -> string {
    f"just text"
}
func main() -> i32 {
    if observed_fstring_no_interpolation() == "just text" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn fstring_expression_interpolation() {
    let src = r#"
func observed_fstring_expression_interpolation() -> string {
    let x = 10;
    f"double is {x * 2}"
}
func main() -> i32 {
    if observed_fstring_expression_interpolation() == "double is 20" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn fstring_with_function_call() {
    let src = r#"
func greet(name: string) -> string {
    f"Hi, {name}!"
}

func observed_fstring_with_function_call() -> string {
    greet("Alice")
}
func main() -> i32 {
    if observed_fstring_with_function_call() == "Hi, Alice!" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_compare_equal() {
    let src = r#"
func main() -> bool {
    "hello" == "hello"
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn string_compare_not_equal() {
    let src = r#"
func main() -> bool {
    "hello" != "world"
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn string_concat_long_chain() {
    let src = r#"
func observed_string_concat_long_chain() -> string {
    let s = "a" + "b" + "c" + "d" + "e";
    s
}
func main() -> i32 {
    if observed_string_concat_long_chain() == "abcde" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn fstring_integer_expression() {
    let src = r#"
func observed_fstring_integer_expression() -> string {
    let a = 10;
    let b = 20;
    f"sum = {a + b}"
}
func main() -> i32 {
    if observed_fstring_integer_expression() == "sum = 30" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn fstring_escape_consistency() {
    let src = r#"
func observed_fstring_escape_consistency() -> string {
    let a = "line1\nline2\tend";
    let b = f"line1\nline2\tend";
    if a != b { return "mismatch" }
    f"ok"
}
func main() -> i32 {
    if observed_fstring_escape_consistency() == "ok" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn fstring_boolean_interpolation() {
    let src = r#"
func observed_fstring_boolean_interpolation() -> string {
    let flag = true;
    f"flag is {flag}"
}
func main() -> i32 {
    if observed_fstring_boolean_interpolation() == "flag is true" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_from_function_return() {
    let src = r#"
func greet() -> string {
    "hello world"
}

func observed_string_from_function_return() -> string {
    greet()
}
func main() -> i32 {
    if observed_string_from_function_return() == "hello world" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn string_concat_with_variable() {
    let src = r#"
func observed_string_concat_with_variable() -> string {
    let prefix = "pre";
    let suffix = "fix";
    prefix + suffix
}
func main() -> i32 {
    if observed_string_concat_with_variable() == "prefix" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

// === Regex builtin tests ===

#[test]
fn regex_match_basic() {
    let src = r#"
func main() -> bool {
    regex_match("hello world", "world")
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn regex_match_no_match() {
    let src = r#"
func main() -> bool {
    regex_match("hello world", "xyz")
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(false));
}

#[test]
fn regex_match_digits() {
    let src = r#"
func main() -> bool {
    regex_match("abc123def", "[0-9]+")
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn regex_match_anchored() {
    let src = r#"
func main() -> bool {
    regex_match("hello world", "^hello")
}
"#;
    assert_eq!(run_source(src), interp::Value::Bool(true));
}

#[test]
fn regex_match_invalid_pattern() {
    let src = r#"
func main() -> bool {
    regex_match("hello", "[")
}
"#;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_source(src)));
    assert!(result.is_err(), "invalid regex pattern should panic");
}

#[test]
fn regex_find_basic() {
    let src = r#"
func observed_regex_find_basic() -> string {
    regex_find("hello world 42", "[0-9]+")
}
func main() -> i32 {
    if observed_regex_find_basic() == "42" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn regex_find_no_match() {
    let src = r#"
func observed_regex_find_no_match() -> string {
    regex_find("hello world", "[0-9]+")
}
func main() -> i32 {
    if observed_regex_find_no_match() == "" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn regex_replace_basic() {
    let src = r#"
func observed_regex_replace_basic() -> string {
    regex_replace("hello 42 world 99 end", "[0-9]+", "X")
}
func main() -> i32 {
    if observed_regex_replace_basic() == "hello X world X end" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn regex_replace_no_match() {
    let src = r#"
func observed_regex_replace_no_match() -> string {
    regex_replace("hello world", "[0-9]+", "X")
}
func main() -> i32 {
    if observed_regex_replace_no_match() == "hello world" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}
