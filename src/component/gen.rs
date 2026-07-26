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
    pub fn export(&mut self, name: &str, build: impl FnOnce(SymbolBuilder) -> SymbolBuilder) {
        #[cfg(debug_assertions)]
        if self.exports.iter().any(|s| s.name == name) {
            panic!("duplicate export registration: {}", name);
        }
        let builder = SymbolBuilder::new(name, AbiSymbolKind::Function);
        let symbol = build(builder).build();
        self.exports.push(symbol);
    }

    /// Register an imported extern function.
    ///
    /// Panics in debug builds if a duplicate import name is registered.
    pub fn import(&mut self, name: &str, build: impl FnOnce(SymbolBuilder) -> SymbolBuilder) {
        #[cfg(debug_assertions)]
        if self.imports.iter().any(|s| s.name == name) {
            panic!("duplicate import registration: {}", name);
        }
        let builder = SymbolBuilder::new(name, AbiSymbolKind::ExternFunction);
        let symbol = build(builder).build();
        self.imports.push(symbol);
    }

    /// Register a type definition.
    ///
    /// Panics in debug builds if a duplicate type name is registered.
    pub fn type_def(&mut self, def: super::types::AbiTypeDef) {
        #[cfg(debug_assertions)]
        if self.types.iter().any(|t| t.name() == def.name()) {
            panic!("duplicate type definition: {}", def.name());
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
/// This is the v1 manual registry. It covers the most critical runtime
/// functions (~60 of 426: RC, list, map, set, string, I/O, concurrency,
/// file I/O, time). The full surface will be migrated incrementally.
pub fn register_core_runtime_abi(gen: &mut AbiGenerator) {
    use AbiPrimitive::*;

    // 0.31.31: register the fat-pointer struct layouts (MimiString/MimiSlice)
    // so String/buffer surfaces carry { data, len, capacity } directly instead
    // of an opaque C-string pointer + separate length.
    register_fat_pointer_types(gen);

    // Register opaque handle types (referenced by list/map/set functions).
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

    // ── List ──
    gen.export("mimi_list_new", |f| {
        f.returns(handle("ListHandle")).effect("alloc")
    });
    gen.export("mimi_list_push_i64", |f| {
        f.param("list", handle("ListHandle"))
            .param("value", prim(I64))
    });
    gen.export("mimi_list_get_i64", |f| {
        f.param("list", handle("ListHandle"))
            .param("index", prim(UIntPtr))
            .returns(prim(I64))
    });
    gen.export("mimi_list_len", |f| {
        f.param("list", handle("ListHandle")).returns(prim(UIntPtr))
    });
    gen.export("mimi_list_free", |f| {
        f.param("list", handle("ListHandle")).effect("dealloc")
    });

    // ── Map ──
    gen.export("mimi_map_new", |f| {
        f.returns(handle("MapHandle")).effect("alloc")
    });
    gen.export("mimi_map_set_i64", |f| {
        f.param("map", handle("MapHandle"))
            .param("key", ptr(prim(U8)))
            .param("value", prim(I64))
    });
    gen.export("mimi_map_get_i64", |f| {
        f.param("map", handle("MapHandle"))
            .param("key", ptr(prim(U8)))
            .returns(prim(I64))
    });
    gen.export("mimi_map_free", |f| {
        f.param("map", handle("MapHandle")).effect("dealloc")
    });

    // ── Set ──
    gen.export("mimi_set_new", |f| {
        f.returns(handle("SetHandle")).effect("alloc")
    });
    gen.export("mimi_set_insert_i64", |f| {
        f.param("set", handle("SetHandle"))
            .param("value", prim(I64))
    });
    gen.export("mimi_set_contains_i64", |f| {
        f.param("set", handle("SetHandle"))
            .param("value", prim(I64))
            .returns(prim(Bool))
    });
    gen.export("mimi_set_free", |f| {
        f.param("set", handle("SetHandle")).effect("dealloc")
    });

    // ── String (0.31.31: fat pointers replace C-string *mut c_char) ──
    gen.export("mimi_string_new", |f| {
        f.param("bytes", fat_slice(prim(U8)))
            .returns(fat_string())
            .effect("alloc")
    });
    gen.export("mimi_string_len", |f| {
        f.param("s", fat_string()).returns(prim(UIntPtr))
    });
    gen.export("mimi_string_free", |f| {
        f.param("s", fat_string()).effect("dealloc")
    });
    // Zero-copy view: hand out a { data, len } slice into an existing string
    // without a marshalling copy (blind review:胶水语言 Marshalling Tax).
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

    // ── Time ──
    gen.export("mimi_sleep_ms", |f| {
        f.param("ms", prim(I64)).effect("io").effect("blocking")
    });
    gen.export("mimi_timestamp", |f| f.returns(prim(I64)).effect("io"));
    gen.export("mimi_timestamp_ms", |f| f.returns(prim(I64)).effect("io"));
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

        // Verify critical functions are registered
        let critical = [
            "mimi_rc_alloc",
            "mimi_rc_retain",
            "mimi_rc_release",
            "mimi_list_new",
            "mimi_list_push_i64",
            "mimi_list_get_i64",
            "mimi_list_len",
            "mimi_list_free",
            "mimi_map_new",
            "mimi_map_set_i64",
            "mimi_map_get_i64",
            "mimi_map_free",
            "mimi_set_new",
            "mimi_set_insert_i64",
            "mimi_set_contains_i64",
            "mimi_set_free",
            "mimi_string_new",
            "mimi_string_len",
            "mimi_string_free",
            "mimi_print_line",
            "mimi_print_err",
            "mimi_runtime_abort",
            "mimi_wall_clock_ms",
            // Concurrency
            "mimi_atomic_i32_new",
            "mimi_atomic_i32_load",
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
            // Time
            "mimi_sleep_ms",
            "mimi_timestamp",
            "mimi_timestamp_ms",
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
