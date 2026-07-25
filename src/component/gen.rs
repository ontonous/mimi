//! ABI generator: produces ComponentIr from runtime exports.
//!
//! 0.31.30 v1: builder-based registry. The generator provides a typed
//! API for registering runtime function signatures, replacing the
//! 352 string-based `get_runtime_fn("name")` lookups in codegen.
//!
//! Future: automated extraction from `register_runtime()` LLVM declarations.

use super::symbol::{AbiCallConv, AbiParam, AbiSymbol, AbiSymbolKind};
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
    pub fn export(&mut self, name: &str, build: impl FnOnce(SymbolBuilder) -> SymbolBuilder) {
        let builder = SymbolBuilder::new(name, AbiSymbolKind::Function);
        let symbol = build(builder).build();
        self.exports.push(symbol);
    }

    /// Register an imported extern function.
    pub fn import(&mut self, name: &str, build: impl FnOnce(SymbolBuilder) -> SymbolBuilder) {
        let builder = SymbolBuilder::new(name, AbiSymbolKind::ExternFunction);
        let symbol = build(builder).build();
        self.imports.push(symbol);
    }

    /// Register a type definition.
    pub fn type_def(&mut self, def: super::types::AbiTypeDef) {
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

    fn build(self) -> AbiSymbol {
        AbiSymbol {
            name: self.name,
            kind: self.kind,
            params: self.params,
            ret: self.ret,
            effects: self.effects,
            is_unsafe: self.is_unsafe,
            call_conv: self.call_conv,
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
pub fn void() -> AbiTypeRef {
    AbiTypeRef::Void
}

/// Convenience: opaque handle type reference.
pub fn handle(name: &str) -> AbiTypeRef {
    AbiTypeRef::Opaque(name.to_string())
}

/// Register the core runtime ABI surface.
///
/// This is the v1 manual registry. It covers the most critical runtime
/// functions (list, map, set, string, RC, I/O). The full 426-function
/// surface will be migrated incrementally.
pub fn register_core_runtime_abi(gen: &mut AbiGenerator) {
    use AbiPrimitive::*;

    // ── RC / Allocation ──
    gen.export("mimi_rc_alloc", |f| {
        f.param("size", prim(UIntPtr)).returns(prim(IntPtr)).effect("alloc")
    });
    gen.export("mimi_rc_retain", |f| {
        f.param("ptr", prim(IntPtr))
    });
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
        f.param("list", handle("ListHandle"))
         .returns(prim(UIntPtr))
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

    // ── String ──
    gen.export("mimi_string_new", |f| {
        f.param("data", ptr(prim(U8)))
         .param("len", prim(UIntPtr))
         .returns(handle("StringHandle"))
         .effect("alloc")
    });
    gen.export("mimi_string_len", |f| {
        f.param("s", handle("StringHandle"))
         .returns(prim(UIntPtr))
    });
    gen.export("mimi_string_free", |f| {
        f.param("s", handle("StringHandle")).effect("dealloc")
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
    gen.export("mimi_wall_clock_ms", |f| {
        f.returns(prim(I64)).effect("io")
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
            f.param("msg", ptr(prim(AbiPrimitive::U8)))
             .unsafe_fn()
        });
        let ir = gen.build();
        let sym = ir.export("mimi_runtime_abort").expect("should exist");
        assert!(sym.is_unsafe);
    }
}
