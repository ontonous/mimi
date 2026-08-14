//! ABI generator: produces ComponentIr from runtime exports.
//!
//! 0.31.30 v1: builder-based registry. The generator provides a typed
//! API for registering runtime function signatures, replacing the
//! 352 string-based `get_runtime_fn("name")` lookups in codegen.
//!
//! Future: automated extraction from `register_runtime()` LLVM declarations.

use super::symbol::{AbiCallConv, AbiCallbackCategory, AbiParam, AbiSymbol, AbiSymbolKind};
use super::types::{AbiPrimitive, AbiTypeRef};
use super::{ComponentIdentity, ComponentIr};

/// ABI generator: builds a ComponentIr from registered runtime exports.
///
/// Usage:
/// ```ignore
/// let mut gen = AbiGenerator::new();
/// gen.export("mimi_list_push_i64", |f| {
///     f.param("list", AbiTypeRef::Primitive(AbiPrimitive::IntPtr))
///      .param("value", AbiTypeRef::Primitive(AbiPrimitive::I64))
///      .returns(AbiTypeRef::Void)
/// });
/// let ir = gen.build();
/// ```
#[derive(Debug)]
pub struct AbiGenerator {
    identity: ComponentIdentity,
    exports: Vec<AbiSymbol>,
    imports: Vec<AbiSymbol>,
    types: Vec<super::types::AbiTypeDef>,
}

impl AbiGenerator {
    /// Create a new generator with default identity.
    pub fn new() -> Self {
        Self {
            identity: ComponentIdentity::default(),
            exports: Vec::new(),
            imports: Vec::new(),
            types: Vec::new(),
        }
    }

    /// Set the component identity.
    pub fn identity(mut self, identity: ComponentIdentity) -> Self {
        self.identity = identity;
        self
    }

    /// Register an exported runtime function.
    ///
    /// Panics in debug builds if a duplicate export name is registered.
    /// In release builds, duplicates are silently skipped (the first
    /// registration wins) to avoid crashing the compiler.
    pub fn export(&mut self, name: &str, build: impl FnOnce(SymbolBuilder) -> SymbolBuilder) {
        if self.exports.iter().any(|s| s.name == name) {
            #[cfg(debug_assertions)]
            panic!("duplicate export registration: {}", name);
            #[cfg(not(debug_assertions))]
            return; // skip duplicate in release
        }
        let builder = SymbolBuilder::new(name, AbiSymbolKind::Function);
        let symbol = build(builder).build();
        self.exports.push(symbol);
    }

    /// Register an imported extern function.
    ///
    /// Panics in debug builds if a duplicate import name is registered.
    /// In release builds, duplicates are silently skipped.
    pub fn import(&mut self, name: &str, build: impl FnOnce(SymbolBuilder) -> SymbolBuilder) {
        if self.imports.iter().any(|s| s.name == name) {
            #[cfg(debug_assertions)]
            panic!("duplicate import registration: {}", name);
            #[cfg(not(debug_assertions))]
            return;
        }
        let builder = SymbolBuilder::new(name, AbiSymbolKind::ExternFunction);
        let symbol = build(builder).build();
        self.imports.push(symbol);
    }

    /// Register a type definition.
    ///
    /// Panics in debug builds if a duplicate type name is registered.
    /// In release builds, duplicates are silently skipped.
    pub fn type_def(&mut self, def: super::types::AbiTypeDef) {
        if self.types.iter().any(|t| t.name() == def.name()) {
            #[cfg(debug_assertions)]
            panic!("duplicate type definition: {}", def.name());
            #[cfg(not(debug_assertions))]
            return;
        }
        self.types.push(def);
    }

    /// Build the ComponentIr.
    pub fn build(self) -> ComponentIr {
        ComponentIr {
            identity: self.identity,
            exports: self.exports,
            imports: self.imports,
            types: self.types,
        }
    }

    /// Number of registered exports.
    pub fn export_count(&self) -> usize {
        self.exports.len()
    }

    /// Number of registered imports.
    pub fn import_count(&self) -> usize {
        self.imports.len()
    }
}

impl Default for AbiGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for AbiSymbol.
#[derive(Debug)]
pub struct SymbolBuilder {
    name: String,
    kind: AbiSymbolKind,
    params: Vec<AbiParam>,
    ret: AbiTypeRef,
    effects: Vec<String>,
    is_unsafe: bool,
    call_conv: AbiCallConv,
    callback_category: Option<AbiCallbackCategory>,
}

impl SymbolBuilder {
    fn new(name: &str, kind: AbiSymbolKind) -> Self {
        Self {
            name: name.to_string(),
            kind,
            params: Vec::new(),
            ret: AbiTypeRef::Void,
            effects: Vec::new(),
            is_unsafe: false,
            call_conv: AbiCallConv::C,
            callback_category: None,
        }
    }

    /// Add a parameter.
    pub fn param(mut self, name: &str, ty: AbiTypeRef) -> Self {
        self.params.push(AbiParam {
            name: name.to_string(),
            ty,
            is_nullable: false,
        });
        self
    }

    /// Add a nullable parameter.
    pub fn nullable_param(mut self, name: &str, ty: AbiTypeRef) -> Self {
        self.params.push(AbiParam {
            name: name.to_string(),
            ty,
            is_nullable: true,
        });
        self
    }

    /// Set the return type.
    pub fn returns(mut self, ty: AbiTypeRef) -> Self {
        self.ret = ty;
        self
    }

    /// Add an effect annotation.
    pub fn effect(mut self, effect: &str) -> Self {
        self.effects.push(effect.to_string());
        self
    }

    /// Mark as unsafe.
    pub fn unsafe_fn(mut self) -> Self {
        self.is_unsafe = true;
        self
    }

    /// Set calling convention.
    pub fn call_conv(mut self, cc: AbiCallConv) -> Self {
        self.call_conv = cc;
        self
    }

    /// 0.31.33: Set callback category.
    pub fn callback(mut self, category: AbiCallbackCategory) -> Self {
        self.kind = AbiSymbolKind::Callback;
        self.callback_category = Some(category);
        self
    }

    fn build(self) -> AbiSymbol {
        AbiSymbol {
            name: self.name,
            kind: self.kind,
            params: self.params,
            ret: self.ret,
            effects: self.effects,
            is_unsafe: self.is_unsafe,
            call_conv: self.call_conv,
            callback_category: self.callback_category,
        }
    }
}

/// Convenience: create a primitive type reference.
pub fn prim(p: AbiPrimitive) -> AbiTypeRef {
    AbiTypeRef::Primitive(p)
}

/// Convenience: create a pointer type reference.
pub fn ptr(inner: AbiTypeRef) -> AbiTypeRef {
    AbiTypeRef::Pointer(Box::new(inner))
}

/// Convenience: void type reference.
#[allow(dead_code)] // Used by tests and future bindgen backends.
pub fn void() -> AbiTypeRef {
    AbiTypeRef::Void
}

/// Convenience: opaque handle type reference.
pub fn handle(name: &str) -> AbiTypeRef {
    AbiTypeRef::Opaque(name.to_string())
}

