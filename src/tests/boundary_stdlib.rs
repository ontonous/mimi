// Boundary tests for directory/path/crypto builtins (v0.28.0)
// Tests edge cases and error conditions

use super::*;

// ====== Directory Operations Boundary Tests ======

#[test]
fn boundary_listdir_nonexistent() {
    let v = run_source(
        "func main() -> i32 { let e = listdir(\"/nonexistent_path_xyz\"); println(len(e)); 0 }",
    );
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn boundary_walk_dir_nonexistent() {
    let v = run_source(
        "func main() -> i32 { let e = walk_dir(\"/nonexistent_path_xyz\"); println(len(e)); 0 }",
    );
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn boundary_is_dir_nonexistent() {
    let v =
        run_source("func main() -> i32 { if is_dir(\"/nonexistent_path_xyz\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn boundary_is_file_nonexistent() {
    let v =
        run_source("func main() -> i32 { if is_file(\"/nonexistent_path_xyz\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn boundary_mkdir_p_existing() {
    let v = run_source("func main() -> i32 { if mkdir_p(\"/tmp\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn boundary_remove_file_nonexistent() {
    let v = run_source(
        "func main() -> i32 { if remove_file(\"/nonexistent_file_xyz\") { 1 } else { 0 } }",
    );
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn boundary_listdir_returns_list() {
    let v = run_source("func main() -> i32 { let e = listdir(\".\"); len(e) }");
    match v {
        interp::Value::Int(n) => assert!(n > 0, "expected entries in current dir, got {}", n),
        _ => panic!("expected Int, got {:?}", v),
    }
}

#[test]
fn boundary_walk_dir_recursive() {
    let v = run_source("func main() -> i32 { let e = walk_dir(\"examples\"); len(e) }");
    match v {
        interp::Value::Int(n) => assert!(n > 10, "expected many files in examples/, got {}", n),
        _ => panic!("expected Int, got {:?}", v),
    }
}

// ====== Crypto Boundary Tests ======

#[test]
fn boundary_sha256_empty() {
    let src = r#"
func observed_sha256_empty() -> string { sha256("") }
func main() -> i32 {
    if observed_sha256_empty() == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_sha256_known_vector() {
    let src = r#"
func observed_sha256_abc() -> string { sha256("abc") }
func main() -> i32 {
    if observed_sha256_abc() == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_sha256_length() {
    let v = run_source("func main() -> i32 { len(sha256(\"test\")) }");
    assert_eq!(v, interp::Value::Int(64));
}

#[test]
fn boundary_base64_encode_empty() {
    let src = r#"
func observed_base64_encode_empty() -> string { base64_encode("") }
func main() -> i32 {
    if observed_base64_encode_empty() == "" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_base64_encode_known() {
    let src = r#"
func observed_base64_encode_hello() -> string { base64_encode("Hello") }
func main() -> i32 {
    if observed_base64_encode_hello() == "SGVsbG8=" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_base64_roundtrip() {
    let v = run_source(
        r#"
func observed_base64_roundtrip() -> string {
    let original = "Hello, Mimi!"
    let encoded = base64_encode(original)
    let decoded = base64_decode(encoded)
    match decoded {
        Ok(s) => s,
        Err(e) => "error"
    }
}
func main() -> i32 {
    if observed_base64_roundtrip() == "Hello, Mimi!" { 0 } else { 1 }
}
"#,
    );
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn boundary_base64_decode_invalid() {
    let v = run_source(
        r#"
func observed_base64_decode_invalid() -> string {
    let decoded = base64_decode("not!valid!!!")
    match decoded {
        Ok(s) => "ok",
        Err(e) => "err"
    }
}
func main() -> i32 {
    if observed_base64_decode_invalid() == "err" { 0 } else { 1 }
}
"#,
    );
    assert_eq!(v, interp::Value::Int(0));
}

// ====== Path Join Tests ======

#[test]
fn boundary_path_join_empty() {
    let src = r#"
func observed_path_join_empty() -> string { path_join("", "b") }
func main() -> i32 {
    if observed_path_join_empty() == "b" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_path_join_triple() {
    let src = r#"
func observed_path_join_triple() -> string { path_join(path_join("a", "b"), "c") }
func main() -> i32 {
    if observed_path_join_triple() == "a/b/c" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_path_ext_no_ext() {
    let v = run_source("func main() -> i32 { len(path_ext(\"Makefile\")) }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn boundary_path_ext_has_ext() {
    let src = r#"
func observed_path_ext_has_ext() -> string { path_ext("file.txt") }
func main() -> i32 {
    if observed_path_ext_has_ext() == "txt" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_path_basename_root() {
    let src = r#"
func observed_path_basename_root() -> string { path_basename("/") }
func main() -> i32 {
    if observed_path_basename_root() == "" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_path_dirname_root() {
    let src = r#"
func observed_path_dirname_root() -> string { path_dirname("/") }
func main() -> i32 {
    if observed_path_dirname_root() == "" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_path_basename_simple() {
    let src = r#"
func observed_path_basename_simple() -> string { path_basename("/a/b/c.txt") }
func main() -> i32 {
    if observed_path_basename_simple() == "c.txt" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn boundary_path_dirname_simple() {
    let src = r#"
func observed_path_dirname_simple() -> string { path_dirname("/a/b/c.txt") }
func main() -> i32 {
    if observed_path_dirname_simple() == "/a/b" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

// ====== Codegen boundary tests ======

// ====== Additional codegen boundary tests ======

#[test]
fn codegen_boundary_is_dir() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            if is_dir(".") { println("dir") } else { println("not") }
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(out.trim(), "dir");
}

#[test]
fn codegen_boundary_is_file() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            if is_file("examples/hello.mimi") { println("file") } else { println("not") }
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(out.trim(), "file");
}

#[test]
fn codegen_boundary_path_ext() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            println(path_ext("test.mimi"))
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(out.trim(), "mimi");
}

#[test]
fn codegen_boundary_path_basename() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            println(path_basename("/a/b/c.txt"))
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(out.trim(), "c.txt");
}

#[test]
fn codegen_boundary_path_dirname() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            println(path_dirname("/a/b/c.txt"))
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(out.trim(), "/a/b");
}

#[test]
fn codegen_boundary_mkdir_p() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            if mkdir_p("/tmp/mimi_cg_test") { println("ok") } else { println("fail") }
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(out.trim(), "ok");
    std::fs::remove_dir("/tmp/mimi_cg_test").ok();
}

#[test]
fn codegen_boundary_for_loop_listdir() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            let entries = listdir("examples")
            let mut count = 0
            for e in entries {
                count = count + 1
            }
            println(count)
            0
        }
    "#,
    )
    .expect("codegen failed");
    let n: i32 = out.trim().parse().unwrap_or(0);
    assert!(n > 10, "expected many entries, got {}", n);
}

#[test]
fn codegen_boundary_sha256_empty() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            println(sha256(""))
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(
        out.trim(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

fn can_codegen() -> bool {
    std::process::Command::new("cc")
        .arg("--version")
        .output()
        .is_ok()
}

#[test]
fn codegen_boundary_sha256() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            println(sha256("abc"))
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(
        out.trim(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn codegen_boundary_base64() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            println(base64_encode("Hello"))
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(out.trim(), "SGVsbG8=");
}

#[test]
fn codegen_boundary_path_join() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            println(path_join(path_join("a", "b"), "c"))
            0
        }
    "#,
    )
    .expect("codegen failed");
    assert_eq!(out.trim(), "a/b/c");
}

#[test]
fn codegen_boundary_listdir() {
    if !can_codegen() {
        return;
    }
    let out = compile_and_run(
        r#"
        func main() -> i32 {
            let e = listdir("examples")
            println(len(e))
            0
        }
    "#,
    )
    .expect("codegen failed");
    let n: i32 = out.trim().parse().unwrap_or(0);
    assert!(n > 0, "expected entries in examples/, got {}", n);
}
