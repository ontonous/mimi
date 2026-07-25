//! Component IR type system.
//!
//! Defines the ABI-level type representation used by the Component IR.
//! These types are language-neutral and map to concrete representations
//! in each target language (C, Rust, Node, Python, Go, Java, C++).

/// ABI type reference: a reference to a type in the Component IR.
///
/// Types can be primitives, struct references, pointers, slices,
/// opaque handles, fat pointers, or void.
#[derive(Debug, Clone, PartialEq)]
pub enum AbiTypeRef {
    /// Primitive scalar type.
    Primitive(AbiPrimitive),
    /// Reference to a named struct/enum/alias definition.
    Named(String),
    /// Thin pointer to another type (*mut T).
    Pointer(Box<AbiTypeRef>),
    /// Fat pointer slice: { data: *mut T, len: usize }.
    Slice(Box<AbiTypeRef>),
    /// Opaque handle (generational, kind-tagged).
    Opaque(String),
    /// 0.31.31: Fat pointer with explicit layout.
    /// Used for String ({ data, len, capacity }) and buffer types.
    FatPointer {
        /// Element type.
        element: Box<AbiTypeRef>,
        /// Whether this includes a capacity field (String) or not (slice).
        has_capacity: bool,
    },
    /// Void (no value).
    Void,
}

impl AbiTypeRef {
    /// True if this is a primitive type.
    pub fn is_primitive(&self) -> bool {
        matches!(self, AbiTypeRef::Primitive(_))
    }

    /// True if this is a pointer type.
    pub fn is_pointer(&self) -> bool {
        matches!(self, AbiTypeRef::Pointer(_))
    }

    /// True if this is void.
    pub fn is_void(&self) -> bool {
        matches!(self, AbiTypeRef::Void)
    }

    /// C type name for this reference.
    pub fn c_type_name(&self) -> String {
        match self {
            AbiTypeRef::Primitive(p) => p.c_name().to_string(),
            AbiTypeRef::Named(name) => name.clone(),
            AbiTypeRef::Pointer(inner) => format!("{}*", inner.c_type_name()),
            AbiTypeRef::Slice(inner) => format!("MimiSlice/* {} */", inner.c_type_name()),
            AbiTypeRef::Opaque(name) => format!("MimiHandle/* {} */", name),
            AbiTypeRef::FatPointer {
                element,
                has_capacity,
            } => {
                if *has_capacity {
                    format!("MimiString/* {} */", element.c_type_name())
                } else {
                    format!("MimiSlice/* {} */", element.c_type_name())
                }
            }
            AbiTypeRef::Void => "void".to_string(),
        }
    }
}

/// Primitive ABI types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiPrimitive {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Bool,
    /// Pointer-sized signed integer (isize).
    IntPtr,
    /// Pointer-sized unsigned integer (usize).
    UIntPtr,
}

impl AbiPrimitive {
    /// C type name.
    pub fn c_name(&self) -> &'static str {
        match self {
            AbiPrimitive::I8 => "int8_t",
            AbiPrimitive::I16 => "int16_t",
            AbiPrimitive::I32 => "int32_t",
            AbiPrimitive::I64 => "int64_t",
            AbiPrimitive::U8 => "uint8_t",
            AbiPrimitive::U16 => "uint16_t",
            AbiPrimitive::U32 => "uint32_t",
            AbiPrimitive::U64 => "uint64_t",
            AbiPrimitive::F32 => "float",
            AbiPrimitive::F64 => "double",
            AbiPrimitive::Bool => "bool",
            AbiPrimitive::IntPtr => "intptr_t",
            AbiPrimitive::UIntPtr => "uintptr_t",
        }
    }

    /// Size in bytes (platform-independent, assuming 64-bit).
    pub fn size_bytes(&self) -> usize {
        match self {
            AbiPrimitive::I8 | AbiPrimitive::U8 | AbiPrimitive::Bool => 1,
            AbiPrimitive::I16 | AbiPrimitive::U16 => 2,
            AbiPrimitive::I32 | AbiPrimitive::U32 | AbiPrimitive::F32 => 4,
            AbiPrimitive::I64
            | AbiPrimitive::U64
            | AbiPrimitive::F64
            | AbiPrimitive::IntPtr
            | AbiPrimitive::UIntPtr => 8,
        }
    }

    /// Parse from a Mimi surface type name.
    pub fn from_mimi_type(name: &str) -> Option<Self> {
        match name {
            "i8" => Some(AbiPrimitive::I8),
            "i16" => Some(AbiPrimitive::I16),
            "i32" | "int" => Some(AbiPrimitive::I32),
            "i64" => Some(AbiPrimitive::I64),
            "u8" => Some(AbiPrimitive::U8),
            "u16" => Some(AbiPrimitive::U16),
            "u32" => Some(AbiPrimitive::U32),
            "u64" => Some(AbiPrimitive::U64),
            "f32" | "float" => Some(AbiPrimitive::F32),
            "f64" => Some(AbiPrimitive::F64),
            "bool" | "Bool" => Some(AbiPrimitive::Bool),
            "isize" => Some(AbiPrimitive::IntPtr),
            "usize" => Some(AbiPrimitive::UIntPtr),
            _ => None,
        }
    }
}

