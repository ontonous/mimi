// ===========================================================================
// v0.28.21 — QuotedAst runtime representation (extracted from runtime/mod.rs)
//
// Runtime representation of `quote! { ... }` AST values for the codegen path:
// a tagged-union `MimiQuotedAst` (repr(C)) plus `mimi_quote_*` constructors /
// accessors and the live-quote registry that makes dropped-handle access safe.
// This module owns `LIVE_QUOTES` and all `mimi_quote_*` extern "C" entry points.
// ===========================================================================

use std::collections::HashSet;
use std::sync::Mutex;

// --- live-quote registry ---

static LIVE_QUOTES: std::sync::OnceLock<Mutex<HashSet<usize>>> = std::sync::OnceLock::new();

fn live_quotes() -> &'static Mutex<HashSet<usize>> {
    LIVE_QUOTES.get_or_init(|| Mutex::new(HashSet::new()))
}

fn quote_register_live(node: *mut MimiQuotedAst) {
    if !node.is_null() {
        live_quotes()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(node as usize);
    }
}

fn quote_read<T>(
    node: *mut MimiQuotedAst,
    invalid: T,
    read: impl FnOnce(&MimiQuotedAst) -> T,
) -> T {
    if node.is_null() {
        return invalid;
    }
    let live = live_quotes().lock().unwrap_or_else(|e| e.into_inner());
    if !live.contains(&(node as usize)) {
        return invalid;
    }
    // The registry lock prevents a concurrent drop while the node is read.
    read(unsafe { &*node })
}

fn quote_take_live(node: *mut MimiQuotedAst) -> bool {
    !node.is_null()
        && live_quotes()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(node as usize))
}

// QuotedAst values produced by `quote! { ... }` are stored in the
// interpreter as `Value::QuoteAst(Box<QuotedAst>)`. The codegen path
// needs an equivalent runtime representation so that `ast_eval(q)` and
// `$(expr)` interpolations can flow through the compiled binary without
// going back to the interpreter. The layout is a tagged union:
//
//   struct MimiQuotedAst {
//       int32_t tag;       // see QAST_* below
//       int32_t argc;      // number of children
//       int64_t data0;     // literal value, or first child ptr
//       int64_t data1;     // binop, or second child ptr
//       int64_t data2;     // third child / extra / children_count
//   };
//
// Variable-arity nodes (Call, Tuple, List, Block, Record) use
// `data0 = children_array_ptr, data2 = children_count`. Children
// themselves are `*mut MimiQuotedAst`, allocated individually via
// `mimi_quote_new_*` helpers and freed recursively by `mimi_quote_drop`.

/// QuotedAst node tag. Values must stay in sync with the interp-side
/// `QuotedAst` variant order (we re-derive the mapping at the call
/// sites so a reordering here would be caught at compile time of the
/// codegen helper).
#[repr(i32)]
pub enum QuotedAstTag {
    QastInt = 0,
    QastFloat,
    QastBool,
    QastString,
    QastUnit,
    QastIdent,
    QastBinary,
    QastUnary,
    QastCall,
    QastField,
    QastIndex,
    QastTuple,
    QastList,
    QastIf,
    QastBlock,
    QastInterp,
    QastLet,
    QastReturn,
    QastBreak,
    QastContinue,
    QastWhile,
    QastAssign,
    QastFor,
    QastLoop,
    QastArena,
    QastUnsafe,
    QastDrop,
    QastOnFailure,
    QastParasteps,
    QastAlloc,
    QastSharedLet,
    QastMatch,
    QastTry,
    QastSpawn,
    QastAwait,
    QastRecord,
    QastNamedArg,
}

pub const QUOTED_AST_ABI_VERSION: i32 = 1;

#[no_mangle]
pub extern "C" fn mimi_quote_abi_version() -> i32 {
    QUOTED_AST_ABI_VERSION
}

fn quote_tag_is_valid(tag: i32) -> bool {
    (QuotedAstTag::QastInt as i32..=QuotedAstTag::QastNamedArg as i32).contains(&tag)
}

/// Runtime QuotedAst node. Layout: `repr(C)` so the codegen
/// `i8*` pointer handed back to user code maps to this struct.
#[repr(C)]
pub struct MimiQuotedAst {
    pub tag: i32,
    pub argc: i32,
    pub data0: i64,
    pub data1: i64,
    pub data2: i64,
}

/// Allocate a leaf (literal / ident / unit) node. `data0` carries the
/// literal value (cast to i64) or the ident-tag identifier (0 for unit
/// or generic; ident data is recovered through `data1` for binary nodes
/// only — the v0.28.21 batch treats `Ident(name)` as a literal slot).
#[no_mangle]
pub extern "C" fn mimi_quote_new_leaf(tag: i32, value: i64) -> *mut MimiQuotedAst {
    if !quote_tag_is_valid(tag) {
        return std::ptr::null_mut();
    }
    let node = Box::new(MimiQuotedAst {
        tag,
        argc: 0,
        data0: value,
        data1: 0,
        data2: 0,
    });
    let node = Box::into_raw(node);
    quote_register_live(node);
    node
}

