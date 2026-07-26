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

/// 0.31.31: Convenience: fat pointer type reference (String-like with capacity).
pub fn fat_string() -> AbiTypeRef {
    AbiTypeRef::FatPointer {
        element: Box::new(AbiTypeRef::Primitive(AbiPrimitive::U8)),
        has_capacity: true,
    }
}

/// 0.31.31: Convenience: fat pointer slice type reference (no capacity).
pub fn fat_slice(element: AbiTypeRef) -> AbiTypeRef {
    AbiTypeRef::FatPointer {
        element: Box::new(element),
        has_capacity: false,
    }
}

/// Register standard fat pointer type definitions.
///
/// 0.31.31: These replace the opaque handle types for String/List/Map/Set.
/// Fat pointers carry { data, len, capacity } directly, eliminating the
/// global handle registry lookup.
pub fn register_fat_pointer_types(gen: &mut AbiGenerator) {
    use super::types::{AbiField, AbiStruct};
    use AbiPrimitive::*;

    // MimiString: { data: *mut u8, len: usize, capacity: usize }
    gen.type_def(super::types::AbiTypeDef::Struct(AbiStruct {
        name: "MimiString".to_string(),
        fields: vec![
            AbiField {
                name: "data".to_string(),
                ty: ptr(prim(U8)),
                offset: Some(0),
            },
            AbiField {
                name: "len".to_string(),
                ty: prim(UIntPtr),
                offset: Some(8),
            },
            AbiField {
                name: "capacity".to_string(),
                ty: prim(UIntPtr),
                offset: Some(16),
            },
        ],
        is_repr_c: true,
        size: Some(24),
        align: Some(8),
    }));

    // MimiSlice: { data: *mut T, len: usize }
    gen.type_def(super::types::AbiTypeDef::Struct(AbiStruct {
        name: "MimiSlice".to_string(),
        fields: vec![
            AbiField {
                name: "data".to_string(),
                ty: ptr(prim(U8)),
                offset: Some(0),
            },
            AbiField {
                name: "len".to_string(),
                ty: prim(UIntPtr),
                offset: Some(8),
            },
        ],
        is_repr_c: true,
        size: Some(16),
        align: Some(8),
    }));
}

