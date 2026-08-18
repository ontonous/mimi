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
        node = unsafe { mimi_quote_new_node(tag_unary, node, std::ptr::null_mut(), 0) };
        assert!(!node.is_null());
    }
    // Would stack-overflow (abort the test process) with the old recursion.
    unsafe {
        mimi_quote_drop(node);
        // Double-drop is a documented no-op (live-quote registry).
        mimi_quote_drop(node);
    }
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
    let list = unsafe { mimi_quote_new_list(tag_list, children.as_ptr(), 3) };
    assert!(!list.is_null());
    unsafe {
        assert_eq!(mimi_quote_list_child(list, 1), children[1]);
        mimi_quote_drop(list);
    }
    // Children were owned by the list node; after drop their live tokens
    // are gone, so the registry-guarded accessor rejects them.
    for c in &children {
        unsafe {
            assert_eq!(mimi_quote_tag(*c), -1);
        }
    }
}

#[test]
fn audit_quote_new_list_argc_overflow_rejected() {
    use crate::runtime::mimi_quote_new_list;
    let tag_list = crate::runtime::QuotedAstTag::QastList as i32;
    // len = i32::MAX + 1 cannot fit the i32 `argc` field. Old code silently
    // truncated; fixed code rejects (children null → nothing to leak).
    let node = unsafe { mimi_quote_new_list(tag_list, std::ptr::null_mut(), i32::MAX as i64 + 1) };
    assert!(node.is_null());
    // Within range still works.
    let ok = unsafe { mimi_quote_new_list(tag_list, std::ptr::null_mut(), 0) };
    assert!(!ok.is_null());
    unsafe {
        crate::runtime::mimi_quote_drop(ok);
    }
}

// ---------------------------------------------------------------------------
// concurrency.rs fix (2026-08-05 audit, MEDIUM): mutex guard thread
// confinement. Same-thread lock/get/set/unlock must work; misuse aborts
// loudly via mimi_runtime_abort (process abort is not testable in-process,
// so only the positive path is asserted here — see module docs in
// src/runtime/concurrency.rs for the ABI invariant).
// ---------------------------------------------------------------------------

#[test]
fn audit_mutex_invalid_guard_fails_loud_without_abort() {
    use crate::runtime::{
        mimi_mutex_get, mimi_mutex_guard_valid, mimi_mutex_set, mimi_mutex_unlock,
    };

    // Invalid/after-unlock handles must not abort the host process; they log
    // and return a safe sentinel so recovered FFI code can continue.
    assert_eq!(unsafe { mimi_mutex_guard_valid(1234567) }, 0);
    assert_eq!(unsafe { mimi_mutex_get(1234567) }, 0);
    unsafe { mimi_mutex_set(1234567, 9) };
    unsafe { mimi_mutex_unlock(1234567) };
    unsafe { mimi_mutex_unlock(1234567) };
}

#[test]
fn audit_mutex_guard_same_thread_roundtrip() {
    use crate::runtime::{
        mimi_mutex_drop, mimi_mutex_get, mimi_mutex_lock, mimi_mutex_new, mimi_mutex_set,
        mimi_mutex_unlock,
    };

    let m = mimi_mutex_new(7);
    assert!(m != 0);
    let g = unsafe { mimi_mutex_lock(m) };
    assert!(g != 0);
    assert_eq!(unsafe { mimi_mutex_get(g) }, 7);
    unsafe { mimi_mutex_set(g, 42) };
    assert_eq!(unsafe { mimi_mutex_get(g) }, 42);
    unsafe { mimi_mutex_unlock(g) };
    // Re-lock after unlock works (a fresh guard handle).
    let g2 = unsafe { mimi_mutex_lock(m) };
    assert!(g2 != 0);
    assert_eq!(unsafe { mimi_mutex_get(g2) }, 42);
    unsafe { mimi_mutex_unlock(g2) };
    unsafe { mimi_mutex_drop(m) };
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
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;
    let path = std::env::temp_dir().join(format!("mimi_h13_{}.txt", std::process::id()));
    std::fs::write(&path, b"x").unwrap();
    let file = std::fs::File::open(&path).unwrap();
    let fd = file.as_raw_fd();
    assert!(
        fd > 2,
        "test fd must be > 2 for the guard to pass, got {fd}"
    );
    // 0.36.13: leak the fd on purpose instead of dropping it — the fd must
    // stay OCCUPIED until the in-process Mimi close_fd runs. The old
    // `drop(file)` freed the fd number early; a parallel test thread could
    // then open a file that reused the same fd, and this test's in-process
    // close_fd closed THAT test's owned fd out from under it — intermittent
    // "IO Safety violation: owned file descriptor already closed" abort of
    // the whole suite under --test-threads=4. With the fd held, no reuse is
    // possible; the single leaked fd is reclaimed on process exit.
    let before_meta = file.metadata().expect("metadata before close");
    let before_id = (before_meta.dev(), before_meta.ino());
    std::mem::forget(file);

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
    // descriptor otherwise. The full suite runs tests in parallel, so another
    // thread may legitimately reuse the just-freed descriptor number before
    // this check. Distinguish that case by comparing the file identity: if
    // the descriptor now names a different file, our close_fd did its job and
    // we must not fail (or touch) the other thread's descriptor.
    // SAFETY: fstat on a raw fd; we only read the result and never close the
    // descriptor from this thread.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd, &mut st) };
    if rc == -1 {
        // SAFETY: errno access after a libc failure.
        let err = std::io::Error::last_os_error().raw_os_error();
        assert_eq!(
            err,
            Some(libc::EBADF),
            "closed fd must report EBADF, got {err:?}"
        );
    } else {
        let now_id = (st.st_dev as u64, st.st_ino as u64);
        assert_ne!(
            now_id, before_id,
            "fd {fd} must be closed after close_fd; it still names the original file"
        );
    }
    let _ = std::fs::remove_file(&path);
}