/// Allocate a binary / unary / index / field-style node with up to two
/// children. The children pointers are themselves returned by
/// `mimi_quote_new_*` and ownership transfers to the new parent.
///
/// # Safety
/// Node/children pointers must be null or come from `mimi_quote_new_*`
/// and must not be used after `mimi_quote_drop` unless the registry
/// contract says otherwise (dropped handles are rejected).
#[no_mangle]
pub unsafe extern "C" fn mimi_quote_new_node(
    tag: i32,
    child0: *mut MimiQuotedAst,
    child1: *mut MimiQuotedAst,
    extra: i64,
) -> *mut MimiQuotedAst {
    if !quote_tag_is_valid(tag) {
        return std::ptr::null_mut();
    }
    let node = Box::new(MimiQuotedAst {
        tag,
        argc: if child1.is_null() { 1 } else { 2 },
        data0: child0 as i64,
        data1: if child1.is_null() { 0 } else { child1 as i64 },
        data2: extra,
    });
    let node = Box::into_raw(node);
    quote_register_live(node);
    node
}

/// Allocate a node backed by a heap-allocated children array (Call,
/// Tuple, List, Block, Record, etc.). The children are stored in a
/// `Vec<*mut MimiQuotedAst>` allocated separately so we can store a
/// thin pointer in `data0` (length tracked in `data2`).
///
/// # Safety
/// Node/children pointers must be null or come from `mimi_quote_new_*`
/// and must not be used after `mimi_quote_drop` unless the registry
/// contract says otherwise (dropped handles are rejected).
#[no_mangle]
pub unsafe extern "C" fn mimi_quote_new_list(
    tag: i32,
    children: *const *mut MimiQuotedAst,
    len: i64,
) -> *mut MimiQuotedAst {
    if !quote_tag_is_valid(tag) {
        return std::ptr::null_mut();
    }
    let len = len.max(0) as usize;
    // Audit fix (quote.rs argc truncation): `argc` is an i32 field; the old
    // code silently truncated `len as i32`, which desynchronized argc from
    // data2/children_count and leaked children past the truncated count.
    // Reject oversized input instead. Ownership of the children transferred
    // to us with the call, so on the failure path they must be dropped.
    let argc = match i32::try_from(len) {
        Ok(v) => v,
        Err(_) => {
            if !children.is_null() && len > 0 {
                // SAFETY: caller guarantees `children` points to `len` valid
                // `*mut MimiQuotedAst` pointers, each owned by the new node;
                // rejecting the node means we must consume that ownership.
                unsafe {
                    for &child in std::slice::from_raw_parts(children, len) {
                        mimi_quote_drop(child);
                    }
                }
            }
            return std::ptr::null_mut();
        }
    };
    if children.is_null() && len > 0 {
        // A non-zero child count with no children array cannot be represented
        // safely: mimi_quote_list_child would index an empty Vec while argc
        // claims it has children. Reject instead of creating a corrupt node.
        return std::ptr::null_mut();
    }
    // SAFETY: caller guarantees `children` points to `len` valid
    // `*mut MimiQuotedAst` pointers, each owned by the new node.
    let vec: Vec<*mut MimiQuotedAst> = if children.is_null() || len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(children, len).to_vec() }
    };
    let boxed: Box<Vec<*mut MimiQuotedAst>> = Box::new(vec);
    let ptr = Box::into_raw(boxed) as i64;
    let node = Box::new(MimiQuotedAst {
        tag,
        argc,
        data0: ptr,
        data1: 0,
        data2: len as i64,
    });
    let node = Box::into_raw(node);
    quote_register_live(node);
    node
}

