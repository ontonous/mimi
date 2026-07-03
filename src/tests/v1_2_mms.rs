use super::*;

#[test]
fn mms_block_basic() {
    let src = r#"
func main() -> i32 {
    mms {
        "func pay requires: balance >= amount"
    }
    42
}
"#;
    // mms block should parse and be ignored at runtime
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn mms_block_with_code() {
    let src = r#"
func pay(amount: i32) {
    mms {
        func Pay(amount):
            desc "Process payment"
            requires: amount > 0
    }
    println(amount);
}

func main() -> i32 {
    pay(100);
    42
}
"#;
    // mms block inside a function should work
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn mms_block_multiple() {
    let src = r#"
func main() -> i32 {
    mms {
        "Step 1: check balance"
    }
    mms {
        "Step 2: charge payment"
    }
    42
}
"#;
    // Multiple mms blocks should work
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn mms_parse_timeout_does_not_hang() {
    // A large MimiSpec block that takes longer than the 100ms timeout to parse.
    // The parser must still return control promptly and not hang waiting for the
    // background thread.
    let mut mms_content = String::from("func Pay(amount):\n");
    for i in 0..30000 {
        mms_content.push_str(&format!("    requires: amount > {}\n", i));
    }
    let src = format!(
        r#"
func main() -> i32 {{
    mms {{
{}
    }}
    42
}}
"#,
        mms_content
    );

    let start = std::time::Instant::now();
    let file = parse(&src);
    let elapsed = start.elapsed();

    // The function should return well before the MimiSpec parse finishes (>100ms).
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "parse hung or was too slow: {:?}",
        elapsed
    );

    // Verify the mms block was parsed into the AST.
    let func = file
        .items
        .iter()
        .find_map(|item| {
            if let crate::ast::Item::Func(f) = item {
                Some(f)
            } else {
                None
            }
        })
        .expect("should have main function");
    let mms_count = func
        .body
        .iter()
        .filter(|s| matches!(s, crate::ast::Stmt::MmsBlock { .. }))
        .count();
    assert_eq!(mms_count, 1, "should have one mms block");
}

#[cfg(target_os = "linux")]
#[test]
fn mms_parse_threads_are_reclaimed() {
    /// Count the number of kernel tasks (threads) for the current process.
    fn thread_count() -> usize {
        std::fs::read_dir("/proc/self/task")
            .map(|entries| entries.count())
            .unwrap_or(0)
    }

    let before = thread_count();

    // Parse many sources with mms blocks. Fast parses join their threads; slow
    // parses that exceed the timeout store the handle and join on Parser drop.
    // In either case the thread count should stay bounded.
    for i in 0..20 {
        let src = format!(
            r#"
func main() -> i32 {{
    mms {{
        func F{}(): requires: x > {}
    }}
    42
}}
"#,
            i, i
        );
        let _ = parse(&src);
    }

    let after = thread_count();
    // Allow for some variance (test runner threads, etc.) but do not permit
    // unbounded growth from leaked MimiSpec worker threads.
    assert!(
        after <= before + 5,
        "thread count grew from {} to {} after parsing mms blocks",
        before,
        after
    );
}
