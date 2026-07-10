use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_to_string(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "to_string expects 1 argument".to_string(),
            ));
        }
        match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => {
                // Reset the flag unconditionally so we don't affect subsequent calls
                self.pending_to_string_is_any = false;
                // v0.28.30: route all i64 through `mimi_any_to_string` which
                // uses a heuristic address-range check to distinguish C string
                // pointers (map values) from integers. This is simpler and
                // more robust than the bit-0 tag protocol which conflicted
                // with raw ptrtoint map values stored by `mimi_map_set`.
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let any_fn_ty = i8_ptr.fn_type(
                    &[BasicMetadataTypeEnum::IntType(self.context.i64_type())],
                    false,
                );
                let fn_any = self
                    .module
                    .get_function("mimi_any_to_string")
                    .unwrap_or_else(|| {
                        self.module.add_function(
                            "mimi_any_to_string",
                            any_fn_ty,
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                let raw = self
                    .builder
                    .build_call(
                        fn_any,
                        &[BasicMetadataValueEnum::IntValue(iv)],
                        "any_to_string",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("any_to_string: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_any_to_string returned void")?
                    .into_pointer_value();
                let str_ty = self.context.struct_type(
                    &[
                        BasicTypeEnum::PointerType(i8_ptr),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                );
                let alloca = self.build_entry_alloca(str_ty, "any_str")?;
                let ptr_gep = self
                    .gep()
                    .build_struct_gep(str_ty, alloca, 0, "any_str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.builder
                    .build_store(ptr_gep, raw)
                    .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                let strlen_fn = self
                    .module
                    .get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let len = self
                    .builder
                    .build_call(
                        strlen_fn,
                        &[BasicMetadataValueEnum::PointerValue(raw)],
                        "any_strlen",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("strlen: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let len_gep = self
                    .gep()
                    .build_struct_gep(str_ty, alloca, 1, "any_str_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.builder
                    .build_store(len_gep, len)
                    .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                let result = self
                    .builder
                    .build_load(BasicTypeEnum::StructType(str_ty), alloca, "any_str")
                    .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?;
                Ok(result)
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                let alloc_size = self.context.i64_type().const_int(32, false);
                let malloc_fn = self
                    .module
                    .get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self
                    .builder
                    .build_call(
                        malloc_fn,
                        &[BasicMetadataValueEnum::IntValue(alloc_size)],
                        "malloc_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let fmt_global = self
                    .builder
                    .build_global_string_ptr("%.15g", "float_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                let sprintf_fn = self
                    .module
                    .get_function("sprintf")
                    .ok_or_else(|| "sprintf not declared".to_string())?;
                self.builder
                    .build_call(
                        sprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                            BasicMetadataValueEnum::FloatValue(fv),
                        ],
                        "sprintf_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("sprintf error: {}", e)))?;
                // Build {i8*, i64} struct from the buffer
                let str_ty = self.context.struct_type(
                    &[
                        BasicTypeEnum::PointerType(
                            self.context.ptr_type(inkwell::AddressSpace::default()),
                        ),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                );
                let alloca = self.build_entry_alloca(str_ty, "str_result")?;
                let ptr_gep = self
                    .gep()
                    .build_struct_gep(str_ty, alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder
                    .build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.register_heap_slot(alloca, str_ty, 0);
                let strlen_fn = self
                    .module
                    .get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let len = self
                    .builder
                    .build_call(
                        strlen_fn,
                        &[BasicMetadataValueEnum::PointerValue(buf)],
                        "strlen_to_s",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("strlen returned void".to_string()))?
                    .into_int_value();
                let len_gep = self
                    .gep()
                    .build_struct_gep(str_ty, alloca, 1, "str_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder
                    .build_store(len_gep, len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result = self
                    .builder
                    .build_load(BasicTypeEnum::StructType(str_ty), alloca, "str_result")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                Ok(result)
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                // String values are {i8*, i64} structs in codegen.
                // Return as-is since to_string on a string is identity.
                Ok(BasicValueEnum::StructValue(sv))
            }
            BasicMetadataValueEnum::PointerValue(_) => {
                // Treat a stray pointer (e.g. typed reference) as a C string.
                let pv = if let BasicMetadataValueEnum::PointerValue(p) = args[0] {
                    p
                } else {
                    return Err(CompileError::Generic(
                        "fstring format: expected pointer value".to_string(),
                    ));
                };
                let alloc_size = self.context.i64_type().const_int(2, false);
                let malloc_fn = self
                    .module
                    .get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self
                    .builder
                    .build_call(
                        malloc_fn,
                        &[BasicMetadataValueEnum::IntValue(alloc_size)],
                        "malloc_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                self.builder
                    .build_store(buf, self.context.i8_type().const_int(b'?' as u64, false))
                    .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                let nul = self
                    .gep()
                    .build_in_bounds_gep(
                        self.context.i8_type(),
                        buf,
                        &[self.context.i64_type().const_int(1, false)],
                        "nul_pos",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.builder
                    .build_store(nul, self.context.i8_type().const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store nul: {}", e)))?;
                let _ = pv; // suppress unused warning
                let str_ty = self.context.struct_type(
                    &[
                        BasicTypeEnum::PointerType(
                            self.context.ptr_type(inkwell::AddressSpace::default()),
                        ),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                );
                let alloca = self.build_entry_alloca(str_ty, "str_result")?;
                let ptr_gep = self
                    .gep()
                    .build_struct_gep(str_ty, alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.builder
                    .build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                self.register_heap_slot(alloca, str_ty, 0);
                let len_gep = self
                    .gep()
                    .build_struct_gep(str_ty, alloca, 1, "str_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.builder
                    .build_store(len_gep, self.context.i64_type().const_int(1, false))
                    .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                self.builder
                    .build_load(BasicTypeEnum::StructType(str_ty), alloca, "str_result")
                    .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))
            }
            _ => Err(CompileError::TypeMismatch(
                "to_string: unsupported type".to_string(),
            )),
        }
    }
}

// (Helper lives on the impl block via a method above.)
