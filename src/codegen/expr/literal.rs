use crate::ast::*;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {

    pub(in crate::codegen) fn compile_literal_expr(
        &mut self,
        lit: &Lit,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match lit {
            Lit::Int(n) => Ok(self.context.i64_type().const_int(*n as u64, true).into()),
            Lit::Float(f) => Ok(self.context.f64_type().const_float(*f).into()),
            Lit::Bool(b) => Ok(self.context.bool_type().const_int(*b as u64, false).into()),
            Lit::Unit => Ok(self.context.i64_type().const_int(0, false).into()),
            Lit::String(s) => {
                let global = self.builder.build_global_string_ptr(s, "str")
                    .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
                Ok(global.as_pointer_value().into())
            }
            Lit::FString(parts) => Ok(self.compile_fstring(parts, vars)?),
        }
    }


    pub(in crate::codegen) fn compile_fstring(
        &mut self,
        parts: &[crate::ast::FStringPart],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let _i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        if parts.is_empty() {
            let global = self.builder.build_global_string_ptr("", "fstr_empty")
                .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
            return Ok(global.as_pointer_value().into());
        }

        // Optimization: if all parts are text, return a single global string
        let all_text: Option<String> = parts.iter().map(|p| {
            match p {
                crate::ast::FStringPart::Text(t) => Some(t.as_str()),
                _ => None,
            }
        }).collect();
        if let Some(text) = all_text {
            let global = self.builder.build_global_string_ptr(&text, "fstr_literal")
                .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
            return Ok(global.as_pointer_value().into());
        }

        // For f-strings with interpolation: use malloc + strcpy + strcat
        let malloc_fn = self.module.get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let strcpy_fn = self.module.get_function("strcpy")
            .ok_or_else(|| "strcpy not declared".to_string())?;
        let strcat_fn = self.module.get_function("strcat")
            .ok_or_else(|| "strcat not declared".to_string())?;
        let strlen_fn = self.module.get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let sprintf_fn = self.module.get_function("sprintf")
            .ok_or_else(|| "sprintf not declared".to_string())?;

        // Allocate a 1024-byte buffer for the result
        let buf_size = i64_ty.const_int(1024, false);
        let buf = self.builder.build_call(malloc_fn, &[
            BasicMetadataValueEnum::IntValue(buf_size),
        ], "fstr_buf")
            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        self.register_heap_alloc(buf);

        // Initialize buffer with empty string
        let empty = self.builder.build_global_string_ptr("", "fstr_empty_init")
            .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
        self.builder.build_call(strcpy_fn, &[
            BasicMetadataValueEnum::PointerValue(buf),
            BasicMetadataValueEnum::PointerValue(empty.as_pointer_value()),
        ], "fstr_init")
            .map_err(|e| CompileError::LlvmError(format!("strcpy error: {}", e)))?;

        // Append each part
        for (i, part) in parts.iter().enumerate() {
            match part {
                crate::ast::FStringPart::Text(t) => {
                    if t.is_empty() { continue; }
                    let global = self.builder.build_global_string_ptr(t, &format!("fstr_part_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
                    self.builder.build_call(strcat_fn, &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::PointerValue(global.as_pointer_value()),
                    ], &format!("fstr_cat_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("strcat error: {}", e)))?;
                }
                crate::ast::FStringPart::Interp(expr) => {
                    let val = self.compile_expr(expr, vars)?;
                    // Convert value to string based on type
                    match val {
                        BasicValueEnum::IntValue(iv) => {
                            let len = self.builder.build_call(strlen_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                            ], "fstr_strlen")
                                .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            let i8_type = self.context.i8_type();
                                                        let pos = { self.gep().build_gep(i8_type, buf, &[len], "fstr_pos") }
                                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                            let ext_iv = if iv.get_type().get_bit_width() < 64 {
                                self.builder.build_int_z_extend(iv, self.context.i64_type(), &format!("fstr_ext_{}", i))
                                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
                            } else { iv };
                            let fmt = self.builder.build_global_string_ptr("%ld", &format!("fstr_fmt_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
                            self.builder.build_call(sprintf_fn, &[
                                BasicMetadataValueEnum::PointerValue(pos),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(ext_iv),
                            ], &format!("fstr_sprintf_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("sprintf error: {}", e)))?;
                        }
                        BasicValueEnum::FloatValue(fv) => {
                            let len = self.builder.build_call(strlen_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                            ], "fstr_strlen")
                                .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            let i8_type = self.context.i8_type();
                                                        let pos = { self.gep().build_gep(i8_type, buf, &[len], "fstr_pos") }
                                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                            let fmt = self.builder.build_global_string_ptr("%f", &format!("fstr_fmt_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
                            self.builder.build_call(sprintf_fn, &[
                                BasicMetadataValueEnum::PointerValue(pos),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::FloatValue(fv),
                            ], &format!("fstr_sprintf_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("sprintf error: {}", e)))?;
                        }
                        BasicValueEnum::PointerValue(pv) => {
                            // String pointer: use strcat
                            self.builder.build_call(strcat_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::PointerValue(pv),
                            ], &format!("fstr_cat_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("strcat error: {}", e)))?;
                        }
                        _ => {
                            let unknown = self.builder.build_global_string_ptr("<unsupported>", &format!("fstr_unsup_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
                            self.builder.build_call(strcat_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::PointerValue(unknown.as_pointer_value()),
                            ], &format!("fstr_cat_unsup_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("strcat error: {}", e)))?;
                        }
                    }
                }
            }
        }

        Ok(buf.into())
    }

}
