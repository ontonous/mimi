//! Wave-1 audit-fix regression tests — runtime_sub.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).

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
