//! Canonical, checker-owned semantic IR.
//!
//! This module is intentionally independent from executable consumers.  The
//! checker constructs these artifacts once; interpreter, native codegen and
//! verifier consume stable identities rather than re-resolving surface AST.

mod types;

pub use types::{
    FunctionTypeAbi, NominalTypeId, OwnershipTypeKind, PrimitiveType, ResolvedType,
    ResolvedTypeCapabilities, ResolvedTypeError, ResolvedTypeId, ResolvedTypeName,
    ResolvedTypeTable, TraitTypeKind, RESOLVED_TYPE_SCHEMA_VERSION,
};
