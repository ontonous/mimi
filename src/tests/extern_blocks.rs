use super::*;

#[test]
fn extern_block_basic() {
    let src = r#"
extern "C" {
    func printf(fmt: string) -> i32;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_multiple_funcs() {
    let src = r#"
extern "C" {
    func malloc(size: i32) -> i32;
    func free(ptr: i32);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_with_cap() {
    let src = r#"
cap FileReadCap;

extern "C" {
    func read(fd: i32, file_cap: FileReadCap) -> string;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_with_borrow() {
    let src = r#"
cap FileReadCap;

extern "C" {
    func read(fd: i32, file_cap: FileReadCap) -> string;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_with_multiple_params() {
    let src = r#"
extern "C" {
    func write(fd: i32, buf: string, len: i32) -> i32;
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_with_no_return() {
    let src = r#"
extern "C" {
    func exit(code: i32);
}

func main() -> i32 {
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn extern_block_libc_name_import_native_i32() {
    // M-001(a): a user FFI import of a libc name at a *narrower* int width
    // (`i32` instead of the runtime's pre-declared `i64`) must not collide with
    // the codegen pre-declared helper. The runtime now declares its libc helpers
    // under the `mimi_rt_*` prefix, leaving the bare libc name free for the
    // user import (links to libc directly). This is the architectural closure of
    // the import-naming collision — builtins (`mimi_rt_*`) and user FFI imports
    // (`*`) live in disjoint symbol namespaces.
    let src = r#"
extern "C" {
    func strlen(s: string) -> i32;
}
func main() -> i32 {
    let s = "hello world";
    println(strlen(s));
    0
}
"#;
    let out = compile_and_run(src).expect("native compile+run of libc-name import failed");
    assert_eq!(out.trim(), "11");
}
