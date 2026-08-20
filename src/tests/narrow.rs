//! 0.1.8 Phase A — view/mutate/ref must not cross a task boundary.
//! DSP synchronous mutate on `func` parameters stays legal.

use super::*;

fn has_code(errors: &[crate::diagnostic::Diagnostic], code: &str) -> bool {
    errors.iter().any(|e| e.code.as_deref() == Some(code))
}

fn assert_err_code(src: &str, expected: &str) {
    let errors = match check_source(src) {
        Err(errors) => errors,
        Ok(()) => panic!("expected error {expected}, but check succeeded\nsrc: {src}"),
    };
    assert!(
        has_code(&errors, expected),
        "expected {expected}, got codes: {:?}\nmessages: {:?}\nsrc: {src}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>(),
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn narrow_rejects_view_across_spawn() {
    assert_err_code(
        r#"
func peek(x: view i32) -> i32 { x }
func main() -> i32 {
    let t = spawn peek(7)
    let _ = await t
    0
}
"#,
        crate::diagnostic::codes::E0442,
    );
}

#[test]
fn narrow_rejects_mutate_in_channel() {
    assert_err_code(
        r#"
func bad(x: mutate i32) -> i32 {
    let ch = channel_new()
    channel_send(ch, x)
    0
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0442,
    );
}

#[test]
fn narrow_rejects_ref_in_future_env() {
    assert_err_code(
        r#"
func id(n: i32) -> i32 { n }
func main() -> i32 {
    let x = 1
    let r = &x
    let t = spawn id(r)
    let _ = await t
    0
}
"#,
        crate::diagnostic::codes::E0442,
    );
}

#[test]
fn narrow_rejects_view_in_mailbox() {
    assert_err_code(
        r#"
actor Worker {
    func handle(x: view i32) {
        let _ = x
    }
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0442,
    );
}

#[test]
fn narrow_dsp_sync_mutate_still_ok() {
    let src = r#"
func bump(xs: mutate List<i32>) -> i32 {
    xs[0] = xs[0] + 1
    xs[0]
}
func main() -> i32 {
    let xs = [1, 2]
    println(bump(xs))
    0
}
"#;
    check_source(src).unwrap_or_else(|diags| {
        panic!(
            "DSP sync mutate must stay legal:\n{}",
            diags
                .iter()
                .map(|d| format!("{}", d))
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
}
