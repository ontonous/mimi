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
#[no_mangle]
pub extern "C" fn mimi_quote_new_node(
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
#[no_mangle]
pub extern "C" fn mimi_quote_new_list(
    tag: i32,
    children: *const *mut MimiQuotedAst,
    len: i64,
) -> *mut MimiQuotedAst {
    if !quote_tag_is_valid(tag) {
        return std::ptr::null_mut();
    }
    let len = len.max(0) as usize;
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
        argc: len as i32,
        data0: ptr,
        data1: 0,
        data2: len as i64,
    });
    let node = Box::into_raw(node);
    quote_register_live(node);
    node
}

/// Recursively free a QuotedAst subtree, including any children-array
/// blobs. Safe to call on null (no-op).
#[no_mangle]
pub extern "C" fn mimi_quote_drop(node: *mut MimiQuotedAst) {
    if !quote_take_live(node) {
        return;
    }
    // SAFETY: `node` was created by `mimi_quote_new_*` and not yet
    // dropped.
    unsafe {
        let n = Box::from_raw(node);
        if n.argc <= 0 {
            return;
        }
        if n.argc == 1 {
            let child = n.data0 as *mut MimiQuotedAst;
            mimi_quote_drop(child);
        } else if n.argc == 2 {
            mimi_quote_drop(n.data0 as *mut MimiQuotedAst);
            mimi_quote_drop(n.data1 as *mut MimiQuotedAst);
        } else {
            // Variable-arity: data0 is a pointer to a `Vec<*mut MimiQuotedAst>`.
            // M9/C15: always attempt Box::from_raw for argc>2. This value was
            // created by mimi_quote_new_list which always uses Box + into_raw,
            // so the pointer is always valid. We only skip if null.
            let arr_ptr = n.data0 as *mut Vec<*mut MimiQuotedAst>;
            if !arr_ptr.is_null() {
                // SAFETY: `arr_ptr` was created by `mimi_quote_new_list`.
                let vec = Box::from_raw(arr_ptr);
                for &child in vec.iter() {
                    mimi_quote_drop(child);
                }
            }
        }
    }
}

/// Read the tag back. Useful for runtime dispatch (e.g. in `ast_eval`
/// when written to interpret the runtime node).
#[no_mangle]
pub extern "C" fn mimi_quote_tag(node: *mut MimiQuotedAst) -> i32 {
    quote_read(node, -1, |node| node.tag)
}

/// Read `data0` (literal value or first child pointer). Callers that
/// want a child pointer can cast the result to `*mut MimiQuotedAst`.
#[no_mangle]
pub extern "C" fn mimi_quote_data0(node: *mut MimiQuotedAst) -> i64 {
    quote_read(node, 0, |node| node.data0)
}

/// Read `data1`.
#[no_mangle]
pub extern "C" fn mimi_quote_data1(node: *mut MimiQuotedAst) -> i64 {
    quote_read(node, 0, |node| node.data1)
}

/// Read `data2`.
#[no_mangle]
pub extern "C" fn mimi_quote_data2(node: *mut MimiQuotedAst) -> i64 {
    quote_read(node, 0, |node| node.data2)
}

/// Read `argc` (number of children).
#[no_mangle]
pub extern "C" fn mimi_quote_argc(node: *mut MimiQuotedAst) -> i32 {
    quote_read(node, 0, |node| node.argc)
}

/// Read child at index `i` from a list-style node. Returns null on
/// out-of-range or if the node isn't list-style.
#[no_mangle]
pub extern "C" fn mimi_quote_list_child(node: *mut MimiQuotedAst, i: i64) -> *mut MimiQuotedAst {
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
        (*vec)[idx]
    })
}
