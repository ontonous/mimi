//! 0.1.8 Phase C — bare Flow records must pack TransitionEpoch at escape.

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
fn epoch_rejects_bare_flow_on_channel() {
    assert_err_code(
        r#"
flow Counter {
    state Ready { n: i32 }
    transition inc(Ready, d: i32) -> Ready {
        return Ready { n: self.n + d }
    }
}
func main() -> i32 {
    let s = Ready { n: 0 }
    let ch = channel_new()
    channel_send(ch, s)
    0
}
"#,
        crate::diagnostic::codes::E0443,
    );
}

#[test]
fn epoch_rejects_bare_flow_in_extern() {
    assert_err_code(
        r#"
flow Counter {
    state Ready { n: i32 }
    transition inc(Ready, d: i32) -> Ready {
        return Ready { n: self.n + d }
    }
}
extern "C" {
    func take(s: Ready) -> i32;
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0443,
    );
}

#[test]
fn epoch_rejects_bare_flow_in_mailbox() {
    assert_err_code(
        r#"
flow Counter {
    state Ready { n: i32 }
    transition inc(Ready, d: i32) -> Ready {
        return Ready { n: self.n + d }
    }
}
actor Worker {
    func handle(s: Ready) {
        let _ = s
    }
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0443,
    );
}
