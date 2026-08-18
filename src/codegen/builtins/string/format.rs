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
        let is_any = self.pending_to_string_is_any;
        self.pending_to_string_is_any = false;
        let to_string_arg_type = self.pending_to_string_arg_type.take();
        match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => {
                if is_any {
                    // Route i64 through `mimi_any_to_string` which uses a heuristic
                    // address-range check to distinguish C string pointers (map values)
                    // from integers, and handles tagged integers via bit-0 protocol.
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
                } else {
                    // 0.35.23 deep-eval: bool (i1) must render as "true"/"false"
                    // (bytecode `Value::Bool.to_string()`), not sprintf "%ld"
                    // which produced "1"/"0". Per call-site the checked path
                    // canonicalizes bool to i1, so a real i1 here is a bool.
                    if iv.get_type().get_bit_width() == 1 {
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let true_g = self
                            .builder
                            .build_global_string_ptr("true", "bool_true")
                            .map_err(|e| CompileError::LlvmError(format!("bool true fmt: {}", e)))?
                            .as_pointer_value();
                        let false_g = self
                            .builder
                            .build_global_string_ptr("false", "bool_false")
                            .map_err(|e| CompileError::LlvmError(format!("bool false fmt: {}", e)))?
                            .as_pointer_value();
                        let i1_ty = self.context.bool_type();
                        let cond = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                iv,
                                i1_ty.const_zero(),
                                "bool_cond",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("bool compare: {}", e)))?;
                        let i64_ty = self.context.i64_type();
                        let ptr = self
                            .builder
                            .build_select(cond, true_g, false_g, "bool_ptr")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("bool select ptr: {}", e))
                            })?
                            .into_pointer_value();
                        let len = self
                            .builder
                            .build_select(
                                cond,
                                i64_ty.const_int(4, false),
                                i64_ty.const_int(5, false),
                                "bool_len",
                            )
                            .map_err(|e| {
                                CompileError::LlvmError(format!("bool select len: {}", e))
                            })?
                            .into_int_value();
                        let str_ty = self.context.struct_type(
                            &[
                                BasicTypeEnum::PointerType(i8_ptr),
                                BasicTypeEnum::IntType(i64_ty),
                            ],
                            false,
                        );
                        let alloca = self.build_entry_alloca(str_ty, "bool_str")?;
                        let ptr_gep = self
                            .gep()
                            .build_struct_gep(str_ty, alloca, 0, "bool_str_ptr")
                            .map_err(|e| CompileError::LlvmError(format!("bool gep ptr: {}", e)))?;
                        self.builder.build_store(ptr_gep, ptr).map_err(|e| {
                            CompileError::LlvmError(format!("bool store ptr: {}", e))
                        })?;
                        let len_gep = self
                            .gep()
                            .build_struct_gep(str_ty, alloca, 1, "bool_str_len")
                            .map_err(|e| CompileError::LlvmError(format!("bool gep len: {}", e)))?;
                        self.builder.build_store(len_gep, len).map_err(|e| {
                            CompileError::LlvmError(format!("bool store len: {}", e))
                        })?;
                        let result = self
                            .builder
                            .build_load(BasicTypeEnum::StructType(str_ty), alloca, "bool_str")
                            .map_err(|e| CompileError::LlvmError(format!("bool load: {}", e)))?;
                        return Ok(result);
                    }
                    // Known integer type: format directly with sprintf to avoid
                    // mimi_any_to_string's tagged-integer heuristic which would
                    // misidentify odd positive integers (e.g. 5 → 5>>1 = 2).
                    let alloc_size = self.context.i64_type().const_int(32, false);
                    let buf = self.malloc_or_abort(alloc_size, "malloc_int_str")?;
                    let fmt_global = self
                        .builder
                        .build_global_string_ptr("%ld", "int_fmt")
                        .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                    let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                    // B3/CG-C3: snprintf returns i32 (not i8*). Prefer the module
                    // declaration from declare_runtime_fns; only declare with correct
                    // signature if missing.
                    let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
                        let i32_ty = self.context.i32_type();
                        let snprintf_ty = i32_ty.fn_type(
                            &[
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                                BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                                BasicMetadataTypeEnum::PointerType(i8_ptr),
                            ],
                            true,
                        );
                        self.module.add_function(
                            "snprintf",
                            snprintf_ty,
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                    // A1: Ensure integer is i64 for snprintf("%ld").
                    // i32 values must be sign-extended (not zero-extended)
                    // to preserve negative values.
                    let iv_i64 = if iv.get_type().get_bit_width() < 64 {
                        if iv.get_type().get_bit_width() == 1 {
                            self.builder
                                .build_int_z_extend(iv, self.context.i64_type(), "int_zext")
                                .map_err(|e| CompileError::LlvmError(format!("zext: {}", e)))?
                        } else {
                            self.builder
                                .build_int_s_extend(iv, self.context.i64_type(), "int_sext")
                                .map_err(|e| CompileError::LlvmError(format!("sext: {}", e)))?
                        }
                    } else {
                        iv
                    };
                    self.builder
                        .build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(alloc_size),
                                BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(iv_i64),
                            ],
                            "snprintf_int",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("snprintf error: {}", e)))?;
                    let str_ty = self.context.struct_type(
                        &[
                            BasicTypeEnum::PointerType(i8_ptr),
                            BasicTypeEnum::IntType(self.context.i64_type()),
                        ],
                        false,
                    );
                    let alloca = self.build_entry_alloca(str_ty, "int_str")?;
                    let ptr_gep = self
                        .gep()
                        .build_struct_gep(str_ty, alloca, 0, "int_str_ptr")
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
                            "strlen_int",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| CompileError::LlvmError("strlen returned void".to_string()))?
                        .into_int_value();
                    let len_gep = self
                        .gep()
                        .build_struct_gep(str_ty, alloca, 1, "int_str_len")
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    self.builder
                        .build_store(len_gep, len)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    let result = self
                        .builder
                        .build_load(BasicTypeEnum::StructType(str_ty), alloca, "int_str")
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                    Ok(result)
                }
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                // Audit fix 9 (full-audit-2026-08-05): unify with
                // mimi_to_string_f64 (Rust `f64::to_string` — shortest
                // round-trip, src/runtime/crypto.rs) instead of snprintf
                // "%.15g", which truncates to 15 significant digits and
                // diverges from the VM's Value::Float Display. This is the
                // same helper io.rs format()/println use (agent E unification),
                // so to_string/format/println now agree on float rendering.
                let to_f64_fn = self.get_runtime_fn("mimi_to_string_f64")?;
                let raw = self
                    .builder
                    .build_call(
                        to_f64_fn,
                        &[BasicMetadataValueEnum::FloatValue(fv)],
                        "to_str_f64",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("to_string error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_to_string_f64 returned void")?
                    .into_pointer_value();
                // Build {i8*, i64} struct from the runtime buffer
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
                    .build_store(ptr_gep, raw)
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
                        &[BasicMetadataValueEnum::PointerValue(raw)],
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
                let fields = sv.get_type().get_field_types();
                let is_string_struct = fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(
                        fields[1],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    );
                if is_string_struct {
                    return Ok(BasicValueEnum::StructValue(sv));
                }
                // Other aggregates (List, record, Option, Result, Set, Map,
                // Tuple, enum) must be rendered through the normal display
                // path. Treating them as string structs produced type
                // confusion (lists were returned as `{ptr,i64}` strings).
                let i64_ty = self.context.i64_type();
                let (print_arg, spec) = self.extract_print_arg(&args[0], i64_ty, "")?;
                if spec != "%s" {
                    return Err(CompileError::TypeMismatch(format!(
                        "to_string: unsupported aggregate representation '{}'",
                        spec
                    )));
                }
                let raw = match print_arg {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => {
                        return Err(CompileError::TypeMismatch(
                            "to_string: aggregate display did not return a string".into(),
                        ))
                    }
                };
                // The display emitter registered `raw` for statement-end
                // flushing; a returned string owns it now, so remove it from
                // that temporary free list to avoid a double free.
                self.display_frees.borrow_mut().retain(|p| *p != raw);
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let str_ty = self.context.struct_type(
                    &[
                        BasicTypeEnum::PointerType(i8_ptr),
                        BasicTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                let alloca = self.build_entry_alloca(str_ty, "aggregate_str")?;
                let ptr_gep = self
                    .gep()
                    .build_struct_gep(str_ty, alloca, 0, "aggregate_str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder
                    .build_store(ptr_gep, raw)
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
                        &[BasicMetadataValueEnum::PointerValue(raw)],
                        "aggregate_strlen",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("strlen returned void".to_string()))?
                    .into_int_value();
                let len_gep = self
                    .gep()
                    .build_struct_gep(str_ty, alloca, 1, "aggregate_str_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder
                    .build_store(len_gep, len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result = self
                    .builder
                    .build_load(BasicTypeEnum::StructType(str_ty), alloca, "aggregate_str")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                Ok(result)
            }
            BasicMetadataValueEnum::PointerValue(pv) => {
                // A list in the checked path may already be lowered to a
                // pointer to the {len, data} struct. Use the inferred type to
                // render it through the list display path instead of treating
                // it as a raw C string (which produced a type-confused "?").
                if let Some(list_ty) = to_string_arg_type
                    .as_deref()
                    .filter(|t| t.starts_with("List"))
                {
                    let list_struct_ty = self.list_struct_type();
                    let loaded = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(list_struct_ty),
                            pv,
                            "to_string_list_load",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?;
                    let sv = loaded.into_struct_value();
                    let raw = self.emit_list_typed_to_string(sv, list_ty)?;
                    // Same ownership transfer as the struct fallback: take the
                    // display buffer out of the temporary free list.
                    self.display_frees.borrow_mut().retain(|p| *p != raw);
                    let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                    let i64_ty = self.context.i64_type();
                    let str_ty = self.context.struct_type(
                        &[
                            BasicTypeEnum::PointerType(i8_ptr),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    );
                    let alloca = self.build_entry_alloca(str_ty, "to_string_list_str")?;
                    let ptr_gep = self
                        .gep()
                        .build_struct_gep(str_ty, alloca, 0, "to_string_list_str_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                    self.builder
                        .build_store(ptr_gep, raw)
                        .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                    self.register_heap_slot(alloca, str_ty, 0);
                    let strlen_fn = self
                        .module
                        .get_function("strlen")
                        .ok_or_else(|| "strlen not declared".to_string())?;
                    let len = self
                        .builder
                        .build_call(
                            strlen_fn,
                            &[BasicMetadataValueEnum::PointerValue(raw)],
                            "to_string_list_strlen",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .try_as_basic_value_opt()
                        .ok_or("to_string list strlen void")?
                        .into_int_value();
                    let len_gep = self
                        .gep()
                        .build_struct_gep(str_ty, alloca, 1, "to_string_list_str_len")
                        .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                    self.builder
                        .build_store(len_gep, len)
                        .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                    return self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(str_ty),
                            alloca,
                            "to_string_list_str",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("load: {}", e)));
                }
                // Unknown stray pointer: keep the old conservative fallback.
                let alloc_size = self.context.i64_type().const_int(2, false);
                let buf = self.malloc_or_abort(alloc_size, "malloc_call")?;
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
