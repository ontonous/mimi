//! FFI contract: describes how a single extern function signature crosses the
//! Mimi <-> C boundary.
//!
//! A contract is derived from the AST `ExternFunc` declaration.  It is consumed
//! by both the interpreter and the codegen backend so that the two FFI paths
//! behave identically: argument marshalling, lifetime extension, and return
//! value translation are driven by the same description.

use crate::ast::{ExternFunc, Type};

/// Contract for one extern function.
#[derive(Debug, Clone)]
pub struct FfiContract {
    /// One contract per parameter, in declaration order.
    pub args: Vec<FfiArgContract>,
    /// Contract for the return value.
    pub ret: FfiRetContract,
}

/// How a single argument is translated from a Mimi value to a C ABI argument.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FfiArgContract {
    /// Scalar integer or boolean passed by value.
    Int,
    /// 64-bit floating point passed by value.
    Float,
    /// A Mimi `string` passed as a temporary borrowed `char*`.
    StringBorrow,
    /// A Mimi `string` whose ownership is transferred to C (C must free).
    StringTransfer,
    /// A linear capability passed as an opaque handle.
    Cap,
    /// Raw immutable pointer `*T`.
    RawPtr(Box<Type>),
    /// Raw mutable pointer `*mut T`.
    RawPtrMut(Box<Type>),
    /// Shared ownership boundary handle `c_shared T`.
    CShared(Box<Type>),
    /// Borrowed read-only boundary handle `c_borrow T`.
    CBorrow(Box<Type>),
    /// Borrowed mutable boundary handle `c_borrow_mut T`.
    CBorrowMut(Box<Type>),
    /// A type that the type checker already rejects but which may still appear
    /// in runtime-only paths (e.g. tests that bypass `core::check`).  The
    /// wrapper reports this as an FFI safety error at call time.
    Unsupported(String),
}

/// How the return value is translated from the C ABI back to a Mimi value.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FfiRetContract {
    /// No return value (`unit`).
    Unit,
    /// Scalar integer or boolean.
    Int,
    /// 64-bit floating point.
    Float,
    /// A null-terminated C string whose ownership is transferred to Mimi.
    String,
    /// Raw immutable pointer `*T`.
    RawPtr(Box<Type>),
    /// Raw mutable pointer `*mut T`.
    RawPtrMut(Box<Type>),
    /// Shared ownership boundary handle `c_shared T`.
    CShared(Box<Type>),
    /// Borrowed read-only boundary handle `c_borrow T`.
    CBorrow(Box<Type>),
    /// Borrowed mutable boundary handle `c_borrow_mut T`.
    CBorrowMut(Box<Type>),
    /// A return type that the type checker rejects but which may be reached at
    /// runtime in tests that bypass `core::check`.
    Unsupported(String),
}

impl FfiContract {
    /// Build a contract from an extern function declaration.
    ///
    /// The caller is responsible for ensuring the declaration has already been
    /// validated by the type checker (`is_valid_extern_type`).  This function
    /// panics on unexpected types so that contract bugs surface early.
    pub fn from_extern(func: &ExternFunc) -> Self {
        let args = func
            .params
            .iter()
            .map(|p| FfiArgContract::from_type(&p.ty))
            .collect();
        let ret = func
            .ret
            .as_ref()
            .map(FfiRetContract::from_type)
            .unwrap_or(FfiRetContract::Unit);
        Self { args, ret }
    }
}

impl FfiArgContract {
    fn from_type(ty: &Type) -> Self {
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" | "i64" | "bool" => FfiArgContract::Int,
                "f64" => FfiArgContract::Float,
                "string" => FfiArgContract::StringBorrow,
                "unit" => FfiArgContract::Int,
                other => FfiArgContract::Unsupported(other.to_string()),
            },
            Type::Cap(_) => FfiArgContract::Cap,
            Type::RawPtr(inner) => FfiArgContract::RawPtr(inner.clone()),
            Type::RawPtrMut(inner) => FfiArgContract::RawPtrMut(inner.clone()),
            Type::CShared(inner) => FfiArgContract::CShared(inner.clone()),
            Type::CBorrow(inner) => FfiArgContract::CBorrow(inner.clone()),
            Type::CBorrowMut(inner) => FfiArgContract::CBorrowMut(inner.clone()),
            Type::RawString => FfiArgContract::StringTransfer,
            other => FfiArgContract::Unsupported(format!("{:?}", other)),
        }
    }
}

impl FfiRetContract {
    fn from_type(ty: &Type) -> Self {
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" | "i64" | "bool" => FfiRetContract::Int,
                "f64" => FfiRetContract::Float,
                "string" => FfiRetContract::String,
                "unit" => FfiRetContract::Unit,
                other => FfiRetContract::Unsupported(other.to_string()),
            },
            Type::RawPtr(inner) => FfiRetContract::RawPtr(inner.clone()),
            Type::RawPtrMut(inner) => FfiRetContract::RawPtrMut(inner.clone()),
            Type::CShared(inner) => FfiRetContract::CShared(inner.clone()),
            Type::CBorrow(inner) => FfiRetContract::CBorrow(inner.clone()),
            Type::CBorrowMut(inner) => FfiRetContract::CBorrowMut(inner.clone()),
            Type::RawString => FfiRetContract::String,
            other => FfiRetContract::Unsupported(format!("{:?}", other)),
        }
    }
}
