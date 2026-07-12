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
                let global = self
                    .builder
                    .build_global_string_ptr(s, "str")
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
        let _i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        if parts.is_empty() {
            let global = self
                .builder
                .build_global_string_ptr("", "fstr_empty")
                .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
            let ptr = global.as_pointer_value();
            let len = self.context.i64_type().const_int(0, false);
            return self.build_string_struct(ptr, len);
        }

        // Optimization: if all parts are text, return a single global string
        let all_text: Option<String> = parts
            .iter()
            .map(|p| match p {
                crate::ast::FStringPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        if let Some(text) = all_text {
            let global = self
                .builder
                .build_global_string_ptr(&text, "fstr_literal")
                .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
            let ptr = global.as_pointer_value();
            let len = self.context.i64_type().const_int(text.len() as u64, false);
            return self.build_string_struct(ptr, len);
        }

        // For f-strings with interpolation: dynamically compute buffer size, then fill
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let strcpy_fn = self
            .module
            .get_function("strcpy")
            .ok_or_else(|| "strcpy not declared".to_string())?;
        let strcat_fn = self
            .module
            .get_function("strcat")
            .ok_or_else(|| "strcat not declared".to_string())?;
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let sprintf_fn = self
            .module
            .get_function("sprintf")
            .ok_or_else(|| "sprintf not declared".to_string())?;

        // Phase 1: Compile each part and compute total buffer size at runtime
        enum CompiledPart<'ctx> {
            Text(String),
            InterpStr(BasicValueEnum<'ctx>),
        }
        let mut compiled_parts: Vec<CompiledPart<'ctx>> = Vec::new();
        let mut total_size = i64_ty.const_int(1, false);
        for (i, part) in parts.iter().enumerate() {
            match part {
                crate::ast::FStringPart::Text(t) => {
                    total_size = self
                        .builder
                        .build_int_add(
                            total_size,
                            i64_ty.const_int(t.len() as u64 + 1, false),
                            &format!("fstr_text_sz_{}", i),
                        )
                        .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                    compiled_parts.push(CompiledPart::Text(t.clone()));
                }
                crate::ast::FStringPart::Interp(expr) => {
                    let val = self.compile_expr(expr, vars)?;
                    match val {
                        BasicValueEnum::IntValue(iv) => {
                            let ext_iv = if iv.get_type().get_bit_width() < 64 {
                                self.builder
                                    .build_int_z_extend(
                                        iv,
                                        self.context.i64_type(),
                                        &format!("fstr_ext_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("zext error: {}", e))
                                    })?
                            } else {
                                iv
                            };
                            let temp_buf = self
                                .build_call(
                                    malloc_fn,
                                    &[BasicMetadataValueEnum::IntValue(
                                        i64_ty.const_int(32, false),
                                    )],
                                    &format!("fstr_temp_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("malloc returned void")?
                                .into_pointer_value();
                            self.register_heap_alloc(temp_buf);
                            let fmt = self
                                .builder
                                .build_global_string_ptr("%ld", &format!("fstr_fmt_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("string error: {}", e))
                                })?;
                            self.build_call(
                                sprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(temp_buf),
                                    BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                    BasicMetadataValueEnum::IntValue(ext_iv),
                                ],
                                &format!("fstr_sprintf_{}", i),
                            )?;
                            let len = self
                                .build_call(
                                    strlen_fn,
                                    &[BasicMetadataValueEnum::PointerValue(temp_buf)],
                                    &format!("fstr_strlen_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts.push(CompiledPart::InterpStr(temp_buf.into()));
                        }
                        BasicValueEnum::FloatValue(fv) => {
                            // MEM-C1 (deep audit): %f can produce up to 317 chars for extreme
                            // float values (e.g. DBL_MAX). Use 512-byte buffer to be safe.
                            let temp_buf = self
                                .build_call(
                                    malloc_fn,
                                    &[BasicMetadataValueEnum::IntValue(
                                        i64_ty.const_int(512, false),
                                    )],
                                    &format!("fstr_temp_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("malloc returned void")?
                                .into_pointer_value();
                            self.register_heap_alloc(temp_buf);
                            let fmt = self
                                .builder
                                .build_global_string_ptr("%f", &format!("fstr_fmt_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("string error: {}", e))
                                })?;
                            self.build_call(
                                sprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(temp_buf),
                                    BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                    BasicMetadataValueEnum::FloatValue(fv),
                                ],
                                &format!("fstr_sprintf_{}", i),
                            )?;
                            let len = self
                                .build_call(
                                    strlen_fn,
                                    &[BasicMetadataValueEnum::PointerValue(temp_buf)],
                                    &format!("fstr_strlen_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts.push(CompiledPart::InterpStr(temp_buf.into()));
                        }
                        BasicValueEnum::PointerValue(pv) => {
                            let len = self
                                .build_call(
                                    strlen_fn,
                                    &[BasicMetadataValueEnum::PointerValue(pv)],
                                    &format!("fstr_strlen_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts.push(CompiledPart::InterpStr(val));
                        }
                        BasicValueEnum::StructValue(sv) => {
                            // String struct {i8*, i64} — extract data pointer for strcat
                            let data_ptr = self
                                .build_extract_value(sv.into(), 0, "fstr_str_data")?
                                .into_pointer_value();
                            let len = self
                                .build_extract_value(sv.into(), 1, "fstr_str_len")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts.push(CompiledPart::InterpStr(data_ptr.into()));
                        }
                        _ => {
                            let unknown = self
                                .builder
                                .build_global_string_ptr(
                                    "<unsupported>",
                                    &format!("fstr_unsup_{}", i),
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("string error: {}", e))
                                })?;
                            let len = self
                                .build_call(
                                    strlen_fn,
                                    &[BasicMetadataValueEnum::PointerValue(
                                        unknown.as_pointer_value(),
                                    )],
                                    &format!("fstr_strlen_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts
                                .push(CompiledPart::InterpStr(unknown.as_pointer_value().into()));
                        }
                    }
                }
            }
        }

        // Phase 2: Allocate correctly-sized buffer and fill
        let buf = self
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(total_size)],
                "fstr_buf",
            )?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        self.register_heap_alloc(buf);

        let empty = self
            .builder
            .build_global_string_ptr("", "fstr_empty_init")
            .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(empty.as_pointer_value()),
            ],
            "fstr_init",
        )?;

        for (i, part) in compiled_parts.iter().enumerate() {
            match part {
                CompiledPart::Text(t) => {
                    if t.is_empty() {
                        continue;
                    }
                    let global = self
                        .builder
                        .build_global_string_ptr(t, &format!("fstr_part_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
                    self.build_call(
                        strcat_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(global.as_pointer_value()),
                        ],
                        &format!("fstr_cat_{}", i),
                    )?;
                }
                CompiledPart::InterpStr(pv) => {
                    let ptr = match pv {
                        BasicValueEnum::PointerValue(p) => *p,
                        _ => {
                            return Err(CompileError::LlvmError(
                                "f-string interp: expected pointer".to_string(),
                            ))
                        }
                    };
                    self.build_call(
                        strcat_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(ptr),
                        ],
                        &format!("fstr_cat_{}", i),
                    )?;
                }
            }
        }

        // Phase 3: Wrap heap-allocated buffer into canonical {i8*, i64} struct
        let len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(buf)],
                "fstr_len",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
            .into_int_value();
        self.build_string_struct(buf, len)
    }
}