/// ABI type definition: a named type in the Component IR.
#[derive(Debug, Clone)]
pub enum AbiTypeDef {
    /// repr(C) struct with explicit field layout.
    Struct(AbiStruct),
    /// C-style enum with explicit discriminants.
    Enum(AbiEnum),
    /// Type alias.
    Alias(AbiAlias),
    /// Opaque handle type (no visible layout).
    Opaque(AbiOpaque),
}

impl AbiTypeDef {
    /// The name of this type definition.
    pub fn name(&self) -> &str {
        match self {
            AbiTypeDef::Struct(s) => &s.name,
            AbiTypeDef::Enum(e) => &e.name,
            AbiTypeDef::Alias(a) => &a.name,
            AbiTypeDef::Opaque(o) => &o.name,
        }
    }
}

/// repr(C) struct definition.
#[derive(Debug, Clone)]
pub struct AbiStruct {
    /// Struct name (e.g., "MimiList").
    pub name: String,
    /// Fields in declaration order.
    pub fields: Vec<AbiField>,
    /// Whether this is repr(C) (always true for ABI structs).
    pub is_repr_c: bool,
    /// Computed size in bytes (if known).
    pub size: Option<usize>,
    /// Computed alignment in bytes (if known).
    pub align: Option<usize>,
}

/// A field in an ABI struct.
#[derive(Debug, Clone)]
pub struct AbiField {
    /// Field name.
    pub name: String,
    /// Field type.
    pub ty: AbiTypeRef,
    /// Byte offset from struct start (if computed).
    pub offset: Option<usize>,
}

/// C-style enum definition.
#[derive(Debug, Clone)]
pub struct AbiEnum {
    /// Enum name.
    pub name: String,
    /// Variants with explicit discriminants.
    pub variants: Vec<(String, i64)>,
    /// Underlying integer type.
    pub repr: AbiPrimitive,
}

/// Type alias definition.
#[derive(Debug, Clone)]
pub struct AbiAlias {
    /// Alias name.
    pub name: String,
    /// Target type.
    pub target: AbiTypeRef,
}

/// Opaque handle type (no visible layout, generational).
#[derive(Debug, Clone)]
pub struct AbiOpaque {
    /// Handle type name (e.g., "ListHandle", "MapHandle").
    pub name: String,
    /// Human-readable description of what this handle points to.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_sizes() {
        assert_eq!(AbiPrimitive::I8.size_bytes(), 1);
        assert_eq!(AbiPrimitive::I32.size_bytes(), 4);
        assert_eq!(AbiPrimitive::I64.size_bytes(), 8);
        assert_eq!(AbiPrimitive::F64.size_bytes(), 8);
        assert_eq!(AbiPrimitive::IntPtr.size_bytes(), 8);
    }

    #[test]
    fn primitive_c_names() {
        assert_eq!(AbiPrimitive::I32.c_name(), "int32_t");
        assert_eq!(AbiPrimitive::F64.c_name(), "double");
        assert_eq!(AbiPrimitive::Bool.c_name(), "bool");
    }

    #[test]
    fn type_ref_c_names() {
        assert_eq!(
            AbiTypeRef::Primitive(AbiPrimitive::I32).c_type_name(),
            "int32_t"
        );
        assert_eq!(
            AbiTypeRef::Pointer(Box::new(AbiTypeRef::Primitive(AbiPrimitive::U8))).c_type_name(),
            "uint8_t*"
        );
        assert_eq!(AbiTypeRef::Void.c_type_name(), "void");
    }

    #[test]
    fn mimi_type_parsing() {
        assert_eq!(AbiPrimitive::from_mimi_type("i32"), Some(AbiPrimitive::I32));
        assert_eq!(AbiPrimitive::from_mimi_type("f64"), Some(AbiPrimitive::F64));
        assert_eq!(
            AbiPrimitive::from_mimi_type("bool"),
            Some(AbiPrimitive::Bool)
        );
        assert_eq!(AbiPrimitive::from_mimi_type("string"), None);
    }
}