/// Convert a Mimi surface type name to an AbiTypeRef.
///
/// Handles:
/// - Primitives (via `AbiPrimitive::from_mimi_type`)
/// - Strings (`string`/`String` → `*mut u8` for C ABI)
/// - Pointers (`*mut T`, `*const T` → `Pointer`)
/// - References (`&T`, `&mut T` → `Pointer` at ABI level)
/// - Slices (`[T]`, `Vec<T>` → `Slice`)
/// - Void (`void`, `()`, empty)
/// - User-defined types → `Named`
///
/// Recursion depth is bounded to 64 levels to prevent stack overflow
/// on pathologically nested type expressions (e.g., `*mut *mut *mut ...`).
pub fn mimi_type_to_abi(name: &str) -> AbiTypeRef {
    mimi_type_to_abi_depth(name.trim(), 0)
}

fn mimi_type_to_abi_depth(name: &str, depth: usize) -> AbiTypeRef {
    if depth > 64 {
        // Bail out: treat as opaque named type at extreme depth
        return AbiTypeRef::Named(name.to_string());
    }
    if let Some(prim) = AbiPrimitive::from_mimi_type(name) {
        return AbiTypeRef::Primitive(prim);
    }
    // Pointer types: *mut T, *const T
    if let Some(inner) = name.strip_prefix("*mut ") {
        return AbiTypeRef::Pointer(Box::new(mimi_type_to_abi_depth(inner, depth + 1)));
    }
    if let Some(inner) = name.strip_prefix("*const ") {
        return AbiTypeRef::Pointer(Box::new(mimi_type_to_abi_depth(inner, depth + 1)));
    }
    // Reference types: &T, &mut T (ABI-equivalent to pointers)
    if let Some(inner) = name.strip_prefix("&mut ") {
        return AbiTypeRef::Pointer(Box::new(mimi_type_to_abi_depth(inner, depth + 1)));
    }
    if let Some(inner) = name.strip_prefix('&') {
        return AbiTypeRef::Pointer(Box::new(mimi_type_to_abi_depth(inner, depth + 1)));
    }
    // Slice types: [T], Vec<T>
    if let Some(inner) = name.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return AbiTypeRef::Slice(Box::new(mimi_type_to_abi_depth(inner, depth + 1)));
    }
    if let Some(inner) = name.strip_prefix("Vec<").and_then(|s| s.strip_suffix('>')) {
        return AbiTypeRef::Slice(Box::new(mimi_type_to_abi_depth(inner, depth + 1)));
    }
    match name {
        "string" | "String" => {
            AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::U8)))
        }
        "void" | "()" | "" => AbiTypeRef::Void,
        _ => AbiTypeRef::Named(name.to_string()),
    }
}