/// Register the core runtime ABI surface.
///
/// This is the v3 manual registry. It covers ~180 of 388 runtime functions:
/// RC, list, map, set, string, I/O, concurrency, file I/O, time, JSON,
/// crypto, regex, net, actor, future, env, exec, sort, capability, misc,
/// list/set serialization, tuple, option/result JSON, executor, actor extras.
/// JSON combinatorial serialization variants (mimi_map_from_json_* etc.)
/// are intentionally excluded — they are internal codegen artifacts.
pub fn register_core_runtime_abi(gen: &mut AbiGenerator) {
    use AbiPrimitive::*;

    // 0.31.31: register the fat-pointer struct layouts (MimiString/MimiSlice)
    // so String/buffer surfaces carry { data, len, capacity } directly instead
    // of an opaque C-string pointer + separate length.
    register_fat_pointer_types(gen);

    // Register opaque handle types (referenced by list/map/set/actor/future).
    use super::types::{AbiOpaque, AbiTypeDef};
    gen.type_def(AbiTypeDef::Opaque(AbiOpaque {
        name: "ListHandle".to_string(),
        description: "Opaque handle to a Mimi list (generational, kind-tagged)".to_string(),
    }));
    gen.type_def(AbiTypeDef::Opaque(AbiOpaque {
        name: "MapHandle".to_string(),
        description: "Opaque handle to a Mimi map (generational, kind-tagged)".to_string(),
    }));
    gen.type_def(AbiTypeDef::Opaque(AbiOpaque {
        name: "SetHandle".to_string(),
        description: "Opaque handle to a Mimi set (generational, kind-tagged)".to_string(),
    }));
    gen.type_def(AbiTypeDef::Opaque(AbiOpaque {
        name: "ActorHandle".to_string(),
        description: "Opaque handle to a Mimi actor".to_string(),
    }));
    gen.type_def(AbiTypeDef::Opaque(AbiOpaque {
        name: "FutureHandle".to_string(),
        description: "Opaque handle to a Mimi future/task".to_string(),
    }));

    // ── RC / Allocation ──
    gen.export("mimi_rc_alloc", |f| {
        f.param("size", prim(UIntPtr))
            .returns(prim(IntPtr))
            .effect("alloc")
    });
    gen.export("mimi_rc_retain", |f| f.param("ptr", prim(IntPtr)));
    gen.export("mimi_rc_release", |f| {
        f.param("ptr", prim(IntPtr)).effect("dealloc")
    });
    gen.export("mimi_rc_upgrade", |f| {
        f.param("weak_ptr", prim(IntPtr)).returns(prim(IntPtr))
    });
    gen.export("mimi_rc_weak_retain", |f| f.param("ptr", prim(IntPtr)));
    gen.export("mimi_rc_weak_release", |f| {
        f.param("ptr", prim(IntPtr)).effect("dealloc")
    });

    // ── List ──
    gen.export("mimi_list_new", |f| {
        f.returns(handle("ListHandle")).effect("alloc")
    });
    gen.export("mimi_list_push_i64", |f| {
        f.param("list", handle("ListHandle"))
            .param("value", prim(I64))
    });
    gen.export("mimi_list_push_f64", |f| {
        f.param("list", handle("ListHandle"))
            .param("value", prim(F64))
    });
    gen.export("mimi_list_push_string", |f| {
        f.param("list", handle("ListHandle"))
            .param("value", ptr(prim(U8)))
    });
    gen.export("mimi_list_get_i64", |f| {
        f.param("list", handle("ListHandle"))
            .param("index", prim(UIntPtr))
            .returns(prim(I64))
    });
    gen.export("mimi_list_get_f64", |f| {
        f.param("list", handle("ListHandle"))
            .param("index", prim(UIntPtr))
            .returns(prim(F64))
    });
    gen.export("mimi_list_get_string", |f| {
        f.param("list", handle("ListHandle"))
            .param("index", prim(UIntPtr))
            .returns(ptr(prim(U8)))
    });
    gen.export("mimi_list_len", |f| {
        f.param("list", handle("ListHandle")).returns(prim(UIntPtr))
    });
    gen.export("mimi_list_free", |f| {
        f.param("list", handle("ListHandle")).effect("dealloc")
    });
    gen.export("mimi_list_push_grow", |f| {
        f.param("list", handle("ListHandle"))
            .param("value", prim(I64))
            .param("capacity", prim(UIntPtr))
    });

    // ── Map ──
    gen.export("mimi_map_new", |f| {
        f.returns(handle("MapHandle")).effect("alloc")
    });
    gen.export("mimi_map_set", |f| {
        f.param("map", handle("MapHandle"))
            .param("key", ptr(prim(U8)))
            .param("value", prim(I64))
    });
    gen.export("mimi_map_get", |f| {
        f.param("map", handle("MapHandle"))
            .param("key", ptr(prim(U8)))
            .returns(prim(I64))
    });
    gen.export("mimi_map_remove", |f| {
        f.param("map", handle("MapHandle"))
            .param("key", ptr(prim(U8)))
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
        f.param("map", handle("MapHandle")).returns(prim(UIntPtr))
    });
    gen.export("mimi_map_destroy", |f| {
        f.param("map", handle("MapHandle")).effect("dealloc")
    });
    gen.export("mimi_map_from_list", |f| {
        f.param("keys", handle("ListHandle"))
            .param("values", handle("ListHandle"))
            .returns(handle("MapHandle"))
            .effect("alloc")
    });

    // ── Set ──
    gen.export("mimi_set_new", |f| {
        f.returns(handle("SetHandle")).effect("alloc")
    });
    gen.export("mimi_set_insert", |f| {
        f.param("set", handle("SetHandle"))
            .param("value", prim(I64))
    });
    gen.export("mimi_set_contains", |f| {
        f.param("set", handle("SetHandle"))
            .param("value", prim(I64))
            .returns(prim(I32))
    });
    gen.export("mimi_set_remove", |f| {
        f.param("set", handle("SetHandle"))
            .param("value", prim(I64))
    });
    gen.export("mimi_set_size", |f| {
        f.param("set", handle("SetHandle")).returns(prim(UIntPtr))
    });
    gen.export("mimi_set_destroy", |f| {
        f.param("set", handle("SetHandle")).effect("dealloc")
    });
    gen.export("mimi_set_to_list", |f| {
        f.param("set", handle("SetHandle"))
            .returns(handle("ListHandle"))
    });

    // ── String ──
    gen.export("mimi_str_clone", |f| {
        f.param("s", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
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
        f.param("s", ptr(prim(U8))).effect("dealloc")
    });
    gen.export("mimi_str_format", |f| {
        f.param("num_args", prim(I64))
            .param("template", ptr(prim(U8)))
            .param("arg0", ptr(prim(U8)))
            .param("arg1", ptr(prim(U8)))
            .param("arg2", ptr(prim(U8)))
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
        f.param("value", prim(I64))
            .param("type_tag", prim(I32))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    // 0.31.31: fat-pointer string surface (zero-copy, replaces C-string marshalling)
    gen.export("mimi_string_new", |f| {
        f.param("bytes", fat_slice(prim(U8)))
            .returns(fat_string())
            .effect("alloc")
    });
    gen.export("mimi_string_len", |f| {
        f.param("s", fat_string()).returns(prim(UIntPtr))
    });
    gen.export("mimi_string_as_slice", |f| {
        f.param("s", fat_string()).returns(fat_slice(prim(U8)))
    });

    // ── I/O ──
    gen.export("mimi_print_line", |f| {
        f.param("data", ptr(prim(U8)))
            .param("len", prim(UIntPtr))
            .effect("io")
    });
    gen.export("mimi_print_err", |f| {
        f.param("data", ptr(prim(U8)))
            .param("len", prim(UIntPtr))
            .effect("io")
    });

    // ── Runtime control ──
    gen.export("mimi_runtime_abort", |f| {
        f.param("msg", ptr(prim(U8)))
            .param("len", prim(UIntPtr))
            .unsafe_fn()
    });
    gen.export("mimi_wall_clock_ms", |f| f.returns(prim(I64)).effect("io"));
    gen.export("mimi_runtime_set_error_handler", |f| {
        f.param("handler", prim(IntPtr))
    });
    gen.export("mimi_install_no_panic_handlers", |f| f.effect("io"));
    gen.export("mimi_restore_no_panic_handlers", |f| f.effect("io"));
    gen.export("mimi_try_exit", |f| f.param("code", prim(I32)));
    gen.export("mimi_try_exit_str", |f| f.param("msg", ptr(prim(U8))));
    gen.export("mimi_assert_state", |f| {
        f.param("cond", prim(I32)).param("msg", ptr(prim(U8)))
    });
    gen.export("mimi_value_type_name", |f| {
        f.param("type_tag", prim(I32)).returns(ptr(prim(U8)))
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

    // ── File I/O ──
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
        f.param("path", ptr(prim(U8)))
            .param("out_len", ptr(prim(UIntPtr)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_write_file_bytes", |f| {
        f.param("path", ptr(prim(U8)))
            .param("data", ptr(prim(U8)))
            .param("len", prim(I64))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_append_file", |f| {
        f.param("path", ptr(prim(U8)))
            .param("data", ptr(prim(U8)))
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

    // ── Time ──
    gen.export("mimi_sleep_ms", |f| {
        f.param("ms", prim(I64)).effect("io").effect("blocking")
    });
    gen.export("mimi_timestamp", |f| f.returns(prim(I64)).effect("io"));
    gen.export("mimi_timestamp_ms", |f| f.returns(prim(I64)).effect("io"));
    gen.export("mimi_now", |f| f.returns(prim(I64)).effect("io"));
    gen.export("mimi_now_ms", |f| f.returns(prim(I64)).effect("io"));

    // ── JSON ──
    gen.export("mimi_json_serialize", |f| {
        f.param("value", prim(I64))
            .param("type_tag", prim(I32))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_json_deserialize", |f| {
        f.param("json", ptr(prim(U8)))
            .returns(prim(I64))
            .effect("alloc")
    });
    gen.export("mimi_json_deserialize_free", |f| {
        f.param("ptr", prim(I64)).effect("dealloc")
    });
    gen.export("mimi_json_as_i64", |f| {
        f.param("json_ptr", prim(I64)).returns(prim(I64))
    });
    gen.export("mimi_json_as_f64", |f| {
        f.param("json_ptr", prim(I64)).returns(prim(F64))
    });
    gen.export("mimi_json_as_bool", |f| {
        f.param("json_ptr", prim(I64)).returns(prim(I32))
    });
    gen.export("mimi_is_valid_json", |f| {
        f.param("json", ptr(prim(U8))).returns(prim(I32))
    });
    gen.export("mimi_json_escape_string", |f| {
        f.param("s", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_from_json", |f| {
        f.param("json", ptr(prim(U8)))
            .param("type_tag", prim(I32))
            .returns(prim(I64))
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

    // ── Net ──
    gen.export("mimi_socket", |f| {
        f.param("domain", prim(I64))
            .param("type_", prim(I64))
            .param("protocol", prim(I64))
            .returns(prim(I64))
            .effect("io")
    });
    gen.export("mimi_connect", |f| {
        f.param("fd", prim(I64))
            .param("addr", ptr(prim(U8)))
            .param("port", prim(I64))
            .returns(prim(I64))
            .effect("io")
            .effect("blocking")
    });
    gen.export("mimi_bind", |f| {
        f.param("fd", prim(I64))
            .param("addr", ptr(prim(U8)))
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
        f.param("fd", prim(I64))
            .param("buf", ptr(prim(U8)))
            .param("buf_len", prim(I64))
            .returns(prim(I64))
            .effect("io")
            .effect("blocking")
    });
    gen.export("mimi_close", |f| f.param("fd", prim(I64)).effect("io"));
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

    // ── Actor ──
    gen.export("mimi_actor_spawn", |f| {
        f.param("fields_ptr", prim(IntPtr))
            .param("fields_size", prim(I64))
            .param("dispatch_fn", prim(IntPtr))
            .returns(handle("ActorHandle"))
            .effect("alloc")
    });
    gen.export("mimi_actor_spawn_detached", |f| {
        f.param("fields_ptr", prim(IntPtr))
            .param("fields_size", prim(I64))
            .param("dispatch_fn", prim(IntPtr))
            .returns(handle("ActorHandle"))
            .effect("alloc")
    });
    gen.export("mimi_actor_call", |f| {
        f.param("handle", handle("ActorHandle"))
            .param("method_id", prim(I32))
            .param("args_ptr", prim(IntPtr))
            .param("args_size", prim(I64))
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
        f.param("handles", prim(IntPtr))
            .param("count", prim(I64))
            .param("method_name", ptr(prim(U8)))
            .param("args_ptr", prim(IntPtr))
            .param("args_size", prim(I64))
            .returns(ptr(prim(I64)))
            .effect("blocking")
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
        f.param("future", handle("FutureHandle"))
            .param("poll_fn", prim(IntPtr))
            .effect("alloc")
    });
    gen.export("mimi_executor_run", |f| f.effect("blocking"));

    // ── Env ──
    gen.export("mimi_getenv", |f| {
        f.param("name", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_set_env", |f| {
        f.param("name", ptr(prim(U8)))
            .param("value", ptr(prim(U8)))
            .effect("io")
    });
    gen.export("mimi_args_count", |f| f.returns(prim(I64)));
    gen.export("mimi_args_get", |f| {
        f.param("i", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_args_init", |f| {
        f.param("argc", prim(I32)).param("argv", prim(IntPtr))
    });
    gen.export("mimi_args_list", |f| {
        f.returns(handle("ListHandle")).effect("alloc")
    });

    // ── Exec ──
    gen.export("mimi_exec", |f| {
        f.param("cmd", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("blocking")
            .effect("alloc")
    });
    gen.export("mimi_exec_safe", |f| {
        f.param("cmd", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("blocking")
            .effect("alloc")
    });
    gen.export("mimi_exec_pipe", |f| {
        f.param("cmd", ptr(prim(U8)))
            .param("input", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("blocking")
            .effect("alloc")
    });
    gen.export("mimi_exec_free", |f| {
        f.param("ptr", ptr(prim(U8))).effect("dealloc")
    });

    // ── Sort ──
    gen.export("mimi_sort_f64_inplace", |f| {
        f.param("list", handle("ListHandle"))
    });
    gen.export("mimi_sort_str_inplace", |f| {
        f.param("list", handle("ListHandle"))
    });

    // ── Capability ──
    gen.export("mimi_cap_register", |f| {
        f.param("name", ptr(prim(U8))).param("level", prim(I32))
    });
    gen.export("mimi_cap_check", |f| {
        f.param("name", ptr(prim(U8)))
            .param("required", prim(I32))
            .returns(prim(I32))
    });
    gen.export("mimi_cap_consume", |f| {
        f.param("name", ptr(prim(U8))).returns(prim(I32))
    });

    // ── Fault injection ──
    gen.export("mimi_inject_fault", |f| {
        f.param("name", ptr(prim(U8)))
            .param("probability", prim(F64))
    });

    // ── List serialization / display ──
    gen.export("mimi_list_serialize", |f| {
        f.param("list", handle("ListHandle"))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_list_deserialize", |f| {
        f.param("data", ptr(prim(U8)))
            .returns(handle("ListHandle"))
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
        f.param("list", handle("ListHandle"))
            .param("index", prim(UIntPtr))
            .returns(prim(I32))
    });
    gen.export("mimi_list_free_elements", |f| {
        f.param("list", handle("ListHandle")).effect("dealloc")
    });

    // ── Set display / serialization ──
    gen.export("mimi_set_to_display", |f| {
        f.param("set", handle("SetHandle"))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_set_list_free", |f| {
        f.param("list", handle("ListHandle")).effect("dealloc")
    });

    // ── Tuple serialization ──
    gen.export("mimi_tuple_serialize", |f| {
        f.param("ptr", prim(IntPtr))
            .param("type_tag", prim(I32))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_tuple_deserialize", |f| {
        f.param("data", ptr(prim(U8)))
            .param("type_tag", prim(I32))
            .returns(prim(IntPtr))
            .effect("alloc")
    });

    // ── Option / Result JSON ──
    gen.export("mimi_option_i64_to_json", |f| {
        f.param("has_value", prim(I32))
            .param("value", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });
    gen.export("mimi_result_i64_to_json", |f| {
        f.param("is_ok", prim(I32))
            .param("value", prim(I64))
            .returns(ptr(prim(U8)))
            .effect("alloc")
    });

    // ── File I/O extras ──
    gen.export("mimi_read_file_partial", |f| {
        f.param("path", ptr(prim(U8)))
            .param("offset", prim(I64))
            .param("len", prim(I64))
            .param("out_len", ptr(prim(UIntPtr)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_read_lines_each", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(handle("ListHandle"))
            .effect("io")
    });
    gen.export("mimi_read_lines_json", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(ptr(prim(U8)))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_file_stat", |f| {
        f.param("path", ptr(prim(U8)))
            .returns(prim(IntPtr))
            .effect("io")
            .effect("alloc")
    });
    gen.export("mimi_file_stat_free", |f| {
        f.param("stat", prim(IntPtr)).effect("dealloc")
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
        f.param("handle", handle("ActorHandle"))
            .param("names", handle("ListHandle"))
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
    gen.export("mimi_match_panic", |f| f.param("msg", ptr(prim(U8))));
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
                .param("value", prim(AbiPrimitive::I64))
                .returns(void())
        });
        let ir = gen.build();
        let sym = ir.export("mimi_list_push_i64").expect("should exist");
        assert_eq!(
            sym.c_decl(),
            "void mimi_list_push_i64(MimiHandle/* ListHandle */ list, int64_t value)"
        );
    }

    #[test]
    fn core_runtime_abi_coverage() {
        let mut gen = AbiGenerator::new();
        register_core_runtime_abi(&mut gen);
        let ir = gen.build();

        // v2 registry: ~180 functions across 22 categories
        assert!(
            ir.exports.len() >= 170,
            "expected >=170 exports, got {}",
            ir.exports.len()
        );

        // Verify critical functions from each category
        let critical = [
            // RC
            "mimi_rc_alloc",
            "mimi_rc_retain",
            "mimi_rc_release",
            "mimi_rc_upgrade",
            "mimi_rc_weak_retain",
            "mimi_rc_weak_release",
            // List
            "mimi_list_new",
            "mimi_list_push_i64",
            "mimi_list_push_f64",
            "mimi_list_push_string",
            "mimi_list_get_i64",
            "mimi_list_get_f64",
            "mimi_list_get_string",
            "mimi_list_len",
            "mimi_list_free",
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
            // I/O + Runtime
            "mimi_print_line",
            "mimi_print_err",
            "mimi_runtime_abort",
            "mimi_wall_clock_ms",
            "mimi_try_exit",
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
            "mimi_sleep_ms",
            "mimi_timestamp",
            "mimi_timestamp_ms",
            "mimi_now",
            "mimi_now_ms",
            // JSON
            "mimi_json_serialize",
            "mimi_json_deserialize",
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
    }

    #[test]
    fn unsafe_flag() {
        let mut gen = AbiGenerator::new();
        gen.export("mimi_runtime_abort", |f| {
            f.param("msg", ptr(prim(AbiPrimitive::U8))).unsafe_fn()
        });
        let ir = gen.build();
        let sym = ir.export("mimi_runtime_abort").expect("should exist");
        assert!(sym.is_unsafe);
    }

    #[test]
    fn fat_pointer_types() {
        let mut gen = AbiGenerator::new();
        register_fat_pointer_types(&mut gen);
        let ir = gen.build();

        // MimiString: { data, len, capacity } = 24 bytes
        let string_ty = ir.type_def("MimiString").expect("MimiString should exist");
        if let super::super::types::AbiTypeDef::Struct(s) = string_ty {
            assert_eq!(s.fields.len(), 3);
            assert_eq!(s.size, Some(24));
            assert!(s.is_repr_c);
        } else {
            panic!("MimiString should be a struct");
        }

        // MimiSlice: { data, len } = 16 bytes
        let slice_ty = ir.type_def("MimiSlice").expect("MimiSlice should exist");
        if let super::super::types::AbiTypeDef::Struct(s) = slice_ty {
            assert_eq!(s.fields.len(), 2);
            assert_eq!(s.size, Some(16));
        } else {
            panic!("MimiSlice should be a struct");
        }
    }

    #[test]
    fn fat_pointer_type_refs() {
        let s = fat_string();
        assert_eq!(s.c_type_name(), "MimiString/* uint8_t */");

        let sl = fat_slice(prim(AbiPrimitive::I64));
        assert_eq!(sl.c_type_name(), "MimiSlice/* int64_t */");
    }
}
