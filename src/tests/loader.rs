use crate::interp::{Interpreter, Value};
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::loader::ModuleLoader;
use std::path::PathBuf;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("mimi_test_{}_{}", label, n));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn cleanup(dir: &PathBuf) {
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn loader_module_relative_import() {
    let dir = temp_dir("relative_import");
    let lib_dir = dir.join("lib");
    fs::create_dir_all(&lib_dir).expect("create lib dir");

    fs::write(lib_dir.join("helper.mimi"), r#"
pub func greet(name: string) -> string {
    "Hello, " + name + "!"
}
"#).expect("write helper.mimi");

    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use lib::helper

func main() -> string {
    helper::greet("world")
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    let s = match &val {
        Value::String(s) => s.clone(),
        _ => { panic!("expected string, got {:?}", val); }
    };
    assert_eq!(s, "Hello, world!", "expected greeting");
    cleanup(&dir);
}

#[test]
fn loader_module_stdlib_import() {
    let dir = temp_dir("stdlib_import");
    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use std::testing

func main() -> i32 {
    testing::assert_eq_int(1, 1)
    0
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");
    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");
    cleanup(&dir);
}

#[test]
fn loader_module_stdlib_re_export() {
    let dir = temp_dir("stdlib_re_export");
    fs::write(dir.join("mimi.json"), r#"{}"#).expect("write json");
    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use std::mymath::abs

func main() -> i32 {
    abs(-5)
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    let n = match &val {
        Value::Int(n) => *n,
        _ => { panic!("expected int, got {:?}", val); }
    };
    assert_eq!(n, 5, "expected abs(-5) == 5");
    cleanup(&dir);
}

#[test]
fn loader_mimi_tokenizer_simple_word() {
    let dir = temp_dir("mimi_tokenizer_word2");
    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use std::mimispec_lexer::tokenize

func main() -> i32 {
    let tokens = tokenize("hello")
    len(tokens)
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    let count = match &val {
        Value::Int(n) => *n as i32,
        _ => { panic!("expected int, got {:?}", val); }
    };
    assert_eq!(count, 2, "expected 2 tokens for 'hello', got {}", count);
    cleanup(&dir);
}

#[test]
fn loader_mimi_tokenizer_simple_with_string() {
    let dir = temp_dir("mimi_tokenizer_str1");
    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use std::mimispec_lexer::tokenize

func main() -> i32 {
    let tokens = tokenize("hello \"world\"")
    len(tokens)
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    let count = match &val {
        Value::Int(n) => *n as i32,
        _ => { panic!("expected int, got {:?}", val); }
    };
    assert_eq!(count, 3, "expected 3 tokens, got {}", count);
    cleanup(&dir);
}

#[test]
fn loader_mimi_tokenizer_simple_newline() {
    let dir = temp_dir("mimi_tokenizer_nl");
    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use std::mimispec_lexer::tokenize

func main() -> i32 {
    let tokens = tokenize("a\nb")
    len(tokens)
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    let count = match &val {
        Value::Int(n) => *n as i32,
        _ => { panic!("expected int, got {:?}", val); }
    };
    assert_eq!(count, 4, "expected 4 tokens, got {}", count);
    cleanup(&dir);
}

#[test]
fn loader_mimi_tokenizer_newline_with_ident() {
    let dir = temp_dir("mimi_tokenizer_nl2");
    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use std::mimispec_lexer::tokenize

func main() -> i32 {
    let tokens = tokenize("a\n    b")
    len(tokens)
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    let count = match &val {
        Value::Int(n) => *n as i32,
        _ => { panic!("expected int, got {:?}", val); }
    };
    assert_eq!(count, 4, "expected 4 tokens, got {}", count);
    cleanup(&dir);
}

#[test]
fn loader_mimi_tokenizer_space_indent() {
    let dir = temp_dir("mimi_tokenizer_indent");
    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use std::mimispec_lexer::tokenize

func main() -> i32 {
    let tokens = tokenize("a:\n    b\nc")
    len(tokens)
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    let count = match &val {
        Value::Int(n) => *n as i32,
        _ => { panic!("expected int, got {:?}", val); }
    };
    assert_eq!(count, 8, "expected 8 tokens, got {}", count);
    cleanup(&dir);
}

#[test]
fn loader_mimi_tokenizer_debug_simple_colon() {
    // Test: ONELINE if-else chain with matched + continue inside while
    let dir2 = temp_dir("mimi_tokenizer_final");
    let main_path = dir2.join("main.mimi");
    fs::write(&main_path, r#"
func main() -> i32 {
    let mut items: List<i32> = []
    let mut pos: i32 = 0
    while pos < 1 {
        let c = str_char_at(":", pos)
        let mut matched = true
        if c == ":" { push(items, 1)
        } else if c == "," { push(items, 2)
        } else { matched = false }
        if matched {
            pos = pos + 1
            continue
        }
        pos = pos + 1
    }
    push(items, 99)
    42
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir2.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    assert_eq!(val, Value::Int(42), "expected Int(42), got {:?}", val);
    cleanup(&dir2);
}

#[test]
fn loader_mimi_tokenizer_debug_colon_newline() {
    let dir = temp_dir("mimi_tokenizer_cnl");
    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use std::mimispec_lexer::tokenize

func main() -> i32 {
    let tokens = tokenize("a:\nb")
    len(tokens)
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    let count = match &val {
        Value::Int(n) => *n as i32,
        _ => { panic!("expected int, got {:?}", val); }
    };
    assert_eq!(count, 5, "expected 5 tokens (Ident, Colon, Newline, Ident, Eof), got {}", count);
    cleanup(&dir);
}

#[test]
fn loader_mimi_tokenizer_with_string() {
    let dir = temp_dir("mimi_tokenizer_str2");
    let main_path = dir.join("main.mimi");
    fs::write(&main_path, r#"
use std::mimispec_lexer::tokenize

func main() -> i32 {
    let tokens = tokenize("a \"b\" c")
    len(tokens)
}
"#).expect("write main.mimi");

    let mut loader = ModuleLoader::new(dir.clone());
    let r = loader.load_main(&main_path);
    assert!(r.is_ok(), "load_main should succeed: {:?}", r.err());
    let merged = loader.merge_all().expect("merge_all should succeed");

    let mut interp = Interpreter::new(&merged);
    let val = interp.run().expect("interpreter should run without error");

    let count = match &val {
        Value::Int(n) => *n as i32,
        _ => { panic!("expected int, got {:?}", val); }
    };
    assert_eq!(count, 4, "expected 4 tokens, got {}", count);
    cleanup(&dir);
}