/// Register the core runtime ABI surface.
///
/// This is the v4 manual registry, corrected against the real runtime on
/// 2026-08-05 (full audit §12): every signature below was re-verified
/// against the `#[no_mangle] pub extern "C"` definitions in `src/runtime/`
/// (mod.rs, capability.rs, net.rs, crypto.rs, actor.rs, fs.rs,
/// concurrency.rs, env.rs, binary_io.rs, future.rs, regex.rs).
///
/// Audit fix: phantom symbols were removed entirely —
/// `mimi_list_new`/`mimi_list_len` (lists are codegen-allocated, no runtime
/// constructor), `mimi_print_line`/`mimi_print_err` (codegen emits libc
/// puts/printf directly), `mimi_sleep_ms` (real: `mimi_sleep`),
/// `mimi_timestamp`/`mimi_timestamp_ms` (real: `mimi_now`/`mimi_now_ms`),
/// and the 0.31.31 `MimiString`/`MimiSlice` fat-pointer surface
/// (`mimi_string_new`/`mimi_string_len`/`mimi_string_as_slice`) which never
/// existed in the runtime and collided with the real `mimi_string_len`
/// (ffi/runtime.rs, `(*const Value) -> i64`). The `FatPointer` type
/// vocabulary stays in `types.rs`; the core registry only registers it when
/// a component actually uses it.
///
/// Runtime-internal alias types used below: `ValueHandle = usize`
/// (runtime/mod.rs:252), `MapHandle = usize` (mod.rs:253),
/// `SetHandle = i64` / `SetValueHandle = i64` (mod.rs:16471-16472).
///
/// JSON combinatorial serialization variants (mimi_map_from_json_* etc.)
/// are intentionally excluded — they are internal codegen artifacts.
pub fn register_core_runtime_abi(gen: &mut AbiGenerator) {
    use AbiPrimitive::*;

    // Register opaque handle types (referenced by list/map/actor/future).
    //
    // Audit 2026-08-05: SetHandle removed — the real runtime `SetHandle`
    // is `type SetHandle = i64` (runtime/mod.rs:16471), not an opaque
    // pointer type; set functions below carry plain `i64`. Opaque renders
    // as `MimiHandle/* Name */` in C; the header preamble emits
    // `typedef uintptr_t MimiHandle;` (c_header.rs).
    use super::types::{AbiOpaque, AbiTypeDef};
    gen.type_def(AbiTypeDef::Opaque(AbiOpaque {
        name: "ListHandle".to_string(),
        description: "Opaque handle to a Mimi list (*mut MimiList)".to_string(),
    }));
    gen.type_def(AbiTypeDef::Opaque(AbiOpaque {
        name: "MapHandle".to_string(),
        description: "Opaque handle to a Mimi map (MapHandle = usize)".to_string(),
    }));
    gen.type_def(AbiTypeDef::Opaque(AbiOpaque {
        name: "ActorHandle".to_string(),
        description: "Opaque handle to a Mimi actor (*mut MimiActorRepr)".to_string(),
    }));
    gen.type_def(AbiTypeDef::Opaque(AbiOpaque {
        name: "FutureHandle".to_string(),
        description: "Opaque handle to a Mimi future/task (*mut FutureRepr)".to_string(),
    }));

    // ── RC / Allocation (runtime/mod.rs:1201-1360) ──
    gen.export("mimi_rc_alloc", |f| {
        f.param("size", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_rc_retain", |f| f.param("ptr", ptr(prim(U8))));
    gen.export("mimi_rc_release", |f| {
        f.param("ptr", ptr(prim(U8))).effect("dealloc")
    });
    gen.export("mimi_rc_upgrade", |f| {
        f.param("ptr", ptr(prim(U8))).returns(ptr(prim(U8)))
    });
    gen.export("mimi_rc_weak_retain", |f| f.param("ptr", ptr(prim(U8))));
    gen.export("mimi_rc_weak_release", |f| {
        f.param("ptr", ptr(prim(U8))).effect("dealloc")
    });

    // ── List (runtime/mod.rs:594-1020) ──
    // Audit 2026-08-05: mimi_list_new / mimi_list_len removed — no such
    // runtime symbols exist. Lists are codegen-allocated MimiList structs;
    // length lives in the struct field, not a runtime function.
    gen.export("mimi_list_push_i64", |f| {
        f.param("list", handle("ListHandle"))
            .param("element", prim(I64))
    });
    gen.export("mimi_list_push_f64", |f| {
        f.param("list", handle("ListHandle"))
            .param("element", prim(F64))
    });
    gen.export("mimi_list_push_string", |f| {
        f.param("list", handle("ListHandle"))
            .param("element", ptr(prim(U8)))
    });
    gen.export("mimi_list_get_i64", |f| {
        f.param("list", handle("ListHandle"))
            .param("index", prim(I64))
            .returns(prim(I64))
    });
    gen.export("mimi_list_get_f64", |f| {
        f.param("list", handle("ListHandle"))
            .param("index", prim(I64))
            .returns(prim(F64))
    });
    gen.export("mimi_list_get_string", |f| {
        f.param("list", handle("ListHandle"))
            .param("index", prim(I64))
            .returns(ptr(prim(U8)))
    });
    gen.export("mimi_list_free", |f| {
        f.param("list", handle("ListHandle"))
            .param("free_elements", prim(Bool))
            .effect("dealloc")
    });
    gen.export("mimi_list_push_grow", |f| {
        f.param("list", handle("ListHandle"))
            .param("additional", prim(I64))
            .returns(ptr(ptr(prim(U8))))
            .effect("alloc")
    });

    // ── Map (runtime/mod.rs:1419-1805; MapHandle = usize, ValueHandle = usize) ──
    gen.export("mimi_map_new", |f| {
        f.returns(handle("MapHandle")).effect("alloc")
    });
    gen.export("mimi_map_set", |f| {
        f.param("map", handle("MapHandle"))
            .param("key", ptr(prim(U8)))
            .param("value", prim(UIntPtr))
    });
    gen.export("mimi_map_get", |f| {
        f.param("map", handle("MapHandle"))
            .param("key", ptr(prim(U8)))
            .returns(prim(UIntPtr))
    });
    gen.export("mimi_map_remove", |f| {
        f.param("map", handle("MapHandle"))
            .param("key", ptr(prim(U8)))
            .returns(prim(I32))
    });
    gen.export("mimi_map_has_key", |f| {
        f.param("map", handle("MapHandle"))
            .param("key", ptr(prim(U8)))
            .returns(prim(I32))
    });
    gen.export("mimi_map_keys", |f| {
        f.param("map", handle("MapHandle"))
            .returns(handle("ListHandle"))
    });
    gen.export("mimi_map_values", |f| {
        f.param("map", handle("MapHandle"))
            .returns(handle("ListHandle"))
    });
    gen.export("mimi_map_size", |f| {
        f.param("map", handle("MapHandle")).returns(prim(I64))
    });
    gen.export("mimi_map_destroy", |f| {
        f.param("map", handle("MapHandle")).effect("dealloc")
    });
    gen.export("mimi_map_from_list", |f| {
        f.param("keys", ptr(prim(UIntPtr)))
            .param("values", ptr(prim(UIntPtr)))
            .param("n", prim(I64))
            .returns(handle("MapHandle"))
            .effect("alloc")
    });

    // ── Set (runtime/mod.rs:16495-18393; SetHandle = i64, SetValueHandle = i64) ──
    // Audit 2026-08-05: handles are plain i64 in the real runtime
    // (`type SetHandle = i64`, mod.rs:16471), not opaque pointer handles.
    gen.export("mimi_set_new", |f| f.returns(prim(I64)).effect("alloc"));
    gen.export("mimi_set_insert", |f| {
        f.param("set", prim(I64))
            .param("value", prim(I64))
            .returns(prim(I64))
    });
    gen.export("mimi_set_contains", |f| {
        f.param("set", prim(I64))
            .param("value", prim(I64))
            .returns(prim(I64))
    });
    gen.export("mimi_set_remove", |f| {
        f.param("set", prim(I64))
            .param("value", prim(I64))
            .returns(prim(I64))
    });
    gen.export("mimi_set_size", |f| {
        f.param("set", prim(I64)).returns(prim(I64))
    });
    gen.export("mimi_set_destroy", |f| {
        f.param("set", prim(I64)).effect("dealloc")
    });
    gen.export("mimi_set_to_list", |f| {
        f.param("set", prim(I64))
            .param("out_len", ptr(prim(I64)))
            .returns(ptr(prim(I64)))
            .effect("alloc")
    });

    // ── String (runtime/mod.rs:1833-2963, crypto.rs:244-288) ──
    gen.export("mimi_str_clone", |f| {
        // real: (ptr: *const c_char, len: i64) -> ValueHandle (mod.rs:1833)
        f.param("ptr", ptr(prim(U8)))
            .param("len", prim(I64))
            .returns(prim(UIntPtr))
            .effect("alloc")
    });
    gen.export("mimi_str_concat", |f| {
        f.param("a", ptr(prim(U8)))
            .param("b", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_str_char_at", |f| {
        f.param("s", ptr(prim(U8)))
            .param("index", prim(I64))
            .returns(ptr(prim(U8)))
    });
    gen.export("mimi_str_substring", |f| {
        f.param("s", ptr(prim(U8)))
            .param("start", prim(I64))
            .param("end", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_str_split", |f| {
        f.param("s", ptr(prim(U8)))
            .param("delim", ptr(prim(U8)))
            .returns(handle("ListHandle"))
            .effect("alloc")
    });
    gen.export("mimi_str_join", |f| {
        f.param("list", handle("ListHandle"))
            .param("sep", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_str_replace", |f| {
        f.param("s", ptr(prim(U8)))
            .param("from", ptr(prim(U8)))
            .param("to", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_string_free", |f| {
        f.param("ptr", ptr(prim(U8))).effect("dealloc")
    });
    gen.export("mimi_str_format", |f| {
        // real: 10 params (num_args, template, arg0..arg7) — crypto.rs:256
        f.param("num_args", prim(I64))
            .param("template", ptr(prim(U8)))
            .param("arg0", ptr(prim(U8)))
            .param("arg1", ptr(prim(U8)))
            .param("arg2", ptr(prim(U8)))
            .param("arg3", ptr(prim(U8)))
            .param("arg4", ptr(prim(U8)))
            .param("arg5", ptr(prim(U8)))
            .param("arg6", ptr(prim(U8)))
            .param("arg7", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_to_string_i64", |f| {
        f.param("val", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_to_string_f64", |f| {
        f.param("val", prim(F64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_any_to_string", |f| {
        // real: (value: ValueHandle = usize) -> *mut c_char (mod.rs:1508)
        f.param("value", prim(UIntPtr))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    // Audit 2026-08-05: mimi_string_new / mimi_string_len / mimi_string_as_slice
    // removed — phantom symbols with no runtime implementation. The real
    // `mimi_string_len` lives in ffi/runtime.rs with an unrelated signature
    // `(*const Value) -> i64` and is not part of the core runtime ABI.

    // ── I/O ──
    // Audit 2026-08-05: mimi_print_line / mimi_print_err removed — no such
    // runtime symbols exist. Codegen prints via libc puts/printf directly
    // (codegen/builtins/io.rs), so there is no runtime print ABI to export.

    // ── Runtime control (runtime/mod.rs) ──
    gen.export("mimi_runtime_abort", |f| {
        // real: (msg: *const c_char) -> ! — noreturn (mod.rs:19089)
        f.param("msg", ptr(prim(U8))).unsafe_fn().effect("noreturn")
    });
    gen.export("mimi_trap_overflow", |f| {
        // real: (op: *const c_char) -> ! — SD-7 trap (mod.rs:19508)
        f.param("op", ptr(prim(U8))).effect("noreturn")
    });
    gen.export("mimi_trap_div_by_zero", |f| {
        // real: () -> ! (mod.rs:19540)
        f.effect("noreturn")
    });
    gen.export("mimi_trap_div_overflow", |f| {
        // real: () -> ! (mod.rs:19555)
        f.effect("noreturn")
    });
    gen.export("mimi_trap_float_not_finite", |f| {
        // real: (op: *const c_char) -> ! — SD-9 finiteness trap (mod.rs:19571)
        f.param("op", ptr(prim(U8))).effect("noreturn")
    });
    gen.export("mimi_trap_no_flow_transition", |f| {
        // real: (flow, verb, from_state: *const c_char) -> ! — 0.36.10
        // recover/reset-on-live-state trap (mirrors the VM's flow-transition
        // miss text, generic E0800)
        f.param("flow", ptr(prim(U8)))
            .param("verb", ptr(prim(U8)))
            .param("from_state", ptr(prim(U8)))
            .effect("noreturn")
    });
    gen.export("mimi_runtime_set_error_handler", |f| {
        // real: (handler: Option<ErrorHandler>) — fn-pointer-sized (mod.rs:19080)
        f.param("handler", prim(IntPtr))
    });
    gen.export("mimi_install_no_panic_handlers", |f| f.effect("io"));
    gen.export("mimi_restore_no_panic_handlers", |f| f.effect("io"));
    gen.export("mimi_try_exit", |f| {
        // real: (payload: i64) -> ! (mod.rs:2992)
        f.param("payload", prim(I64)).effect("noreturn")
    });
    gen.export("mimi_try_exit_str", |f| {
        // real: (str: *const c_char, len: i64) -> ! (mod.rs:2999)
        f.param("msg", ptr(prim(U8)))
            .param("len", prim(I64))
            .effect("noreturn")
    });
    gen.export("mimi_assert_state", |f| {
        // real: (actual_state, expected_state: *const c_char) -> i64 (mod.rs:19266)
        f.param("actual_state", ptr(prim(U8)))
            .param("expected_state", ptr(prim(U8)))
            .returns(prim(I64))
    });
    gen.export("mimi_value_type_name", |f| {
        // real: (_handle: ValueHandle = usize) -> *const c_char (mod.rs:1810)
        f.param("handle", prim(UIntPtr)).returns(ptr(prim(U8)))
    });

    // ── Concurrency: Atomic ──
    gen.export("mimi_atomic_i32_new", |f| {
        f.param("value", prim(I32))
            .returns(prim(I64))
            .effect("alloc")
    });
    gen.export("mimi_atomic_i32_load", |f| {
        f.param("handle", prim(I64)).returns(prim(I32))
    });
    gen.export("mimi_atomic_i32_store", |f| {
        f.param("handle", prim(I64)).param("value", prim(I32))
    });
    gen.export("mimi_atomic_i32_fetch_add", |f| {
        f.param("handle", prim(I64))
            .param("delta", prim(I32))
            .returns(prim(I32))
    });
    gen.export("mimi_atomic_i32_compare_exchange", |f| {
        f.param("handle", prim(I64))
            .param("expected", prim(I32))
            .param("desired", prim(I32))
            .returns(prim(I32))
    });
    gen.export("mimi_atomic_i32_drop", |f| {
        f.param("handle", prim(I64)).effect("dealloc")
    });
    gen.export("mimi_atomic_i64_new", |f| {
        f.param("value", prim(I64))
            .returns(prim(I64))
            .effect("alloc")
    });
    gen.export("mimi_atomic_i64_load", |f| {
        f.param("handle", prim(I64)).returns(prim(I64))
    });
    gen.export("mimi_atomic_i64_store", |f| {
        f.param("handle", prim(I64)).param("value", prim(I64))
    });
    gen.export("mimi_atomic_i64_fetch_add", |f| {
        f.param("handle", prim(I64))
            .param("delta", prim(I64))
            .returns(prim(I64))
    });
    gen.export("mimi_atomic_i64_drop", |f| {
        f.param("handle", prim(I64)).effect("dealloc")
    });
    gen.export("mimi_atomic_bool_new", |f| {
        f.param("value", prim(I32))
            .returns(prim(I64))
            .effect("alloc")
    });
    gen.export("mimi_atomic_bool_load", |f| {
        f.param("handle", prim(I64)).returns(prim(I32))
    });
    gen.export("mimi_atomic_bool_store", |f| {
        f.param("handle", prim(I64)).param("value", prim(I32))
    });
    gen.export("mimi_atomic_bool_drop", |f| {
        f.param("handle", prim(I64)).effect("dealloc")
    });

    // ── Concurrency: Mutex ──
    gen.export("mimi_mutex_new", |f| {
        f.param("value", prim(I64))
            .returns(prim(I64))
            .effect("alloc")
    });
    gen.export("mimi_mutex_lock", |f| {
        f.param("handle", prim(I64))
            .returns(prim(I64))
            .effect("blocking")
    });
    gen.export("mimi_mutex_get", |f| {
        f.param("guard_handle", prim(I64)).returns(prim(I64))
    });
    gen.export("mimi_mutex_set", |f| {
        f.param("guard_handle", prim(I64)).param("value", prim(I64))
    });
    gen.export("mimi_mutex_unlock", |f| f.param("guard_handle", prim(I64)));
    gen.export("mimi_mutex_drop", |f| {
        f.param("handle", prim(I64)).effect("dealloc")
    });

    // ── Concurrency: Channel ──
    gen.export("mimi_channel_new", |f| f.returns(prim(I64)).effect("alloc"));
    gen.export("mimi_channel_send", |f| {
        f.param("handle", prim(I64))
            .param("value", prim(I64))
            .effect("blocking")
    });
    gen.export("mimi_channel_recv", |f| {
        f.param("handle", prim(I64))
            .returns(prim(I64))
            .effect("blocking")
    });
    gen.export("mimi_channel_try_recv", |f| {
        f.param("handle", prim(I64)).returns(prim(I64))
    });
    gen.export("mimi_channel_drop", |f| {
        f.param("handle", prim(I64)).effect("dealloc")
    });

    // ── Concurrency: Session ──
    gen.export("mimi_session_pair", |f| {
        f.returns(prim(I64)).effect("alloc")
    });
    gen.export("mimi_session_lo", |f| {
        f.param("pair", prim(I64)).returns(prim(I64))
    });
    gen.export("mimi_session_hi", |f| {
        f.param("pair", prim(I64)).returns(prim(I64))
    });

    // ── File I/O (runtime/fs.rs, runtime/binary_io.rs) ──
    gen.export("mimi_is_dir", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_is_file", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_mkdir_p", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_remove_file", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_read_file_bytes", |f| {
        // real: (path: *const c_char) -> *mut c_char (binary_io.rs:53)
        f.param("path", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_write_file_bytes", |f| {
        // real: (path, data: *const c_char) -> i32 (binary_io.rs:73)
        f.param("path", ptr(prim(U8)))
            .param("data", ptr(prim(U8)))
            .returns(prim(I32))
            .effect("io")
    });
    gen.export("mimi_append_file", |f| {
        f.param("path", ptr(prim(U8)))
            .param("content", ptr(prim(U8)))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_listdir", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(handle("ListHandle"))
            .effect("io")
    });
    gen.export("mimi_path_join", |f| {
        f.param("a", ptr(prim(U8)))
            .param("b", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_path_basename", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_path_dirname", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_path_ext", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });

    // ── Time (runtime/mod.rs:3056-3072) ──
    // Audit 2026-08-05: mimi_sleep_ms / mimi_timestamp / mimi_timestamp_ms
    // removed — phantoms. Real symbols: mimi_sleep, mimi_now, mimi_now_ms.
    gen.export("mimi_sleep", |f| {
        f.param("ms", prim(I64)).effect("io").effect("blocking")
    });
    gen.export("mimi_now", |f| f.returns(prim(I64)).effect("io"));
    gen.export("mimi_now_ms", |f| f.returns(prim(I64)).effect("io"));

    // ── JSON (runtime/mod.rs) ──
    gen.export("mimi_json_serialize", |f| {
        // real: (data: *mut c_void, len: i64, elem_type: i64) -> *mut c_char (mod.rs:18460)
        f.param("data", ptr(prim(U8)))
            .param("len", prim(I64))
            .param("elem_type", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_json_deserialize", |f| {
        // real: (json, out_len: *mut i64, elem_type: i64) -> *mut c_void (mod.rs:18534).
        // The old registry entry dropped out_len — SDK calls built from it
        // would let the runtime write through a garbage pointer.
        f.param("json", ptr(prim(U8)))
            .param("out_len", ptr(prim(I64)))
            .param("elem_type", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_json_deserialize_free", |f| {
        // real: (buf: *mut c_void, len: i64, elem_type: i64) (mod.rs:18772)
        f.param("buf", ptr(prim(U8)))
            .param("len", prim(I64))
            .param("elem_type", prim(I64))
            .effect("dealloc")
    });
    gen.export("mimi_json_as_i64", |f| {
        // real: (json: *const c_char) -> i64 (mod.rs:16405)
        f.param("json", ptr(prim(U8))).returns(prim(I64))
    });
    gen.export("mimi_json_as_f64", |f| {
        // real: (json: *const c_char) -> f64 (mod.rs:16430)
        f.param("json", ptr(prim(U8))).returns(prim(F64))
    });
    gen.export("mimi_json_as_bool", |f| {
        // real: (json: *const c_char) -> i64 (mod.rs:16444)
        f.param("json", ptr(prim(U8))).returns(prim(I64))
    });
    gen.export("mimi_is_valid_json", |f| {
        // real: (json_str: *const c_char) -> i64 (mod.rs:3398)
        f.param("json", ptr(prim(U8))).returns(prim(I64))
    });
    gen.export("mimi_json_escape_string", |f| {
        f.param("s", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_from_json", |f| {
        // real: (json_str: *const c_char) -> *mut c_void (mod.rs:3384)
        f.param("json", ptr(prim(U8))).returns(ptr(prim(U8)))
    });

    // ── Crypto ──
    gen.export("mimi_sha256", |f| {
        f.param("data", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_sha256_n", |f| {
        f.param("data", ptr(prim(U8)))
            .param("len", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_base64_encode", |f| {
        f.param("data", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_base64_decode", |f| {
        f.param("data", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });

    // ── Regex ──
    gen.export("mimi_regex_match", |f| {
        f.param("text", ptr(prim(U8)))
            .param("pattern", ptr(prim(U8)))
            .returns(prim(I32))
    });
    gen.export("mimi_regex_find", |f| {
        f.param("text", ptr(prim(U8)))
            .param("pattern", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_regex_find_all", |f| {
        f.param("text", ptr(prim(U8)))
            .param("pattern", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_regex_replace", |f| {
        f.param("text", ptr(prim(U8)))
            .param("pattern", ptr(prim(U8)))
            .param("replacement", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_regex_capture_groups", |f| {
        f.param("text", ptr(prim(U8)))
            .param("pattern", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });

    // ── Net (runtime/net.rs) ──
    gen.export("mimi_socket", |f| {
        f.param("domain", prim(I64))
            .param("type_", prim(I64))
            .param("protocol", prim(I64))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_connect", |f| {
        f.param("fd", prim(I64))
            .param("host", ptr(prim(U8)))
            .param("port", prim(I64))
            .returns(prim(I64))
            .effect("io")
            .effect("blocking")
    });
    gen.export("mimi_bind", |f| {
        // real: (fd: i64, port: i64) -> i64 — binds INADDR_ANY (net.rs:112)
        f.param("fd", prim(I64))
            .param("port", prim(I64))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_listen", |f| {
        f.param("fd", prim(I64))
            .param("backlog", prim(I64))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_accept", |f| {
        f.param("fd", prim(I64))
            .returns(prim(I64))
            .effect("io")
            .effect("blocking")
    });
    gen.export("mimi_send", |f| {
        f.param("fd", prim(I64))
            .param("data", ptr(prim(U8)))
            .param("len", prim(I64))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_recv", |f| {
        // real: (fd: i64, buf_size: i64, out_len: *mut i64) -> *mut c_char (net.rs:192).
        // The runtime allocates the receive buffer itself and reports the
        // byte count through out_len; the old registry modeled a
        // caller-provided buffer + i64 return — wrong on both counts.
        f.param("fd", prim(I64))
            .param("buf_size", prim(I64))
            .param("out_len", ptr(prim(I64)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("blocking")
            .effect("alloc")
    });
    gen.export("mimi_close", |f| {
        // real: (fd: i64) -> i64 (net.rs:226)
        f.param("fd", prim(I64)).returns(prim(I64)).effect("io")
    });
    gen.export("mimi_http_get", |f| {
        f.param("url", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("blocking")
            .effect("alloc")
    });
    gen.export("mimi_http_post", |f| {
        f.param("url", ptr(prim(U8)))
            .param("body", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("blocking")
            .effect("alloc")
    });

    // ── Actor (runtime/actor.rs) ──
    gen.export("mimi_actor_spawn", |f| {
        // real: (fields_ptr: *const c_void, fields_size: i64,
        //        dispatch_fn: Option<ActorDispatchFn>) -> *mut c_void (actor.rs:161)
        f.param("fields_ptr", ptr(prim(U8)))
            .param("fields_size", prim(I64))
            .param("dispatch_fn", prim(IntPtr))
            .returns(handle("ActorHandle"))
            .effect("alloc")
    });
    gen.export("mimi_actor_spawn_detached", |f| {
        // real: (fields_ptr: *const c_void, fields_size: i64,
        //        dispatch_fn: Option<ActorDispatchFn>) -> *mut c_void (actor.rs:171)
        f.param("fields_ptr", ptr(prim(U8)))
            .param("fields_size", prim(I64))
            .param("dispatch_fn", prim(IntPtr))
            .returns(handle("ActorHandle"))
            .effect("alloc")
    });
    gen.export("mimi_actor_call", |f| {
        // real: (handle, method_id: i32, args_ptr: *const c_void,
        //        args_size: i64, result_ptr: *mut c_void) -> i64 (actor.rs:399)
        f.param("handle", handle("ActorHandle"))
            .param("method_id", prim(I32))
            .param("args_ptr", ptr(prim(U8)))
            .param("args_size", prim(I64))
            .param("result_ptr", ptr(prim(U8)))
            .returns(prim(I64))
            .effect("blocking")
    });
    gen.export("mimi_actor_drop", |f| {
        f.param("handle", handle("ActorHandle")).effect("dealloc")
    });
    gen.export("mimi_actor_id", |f| {
        f.param("handle", handle("ActorHandle")).returns(prim(U64))
    });
    gen.export("mimi_actor_current_id", |f| f.returns(prim(U64)));
    gen.export("mimi_actor_fault", |f| {
        f.param("handle", handle("ActorHandle"))
    });
    gen.export("mimi_actor_is_faulted", |f| {
        f.param("handle", handle("ActorHandle")).returns(prim(I32))
    });
    gen.export("mimi_actor_mailbox_depth", |f| {
        f.param("handle", handle("ActorHandle")).returns(prim(I64))
    });
    gen.export("mimi_actor_max_children", |f| f.returns(prim(I64)));
    gen.export("mimi_actor_spawn_count", |f| f.returns(prim(I64)));
    gen.export("mimi_actor_system_kill", |f| {
        f.param("handle", handle("ActorHandle"))
    });
    gen.export("mimi_broadcast", |f| {
        // real: (handles: *const *mut c_void, count: i64,
        //        method_name: *const c_char, out_len: *mut i64) -> *mut i64
        // (actor.rs:720). The old registry carried phantom args_ptr/args_size
        // parameters that do not exist.
        f.param("handles", ptr(ptr(prim(U8))))
            .param("count", prim(I64))
            .param("method_name", ptr(prim(U8)))
            .param("out_len", ptr(prim(I64)))
            .returns(ptr(prim(I64)))
            .effect("blocking")
            .effect("alloc")
    });
    gen.export("mimi_broadcast_free", |f| {
        f.param("ptr", ptr(prim(I64)))
            .param("len", prim(I64))
            .effect("dealloc")
    });

    // ── Future / Async ──
    gen.export("mimi_future_alloc", |f| {
        f.param("result_size", prim(U64))
            .returns(handle("FutureHandle"))
            .effect("alloc")
    });
    gen.export("mimi_future_free", |f| {
        f.param("fut", handle("FutureHandle")).effect("dealloc")
    });
    gen.export("mimi_future_set_completed", |f| {
        f.param("fut", handle("FutureHandle"))
    });
    gen.export("mimi_future_is_completed", |f| {
        f.param("fut", handle("FutureHandle")).returns(prim(I32))
    });
    gen.export("mimi_await_future", |f| {
        f.param("future", handle("FutureHandle")).effect("blocking")
    });
    gen.export("mimi_spawn_future", |f| {
        // real: (future: *mut FutureRepr, poll_fn: extern fn) -> *mut c_void (future.rs:159)
        f.param("future", handle("FutureHandle"))
            .param("poll_fn", prim(IntPtr))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_executor_run", |f| f.effect("blocking"));

    // ── Env (runtime/env.rs, runtime/fs.rs) ──
    gen.export("mimi_getenv", |f| {
        f.param("name", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_set_env", |f| {
        // real: (key, value: *const c_char) -> i64 (fs.rs:717)
        f.param("key", ptr(prim(U8)))
            .param("value", ptr(prim(U8)))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_args_count", |f| f.returns(prim(I64)));
    gen.export("mimi_args_get", |f| {
        f.param("i", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_args_init", |f| {
        // real: (argc: i32, argv: *mut *mut c_char) (env.rs:36)
        f.param("argc", prim(I32)).param("argv", ptr(ptr(prim(U8))))
    });
    gen.export("mimi_args_list", |f| {
        f.returns(handle("ListHandle")).effect("alloc")
    });

    // ── Exec (runtime/fs.rs) ──
    // Real return type is *mut MimiExecResult (layout internal to the
    // runtime); modeled as an opaque byte pointer here. Free with
    // mimi_exec_free.
    gen.export("mimi_exec", |f| {
        // real: (cmd: *const c_char) -> *mut MimiExecResult (fs.rs:268)
        f.param("cmd", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("blocking")
            .effect("alloc")
    });
    gen.export("mimi_exec_safe", |f| {
        // real: (prog: *const c_char, args: *mut MimiList) -> *mut MimiExecResult (fs.rs:433)
        f.param("prog", ptr(prim(U8)))
            .param("args", handle("ListHandle"))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("blocking")
            .effect("alloc")
    });
    gen.export("mimi_exec_pipe", |f| {
        // real: (cmd: *const c_char) -> *mut c_char (fs.rs:396)
        f.param("cmd", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("blocking")
            .effect("alloc")
    });
    gen.export("mimi_exec_free", |f| {
        // real: (res: *mut MimiExecResult) (fs.rs:360)
        f.param("res", ptr(prim(U8))).effect("dealloc")
    });

    // ── Sort (runtime/mod.rs:18415-18429) ──
    // Real sort primitives operate on raw buffers, not list handles.
    gen.export("mimi_sort_f64_inplace", |f| {
        // real: (data: *mut u8, count: i64) — count*8 writable bytes
        f.param("data", ptr(prim(U8))).param("count", prim(I64))
    });
    gen.export("mimi_sort_str_inplace", |f| {
        // real: (data: *mut *mut c_char, count: i64) — array of C-string slots
        f.param("data", ptr(ptr(prim(U8))))
            .param("count", prim(I64))
    });

    // ── Capability (runtime/capability.rs) ──
    gen.export("mimi_cap_register", |f| {
        // real: (name: *const c_char) -> i64 (capability.rs:34)
        f.param("name", ptr(prim(U8))).returns(prim(I64))
    });
    gen.export("mimi_cap_check", |f| {
        // real: (cap: i64, name: *const c_char) -> bool (capability.rs:69)
        f.param("cap", prim(I64))
            .param("name", ptr(prim(U8)))
            .returns(prim(Bool))
    });
    gen.export("mimi_cap_consume", |f| {
        // real: (cap: i64, name: *const c_char) -> bool (capability.rs:86)
        f.param("cap", prim(I64))
            .param("name", ptr(prim(U8)))
            .returns(prim(Bool))
    });
    gen.export("mimi_cap_drop", |f| {
        // real: (cap: i64) (capability.rs:61) — releases the table entry
        f.param("cap", prim(I64)).effect("dealloc")
    });

    // ── Fault injection ──
    gen.export("mimi_inject_fault", |f| {
        // real: (state_name: *const c_char) -> i64 (mod.rs:19243)
        f.param("state_name", ptr(prim(U8))).returns(prim(I64))
    });

    // ── List serialization / display (runtime/mod.rs) ──
    gen.export("mimi_list_serialize", |f| {
        // real: (data: *mut c_void, len: i64) -> *mut c_char (mod.rs:18526)
        f.param("data", ptr(prim(U8)))
            .param("len", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_list_deserialize", |f| {
        // real: (json: *const c_char, out_len: *mut i64) -> *mut c_void (mod.rs:18798)
        f.param("json", ptr(prim(U8)))
            .param("out_len", ptr(prim(I64)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_list_to_string", |f| {
        f.param("list", handle("ListHandle"))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_list_i32_to_string", |f| {
        f.param("list", handle("ListHandle"))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_list_element_kind", |f| {
        // real: (list: *const MimiList) -> i8 (mod.rs:911)
        f.param("list", handle("ListHandle")).returns(prim(I8))
    });
    gen.export("mimi_list_free_elements", |f| {
        f.param("list", handle("ListHandle")).effect("dealloc")
    });

    // ── Set display / serialization ──
    gen.export("mimi_set_to_display", |f| {
        // real: (handle: SetHandle = i64) -> *mut c_char (mod.rs:16527)
        f.param("set", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_set_list_free", |f| {
        // real: (ptr: *mut SetValueHandle, len: i64) (mod.rs:18393)
        f.param("ptr", ptr(prim(I64)))
            .param("len", prim(I64))
            .effect("dealloc")
    });

    // ── Tuple serialization ──
    gen.export("mimi_tuple_serialize", |f| {
        // real: (values: *mut i64, count: i64, elem_types: *mut i64) -> *mut c_char (mod.rs:18806)
        f.param("values", ptr(prim(I64)))
            .param("count", prim(I64))
            .param("elem_types", ptr(prim(I64)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_tuple_deserialize", |f| {
        // real: (json: *const c_char, count: i64, elem_types: *mut i64,
        //        out_values: *mut i64) -> i64 (mod.rs:18870)
        f.param("json", ptr(prim(U8)))
            .param("count", prim(I64))
            .param("elem_types", ptr(prim(I64)))
            .param("out_values", ptr(prim(I64)))
            .returns(prim(I64))
    });

    // ── Option / Result JSON ──
    gen.export("mimi_option_i64_to_json", |f| {
        // real: (disc: i64, payload: i64) -> *mut c_char (mod.rs:16507)
        f.param("disc", prim(I64))
            .param("payload", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_result_i64_to_json", |f| {
        // real: (disc: i64, ok: i64, err: i64) -> *mut c_char (mod.rs:16517)
        f.param("disc", prim(I64))
            .param("ok", prim(I64))
            .param("err", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });

    // ── File I/O extras (runtime/binary_io.rs, runtime/fs.rs) ──
    gen.export("mimi_read_file_partial", |f| {
        // real: (path: *const c_char, max_bytes: i64) -> *mut c_char (binary_io.rs:19)
        f.param("path", ptr(prim(U8)))
            .param("max_bytes", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_read_lines_each", |f| {
        // real: (path: *const c_char, callback_fn: extern "C" fn(*const c_char)) -> i64
        // (binary_io.rs:102)
        f.param("path", ptr(prim(U8)))
            .param("callback_fn", prim(IntPtr))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_read_lines_json", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_file_stat", |f| {
        // real: (path: *const c_char, err_out: *mut *mut c_char) -> *mut MimiStatResult
        // (fs.rs:551); result layout is runtime-internal, exposed as opaque bytes
        f.param("path", ptr(prim(U8)))
            .param("err_out", ptr(ptr(prim(U8))))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_file_stat_free", |f| {
        // real: (res: *mut MimiStatResult) (fs.rs:538)
        f.param("res", ptr(prim(U8))).effect("dealloc")
    });
    gen.export("mimi_walk_dir", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(handle("ListHandle"))
            .effect("io")
    });

    // ── Executor ──
    gen.export("mimi_executor_spawn", |f| {
        f.param("future", handle("FutureHandle"))
            .param("poll_fn", prim(IntPtr))
            .effect("alloc")
    });

    // ── Actor extras ──
    gen.export("mimi_actor_set_mailbox_depth", |f| {
        f.param("handle", handle("ActorHandle"))
            .param("depth", prim(I64))
    });
    gen.export("mimi_actor_set_max_children", |f| f.param("max", prim(I64)));
    gen.export("mimi_actor_set_method_names", |f| {
        // real: (handle, names: *const *const c_char, count: i64) (actor.rs:662) —
        // a raw array of C strings plus its length, not a list handle.
        f.param("handle", handle("ActorHandle"))
            .param("names", ptr(ptr(prim(U8))))
            .param("count", prim(I64))
    });
    gen.export("mimi_actor_method_id", |f| {
        f.param("handle", handle("ActorHandle"))
            .param("name", ptr(prim(U8)))
            .returns(prim(I32))
    });
    gen.export("mimi_actor_is_muted", |f| {
        f.param("handle", handle("ActorHandle")).returns(prim(I32))
    });

    // ── Misc ──
    gen.export("mimi_match_panic", |f| {
        // real: () -> ! (mod.rs:3046) — non-exhaustive match trap
        f.effect("noreturn")
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::types::AbiPrimitive;

    #[test]
    fn generator_builds_component_ir() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        assert!(gen.export_count() > 0);
        let ir = gen.build();

        assert!(!ir.exports.is_empty());
        assert!(ir.export("mimi_list_push_i64").is_some());
        assert!(ir.export("mimi_rc_alloc").is_some());
        assert!(ir.export("nonexistent").is_none());

        let list_push = ir.export("mimi_list_push_i64").expect("should exist");
        assert_eq!(list_push.params.len(), 2);
        assert!(list_push.ret.is_void());
        assert!(!list_push.is_unsafe);
    }

    #[test]
    fn c_decl_output() {
        let mut gen = AbiGenerator::new();
        gen.export("mimi_list_push_i64", |f| {
            f.param("list", handle("ListHandle"))
                .param("element", prim(AbiPrimitive::I64))
                .returns(void())
        });
        let ir = gen.build();
        let sym = ir.export("mimi_list_push_i64").expect("should exist");
        assert_eq!(
            sym.c_decl(),
            "void mimi_list_push_i64(MimiHandle/* ListHandle */ list, int64_t element)"
        );
    }

    #[test]
    fn core_runtime_abi_coverage() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();

        // v4 registry (audit 2026-08-05): 186 exports after phantom removal.
        assert!(
            ir.exports.len() >= 180,
            "expected >=180 exports, got {}",
            ir.exports.len()
        );

        // Verify critical functions from each category.
        // Audit 2026-08-05: phantom symbols (mimi_list_new, mimi_list_len,
        // mimi_print_line, mimi_print_err, mimi_sleep_ms, mimi_timestamp,
        // mimi_timestamp_ms, mimi_string_new/len/as_slice) are no longer
        // asserted — they never existed in the runtime.
        let critical = [
            // RC
            "mimi_rc_alloc",
            "mimi_rc_retain",
            "mimi_rc_release",
            "mimi_rc_upgrade",
            "mimi_rc_weak_retain",
            "mimi_rc_weak_release",
            // List
            "mimi_list_push_i64",
            "mimi_list_push_f64",
            "mimi_list_push_string",
            "mimi_list_get_i64",
            "mimi_list_get_f64",
            "mimi_list_get_string",
            "mimi_list_free",
            "mimi_list_push_grow",
            // Map
            "mimi_map_new",
            "mimi_map_set",
            "mimi_map_get",
            "mimi_map_remove",
            "mimi_map_has_key",
            "mimi_map_keys",
            "mimi_map_values",
            "mimi_map_size",
            "mimi_map_destroy",
            "mimi_map_from_list",
            // Set
            "mimi_set_new",
            "mimi_set_insert",
            "mimi_set_contains",
            "mimi_set_remove",
            "mimi_set_size",
            "mimi_set_destroy",
            "mimi_set_to_list",
            // String
            "mimi_str_clone",
            "mimi_str_concat",
            "mimi_str_char_at",
            "mimi_str_substring",
            "mimi_str_split",
            "mimi_str_join",
            "mimi_str_replace",
            "mimi_string_free",
            "mimi_str_format",
            "mimi_to_string_i64",
            "mimi_to_string_f64",
            "mimi_any_to_string",
            // Runtime control
            "mimi_runtime_abort",
            "mimi_try_exit",
            "mimi_try_exit_str",
            "mimi_assert_state",
            // Concurrency
            "mimi_atomic_i32_new",
            "mimi_atomic_i32_compare_exchange",
            "mimi_mutex_new",
            "mimi_mutex_lock",
            "mimi_mutex_unlock",
            "mimi_channel_new",
            "mimi_channel_send",
            "mimi_channel_recv",
            "mimi_session_pair",
            // File I/O
            "mimi_is_dir",
            "mimi_is_file",
            "mimi_mkdir_p",
            "mimi_remove_file",
            "mimi_read_file_bytes",
            "mimi_write_file_bytes",
            "mimi_append_file",
            "mimi_listdir",
            "mimi_path_join",
            "mimi_path_basename",
            // Time
            "mimi_sleep",
            "mimi_now",
            "mimi_now_ms",
            // JSON
            "mimi_json_serialize",
            "mimi_json_deserialize",
            "mimi_json_deserialize_free",
            "mimi_json_as_i64",
            "mimi_json_as_f64",
            "mimi_json_as_bool",
            "mimi_is_valid_json",
            "mimi_json_escape_string",
            "mimi_from_json",
            // Crypto
            "mimi_sha256",
            "mimi_sha256_n",
            "mimi_base64_encode",
            "mimi_base64_decode",
            // Regex
            "mimi_regex_match",
            "mimi_regex_find",
            "mimi_regex_find_all",
            "mimi_regex_replace",
            "mimi_regex_capture_groups",
            // Net
            "mimi_socket",
            "mimi_connect",
            "mimi_bind",
            "mimi_listen",
            "mimi_accept",
            "mimi_send",
            "mimi_recv",
            "mimi_close",
            "mimi_http_get",
            "mimi_http_post",
            // Actor
            "mimi_actor_spawn",
            "mimi_actor_call",
            "mimi_actor_drop",
            "mimi_actor_id",
            "mimi_actor_current_id",
            "mimi_actor_fault",
            "mimi_actor_is_faulted",
            "mimi_actor_mailbox_depth",
            "mimi_actor_spawn_count",
            "mimi_broadcast",
            // Future
            "mimi_future_alloc",
            "mimi_future_free",
            "mimi_future_is_completed",
            "mimi_future_set_completed",
            "mimi_await_future",
            "mimi_spawn_future",
            "mimi_executor_run",
            // Env
            "mimi_getenv",
            "mimi_set_env",
            "mimi_args_count",
            "mimi_args_get",
            "mimi_args_init",
            "mimi_args_list",
            // Exec
            "mimi_exec",
            "mimi_exec_safe",
            "mimi_exec_pipe",
            "mimi_exec_free",
            // Sort
            "mimi_sort_f64_inplace",
            "mimi_sort_str_inplace",
            // Capability
            "mimi_cap_register",
            "mimi_cap_check",
            "mimi_cap_consume",
            "mimi_cap_drop",
            // Fault
            "mimi_inject_fault",
            // List serialization
            "mimi_list_serialize",
            "mimi_list_deserialize",
            "mimi_list_to_string",
            "mimi_list_element_kind",
            "mimi_list_free_elements",
            // Set display
            "mimi_set_to_display",
            "mimi_set_list_free",
            // Tuple
            "mimi_tuple_serialize",
            "mimi_tuple_deserialize",
            // Option/Result JSON
            "mimi_option_i64_to_json",
            "mimi_result_i64_to_json",
            // File I/O extras
            "mimi_read_file_partial",
            "mimi_read_lines_each",
            "mimi_read_lines_json",
            "mimi_file_stat",
            "mimi_file_stat_free",
            "mimi_walk_dir",
            // Executor
            "mimi_executor_spawn",
            // Actor extras
            "mimi_actor_set_mailbox_depth",
            "mimi_actor_set_max_children",
            "mimi_actor_set_method_names",
            "mimi_actor_method_id",
            "mimi_actor_is_muted",
            // Misc
            "mimi_match_panic",
        ];
        for name in &critical {
            assert!(
                ir.export(name).is_some(),
                "missing critical runtime export: {}",
                name
            );
        }

        // Phantom symbols must NOT be in the registry (audit fix 2026-08-05).
        // mimi_string_len deliberately excluded: the real symbol lives in
        // ffi/runtime.rs with a different signature ((*const Value) -> i64).
        let phantoms = [
            "mimi_list_new",
            "mimi_list_len",
            "mimi_print_line",
            "mimi_print_err",
            "mimi_sleep_ms",
            "mimi_timestamp",
            "mimi_timestamp_ms",
            "mimi_string_new",
            "mimi_string_len",
            "mimi_string_as_slice",
        ];
        for name in &phantoms {
            assert!(
                ir.export(name).is_none(),
                "phantom symbol still registered: {}",
                name
            );
        }
    }

    #[test]
    fn unsafe_flag() {
        let mut gen = AbiGenerator::new();
        gen.export("mimi_runtime_abort", |f| {
            f.param("msg", ptr(prim(AbiPrimitive::U8)))
                .unsafe_fn()
                .effect("noreturn")
        });
        let ir = gen.build();
        let sym = ir.export("mimi_runtime_abort").expect("should exist");
        assert!(sym.is_unsafe);
    }

    // Audit 2026-08-05: fat_pointer_types / fat_pointer_type_refs tests
    // removed together with the phantom MimiString/MimiSlice registry
    // surface. The FatPointer vocabulary remains in types.rs for components
    // that define their own fat-pointer types.
}
