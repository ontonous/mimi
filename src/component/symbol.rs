//! Component IR symbol definitions.
//!
//! Defines the ABI-level symbol representation: functions, parameters,
//! calling conventions, and symbol kinds.

use super::types::AbiTypeRef;

/// ABI symbol: a function or method in the Component IR.
#[derive(Debug, Clone)]
pub struct AbiSymbol {
    /// Symbol name (e.g., "mimi_list_push_i64").
    pub name: String,
    /// Symbol kind.
    pub kind: AbiSymbolKind,
    /// Parameters in order.
    pub params: Vec<AbiParam>,
    /// Return type.
    pub ret: AbiTypeRef,
    /// Effect annotations (e.g., "io", "alloc", "blocking").
    pub effects: Vec<String>,
    /// Whether this function is unsafe (requires caller vigilance).
    pub is_unsafe: bool,
    /// Calling convention.
    pub call_conv: AbiCallConv,
}

impl AbiSymbol {
    /// C function declaration string.
    pub fn c_decl(&self) -> String {
        let params = if self.params.is_empty() {
            "void".to_string()
        } else {
            self.params
                .iter()
                .map(|p| format!("{} {}", p.ty.c_type_name(), p.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "{} {}({})",
            self.ret.c_type_name(),
            self.name,
            params
        )
    }

    /// True if this is a runtime export (mimi_* naming convention).
    pub fn is_runtime_export(&self) -> bool {
        self.name.starts_with("mimi_")
    }
}

/// Symbol kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiSymbolKind {
    /// Free function.
    Function,
    /// Extern function (user-declared extern "C").
    ExternFunction,
    /// Method (associated with a type).
    Method,
    /// Constructor.
    Constructor,
    /// Destructor.
    Destructor,
}

/// ABI parameter.
#[derive(Debug, Clone)]
pub struct AbiParam {
    /// Parameter name.
    pub name: String,
    /// Parameter type.
    pub ty: AbiTypeRef,
    /// Whether this parameter is nullable (for pointer types).
    pub is_nullable: bool,
}

/// Calling convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiCallConv {
    /// C calling convention (extern "C").
    C,
    /// System V AMD64 (Linux/macOS).
    SystemV,
    /// Windows x64.
    Win64,
    /// Fast call.
    Fast,
    /// Mimi internal (not exposed to FFI).
    MimiInternal,
}

impl AbiCallConv {
    /// LLVM calling convention name.
    pub fn llvm_name(&self) -> &'static str {
        match self {
            AbiCallConv::C => "ccc",
            AbiCallConv::SystemV => "x86_64_sysvcc",
            AbiCallConv::Win64 => "win64cc",
            AbiCallConv::Fast => "fastcc",
            AbiCallConv::MimiInternal => "mimi_internal",
        }
    }
}

impl Default for AbiCallConv {
    fn default() -> Self {
        AbiCallConv::C
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::types::AbiPrimitive;

    #[test]
    fn c_decl_generation() {
        let sym = AbiSymbol {
            name: "mimi_list_push_i64".to_string(),
            kind: AbiSymbolKind::Function,
            params: vec![
                AbiParam {
                    name: "list".to_string(),
                    ty: AbiTypeRef::Primitive(AbiPrimitive::IntPtr),
                    is_nullable: false,
                },
                AbiParam {
                    name: "value".to_string(),
                    ty: AbiTypeRef::Primitive(AbiPrimitive::I64),
                    is_nullable: false,
                },
            ],
            ret: AbiTypeRef::Void,
            effects: vec![],
            is_unsafe: false,
            call_conv: AbiCallConv::C,
        };

        assert_eq!(
            sym.c_decl(),
            "void mimi_list_push_i64(intptr_t list, int64_t value)"
        );
        assert!(sym.is_runtime_export());
    }

    #[test]
    fn c_decl_no_params() {
        let sym = AbiSymbol {
            name: "mimi_timestamp".to_string(),
            kind: AbiSymbolKind::Function,
            params: vec![],
            ret: AbiTypeRef::Primitive(AbiPrimitive::I64),
            effects: vec!["io".to_string()],
            is_unsafe: false,
            call_conv: AbiCallConv::C,
        };

        assert_eq!(sym.c_decl(), "int64_t mimi_timestamp(void)");
    }
}
