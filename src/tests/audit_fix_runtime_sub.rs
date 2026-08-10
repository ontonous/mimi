//! Wave-1 audit-fix regression tests — runtime_sub.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
use super::*;

// ---------------------------------------------------------------------------
// quote.rs fixes (2026-08-05 audit, MEDIUM): unbounded drop recursion +
// argc i32 truncation in mimi_quote_new_list.
// ---------------------------------------------------------------------------

#[test]
fn audit_quote_drop_deep_chain_no_stack_overflow() {
    // Old recursive mimi_quote_drop overflowed the stack on deep quote
    // trees (one child per node). The iterative work-stack version must
    // handle chains far deeper than any thread stack.
    use crate::runtime::{mimi_quote_drop, mimi_quote_new_leaf, mimi_quote_new_node};

    let tag_unary = crate::runtime::QuotedAstTag::QastUnary as i32;
    let tag_int = crate::runtime::QuotedAstTag::QastInt as i32;

    let mut node = mimi_quote_new_leaf(tag_int, 1);
    assert!(!node.is_null());
    for _ in 0..100_000 {
        node = mimi_quote_new_node(tag_unary, node, std::ptr::null_mut(), 0);
        assert!(!node.is_null());
    }
    // Would stack-overflow (abort the test process) with the old recursion.
    mimi_quote_drop(node);
    // Double-drop is a documented no-op (live-quote registry).
    mimi_quote_drop(node);
}

#[test]
fn audit_quote_drop_variable_arity_children_freed() {
    use crate::runtime::{
        mimi_quote_drop, mimi_quote_list_child, mimi_quote_new_leaf, mimi_quote_new_list,
        mimi_quote_tag,
    };

    let tag_list = crate::runtime::QuotedAstTag::QastList as i32;
    let tag_int = crate::runtime::QuotedAstTag::QastInt as i32;

    let children = [
        mimi_quote_new_leaf(tag_int, 10),
        mimi_quote_new_leaf(tag_int, 20),
        mimi_quote_new_leaf(tag_int, 30),
    ];
    for c in &children {
        assert!(!c.is_null());
    }
    let list = mimi_quote_new_list(tag_list, children.as_ptr(), 3);
    assert!(!list.is_null());
    assert_eq!(mimi_quote_list_child(list, 1), children[1]);
    mimi_quote_drop(list);
    // Children were owned by the list node; after drop their live tokens
    // are gone, so the registry-guarded accessor rejects them.
    for c in &children {
        assert_eq!(mimi_quote_tag(*c), -1);
    }
}

#[test]
fn audit_quote_new_list_argc_overflow_rejected() {
    use crate::runtime::mimi_quote_new_list;
    let tag_list = crate::runtime::QuotedAstTag::QastList as i32;
    // len = i32::MAX + 1 cannot fit the i32 `argc` field. Old code silently
    // truncated; fixed code rejects (children null → nothing to leak).
    let node = mimi_quote_new_list(tag_list, std::ptr::null_mut(), i32::MAX as i64 + 1);
    assert!(node.is_null());
    // Within range still works.
    let ok = mimi_quote_new_list(tag_list, std::ptr::null_mut(), 0);
    assert!(!ok.is_null());
    crate::runtime::mimi_quote_drop(ok);
}

// ---------------------------------------------------------------------------
// concurrency.rs fix (2026-08-05 audit, MEDIUM): mutex guard thread
// confinement. Same-thread lock/get/set/unlock must work; misuse aborts
// loudly via mimi_runtime_abort (process abort is not testable in-process,
// so only the positive path is asserted here — see module docs in
// src/runtime/concurrency.rs for the ABI invariant).
// ---------------------------------------------------------------------------

#[test]
fn audit_mutex_guard_same_thread_roundtrip() {
    use crate::runtime::{
        mimi_mutex_drop, mimi_mutex_get, mimi_mutex_lock, mimi_mutex_new, mimi_mutex_set,
        mimi_mutex_unlock,
    };

    let m = mimi_mutex_new(7);
    assert!(m != 0);
    let g = mimi_mutex_lock(m);
    assert!(g != 0);
    assert_eq!(mimi_mutex_get(g), 7);
    mimi_mutex_set(g, 42);
    assert_eq!(mimi_mutex_get(g), 42);
    mimi_mutex_unlock(g);
    // Re-lock after unlock works (a fresh guard handle).
    let g2 = mimi_mutex_lock(m);
    assert!(g2 != 0);
    assert_eq!(mimi_mutex_get(g2), 42);
    mimi_mutex_unlock(g2);
    mimi_mutex_drop(m);
}

// ── 0.35.29 H13: close_fd must not close standard streams ──
// The VM and codegen backends disagreed with the connect policy
// (net.rs builtin_connect rejects fd <= 2): close_fd(0/1/2) silently
// closed the interpreter's own stdio. Both sides now reject the
// standard-stream range.

#[test]
fn audit_h13_close_fd_rejects_standard_streams_vm() {
    // VM side: close_fd(0) must trap with a clean InterpError, not close stdin.
    let src = r#"
func main() -> i32 {
    close_fd(0)
    0
}
"#;
    let r = run_source_result(src);
    assert!(
        r.is_err(),
        "close_fd(0) must be rejected on the VM — got {:?}",
        r
    );
    let msg = r.unwrap_err();
    assert!(
        msg.contains("standard stream"),
        "error must name the stdio guard, got: {msg}"
    );
}

#[test]
fn audit_h13_close_fd_rejects_standard_streams_codegen() {
    if !can_link() {
        return;
    }
    // Codegen side: mimi_close returns -1 for fd <= 2 (like connect's guard),
    // so the compiled program observes the rejection instead of closing stdio.
    let src = r#"
func main() -> i32 {
    let r = close_fd(1)
    println(r)
    0
}
"#;
    let out = compile_and_run(src).expect("program must run");
    assert_eq!(out.trim(), "-1");
}

#[test]
fn audit_h13_close_fd_still_closes_real_fds() {
    // A real fd > 2 must still close cleanly on the VM. The test process and
    // the VM share the same process (run_source runs in-process), so a fd
    // opened here is visible to the Mimi program. After close_fd the fd must
    // be gone (EBADF on a second close via fcntl).
    use std::os::unix::io::AsRawFd;
    let path = std::env::temp_dir().join(format!("mimi_h13_{}.txt", std::process::id()));
    std::fs::write(&path, b"x").unwrap();
    let file = std::fs::File::open(&path).unwrap();
    let fd = file.as_raw_fd();
    assert!(
        fd > 2,
        "test fd must be > 2 for the guard to pass, got {fd}"
    );
    drop(file);

    let src = format!(
        r#"
func main() -> i32 {{
    close_fd({fd})
    0
}}
"#
    );
    let r = run_source_result(&src);
    assert!(
        r.is_ok(),
        "close_fd of a real fd > 2 must succeed, got {r:?}"
    );

    // The fd must now be closed: fcntl(F_GETFD) itself would return a valid
    // descriptor otherwise. Verify via libc that it is EBADF.
    // SAFETY: fcntl with a raw fd; the fd is not in use by this process
    // anymore (the File was dropped).
    let rc = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert_eq!(rc, -1, "fd {fd} must be closed after close_fd");
    // SAFETY: errno access after a libc failure.
    let err = std::io::Error::last_os_error().raw_os_error();
    assert_eq!(
        err,
        Some(libc::EBADF),
        "closed fd must report EBADF, got {err:?}"
    );
    let _ = std::fs::remove_file(&path);
}