/// Free a QuotedAst subtree, including any children-array blobs. Safe to
/// call on null (no-op); idempotent for already-dropped nodes (live-quote
/// registry).
///
/// Audit fix (quote.rs unbounded recursion): implemented iteratively with an
/// explicit heap work stack instead of recursion — a deeply nested quote
/// tree (one child per node) used to overflow the stack here.
///
/// # Safety
/// Node/children pointers must be null or come from `mimi_quote_new_*`
/// and must not be used after `mimi_quote_drop` unless the registry
/// contract says otherwise (dropped handles are rejected).
#[no_mangle]
pub unsafe extern "C" fn mimi_quote_drop(node: *mut MimiQuotedAst) {
    // Explicit work stack: nodes whose children still need processing.
    let mut work: Vec<*mut MimiQuotedAst> = Vec::new();
    if quote_take_live(node) {
        work.push(node);
    }
    while let Some(n) = work.pop() {
        // SAFETY: `n` was created by `mimi_quote_new_*` and its live token
        // was taken (at push time), so this call holds exclusive ownership
        // and the node has not been dropped yet.
        unsafe {
            let nn = Box::from_raw(n);
            if nn.argc <= 0 {
                continue;
            }
            if nn.argc == 1 {
                let child = nn.data0 as *mut MimiQuotedAst;
                if quote_take_live(child) {
                    work.push(child);
                }
            } else if nn.argc == 2 {
                let c0 = nn.data0 as *mut MimiQuotedAst;
                let c1 = nn.data1 as *mut MimiQuotedAst;
                if quote_take_live(c0) {
                    work.push(c0);
                }
                if quote_take_live(c1) {
                    work.push(c1);
                }
            } else {
                // Variable-arity: data0 is a pointer to a `Vec<*mut MimiQuotedAst>`.
                // M9/C15: always attempt Box::from_raw for argc>2. This value was
                // created by mimi_quote_new_list which always uses Box + into_raw,
                // so the pointer is always valid. We only skip if null.
                let arr_ptr = nn.data0 as *mut Vec<*mut MimiQuotedAst>;
                if !arr_ptr.is_null() {
                    // SAFETY: `arr_ptr` was created by `mimi_quote_new_list`.
                    let vec = Box::from_raw(arr_ptr);
                    for &child in vec.iter() {
                        if quote_take_live(child) {
                            work.push(child);
                        }
                    }
                }
            }
        }
    }
}

/// Read the tag back. Useful for runtime dispatch (e.g. in `ast_eval`
/// when written to interpret the runtime node).
///
/// # Safety
/// Node/children pointers must be null or come from `mimi_quote_new_*`
/// and must not be used after `mimi_quote_drop` unless the registry
/// contract says otherwise (dropped handles are rejected).
#[no_mangle]
pub unsafe extern "C" fn mimi_quote_tag(node: *mut MimiQuotedAst) -> i32 {
    quote_read(node, -1, |node| node.tag)
}

/// Read `data0` (literal value or first child pointer). Callers that
/// want a child pointer can cast the result to `*mut MimiQuotedAst`.
///
/// # Safety
/// Node/children pointers must be null or come from `mimi_quote_new_*`
/// and must not be used after `mimi_quote_drop` unless the registry
/// contract says otherwise (dropped handles are rejected).
#[no_mangle]
pub unsafe extern "C" fn mimi_quote_data0(node: *mut MimiQuotedAst) -> i64 {
    quote_read(node, 0, |node| node.data0)
}

/// Read `data1`.
///
/// # Safety
/// Node/children pointers must be null or come from `mimi_quote_new_*`
/// and must not be used after `mimi_quote_drop` unless the registry
/// contract says otherwise (dropped handles are rejected).
#[no_mangle]
pub unsafe extern "C" fn mimi_quote_data1(node: *mut MimiQuotedAst) -> i64 {
    quote_read(node, 0, |node| node.data1)
}

/// Read `data2`.
///
/// # Safety
/// Node/children pointers must be null or come from `mimi_quote_new_*`
/// and must not be used after `mimi_quote_drop` unless the registry
/// contract says otherwise (dropped handles are rejected).
#[no_mangle]
pub unsafe extern "C" fn mimi_quote_data2(node: *mut MimiQuotedAst) -> i64 {
    quote_read(node, 0, |node| node.data2)
}

/// Read `argc` (number of children).
///
/// # Safety
/// Node/children pointers must be null or come from `mimi_quote_new_*`
/// and must not be used after `mimi_quote_drop` unless the registry
/// contract says otherwise (dropped handles are rejected).
#[no_mangle]
pub unsafe extern "C" fn mimi_quote_argc(node: *mut MimiQuotedAst) -> i32 {
    quote_read(node, 0, |node| node.argc)
}

/// Read child at index `i` from a list-style node. Returns null on
/// out-of-range or if the node isn't list-style.
///
/// # Safety
/// Node/children pointers must be null or come from `mimi_quote_new_*`
/// and must not be used after `mimi_quote_drop` unless the registry
/// contract says otherwise (dropped handles are rejected).
#[no_mangle]
pub unsafe extern "C" fn mimi_quote_list_child(
    node: *mut MimiQuotedAst,
    i: i64,
) -> *mut MimiQuotedAst {
    quote_read(node, std::ptr::null_mut(), |node| unsafe {
        if node.argc <= 2 {
            return std::ptr::null_mut();
        }
        let arr_ptr = node.data0 as *const Vec<*mut MimiQuotedAst>;
        if arr_ptr.is_null() {
            return std::ptr::null_mut();
        }
        let idx = i as usize;
        let len = node.argc as usize;
        if idx >= len {
            return std::ptr::null_mut();
        }
        // SAFETY: `arr_ptr` is a valid `Vec` created by `mimi_quote_new_list`.
        let vec = &*arr_ptr;
        if idx >= vec.len() {
            return std::ptr::null_mut();
        }
        (*vec)[idx]
    })
}
