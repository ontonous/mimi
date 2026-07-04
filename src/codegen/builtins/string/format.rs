use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_to_string(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "to_string expects 1 argument".to_string(),
            ));
        }
        match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => {
                let alloc_size = self.context.i64_type().const_int(21, false);
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
                    .build_global_string_ptr("%ld", "int_fmt")
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
                            BasicMetadataValueEnum::IntValue(iv),
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
                    unreachable!()
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
                let nul = unsafe {
                    self.builder.build_in_bounds_gep(
                        self.context.i8_type(),
                        buf,
                        &[self.context.i64_type().const_int(1, false)],
                        "nul_pos",
                    )
                }
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

    /// Forward a generic struct value to the runtime's `mimi_list_to_string`
    /// helper, which understands the canonical `{i64 len, i8* data}`
    /// list layout. Returns a `{i8*, i64}` string struct that downstream
    /// `print` / `println` calls can consume.
    #[allow(dead_code)] // kept for future List-to-string support
    fn call_list_to_string_runtime(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // Materialize the struct into an alloca so we can pass a stable
        // pointer to the runtime helper.
        let list_struct_ty = self.list_struct_type();
        let alloca = self
            .builder
            .build_alloca(list_struct_ty, "vts_list_alloca")
            .map_err(|e| CompileError::LlvmError(format!("vts alloca: {}", e)))?;
        self.builder
            .build_store(alloca, sv)
            .map_err(|e| CompileError::LlvmError(format!("vts store: {}", e)))?;
        // Declare `mimi_list_to_string(*const MimiList) -> *mut c_char`
        // if not already in the module.
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = i8_ptr_ty.fn_type(
            &[inkwell::types::BasicMetadataTypeEnum::PointerType(
                i8_ptr_ty,
            )],
            false,
        );
        let callee = self
            .module
            .get_function("mimi_list_to_string")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_list_to_string",
                    fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let raw = self
            .build_call(
                callee,
                &[BasicMetadataValueEnum::PointerValue(alloca)],
                "vts_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("vts call: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_list_to_string returned void")?
            .into_pointer_value();
        // Wrap the raw C string into the canonical `{i8*, i64}` struct.
        let str_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(raw)],
                "vts_strlen",
            )
            .map_err(|e| CompileError::LlvmError(format!("vts strlen: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        let res_alloca = self.build_entry_alloca(str_ty, "vts_str_alloca")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(str_ty, res_alloca, 0, "vts_str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("vts str gep: {}", e)))?;
        self.builder
            .build_store(ptr_gep, raw)
            .map_err(|e| CompileError::LlvmError(format!("vts str store: {}", e)))?;
        self.register_heap_slot(res_alloca, str_ty, 0);
        let len_gep = self
            .gep()
            .build_struct_gep(str_ty, res_alloca, 1, "vts_str_len")
            .map_err(|e| CompileError::LlvmError(format!("vts str len gep: {}", e)))?;
        self.builder
            .build_store(len_gep, len)
            .map_err(|e| CompileError::LlvmError(format!("vts str len store: {}", e)))?;
        self.builder
            .build_load(BasicTypeEnum::StructType(str_ty), res_alloca, "vts_str")
            .map_err(|e| CompileError::LlvmError(format!("vts str load: {}", e)))
    }

    /// Allocate a heap string with the given contents and return it as a
    /// `{i8*, i64}` struct value, mirroring the layout used by `to_string`.
    #[allow(dead_code)] // kept for future generic string-literal lowering
    fn build_string_literal(&self, s: &str) -> MimiResult<BasicValueEnum<'ctx>> {
        // Allocate enough room (len + 1 for the NUL terminator) and use
        // sprintf("%s", …) to copy the literal into the heap buffer.
        let len_with_nul = self.context.i64_type().const_int(s.len() as u64 + 1, false);
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let buf = self
            .builder
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(len_with_nul)],
                "malloc_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("malloc: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        let fmt_global = self
            .builder
            .build_global_string_ptr("%s", "str_fmt")
            .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
        let sprintf_fn = self
            .module
            .get_function("sprintf")
            .ok_or_else(|| "sprintf not declared".to_string())?;
        let src_global = self
            .builder
            .build_global_string_ptr(s, "str_src")
            .map_err(|e| CompileError::LlvmError(format!("src: {}", e)))?;
        self.builder
            .build_call(
                sprintf_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                    BasicMetadataValueEnum::PointerValue(src_global.as_pointer_value()),
                ],
                "sprintf_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("sprintf: {}", e)))?;
        let len = self.context.i64_type().const_int(s.len() as u64, false);
        let str_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
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
            .build_store(len_gep, len)
            .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
        self.builder
            .build_load(BasicTypeEnum::StructType(str_ty), alloca, "str_result")
            .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))
    }
}

// (Helper lives on the impl block via a method above.)