// ── 0.35.37 M8: buf_nul_terminate bounds defense ──
// The runtime helper now carries alloc_size and aborts (not heap-corrupts)
// when offset >= alloc_size. Process abort is not testable in-process, so:
//  1. positive path: a legal offset writes the NUL exactly (asserted via the
//     buffer contents), and
//  2. the out-of-bounds case is verified in a child process that must abort
//     with the exact diagnostic (fail-loud, not silent corruption).

#[test]
fn audit_m8_buf_nul_terminate_positive_writes_exact_nul() {
    use crate::runtime::mimi_runtime_buf_nul_terminate;
    // SAFETY: 8-byte heap buffer; offset 7 (with NUL) is the last byte.
    let buf = unsafe { libc::malloc(8) } as *mut u8;
    assert!(!buf.is_null());
    // SAFETY: freshly allocated buffer, write within bounds.
    unsafe { std::ptr::write_bytes(buf, b'x', 8) };
    unsafe {
        mimi_runtime_buf_nul_terminate(buf, 7, 8);
    }
    // SAFETY: buffer was allocated above; reads are within bounds.
    unsafe {
        assert_eq!(*buf.offset(7), 0, "NUL must be written at offset 7");
        assert_eq!(
            *buf.offset(6),
            b'x',
            "bytes before offset must be untouched"
        );
    }
    // SAFETY: buffer from libc::malloc is freed by libc::free.
    unsafe { libc::free(buf as *mut libc::c_void) };
}

#[test]
fn audit_m8_buf_nul_terminate_oob_aborts_in_child() {
    use std::process::Command;
    // Re-run this same test binary with a filter that forces the abort path.
    //
    // 0.35.37 hardening: Stdio::piped() allocated fresh fds that raced with
    // audit_h13's close_fd tests under --test-threads (EBADF on spawn).
    // Redirect the child's stderr to a FILE opened before spawn instead:
    // the fd is stable for the child's lifetime, no piped allocation race,
    // and the abort message is still captured for the fail-loud assertion.
    let exe = std::env::current_exe().expect("current exe");
    let err_path = std::env::temp_dir().join(format!(
        "mimi_m8_err_{}_{}.log",
        std::process::id(),
        std::thread::current().name().unwrap_or("t")
    ));
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");
    // SAFETY: err_file is a valid OS handle for Stdio::from.
    let status = Command::new(exe)
        .args([
            "--exact",
            "tests::audit_fix_runtime_sub::audit_m8_oob_abort_helper",
            "--nocapture",
        ])
        .env("MIMI_M8_OOB_HELPER", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(err_file))
        .status()
        .expect("spawn child");
    assert!(
        !status.success(),
        "out-of-bounds NUL terminate must abort (got success)"
    );
    let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
    assert!(
        stderr.contains("heap-corrupting write prevented"),
        "abort message must name the hazard, got: {stderr}"
    );
    let _ = std::fs::remove_file(&err_path);
}

/// Helper executed in a child process: an offset past the allocation must
/// abort (never silently write out of bounds).
#[test]
fn audit_m8_oob_abort_helper() {
    if std::env::var("MIMI_M8_OOB_HELPER").is_err() {
        return; // not the child invocation — no-op so the test binary stays green
    }
    use crate::runtime::mimi_runtime_buf_nul_terminate;
    // SAFETY: 4-byte buffer; offset 7 (with NUL) exceeds alloc_size 4.
    let buf = unsafe { libc::malloc(4) } as *mut u8;
    assert!(!buf.is_null());
    unsafe {
        mimi_runtime_buf_nul_terminate(buf, 7, 4);
    }
    // SAFETY: unreachable when the guard works; frees in case it doesn't.
    unsafe { libc::free(buf as *mut libc::c_void) };
}
