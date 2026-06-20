use super::*;

#[test]
fn string_concat() {
    let src = r#"
func main() -> string {
    let s = "hello" + " " + "world";
    s
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello world".to_string()));
}

#[test]
fn string_concat_empty() {
    let src = r#"
func main() -> string {
    let s = "" + "abc" + "";
    s
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("abc".to_string()));
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
    func index_of(sub: string) -> i32
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
    func index_of(sub: string) -> i32 { str_index_of(self, sub).1 }
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

func main() -> string {
    let s = "  hi  "
    s.trim()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hi".to_string()));
}

#[test]
fn string_trait_to_upper() {
    let src = r#"
trait Str { func to_upper() -> string }
impl Str for string { func to_upper() -> string { str_to_upper(self) } }

func main() -> string {
    let s = "hello"
    s.to_upper()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("HELLO".to_string()));
}

#[test]
fn string_trait_to_lower() {
    let src = r#"
trait Str { func to_lower() -> string }
impl Str for string { func to_lower() -> string { str_to_lower(self) } }

func main() -> string {
    let s = "HELLO"
    s.to_lower()
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello".to_string()));
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

func main() -> string {
    let s = "hello"
    s.char_at(1)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("e".to_string()));
}

#[test]
fn string_trait_substring() {
    let src = r#"
trait Str { func substring(start: i32, end: i32) -> string }
impl Str for string { func substring(start: i32, end: i32) -> string { str_substring(self, start, end) } }

func main() -> string {
    let s = "hello world"
    s.substring(0, 5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello".to_string()));
}

#[test]
fn string_trait_replace() {
    let src = r#"
trait Str { func replace(from: string, to: string) -> string }
impl Str for string { func replace(from: string, to: string) -> string { str_replace(self, from, to) } }

func main() -> string {
    let s = "hello world"
    s.replace("world", "there")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello there".to_string()));
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
trait Str { func index_of(sub: string) -> i32 }
impl Str for string { func index_of(sub: string) -> i32 { str_index_of(self, sub).1 } }

func main() -> i32 {
    let s = "hello world"
    s.index_of("world")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn fstring_basic() {
    let src = r#"
func main() -> string {
    let name = "World";
    f"Hello, {name}!"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("Hello, World!".to_string()));
}

#[test]
fn fstring_multiple_interpolations() {
    let src = r#"
func main() -> string {
    let a = 1;
    let b = 2;
    f"{a} + {b} = {a + b}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("1 + 2 = 3".to_string()));
}

#[test]
fn fstring_no_interpolation() {
    let src = r#"
func main() -> string {
    f"just text"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("just text".to_string()));
}

#[test]
fn fstring_expression_interpolation() {
    let src = r#"
func main() -> string {
    let x = 10;
    f"double is {x * 2}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("double is 20".to_string()));
}

#[test]
fn fstring_with_function_call() {
    let src = r#"
func greet(name: string) -> string {
    f"Hi, {name}!"
}

func main() -> string {
    greet("Alice")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("Hi, Alice!".to_string()));
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
func main() -> string {
    let s = "a" + "b" + "c" + "d" + "e";
    s
}
"#;
    assert_eq!(run_source(src), interp::Value::String("abcde".to_string()));
}

#[test]
fn fstring_integer_expression() {
    let src = r#"
func main() -> string {
    let a = 10;
    let b = 20;
    f"sum = {a + b}"
}
"#;
    assert_eq!(run_source(src), interp::Value::String("sum = 30".to_string()));
}

#[test]
fn fstring_boolean_interpolation() {
    let src = r#"
func main() -> string {
    let flag = true;
    f"flag is {flag}"
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("flag is true".to_string()));
}

#[test]
fn string_from_function_return() {
    let src = r#"
func greet() -> string {
    "hello world"
}

func main() -> string {
    greet()
}
"#;
    assert_eq!(run_source(src), interp::Value::String("hello world".to_string()));
}

#[test]
fn string_concat_with_variable() {
    let src = r#"
func main() -> string {
    let prefix = "pre";
    let suffix = "fix";
    prefix + suffix
}
"#;
    assert_eq!(run_source(src), interp::Value::String("prefix".to_string()));
}
