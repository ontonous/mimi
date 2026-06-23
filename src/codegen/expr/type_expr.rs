use crate::ast::*;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {

    pub(in crate::codegen) fn compile_typeof_expr(
        &mut self,
        inner: &Box<Expr>,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // type_name(x): resolve type name at compile time
        let type_str = match inner.as_ref() {
            Expr::Ident(var_name) => self.var_type_names.get(var_name)
                .cloned().unwrap_or_else(|| "unknown".to_string()),
            _ => "unknown".to_string(),
        };
        // Build string literal struct { i8*, i64 }
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let global = self.builder.build_global_string_ptr(&type_str, "typename")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let string_ty = self.context.struct_type(&[
            BasicTypeEnum::PointerType(i8_ptr),
            BasicTypeEnum::IntType(i64_ty),
        ], false);
        let alloca = self.builder.build_alloca(string_ty, "type_str")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let ptr_gep = self.gep().build_struct_gep(string_ty, alloca, 0, "ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(ptr_gep, global.as_pointer_value())
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let len_gep = self.gep().build_struct_gep(string_ty, alloca, 1, "len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(len_gep, i64_ty.const_int(type_str.len() as u64, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(alloca.into())
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
