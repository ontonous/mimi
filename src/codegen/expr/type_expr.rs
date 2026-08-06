use crate::ast::*;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::values::BasicValueEnum;
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_typeof_expr(
        &mut self,
        inner: &Expr,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // type_name(x): resolve type name at compile time
        // 2026-08-06 (audit 1l): the parser rewrites `type_name(x)` into
        // Expr::TypeOf(inner) where inner is a *Located* expression (span
        // wrapper). The old `Expr::Ident(var_name)` match missed the wrapper
        // and always produced "unknown" — so `type_name(s)` on any variable
        // compiled to the "unknown" global string while the VM printed the
        // real type. Unwrap before matching.
        let type_str = match inner.unlocated() {
            Expr::Ident(var_name) => self
                .var_type_names
                .get(var_name)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            _ => "unknown".to_string(),
        };
        // Build string literal struct { i8*, i64 }
        let global = self
            .builder
            .build_global_string_ptr(&type_str, "typename")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        // 2026-08-06 (audit 1l): the old implementation returned the address
        // of a stack alloca holding {ptr, len} — a third, unsupported string
        // shape. Single-arg `println` takes the PointerValue fast path and
        // `puts`'d the alloca address as a C string, printing struct bytes
        // (garbage like "�G "). Return the canonical struct VALUE
        // ({i8*, i64} via wrap_c_string), matching str_char_at/substring etc.
        self.wrap_c_string(global.as_pointer_value())
    }

    pub(in crate::codegen) fn compile_typeinfo_expr(
        &mut self,
        ty: &Type,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // type_info(T): compile-time reflection on type (future)
        let _ = ty;
        Err("type_info is not available in codegen mode (compile-time reflection only)".into())
    }
}
