#![allow(clippy::unwrap_used)]
use super::super::CallSiteValueExt;
use super::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
use inkwell::IntPredicate;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_println(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let arg_types: Vec<String> = self.pending_print_arg_types.clone();
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "println expects at least 1 argument".to_string(),
            ));
        }
        let i64_ty = self.context.i64_type();
        // Single string pointer: use puts (which appends newline automatically).
        // Skip this fast path for list/record pointers, which need formatting.
        if args.len() == 1 {
            if let BasicMetadataValueEnum::PointerValue(_) = args[0] {
                let ty = arg_types.first().map(|s| s.as_str()).unwrap_or("");
                let is_list = ty.starts_with("List");
                let is_record = !ty.is_empty()
                    && self
                        .type_defs
                        .get(ty)
                        .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Record(_)));
                if !is_list && !is_record {
                    let puts = self.get_runtime_fn("puts")?;
                    self.build_call(puts, args, "puts_call")?;
                    return Ok(i64_ty.const_int(0, false).into());
                }
            }
        }
        // Build format and arg list, handling struct/enum values by extracting the payload
        let mut print_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        let mut fmt_str = String::new();
        for (i, arg) in args.iter().enumerate() {
            // P0-3: insert a single space between adjacent args, matching
            // the interpreter's `parts.join(" ")` semantics.
            if i > 0 {
                fmt_str.push(' ');
            }
            let arg_type = arg_types.get(i).cloned().unwrap_or_default();
            let (print_arg, spec) = self.extract_print_arg(arg, i64_ty, &arg_type)?;
            print_args.push(print_arg);
            fmt_str.push_str(&spec);
        }
        fmt_str.push('\n');
        let fmt_global = self
            .builder
            .build_global_string_ptr(&fmt_str, "println_fmt")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        let mut printf_args = vec![BasicMetadataValueEnum::PointerValue(
            fmt_global.as_pointer_value(),
        )];
        printf_args.extend(print_args);
        let printf = self.get_runtime_fn("printf")?;
        self.build_call(printf, &printf_args, "printf_call")?;
        Ok(i64_ty.const_int(0, false).into())
    }

    fn extract_print_arg(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
        arg_type: &str,
    ) -> MimiResult<(BasicMetadataValueEnum<'ctx>, String)> {
        match arg {
            BasicMetadataValueEnum::StructValue(sv) => {
                let fields = sv.get_type().get_field_types();
                let num_fields = fields.len();
                // Named record: Display-like `Name { field: value, ... }`
                if !arg_type.is_empty()
                    && self.type_defs.get(arg_type).is_some_and(|td| {
                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                    })
                {
                    let alloca = self.build_alloca(
                        BasicTypeEnum::StructType(sv.get_type()),
                        "print_rec",
                    )?;
                    self.build_store(alloca, *sv)?;
                    let str_ptr = self.emit_record_display(arg_type, alloca)?;
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ));
                }
                // Custom enum: {i32 tag, i64 payload}
                if !arg_type.is_empty()
                    && self.type_defs.get(arg_type).is_some_and(|td| {
                        matches!(td.kind, crate::ast::TypeDefKind::Enum(_))
                    })
                {
                    let str_ptr = self.emit_enum_display(arg_type, *sv)?;
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ));
                }
                // Enum-like {i32, i64}: resolve type from arg_type or variant name.
                if num_fields == 2
                    && matches!(
                        fields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 32
                    )
                    && matches!(
                        fields[1],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    )
                {
                    let enum_ty = if self.type_defs.get(arg_type).is_some_and(|td| {
                        matches!(td.kind, crate::ast::TypeDefKind::Enum(_))
                    }) {
                        Some(arg_type.to_string())
                    } else if let Some((owner, _)) = self.find_variant_owner(arg_type) {
                        Some(owner)
                    } else {
                        None
                    };
                    if let Some(et) = enum_ty {
                        let str_ptr = self.emit_enum_display(&et, *sv)?;
                        return Ok((
                            BasicMetadataValueEnum::PointerValue(str_ptr),
                            "%s".to_string(),
                        ));
                    }
                }
                // Detect Mimi string struct: {i8*, i64}
                if num_fields == 2 && matches!(fields[0], BasicTypeEnum::PointerType(_)) {
                    let ptr = self.build_extract_value((*sv).into(), 0, "str_ptr")?;
                    match ptr {
                        BasicValueEnum::PointerValue(pv) => {
                            Ok((BasicMetadataValueEnum::PointerValue(pv), "%s".to_string()))
                        }
                        _ => Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string())),
                    }
                } else if num_fields == 2
                    && matches!(
                        fields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    )
                    && matches!(fields[1], BasicTypeEnum::PointerType(_))
                {
                    // Mimi list struct: {i64 len, ptr data} — require i64 len
                    // so Option {i1, ptr} is not misclassified as List.
                    let str_ptr =
                        if arg_type == "List<string>" || arg_type.starts_with("List<string>") {
                            self.emit_list_string_to_string(*sv)?
                        } else if arg_type.starts_with("List<List<")
                            || arg_type
                                .strip_prefix("List<")
                                .is_some_and(|s| s.starts_with("List<"))
                        {
                            // Nested list: pick inner-list formatter from element type.
                            let mid = Self::strip_first_type_arg(arg_type, "List")
                                .unwrap_or_else(|| "List".to_string());
                            let elem = Self::strip_first_type_arg(&mid, "List")
                                .unwrap_or_default();
                            let inner_fn = if elem == "string" {
                                "mimi_list_to_string"
                            } else if elem.starts_with("Map") {
                                "mimi_list_map_to_string"
                            } else if elem.starts_with("Set") {
                                "mimi_list_set_to_string"
                            } else {
                                "mimi_list_i32_to_string"
                            };
                            self.emit_list_list_to_string(*sv, inner_fn)?
                        } else if let Some(inner) = arg_type
                            .strip_prefix("List<")
                            .and_then(|s| s.strip_suffix('>'))
                        {
                            if self.type_defs.get(inner).is_some_and(|td| {
                                matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                            }) {
                                self.emit_list_record_to_string(*sv, inner)?
                            } else if inner.starts_with("Option") {
                                self.emit_list_option_to_string(*sv, inner)?
                            } else if inner.starts_with("Result") {
                                self.emit_list_result_to_string(*sv, inner)?
                            } else if self.type_defs.get(inner).is_some_and(|td| {
                                matches!(td.kind, crate::ast::TypeDefKind::Enum(_))
                            }) {
                                self.emit_list_enum_to_string(*sv, inner)?
                            } else if inner.starts_with("Map") {
                                self.emit_list_map_to_string(*sv, inner)?
                            } else if inner.starts_with("Set") || inner == "set" {
                                self.emit_list_set_to_string(*sv, inner)?
                            } else if inner.starts_with('(') {
                                // List of product tuples stored as ptrtoint.
                                self.emit_list_product_tuple_to_string(*sv, inner)?
                            } else {
                                self.emit_list_i32_to_string(*sv)?
                            }
                        } else {
                            self.emit_list_i32_to_string(*sv)?
                        };
                    Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ))
                } else if num_fields == 2
                    && matches!(
                        fields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                    && matches!(fields[1], BasicTypeEnum::PointerType(_))
                {
                    // Option with pointer payload (e.g. Option<record>):
                    // disc i1 + payload ptr. Prefer typed Option path when known.
                    let inner_rec = arg_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .filter(|inner| {
                            self.type_defs.get(*inner).is_some_and(|td| {
                                matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                            })
                        });
                    // Also when arg_type is bare "Option" but payload is a record
                    // pointer — cannot recover name; fall back to Some(%p).
                    let str_ptr = self.emit_option_to_string(*sv, inner_rec, arg_type)?;
                    Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ))
                } else if num_fields >= 2
                    && fields
                        .iter()
                        .all(|f| matches!(f, BasicTypeEnum::IntType(_)))
                    && matches!(
                        fields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                    && !arg_type.starts_with("Option")
                    && !arg_type.starts_with("Result")
                    && !self.type_defs.contains_key(arg_type)
                {
                    // Bool-headed int tuple: `(true, 1)` / map_get.
                    // Skip when arg_type is Option/Result/named enum (same layout).
                    let str_ptr = self.emit_int_tuple_to_string(*sv)?;
                    Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ))
                } else if num_fields >= 2 {
                    // Option/Result/enum-like: print payload field (field 1).
                    // For Option None (disc=0), interp prints `None()` — approximate
                    // by printing payload only when disc!=0, else "None".
                    if (arg_type.starts_with("Option") || arg_type == "Option")
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        )
                    {
                        let inner_rec = arg_type
                            .strip_prefix("Option<")
                            .and_then(|s| s.strip_suffix('>'))
                            .filter(|inner| {
                                self.type_defs.get(*inner).is_some_and(|td| {
                                    matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                })
                            });
                        let str_ptr = self.emit_option_to_string(*sv, inner_rec, arg_type)?;
                        return Ok((
                            BasicMetadataValueEnum::PointerValue(str_ptr),
                            "%s".to_string(),
                        ));
                    }
                    if (arg_type.starts_with("Result") || arg_type == "Result")
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        )
                        && num_fields >= 3
                    {
                        let ok_rec = arg_type
                            .strip_prefix("Result<")
                            .and_then(|s| s.split(',').next())
                            .map(|s| s.trim())
                            .filter(|inner| {
                                !inner.is_empty()
                                    && self.type_defs.get(*inner).is_some_and(|td| {
                                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                    })
                            });
                        let str_ptr =
                            self.emit_result_to_string_typed(*sv, ok_rec, arg_type)?;
                        return Ok((
                            BasicMetadataValueEnum::PointerValue(str_ptr),
                            "%s".to_string(),
                        ));
                    }
                    // Heterogeneous product / user tuple: format all fields.
                    // Skip named enums (i32 tag + payload) already handled above.
                    let is_named = !arg_type.is_empty() && self.type_defs.contains_key(arg_type);
                    let is_enum_layout = num_fields == 2
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 32
                        )
                        && matches!(fields[1], BasicTypeEnum::IntType(t) if t.get_bit_width() == 64);
                    if !is_named && !is_enum_layout {
                        let str_ptr = self.emit_product_tuple_to_string(*sv)?;
                        return Ok((
                            BasicMetadataValueEnum::PointerValue(str_ptr),
                            "%s".to_string(),
                        ));
                    }
                    let payload = self.build_extract_value((*sv).into(), 1, "payload")?;
                    match payload {
                        BasicValueEnum::IntValue(iv) => {
                            let ext = if iv.get_type().get_bit_width() < 64 {
                                if iv.get_type().get_bit_width() == 1 {
                                    self.builder
                                        .build_int_z_extend(iv, i64_ty, "payload_zext")
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                } else {
                                    self.builder
                                        .build_int_s_extend(iv, i64_ty, "payload_sext")
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                }
                            } else {
                                iv
                            };
                            Ok((BasicMetadataValueEnum::IntValue(ext), "%ld".to_string()))
                        }
                        _ => Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string())),
                    }
                } else {
                    Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string()))
                }
            }
            BasicMetadataValueEnum::PointerValue(pv) => {
                let pv = *pv;
                if arg_type.starts_with("List") {
                    // The pointer points to a list struct alloca; load it and
                    // reuse the struct formatting path above.
                    let list_struct_ty = self.list_struct_type();
                    let loaded = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(list_struct_ty),
                            pv,
                            "print_list_ptr_load",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    return self.extract_print_arg(
                        &BasicMetadataValueEnum::StructValue(loaded.into_struct_value()),
                        i64_ty,
                        arg_type,
                    );
                }
                // Named record stored as pointer to struct alloca.
                if !arg_type.is_empty()
                    && self.type_defs.get(arg_type).is_some_and(|td| {
                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                    })
                {
                    let str_ptr = self.emit_record_display(arg_type, pv)?;
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        "%s".to_string(),
                    ));
                }
                // Map/Set opaque i64 handle may arrive as int; pointer is C string.
                Ok((BasicMetadataValueEnum::PointerValue(pv), "%s".to_string()))
            }
            BasicMetadataValueEnum::IntValue(iv) => {
                // Map/Set opaque handles: serialize via runtime JSON helpers.
                if arg_type == "Map" || arg_type.starts_with("Map<") {
                    let fn_name = if arg_type.contains("Map<string, string>") {
                        "mimi_map_to_json_string"
                    } else if arg_type.contains("Map<string, bool>") {
                        "mimi_map_to_json_bool"
                    } else if arg_type.contains("Map<string, f64>")
                        || arg_type.contains("Map<string, f32>")
                    {
                        "mimi_map_to_json_f64"
                    } else {
                        "mimi_map_to_json_i64"
                    };
                    let func = self.get_runtime_fn(fn_name)?;
                    let raw = self
                        .build_call(
                            func,
                            &[BasicMetadataValueEnum::IntValue(*iv)],
                            "print_map_json",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("map to_json void")?
                        .into_pointer_value();
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(raw),
                        "%s".to_string(),
                    ));
                }
                if arg_type == "Set" || arg_type.starts_with("Set<") || arg_type == "set" {
                    let fn_name = if arg_type.contains("Set<string>") {
                        "mimi_set_to_display_string"
                    } else if arg_type.contains("Set<bool>") {
                        "mimi_set_to_display_bool"
                    } else if arg_type.contains("Set<f64>") || arg_type.contains("Set<f32>") {
                        "mimi_set_to_display_f64"
                    } else {
                        "mimi_set_to_display"
                    };
                    let func = self.get_runtime_fn(fn_name)?;
                    let raw = self
                        .build_call(
                            func,
                            &[BasicMetadataValueEnum::IntValue(*iv)],
                            "print_set_disp",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("set display void")?
                        .into_pointer_value();
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(raw),
                        "%s".to_string(),
                    ));
                }
                // A1: Ensure integer is i64 for printf("%ld").
                // i1 bool: print "true"/"false" to match interpreter.
                let bw = iv.get_type().get_bit_width();
                if bw == 1 {
                    let true_g = self
                        .builder
                        .build_global_string_ptr("true", "print_bool_true")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let false_g = self
                        .builder
                        .build_global_string_ptr("false", "print_bool_false")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let zero = iv.get_type().const_int(0, false);
                    let is_true = self
                        .builder
                        .build_int_compare(IntPredicate::NE, *iv, zero, "print_bool")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let selected = self
                        .builder
                        .build_select(
                            is_true,
                            true_g.as_pointer_value(),
                            false_g.as_pointer_value(),
                            "print_bool_str",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    return Ok((
                        BasicMetadataValueEnum::PointerValue(selected.into_pointer_value()),
                        "%s".to_string(),
                    ));
                }
                let ext_iv = if bw < 64 {
                    self.builder
                        .build_int_s_extend(*iv, i64_ty, "print_sext")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    *iv
                };
                Ok((BasicMetadataValueEnum::IntValue(ext_iv), "%ld".to_string()))
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                // P0-3: use %g (shortest round-trip) to match the
                // interpreter's `{}` Display format. %f always prints 6
                // decimals (e.g. "3.140000" for 3.14), %g prints "3.14".
                Ok((BasicMetadataValueEnum::FloatValue(*fv), "%g".to_string()))
            }
            _ => Ok((*arg, "%p".to_string())),
        }
    }

    /// Format `List<Map>` as `[{"a":1}, {"b":2}]` via map JSON helpers.
    fn emit_list_map_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        map_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_map_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        let buf = self.malloc_or_abort(i64_ty.const_int(4096, false), "list_map_buf")?;
        let open = self
            .builder
            .build_global_string_ptr("[", "list_map_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "list_map_open_cpy",
        )?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let idx_alloca = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "list_map_i")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        let loop_bb = self.context.append_basic_block(parent, "list_map_loop");
        let body_bb = self.context.append_basic_block(parent, "list_map_body");
        let done_bb = self.context.append_basic_block(parent, "list_map_done");
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "list_map_idx")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "list_map_cont")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(cont, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(body_bb);
        let zero = i64_ty.const_int(0, false);
        let need_comma = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx, zero, "list_map_comma")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let comma_bb = self.context.append_basic_block(parent, "list_map_comma_bb");
        let elem_bb = self.context.append_basic_block(parent, "list_map_elem");
        self.builder
            .build_conditional_branch(need_comma, comma_bb, elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(comma_bb);
        let comma = self
            .builder
            .build_global_string_ptr(", ", "list_map_comma_s")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
            ],
            "list_map_strcat_comma",
        )?;
        self.builder
            .build_unconditional_branch(elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(elem_bb);
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, alloca, 1, "list_map_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_map_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let elem_slot = unsafe {
            self.builder
                .build_gep(i64_ty, data_ptr, &[idx], "list_map_slot")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        };
        let handle = self
            .builder
            .build_load(i64_ty, elem_slot, "list_map_handle")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let map_fn_name = if map_type.contains("Map<string, string>") {
            "mimi_map_to_json_string"
        } else if map_type.contains("Map<string, bool>") {
            "mimi_map_to_json_bool"
        } else if map_type.contains("Map<string, f64>") || map_type.contains("Map<string, f32>") {
            "mimi_map_to_json_f64"
        } else {
            "mimi_map_to_json_i64"
        };
        let map_fn = self.get_runtime_fn(map_fn_name)?;
        let map_str = self
            .build_call(
                map_fn,
                &[BasicMetadataValueEnum::IntValue(handle)],
                "list_map_json",
            )?
            .try_as_basic_value_opt()
            .ok_or("map to_json void")?
            .into_pointer_value();
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(map_str),
            ],
            "list_map_strcat_elem",
        )?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "list_map_next")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx_alloca, next)?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(done_bb);
        let close = self
            .builder
            .build_global_string_ptr("]", "list_map_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "list_map_close",
        )?;
        Ok(buf)
    }

    /// Format `List<Set>` as `[Set{…}, ...]` via set display helpers.
    fn emit_list_set_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        set_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_set_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        let buf = self.malloc_or_abort(i64_ty.const_int(4096, false), "list_set_buf")?;
        let open = self
            .builder
            .build_global_string_ptr("[", "list_set_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "list_set_open_cpy",
        )?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let idx_alloca = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "list_set_i")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        let loop_bb = self.context.append_basic_block(parent, "list_set_loop");
        let body_bb = self.context.append_basic_block(parent, "list_set_body");
        let done_bb = self.context.append_basic_block(parent, "list_set_done");
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "list_set_idx")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "list_set_cont")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(cont, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(body_bb);
        let zero = i64_ty.const_int(0, false);
        let need_comma = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx, zero, "list_set_comma")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let comma_bb = self.context.append_basic_block(parent, "list_set_comma_bb");
        let elem_bb = self.context.append_basic_block(parent, "list_set_elem");
        self.builder
            .build_conditional_branch(need_comma, comma_bb, elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(comma_bb);
        let comma = self
            .builder
            .build_global_string_ptr(", ", "list_set_comma_s")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
            ],
            "list_set_strcat_comma",
        )?;
        self.builder
            .build_unconditional_branch(elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(elem_bb);
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, alloca, 1, "list_set_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_set_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let elem_slot = unsafe {
            self.builder
                .build_gep(i64_ty, data_ptr, &[idx], "list_set_slot")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        };
        let handle = self
            .builder
            .build_load(i64_ty, elem_slot, "list_set_handle")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let set_fn_name = if set_type.contains("Set<string>") {
            "mimi_set_to_display_string"
        } else if set_type.contains("Set<bool>") {
            "mimi_set_to_display_bool"
        } else if set_type.contains("Set<f64>") || set_type.contains("Set<f32>") {
            "mimi_set_to_display_f64"
        } else {
            "mimi_set_to_display"
        };
        let set_fn = self.get_runtime_fn(set_fn_name)?;
        let set_str = self
            .build_call(
                set_fn,
                &[BasicMetadataValueEnum::IntValue(handle)],
                "list_set_disp",
            )?
            .try_as_basic_value_opt()
            .ok_or("set display void")?
            .into_pointer_value();
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(set_str),
            ],
            "list_set_strcat_elem",
        )?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "list_set_next")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx_alloca, next)?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(done_bb);
        let close = self
            .builder
            .build_global_string_ptr("]", "list_set_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "list_set_close",
        )?;
        Ok(buf)
    }

    /// Serialize `List<(…)>` (ptrtoint slots) to a compact JSON array of arrays.
    pub(in crate::codegen) fn emit_list_product_tuple_to_json(
        &self,
        list_alloca: inkwell::values::PointerValue<'ctx>,
        elem_type_str: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let elem_ty = crate::codegen::extract_list_elem_type(&format!("List<{}>", elem_type_str))
            .unwrap_or_else(|| crate::ast::Type::Name("i32".into(), vec![]));
        let sty = match self.llvm_type_for(&elem_ty) {
            Some(BasicTypeEnum::StructType(s)) => s,
            _ => {
                return Err(CompileError::Generic(
                    "to_json List of tuple: cannot map element type".into(),
                ));
            }
        };
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let len = self.load_list_len(list_alloca)?;
        let buf = self.malloc_or_abort(i64_ty.const_int(8192, false), "list_tup_json_buf")?;
        let open = self
            .builder
            .build_global_string_ptr("[", "list_tup_json_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "list_tup_json_open_cpy",
        )?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let idx_alloca = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "list_tup_json_i")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        let loop_bb = self.context.append_basic_block(parent, "list_tup_json_loop");
        let body_bb = self.context.append_basic_block(parent, "list_tup_json_body");
        let done_bb = self.context.append_basic_block(parent, "list_tup_json_done");
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "list_tup_json_idx")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "list_tup_json_cont")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(cont, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(body_bb);
        let zero = i64_ty.const_int(0, false);
        let need_comma = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx, zero, "list_tup_json_comma")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let comma_bb = self
            .context
            .append_basic_block(parent, "list_tup_json_comma_bb");
        let elem_bb = self.context.append_basic_block(parent, "list_tup_json_elem");
        self.builder
            .build_conditional_branch(need_comma, comma_bb, elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(comma_bb);
        let comma = self
            .builder
            .build_global_string_ptr(",", "list_tup_json_comma_s")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
            ],
            "list_tup_json_strcat_comma",
        )?;
        self.builder
            .build_unconditional_branch(elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(elem_bb);
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_tup_json_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_tup_json_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let elem_slot = unsafe {
            self.builder
                .build_gep(i64_ty, data_ptr, &[idx], "list_tup_json_slot")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        };
        let elem_i64 = self
            .builder
            .build_load(i64_ty, elem_slot, "list_tup_json_elem")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let elem_ptr = self
            .builder
            .build_int_to_ptr(elem_i64, i8_ptr, "list_tup_json_as_ptr")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let loaded = self
            .builder
            .build_load(BasicTypeEnum::StructType(sty), elem_ptr, "list_tup_json_ld")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_struct_value();
        let piece = self.emit_product_tuple_to_json(loaded)?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(piece),
            ],
            "list_tup_json_strcat_elem",
        )?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "list_tup_json_next")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx_alloca, next)?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(done_bb);
        let close = self
            .builder
            .build_global_string_ptr("]", "list_tup_json_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "list_tup_json_close",
        )?;
        Ok(buf)
    }

    /// Format `List<(…)>` product tuples (ptrtoint slots) as `[(1, 2), …]`.
    fn emit_list_product_tuple_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        elem_type_str: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let elem_ty = crate::codegen::extract_list_elem_type(&format!("List<{}>", elem_type_str))
            .unwrap_or_else(|| crate::ast::Type::Name("i32".into(), vec![]));
        let sty = match self.llvm_type_for(&elem_ty) {
            Some(BasicTypeEnum::StructType(s)) => s,
            _ => {
                return self.emit_list_i32_to_string(sv);
            }
        };
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_tup_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        let buf = self.malloc_or_abort(i64_ty.const_int(4096, false), "list_tup_buf")?;
        let open = self
            .builder
            .build_global_string_ptr("[", "list_tup_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "list_tup_open_cpy",
        )?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let idx_alloca = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "list_tup_i")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        let loop_bb = self.context.append_basic_block(parent, "list_tup_loop");
        let body_bb = self.context.append_basic_block(parent, "list_tup_body");
        let done_bb = self.context.append_basic_block(parent, "list_tup_done");
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "list_tup_idx")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "list_tup_cont")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(cont, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(body_bb);
        let zero = i64_ty.const_int(0, false);
        let need_comma = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx, zero, "list_tup_comma")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let comma_bb = self.context.append_basic_block(parent, "list_tup_comma_bb");
        let elem_bb = self.context.append_basic_block(parent, "list_tup_elem");
        self.builder
            .build_conditional_branch(need_comma, comma_bb, elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(comma_bb);
        let comma = self
            .builder
            .build_global_string_ptr(", ", "list_tup_comma_s")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
            ],
            "list_tup_strcat_comma",
        )?;
        self.builder
            .build_unconditional_branch(elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(elem_bb);
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, alloca, 1, "list_tup_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_tup_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let elem_slot = unsafe {
            self.builder
                .build_gep(i64_ty, data_ptr, &[idx], "list_tup_slot")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        };
        let elem_i64 = self
            .builder
            .build_load(i64_ty, elem_slot, "list_tup_elem")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let elem_ptr = self
            .builder
            .build_int_to_ptr(elem_i64, i8_ptr, "list_tup_as_ptr")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let loaded = self
            .builder
            .build_load(BasicTypeEnum::StructType(sty), elem_ptr, "list_tup_ld")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_struct_value();
        let piece = self.emit_product_tuple_to_string(loaded)?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(piece),
            ],
            "list_tup_strcat_elem",
        )?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "list_tup_next")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx_alloca, next)?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(done_bb);
        let close = self
            .builder
            .build_global_string_ptr("]", "list_tup_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "list_tup_close",
        )?;
        Ok(buf)
    }

    /// Format `List<Enum>` as `[Red(), Blue(7), ...]`.
    fn emit_list_enum_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        enum_name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_enum_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        let buf = self.malloc_or_abort(i64_ty.const_int(4096, false), "list_enum_buf")?;
        let open = self
            .builder
            .build_global_string_ptr("[", "list_enum_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "list_enum_open_cpy",
        )?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let idx_alloca = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "list_enum_i")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        let loop_bb = self.context.append_basic_block(parent, "list_enum_loop");
        let body_bb = self.context.append_basic_block(parent, "list_enum_body");
        let done_bb = self.context.append_basic_block(parent, "list_enum_done");
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "list_enum_idx")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "list_enum_cont")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(cont, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(body_bb);
        let zero = i64_ty.const_int(0, false);
        let need_comma = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx, zero, "list_enum_comma")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let comma_bb = self.context.append_basic_block(parent, "list_enum_comma_bb");
        let elem_bb = self.context.append_basic_block(parent, "list_enum_elem");
        self.builder
            .build_conditional_branch(need_comma, comma_bb, elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(comma_bb);
        let comma = self
            .builder
            .build_global_string_ptr(", ", "list_enum_comma_s")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
            ],
            "list_enum_strcat_comma",
        )?;
        self.builder
            .build_unconditional_branch(elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(elem_bb);
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, alloca, 1, "list_enum_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_enum_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let elem_slot = unsafe {
            self.builder
                .build_gep(i64_ty, data_ptr, &[idx], "list_enum_slot")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        };
        let elem_i64 = self
            .builder
            .build_load(i64_ty, elem_slot, "list_enum_elem")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let enum_ptr = self
            .builder
            .build_int_to_ptr(elem_i64, i8_ptr, "list_enum_as_ptr")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let enum_sty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.i32_type()),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let loaded = self
            .builder
            .build_load(BasicTypeEnum::StructType(enum_sty), enum_ptr, "list_enum_ld")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_struct_value();
        let enum_str = self.emit_enum_display(enum_name, loaded)?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(enum_str),
            ],
            "list_enum_strcat_elem",
        )?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "list_enum_next")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx_alloca, next)?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(done_bb);
        let close = self
            .builder
            .build_global_string_ptr("]", "list_enum_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "list_enum_close",
        )?;
        Ok(buf)
    }

    /// Format `List<Result<…>>` as `[Ok(…), Err(…), ...]`.
    /// `elem_res_type` is the full Result type (e.g. `Result<Map<string, i32>, i32>`).
    fn emit_list_result_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        elem_res_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_res_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        let buf = self.malloc_or_abort(i64_ty.const_int(4096, false), "list_res_buf")?;
        let open = self
            .builder
            .build_global_string_ptr("[", "list_res_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "list_res_open_cpy",
        )?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let idx_alloca = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "list_res_i")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        let loop_bb = self.context.append_basic_block(parent, "list_res_loop");
        let body_bb = self.context.append_basic_block(parent, "list_res_body");
        let done_bb = self.context.append_basic_block(parent, "list_res_done");
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "list_res_idx")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "list_res_cont")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(cont, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(body_bb);
        let zero = i64_ty.const_int(0, false);
        let need_comma = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx, zero, "list_res_comma")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let comma_bb = self.context.append_basic_block(parent, "list_res_comma_bb");
        let elem_bb = self.context.append_basic_block(parent, "list_res_elem");
        self.builder
            .build_conditional_branch(need_comma, comma_bb, elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(comma_bb);
        let comma = self
            .builder
            .build_global_string_ptr(", ", "list_res_comma_s")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
            ],
            "list_res_strcat_comma",
        )?;
        self.builder
            .build_unconditional_branch(elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(elem_bb);
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, alloca, 1, "list_res_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_res_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let elem_slot = unsafe {
            self.builder
                .build_gep(i64_ty, data_ptr, &[idx], "list_res_slot")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        };
        let elem_i64 = self
            .builder
            .build_load(i64_ty, elem_slot, "list_res_elem")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let res_ptr = self
            .builder
            .build_int_to_ptr(elem_i64, i8_ptr, "list_res_as_ptr")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let res_sty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.bool_type()),
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let loaded = self
            .builder
            .build_load(BasicTypeEnum::StructType(res_sty), res_ptr, "list_res_ld")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_struct_value();
        let res_str = self.emit_result_to_string_typed(loaded, None, elem_res_type)?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(res_str),
            ],
            "list_res_strcat_elem",
        )?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "list_res_next")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx_alloca, next)?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(done_bb);
        let close = self
            .builder
            .build_global_string_ptr("]", "list_res_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "list_res_close",
        )?;
        Ok(buf)
    }

    /// Format `List<Option<…>>` as `[Some(…), None(), …]`.
    /// `elem_opt_type` is the full Option type string (e.g. `Option<Map<string, i32>>`).
    fn emit_list_option_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        elem_opt_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        // Reuse list-of-record style loop but format each i64 slot as Option
        // by treating list data as array of {i1,i64} pointers/values stored as i64.
        // List elements for Option are typically by-value structs spilled as ptrtoint
        // of stack Option or packed; walk as i64 and interpret as Option via temp.
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_opt_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        let buf = self.malloc_or_abort(i64_ty.const_int(4096, false), "list_opt_buf")?;
        let open = self
            .builder
            .build_global_string_ptr("[", "list_opt_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "list_opt_open_cpy",
        )?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let idx_alloca = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "list_opt_i")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        let loop_bb = self.context.append_basic_block(parent, "list_opt_loop");
        let body_bb = self.context.append_basic_block(parent, "list_opt_body");
        let done_bb = self.context.append_basic_block(parent, "list_opt_done");
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "list_opt_idx")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "list_opt_cont")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(cont, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(body_bb);
        let zero = i64_ty.const_int(0, false);
        let need_comma = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx, zero, "list_opt_comma")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let comma_bb = self.context.append_basic_block(parent, "list_opt_comma_bb");
        let elem_bb = self.context.append_basic_block(parent, "list_opt_elem");
        self.builder
            .build_conditional_branch(need_comma, comma_bb, elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(comma_bb);
        let comma = self
            .builder
            .build_global_string_ptr(", ", "list_opt_comma_s")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
            ],
            "list_opt_strcat_comma",
        )?;
        self.builder
            .build_unconditional_branch(elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(elem_bb);
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, alloca, 1, "list_opt_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_opt_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        // Elements are pointers to Option structs (or ptrtoint of them).
        let elem_slot = unsafe {
            self.builder
                .build_gep(i64_ty, data_ptr, &[idx], "list_opt_slot")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        };
        let elem_i64 = self
            .builder
            .build_load(i64_ty, elem_slot, "list_opt_elem")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let opt_ptr = self
            .builder
            .build_int_to_ptr(elem_i64, i8_ptr, "list_opt_as_ptr")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let opt_sty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.bool_type()),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let loaded = self
            .builder
            .build_load(BasicTypeEnum::StructType(opt_sty), opt_ptr, "list_opt_ld")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_struct_value();
        let opt_str = self.emit_option_to_string(loaded, None, elem_opt_type)?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(opt_str),
            ],
            "list_opt_strcat_elem",
        )?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "list_opt_next")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx_alloca, next)?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(done_bb);
        let close = self
            .builder
            .build_global_string_ptr("]", "list_opt_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "list_opt_close",
        )?;
        Ok(buf)
    }

    /// Format `List<Record>` as `[Point { ... }, ...]` matching interp Display.
    fn emit_list_record_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        record_name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_ty = self.list_struct_type();
        let alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "list_rec_print")?;
        self.build_store(alloca, sv)?;
        let len = self.load_list_len(alloca)?;
        // Heap buffer for final string (grow generously).
        let buf_size = i64_ty.const_int(4096, false);
        let buf = self.malloc_or_abort(buf_size, "list_rec_buf")?;
        // Write '['
        let open = self
            .builder
            .build_global_string_ptr("[", "list_rec_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "list_rec_open_cpy",
        )?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let idx_alloca = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "list_rec_i")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        let loop_bb = self.context.append_basic_block(parent, "list_rec_loop");
        let body_bb = self.context.append_basic_block(parent, "list_rec_body");
        let done_bb = self.context.append_basic_block(parent, "list_rec_done");
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "list_rec_idx")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "list_rec_cont")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(cont, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(body_bb);
        // comma if idx > 0
        let zero = i64_ty.const_int(0, false);
        let need_comma = self
            .builder
            .build_int_compare(IntPredicate::UGT, idx, zero, "need_comma")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let comma_bb = self.context.append_basic_block(parent, "list_rec_comma");
        let elem_bb = self.context.append_basic_block(parent, "list_rec_elem");
        self.builder
            .build_conditional_branch(need_comma, comma_bb, elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(comma_bb);
        let comma = self
            .builder
            .build_global_string_ptr(", ", "list_rec_comma_s")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
            ],
            "list_rec_strcat_comma",
        )?;
        self.builder
            .build_unconditional_branch(elem_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(elem_bb);
        // load element i64 (ptrtoint of record or by-value packed)
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, alloca, 1, "list_rec_data_gep")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let data_ptr = self
            .builder
            .build_load(i8_ptr, data_gep, "list_rec_data")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_pointer_value();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(
                    i64_ty,
                    data_ptr,
                    &[idx],
                    "list_rec_elem_ptr",
                )
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        };
        let elem_i64 = self
            .builder
            .build_load(i64_ty, elem_ptr, "list_rec_elem")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?
            .into_int_value();
        // Treat as pointer to record struct
        let rec_ptr = self
            .builder
            .build_int_to_ptr(elem_i64, i8_ptr, "list_rec_as_ptr")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let rec_str = self.emit_record_display(record_name, rec_ptr)?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(rec_str),
            ],
            "list_rec_strcat_elem",
        )?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "list_rec_next")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_store(idx_alloca, next)?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(done_bb);
        let close = self
            .builder
            .build_global_string_ptr("]", "list_rec_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "list_rec_close_cpy",
        )?;
        Ok(buf)
    }

    /// Format custom enum `{i32 tag, i64 payload}` as `Variant` / `Variant(n)`.
    fn emit_enum_display(
        &self,
        type_name: &str,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let td = self.type_defs.get(type_name).ok_or_else(|| {
            CompileError::LlvmError(format!("no type def for enum {}", type_name))
        })?;
        let variants = match &td.kind {
            crate::ast::TypeDefKind::Enum(vs) => vs.clone(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "{} is not an enum",
                    type_name
                )))
            }
        };
        let mut sorted = variants;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let tag = self
            .build_extract_value(sv.into(), 0, "enum_tag")?
            .into_int_value();
        let payload = self
            .build_extract_value(sv.into(), 1, "enum_pay")?
            .into_int_value();
        let payload_i64 = if payload.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(payload, i64_ty, "enum_pay_i64")
                .map_err(|e| CompileError::LlvmError(e.to_string()))?
        } else {
            payload
        };
        let buf = self.malloc_or_abort(i64_ty.const_int(128, false), "enum_disp_buf")?;
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module.add_function(
                "snprintf",
                ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
        let merge_bb = self.context.append_basic_block(parent, "enum_disp_merge");
        let default_bb = self.context.append_basic_block(parent, "enum_disp_default");
        let mut switch_cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        let mut case_bbs: Vec<(usize, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();
        for (i, v) in sorted.iter().enumerate() {
            let case_bb = self
                .context
                .append_basic_block(parent, &format!("enum_disp_{}", v.name));
            switch_cases.push((tag.get_type().const_int(i as u64, false), case_bb));
            case_bbs.push((i, case_bb));
        }
        self.builder
            .build_switch(tag, default_bb, &switch_cases)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        for (i, case_bb) in case_bbs {
            self.builder.position_at_end(case_bb);
            let v = &sorted[i];
            let has_payload = v.payload.is_some();
            if has_payload {
                // String payloads are ptrtoint of {ptr,len}; decode when heapish.
                let is_str_payload = matches!(
                    &v.payload,
                    Some(crate::ast::VariantPayload::Tuple(ts))
                        if ts.len() == 1
                            && matches!(&ts[0], crate::ast::Type::Name(n, _) if n == "string")
                );
                if is_str_payload {
                    let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                    let str_sty = self.context.struct_type(
                        &[
                            BasicTypeEnum::PointerType(i8_ptr),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    );
                    let as_ptr = self
                        .builder
                        .build_int_to_ptr(payload_i64, i8_ptr, &format!("enum_str_{}", v.name))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let loaded = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(str_sty),
                            as_ptr,
                            &format!("enum_str_ld_{}", v.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_struct_value();
                    let data_ptr = self
                        .build_extract_value(loaded.into(), 0, &format!("enum_data_{}", v.name))?
                        .into_pointer_value();
                    let fmt = self
                        .builder
                        .build_global_string_ptr(
                            &format!("{}(%s)", v.name),
                            &format!("enum_sfmt_{}", v.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(i64_ty.const_int(128, false)),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(data_ptr),
                        ],
                        &format!("enum_sn_s_{}", v.name),
                    )?;
                } else {
                    let fmt = self
                        .builder
                        .build_global_string_ptr(
                            &format!("{}(%ld)", v.name),
                            &format!("enum_fmt_{}", v.name),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(i64_ty.const_int(128, false)),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(payload_i64),
                        ],
                        &format!("enum_sn_{}", v.name),
                    )?;
                }
            } else {
                let fmt = self
                    .builder
                    .build_global_string_ptr(
                        &format!("{}()", v.name),
                        &format!("enum_ufmt_{}", v.name),
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                self.build_call(
                    strcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                    ],
                    &format!("enum_cpy_{}", v.name),
                )?;
            }
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        }
        self.builder.position_at_end(default_bb);
        let unk = self
            .builder
            .build_global_string_ptr("Enum(?)", "enum_unk")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(unk.as_pointer_value()),
            ],
            "enum_unk_cpy",
        )?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(merge_bb);
        Ok(buf)
    }

    /// Format a named Record as `Name { field: value, ... }` (interp Display style).
    fn emit_record_display(
        &self,
        type_name: &str,
        struct_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let td = self.type_defs.get(type_name).ok_or_else(|| {
            CompileError::LlvmError(format!("no type def for {}", type_name))
        })?;
        let fields = match &td.kind {
            crate::ast::TypeDefKind::Record(fields) => fields.clone(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "{} is not a record",
                    type_name
                )))
            }
        };
        let llvm_ty = *self.type_llvm.get(type_name).ok_or_else(|| {
            CompileError::LlvmError(format!("no LLVM type for {}", type_name))
        })?;
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return Err(CompileError::LlvmError(format!(
                "{} is not a struct",
                type_name
            )));
        };
        let i64_ty = self.context.i64_type();
        // Sorted field names match interp Display (dual-stable).
        let mut idx_map: Vec<(usize, _)> = fields.iter().enumerate().collect();
        idx_map.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        let mut fmt = format!("{} {{ ", type_name);
        let mut sprintf_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for (pos, (i, field)) in idx_map.iter().enumerate() {
            if pos > 0 {
                fmt.push_str(", ");
            }
            let gep = self
                .gep()
                .build_struct_gep(sty, struct_ptr, *i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            let ft = sty
                .get_field_type_at_index(*i as u32)
                .ok_or_else(|| CompileError::LlvmError("missing field".into()))?;
            let field_val = self.build_load(ft, gep, &format!("disp_{}", field.name))?;
            match &field.ty {
                crate::ast::Type::Name(n, _) if n == "string" => {
                    fmt.push_str(&format!("{}: %s", field.name));
                    let sv = field_val.into_struct_value();
                    let dp = self
                        .build_extract_value(sv.into(), 0, &format!("{}_p", field.name))?
                        .into_pointer_value();
                    sprintf_args.push(BasicMetadataValueEnum::PointerValue(dp));
                }
                crate::ast::Type::Name(n, _) if matches!(n.as_str(), "i32" | "i64") => {
                    fmt.push_str(&format!("{}: %ld", field.name));
                    let iv = field_val.into_int_value();
                    let i64v = if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(iv, i64_ty, "disp_sext")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    };
                    sprintf_args.push(BasicMetadataValueEnum::IntValue(i64v));
                }
                crate::ast::Type::Name(n, _) if n == "bool" => {
                    fmt.push_str(&format!("{}: %s", field.name));
                    let iv = field_val.into_int_value();
                    let true_g = self
                        .builder
                        .build_global_string_ptr("true", &format!("{}_t", field.name))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let false_g = self
                        .builder
                        .build_global_string_ptr("false", &format!("{}_f", field.name))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let zero = iv.get_type().const_int(0, false);
                    let is_t = self
                        .builder
                        .build_int_compare(IntPredicate::NE, iv, zero, "disp_b")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let sel = self
                        .builder
                        .build_select(
                            is_t,
                            true_g.as_pointer_value(),
                            false_g.as_pointer_value(),
                            "disp_bs",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    sprintf_args.push(BasicMetadataValueEnum::PointerValue(
                        sel.into_pointer_value(),
                    ));
                }
                crate::ast::Type::Name(n, _) if n == "f64" => {
                    fmt.push_str(&format!("{}: %g", field.name));
                    sprintf_args.push(BasicMetadataValueEnum::FloatValue(
                        field_val.into_float_value(),
                    ));
                }
                crate::ast::Type::Name(n, _)
                    if self.type_defs.get(n).is_some_and(|td| {
                        matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                    }) =>
                {
                    // Nested named record: recursive Display string.
                    fmt.push_str(&format!("{}: %s", field.name));
                    let nested_ptr = match field_val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        BasicValueEnum::StructValue(sv) => {
                            let nested_ty = *self.type_llvm.get(n).ok_or_else(|| {
                                CompileError::LlvmError(format!("no LLVM type for {}", n))
                            })?;
                            let BasicTypeEnum::StructType(nsty) = nested_ty else {
                                return Err(CompileError::LlvmError(format!(
                                    "{} is not a struct",
                                    n
                                )));
                            };
                            let alloca = self.build_alloca(
                                BasicTypeEnum::StructType(nsty),
                                &format!("nest_{}", field.name),
                            )?;
                            self.build_store(alloca, sv)?;
                            alloca
                        }
                        _ => {
                            return Err(CompileError::LlvmError(format!(
                                "nested record field '{}' unexpected kind",
                                field.name
                            )))
                        }
                    };
                    let nested_str = self.emit_record_display(n, nested_ptr)?;
                    sprintf_args.push(BasicMetadataValueEnum::PointerValue(nested_str));
                }
                _ => {
                    fmt.push_str(&format!("{}: ?", field.name));
                }
            }
        }
        fmt.push_str(" }");
        let est = (fmt.len() + fields.len() * 64 + 64).max(128) as u64;
        let buf_size = i64_ty.const_int(est, false);
        let buf = self.malloc_or_abort(buf_size, "rec_disp_buf")?;
        let fmt_ptr = self
            .builder
            .build_global_string_ptr(&fmt, "rec_disp_fmt")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let mut all_args = vec![
            BasicMetadataValueEnum::PointerValue(buf),
            BasicMetadataValueEnum::IntValue(buf_size),
            BasicMetadataValueEnum::PointerValue(fmt_ptr.as_pointer_value()),
        ];
        all_args.extend(sprintf_args);
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module.add_function(
                "snprintf",
                ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        self.build_call(snprintf_fn, &all_args, "rec_disp_snprintf")?;
        Ok(buf)
    }

    /// Pick map JSON runtime helper from a type string containing `Map<…>`.
    fn map_json_fn_for_type(type_name: &str) -> &'static str {
        if type_name.contains("Map<string, string>") {
            "mimi_map_to_json_string"
        } else if type_name.contains("Map<string, bool>") {
            "mimi_map_to_json_bool"
        } else if type_name.contains("Map<string, f64>")
            || type_name.contains("Map<string, f32>")
        {
            "mimi_map_to_json_f64"
        } else {
            "mimi_map_to_json_i64"
        }
    }

    /// Pick set Display runtime helper from a type string containing `Set<…>`.
    fn set_display_fn_for_type(type_name: &str) -> &'static str {
        if type_name.contains("Set<string>") {
            "mimi_set_to_display_string"
        } else if type_name.contains("Set<bool>") {
            "mimi_set_to_display_bool"
        } else if type_name.contains("Set<f64>") || type_name.contains("Set<f32>") {
            "mimi_set_to_display_f64"
        } else {
            "mimi_set_to_display"
        }
    }

    /// Strip first type argument from `Prefix<A, …>` / `Prefix<A>` → `A`.
    /// Handles nested brackets (e.g. `Result<Option<Map<string, i32>>, i32>`).
    fn strip_first_type_arg(type_name: &str, prefix: &str) -> Option<String> {
        let rest = type_name.strip_prefix(prefix)?.strip_prefix('<')?;
        let mut depth = 0i32;
        for (i, ch) in rest.char_indices() {
            match ch {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth < 0 {
                        return Some(rest[..i].trim().to_string());
                    }
                }
                ',' if depth == 0 => return Some(rest[..i].trim().to_string()),
                _ => {}
            }
        }
        None
    }

    /// Format Result {i1, ok, err} as `Ok(...)` / `Err(...)` (int, string, or record Ok).
    fn emit_result_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        ok_record: Option<&str>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        self.emit_result_to_string_typed(sv, ok_record, "")
    }

    fn emit_result_to_string_typed(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        ok_record: Option<&str>,
        arg_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let fields = sv.get_type().get_field_types();
        let disc = self
            .build_extract_value(sv.into(), 0, "res_disc")?
            .into_int_value();
        let ok_val = self.build_extract_value(sv.into(), 1, "res_ok")?;
        let err_val = self.build_extract_value(sv.into(), 2, "res_err")?;
        let buf_size = i64_ty.const_int(256, false);
        let buf = self.malloc_or_abort(buf_size, "res_print_buf")?;
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module.add_function(
                "snprintf",
                ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent fn".into()))?;
        let ok_bb = self.context.append_basic_block(parent, "res_print_ok");
        let err_bb = self.context.append_basic_block(parent, "res_print_err");
        let merge_bb = self.context.append_basic_block(parent, "res_print_merge");
        let zero = disc.get_type().const_int(0, false);
        let is_ok = self
            .builder
            .build_int_compare(IntPredicate::NE, disc, zero, "res_is_ok")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(is_ok, ok_bb, err_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;

        let emit_arm = |label: &str,
                        bb: inkwell::basic_block::BasicBlock<'ctx>,
                        val: BasicValueEnum<'ctx>,
                        field_ty: BasicTypeEnum<'ctx>|
         -> MimiResult<()> {
            self.builder.position_at_end(bb);
            // Ok arm with named record payload (pointer or ptrtoint).
            if label == "ok" {
                if let Some(rec_name) = ok_record {
                    let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                    let rec_ptr = match val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        BasicValueEnum::IntValue(iv) => self
                            .builder
                            .build_int_to_ptr(iv, i8_ptr, "res_ok_rec")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                        _ => {
                            return Err(CompileError::LlvmError(
                                "Result Ok record payload unexpected kind".into(),
                            ))
                        }
                    };
                    let rec_str = self.emit_record_display(rec_name, rec_ptr)?;
                    let fmt = self
                        .builder
                        .build_global_string_ptr("Ok(%s)", "res_ok_rec_fmt")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(buf_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(rec_str),
                        ],
                        "res_ok_rec_snprintf",
                    )?;
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    return Ok(());
                }
            }
            match field_ty {
                BasicTypeEnum::PointerType(_) if label == "ok" => {
                    let ptr = val.into_pointer_value();
                    // Result of List: pointer to list struct.
                    // Try load as list and format; otherwise %p fallback.
                    let list_ty = self.list_struct_type();
                    let loaded = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(list_ty),
                            ptr,
                            "res_ok_list_ld",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_struct_value();
                    let list_str = self.emit_list_i32_to_string(loaded)?;
                    let fmt = self
                        .builder
                        .build_global_string_ptr("Ok(%s)", "res_ok_list_fmt")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(buf_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(list_str),
                        ],
                        "res_ok_list_snprintf",
                    )?;
                }
                BasicTypeEnum::StructType(sty)
                    if label == "ok"
                        && sty.get_field_types().len() == 2
                        && matches!(
                            sty.get_field_types()[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        )
                        && matches!(sty.get_field_types()[1], BasicTypeEnum::PointerType(_)) =>
                {
                    // Nested List by-value in Result Ok: {i64, ptr}.
                    let list_str = self.emit_list_i32_to_string(val.into_struct_value())?;
                    let fmt = self
                        .builder
                        .build_global_string_ptr("Ok(%s)", "res_ok_list_sv_fmt")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(buf_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(list_str),
                        ],
                        "res_ok_list_sv_snprintf",
                    )?;
                }
                BasicTypeEnum::StructType(sty)
                    if label == "ok"
                        && sty.get_field_types().len() >= 2
                        && !matches!(
                            sty.get_field_types()[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        ) =>
                {
                    // Product tuple by-value in Result Ok: e.g. (i32,i32).
                    let tup_str =
                        self.emit_product_tuple_to_string(val.into_struct_value())?;
                    let fmt = self
                        .builder
                        .build_global_string_ptr("Ok(%s)", "res_ok_tup_fmt")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::IntValue(buf_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(tup_str),
                        ],
                        "res_ok_tup_snprintf",
                    )?;
                }
                BasicTypeEnum::IntType(_) => {
                    let iv = val.into_int_value();
                    let as_i64 = if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(iv, i64_ty, &format!("{}_i64", label))
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    };
                    // Result of Map/Set: Ok payload is opaque handle (i64).
                    if label == "ok"
                        && (arg_type.contains("Map<") || arg_type.contains("Set<"))
                    {
                        let disp = if arg_type.contains("Map<") {
                            let fn_name = Self::map_json_fn_for_type(arg_type);
                            let func = self.get_runtime_fn(fn_name)?;
                            self.build_call(
                                func,
                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                "res_ok_map",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("map to_json void")?
                            .into_pointer_value()
                        } else {
                            let fn_name = Self::set_display_fn_for_type(arg_type);
                            let func = self.get_runtime_fn(fn_name)?;
                            self.build_call(
                                func,
                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                "res_ok_set",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("set display void")?
                            .into_pointer_value()
                        };
                        let fmt = self
                            .builder
                            .build_global_string_ptr("Ok(%s)", "res_ok_ms_fmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(disp),
                            ],
                            "res_ok_ms_snprintf",
                        )?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        return Ok(());
                    }
                    // Result<T,string> stores Err as ptrtoint of heap {ptr,len} string.
                    // Only decode as string when value looks like a heap pointer
                    // (>= 1MB, 8-byte aligned); small integers stay numeric.
                    let min_heap = i64_ty.const_int(1_048_576, false);
                    let is_heapish = if iv.get_type().get_bit_width() == 64 {
                        let ge = self
                            .builder
                            .build_int_compare(
                                IntPredicate::UGE,
                                as_i64,
                                min_heap,
                                &format!("{}_ge_heap", label),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let and7 = self
                            .builder
                            .build_and(
                                as_i64,
                                i64_ty.const_int(7, false),
                                &format!("{}_and7", label),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let aligned = self
                            .builder
                            .build_int_compare(
                                IntPredicate::EQ,
                                and7,
                                i64_ty.const_int(0, false),
                                &format!("{}_aligned", label),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder
                            .build_and(ge, aligned, &format!("{}_heapish", label))
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        self.context.bool_type().const_int(0, false)
                    };

                    let parent_fn = self
                        .builder
                        .get_insert_block()
                        .and_then(|bb| bb.get_parent())
                        .ok_or_else(|| CompileError::LlvmError("no parent".into()))?;
                    let str_bb =
                        self.context
                            .append_basic_block(parent_fn, &format!("res_{}_str", label));
                    let int_bb =
                        self.context
                            .append_basic_block(parent_fn, &format!("res_{}_int", label));
                    let arm_merge = self.context.append_basic_block(
                        parent_fn,
                        &format!("res_{}_arm_merge", label),
                    );
                    self.builder
                        .build_conditional_branch(is_heapish, str_bb, int_bb)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;

                    self.builder.position_at_end(str_bb);
                    {
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let str_sty = self.context.struct_type(
                            &[
                                BasicTypeEnum::PointerType(i8_ptr),
                                BasicTypeEnum::IntType(i64_ty),
                            ],
                            false,
                        );
                        let as_ptr = self
                            .builder
                            .build_int_to_ptr(as_i64, i8_ptr, &format!("{}_as_ptr", label))
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let loaded = self
                            .builder
                            .build_load(
                                BasicTypeEnum::StructType(str_sty),
                                as_ptr,
                                &format!("{}_str_ld", label),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            .into_struct_value();
                        let data_ptr = self
                            .build_extract_value(loaded.into(), 0, &format!("{}_data", label))?
                            .into_pointer_value();
                        let fmt = self
                            .builder
                            .build_global_string_ptr(
                                &format!("{}(%s)", if label == "ok" { "Ok" } else { "Err" }),
                                &format!("res_{}_sfmt", label),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(data_ptr),
                            ],
                            &format!("res_{}_snprintf_s", label),
                        )?;
                    }
                    self.builder
                        .build_unconditional_branch(arm_merge)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;

                    self.builder.position_at_end(int_bb);
                    {
                        let fmt = self
                            .builder
                            .build_global_string_ptr(
                                &format!("{}(%ld)", if label == "ok" { "Ok" } else { "Err" }),
                                &format!("res_{}_fmt", label),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(as_i64),
                            ],
                            &format!("res_{}_snprintf", label),
                        )?;
                    }
                    self.builder
                        .build_unconditional_branch(arm_merge)
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.builder.position_at_end(arm_merge);
                }
                BasicTypeEnum::StructType(sty) => {
                    let fields_st = sty.get_field_types();
                    let sv = val.into_struct_value();
                    // Nested Result {i1, ok, err}
                    if fields_st.len() >= 3
                        && matches!(
                            fields_st[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        )
                    {
                        let nested = self.emit_result_to_string(sv, None)?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr(
                                &format!("{}(%s)", if label == "ok" { "Ok" } else { "Err" }),
                                &format!("res_{}_nfmt", label),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(nested),
                            ],
                            &format!("res_{}_snprintf_n", label),
                        )?;
                    } else if fields_st.len() == 2
                        && matches!(
                            fields_st[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                        )
                    {
                        // Nested Option — pass full Option<…> type from Result Ok arm.
                        let opt_ty = Self::strip_first_type_arg(arg_type, "Result")
                            .unwrap_or_else(|| "Option".to_string());
                        let nested = self.emit_option_to_string(sv, None, &opt_ty)?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr(
                                &format!("{}(%s)", if label == "ok" { "Ok" } else { "Err" }),
                                &format!("res_{}_ofmt", label),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(nested),
                            ],
                            &format!("res_{}_snprintf_o", label),
                        )?;
                    } else if fields_st.len() == 2
                        && matches!(
                            fields_st[0],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 32
                        )
                        && matches!(
                            fields_st[1],
                            BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                        )
                    {
                        // Nested custom enum {i32, i64}
                        let ok_inner = Self::strip_first_type_arg(arg_type, "Result")
                            .unwrap_or_default();
                        let enum_ty = if self.type_defs.get(&ok_inner).is_some_and(|td| {
                            matches!(td.kind, crate::ast::TypeDefKind::Enum(_))
                        }) {
                            Some(ok_inner.as_str())
                        } else {
                            self.type_defs.iter().find_map(|(n, td)| {
                                if matches!(td.kind, crate::ast::TypeDefKind::Enum(_)) {
                                    Some(n.as_str())
                                } else {
                                    None
                                }
                            })
                        };
                        if let Some(et) = enum_ty {
                            let nested = self.emit_enum_display(et, sv)?;
                            let fmt = self
                                .builder
                                .build_global_string_ptr(
                                    &format!("{}(%s)", if label == "ok" { "Ok" } else { "Err" }),
                                    &format!("res_{}_efmt", label),
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(buf),
                                    BasicMetadataValueEnum::IntValue(buf_size),
                                    BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                    BasicMetadataValueEnum::PointerValue(nested),
                                ],
                                &format!("res_{}_snprintf_e", label),
                            )?;
                        }
                    } else if fields_st.len() >= 1
                        && matches!(fields_st[0], BasicTypeEnum::PointerType(_))
                    {
                        // string {ptr,len}
                        let ptr = self
                            .build_extract_value(sv.into(), 0, &format!("{}_str", label))?
                            .into_pointer_value();
                        let fmt = self
                            .builder
                            .build_global_string_ptr(
                                &format!("{}(%s)", if label == "ok" { "Ok" } else { "Err" }),
                                &format!("res_{}_sfmt", label),
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(ptr),
                            ],
                            &format!("res_{}_snprintf_s", label),
                        )?;
                    }
                }
                _ => {
                    let fmt = self
                        .builder
                        .build_global_string_ptr(
                            if label == "ok" { "Ok(?)" } else { "Err(?)" },
                            &format!("res_{}_unk", label),
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let strcpy_fn = self.get_runtime_fn("strcpy")?;
                    self.build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        ],
                        &format!("res_{}_strcpy", label),
                    )?;
                }
            }
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
            Ok(())
        };

        emit_arm("ok", ok_bb, ok_val, fields[1])?;
        emit_arm("err", err_bb, err_val, fields[2])?;
        self.builder.position_at_end(merge_bb);
        Ok(buf)
    }

    /// Format Option {i1, i64} as `Some(...)` / `None()` matching interp Display.
    /// When `inner_record` is Some, payload is ptrtoint of that record type.
    fn emit_option_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        inner_record: Option<&str>,
        arg_type: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let disc = self
            .build_extract_value(sv.into(), 0, "opt_disc")?
            .into_int_value();
        let payload_bv = self.build_extract_value(sv.into(), 1, "opt_pay")?;
        // Classify payload for Some(...) formatting.
        enum OptPay<'a> {
            Int(inkwell::values::IntValue<'a>),
            Float(inkwell::values::FloatValue<'a>),
            StrPtr(inkwell::values::PointerValue<'a>),
            RecPtr(inkwell::values::PointerValue<'a>),
            /// Nested Option payload is ptrtoint of heap Option; load only in Some arm.
            NestedOpt(inkwell::values::IntValue<'a>),
            /// Nested Result payload is ptrtoint of heap Result; load only in Some arm.
            NestedRes(inkwell::values::IntValue<'a>),
        }
        let pay_kind = match payload_bv {
            BasicValueEnum::IntValue(iv) => {
                let bw = iv.get_type().get_bit_width();
                if bw == 1 && inner_record.is_none() {
                    // Bool payload: print true/false via string path.
                    let true_g = self
                        .builder
                        .build_global_string_ptr("true", "opt_bool_t")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let false_g = self
                        .builder
                        .build_global_string_ptr("false", "opt_bool_f")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let zero = iv.get_type().const_int(0, false);
                    let is_t = self
                        .builder
                        .build_int_compare(IntPredicate::NE, iv, zero, "opt_bool")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let sel = self
                        .builder
                        .build_select(
                            is_t,
                            true_g.as_pointer_value(),
                            false_g.as_pointer_value(),
                            "opt_bool_s",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    OptPay::StrPtr(sel.into_pointer_value())
                } else {
                    let as_i64 = if bw < 64 {
                        self.builder
                            .build_int_s_extend(iv, i64_ty, "opt_pay_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        iv
                    };
                    if inner_record.is_some() {
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let p = self
                            .builder
                            .build_int_to_ptr(as_i64, i8_ptr, "opt_rec_from_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        OptPay::RecPtr(p)
                    } else if arg_type == "Option<bool>"
                        || arg_type.ends_with("<bool>")
                        || arg_type.contains("bool>")
                    {
                        // Bool stored as i64 0/1: print true/false.
                        let true_g = self
                            .builder
                            .build_global_string_ptr("true", "opt_bool_t2")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let false_g = self
                            .builder
                            .build_global_string_ptr("false", "opt_bool_f2")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let zero = i64_ty.const_int(0, false);
                        let is_t = self
                            .builder
                            .build_int_compare(IntPredicate::NE, as_i64, zero, "opt_bool2")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let sel = self
                            .builder
                            .build_select(
                                is_t,
                                true_g.as_pointer_value(),
                                false_g.as_pointer_value(),
                                "opt_bool_s2",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        OptPay::StrPtr(sel.into_pointer_value())
                    } else if arg_type.contains("Map<") || arg_type == "Option<Map>" {
                        let fn_name = Self::map_json_fn_for_type(arg_type);
                        let func = self.get_runtime_fn(fn_name)?;
                        let raw = self
                            .build_call(
                                func,
                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                "opt_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("map to_json void")?
                            .into_pointer_value();
                        OptPay::StrPtr(raw)
                    } else if arg_type.contains("Set<") || arg_type == "Option<Set>" {
                        let fn_name = Self::set_display_fn_for_type(arg_type);
                        let func = self.get_runtime_fn(fn_name)?;
                        let raw = self
                            .build_call(
                                func,
                                &[BasicMetadataValueEnum::IntValue(as_i64)],
                                "opt_set_disp",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("set display void")?
                            .into_pointer_value();
                        OptPay::StrPtr(raw)
                    } else if arg_type.contains("List<") || arg_type.starts_with("Option<List") {
                        // Option of List stored as ptrtoint of list struct.
                        // Use runtime helper so null payload (None) is safe — do not
                        // GEP/load before the is_some branch.
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let list_ptr = self
                            .builder
                            .build_int_to_ptr(as_i64, i8_ptr, "opt_list_from_i64")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let fn_name = if arg_type.contains("List<string>") {
                            "mimi_list_to_string"
                        } else {
                            "mimi_list_i32_to_string"
                        };
                        let fn_ty = i8_ptr.fn_type(
                            &[BasicMetadataTypeEnum::PointerType(i8_ptr)],
                            false,
                        );
                        let list_fn = self.module.get_function(fn_name).unwrap_or_else(|| {
                            self.module.add_function(
                                fn_name,
                                fn_ty,
                                Some(inkwell::module::Linkage::External),
                            )
                        });
                        let list_str = self
                            .build_call(
                                list_fn,
                                &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                                "opt_list_disp",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list display void")?
                            .into_pointer_value();
                        OptPay::StrPtr(list_str)
                    } else if arg_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .is_some_and(|inner| inner.starts_with("Option"))
                    {
                        // Defer load of nested Option until Some arm (None has null payload).
                        OptPay::NestedOpt(as_i64)
                    } else if arg_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .is_some_and(|inner| inner.starts_with("Result"))
                    {
                        OptPay::NestedRes(as_i64)
                    } else {
                        OptPay::Int(as_i64)
                    }
                }
            }
            BasicValueEnum::FloatValue(fv) => OptPay::Float(fv),
            BasicValueEnum::PointerValue(pv) => {
                if inner_record.is_some() {
                    OptPay::RecPtr(pv)
                } else if arg_type.starts_with("Option<List")
                    || arg_type.contains("List<")
                {
                    // Option of list: payload is pointer to {i64,ptr} list.
                    let list_ty = self.list_struct_type();
                    let loaded = self
                        .builder
                        .build_load(
                            BasicTypeEnum::StructType(list_ty),
                            pv,
                            "opt_list_ld",
                        )
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        .into_struct_value();
                    let list_str = if arg_type.contains("List<string>") {
                        self.emit_list_string_to_string(loaded)?
                    } else {
                        self.emit_list_i32_to_string(loaded)?
                    };
                    OptPay::StrPtr(list_str)
                } else {
                    OptPay::StrPtr(pv)
                }
            }
            BasicValueEnum::StructValue(psv) => {
                let pfields = psv.get_type().get_field_types();
                // Nested Result {i1, ok, err} inside Option.
                if pfields.len() >= 3
                    && matches!(
                        pfields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                {
                    let res_ty = Self::strip_first_type_arg(arg_type, "Option")
                        .unwrap_or_else(|| "Result".to_string());
                    let nested = self.emit_result_to_string_typed(psv, None, &res_ty)?;
                    OptPay::StrPtr(nested)
                } else if pfields.len() == 2
                    && matches!(
                        pfields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 1
                    )
                {
                    // Nested Option {i1, ...}: recursive Display via emit_option_to_string.
                    // Strip one Option layer: Option<Option<List<i32>>> → Option<List<i32>>
                    let inner_ty = Self::strip_first_type_arg(arg_type, "Option")
                        .unwrap_or_else(|| "Option".to_string());
                    let nested = self.emit_option_to_string(psv, None, &inner_ty)?;
                    OptPay::StrPtr(nested)
                } else if pfields.len() == 2
                    && matches!(
                        pfields[0],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 32
                    )
                    && matches!(
                        pfields[1],
                        BasicTypeEnum::IntType(t) if t.get_bit_width() == 64
                    )
                {
                    // Nested custom enum {i32 tag, i64 payload}.
                    let enum_ty = arg_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .filter(|n| {
                            self.type_defs.get(*n).is_some_and(|td| {
                                matches!(td.kind, crate::ast::TypeDefKind::Enum(_))
                            })
                        })
                        .or_else(|| {
                            // Try find any enum type that matches layout — use first matching.
                            self.type_defs.iter().find_map(|(n, td)| {
                                if matches!(td.kind, crate::ast::TypeDefKind::Enum(_)) {
                                    Some(n.as_str())
                                } else {
                                    None
                                }
                            })
                        });
                    if let Some(et) = enum_ty {
                        let nested = self.emit_enum_display(et, psv)?;
                        OptPay::StrPtr(nested)
                    } else {
                        OptPay::Int(i64_ty.const_int(0, false))
                    }
                } else if pfields.len() >= 1
                    && matches!(pfields[0], BasicTypeEnum::PointerType(_))
                {
                    // string {ptr,len}
                    let dp = self
                        .build_extract_value(psv.into(), 0, "opt_str_ptr")?
                        .into_pointer_value();
                    OptPay::StrPtr(dp)
                } else if pfields.len() >= 2 {
                    // Product tuple / multi-field struct by-value in Option payload.
                    let tup_str = self.emit_product_tuple_to_string(psv)?;
                    OptPay::StrPtr(tup_str)
                } else {
                    OptPay::Int(i64_ty.const_int(0, false))
                }
            }
            other => {
                return Err(CompileError::LlvmError(format!(
                    "option payload unexpected kind {:?}",
                    other
                )))
            }
        };
        let none_str = self
            .builder
            .build_global_string_ptr("None()", "opt_none_str")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let buf_size = i64_ty.const_int(512, false);
        let buf = self.malloc_or_abort(buf_size, "opt_print_buf")?;
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module.add_function(
                "snprintf",
                ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("no parent fn".into()))?;
        let some_bb = self.context.append_basic_block(parent, "opt_print_some");
        let none_bb = self.context.append_basic_block(parent, "opt_print_none");
        let merge_bb = self.context.append_basic_block(parent, "opt_print_merge");
        let zero = disc.get_type().const_int(0, false);
        let is_some = self
            .builder
            .build_int_compare(IntPredicate::NE, disc, zero, "opt_is_some")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder
            .build_conditional_branch(is_some, some_bb, none_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(some_bb);
        match pay_kind {
            OptPay::RecPtr(rec_ptr) => {
                let rec_name = inner_record.unwrap_or("Record");
                let rec_str = self.emit_record_display(rec_name, rec_ptr)?;
                let some_fmt = self
                    .builder
                    .build_global_string_ptr("Some(%s)", "opt_some_sfmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(buf_size),
                        BasicMetadataValueEnum::PointerValue(some_fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(rec_str),
                    ],
                    "opt_some_snprintf_s",
                )?;
            }
            OptPay::StrPtr(sp) => {
                let some_fmt = self
                    .builder
                    .build_global_string_ptr("Some(%s)", "opt_some_str_fmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(buf_size),
                        BasicMetadataValueEnum::PointerValue(some_fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(sp),
                    ],
                    "opt_some_snprintf_str",
                )?;
            }
            OptPay::Int(payload_i64) => {
                let some_fmt = self
                    .builder
                    .build_global_string_ptr("Some(%ld)", "opt_some_fmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(buf_size),
                        BasicMetadataValueEnum::PointerValue(some_fmt.as_pointer_value()),
                        BasicMetadataValueEnum::IntValue(payload_i64),
                    ],
                    "opt_some_snprintf",
                )?;
            }
            OptPay::NestedOpt(as_i64) => {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let nested_ptr = self
                    .builder
                    .build_int_to_ptr(as_i64, i8_ptr, "opt_nested_from_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let opt_sty = self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(self.context.bool_type()),
                        BasicTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                let loaded = self
                    .builder
                    .build_load(
                        BasicTypeEnum::StructType(opt_sty),
                        nested_ptr,
                        "opt_nested_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                let inner_ty = Self::strip_first_type_arg(arg_type, "Option")
                    .unwrap_or_else(|| "Option".to_string());
                let nested = self.emit_option_to_string(loaded, None, &inner_ty)?;
                let some_fmt = self
                    .builder
                    .build_global_string_ptr("Some(%s)", "opt_nested_sfmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(buf_size),
                        BasicMetadataValueEnum::PointerValue(some_fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(nested),
                    ],
                    "opt_nested_snprintf",
                )?;
            }
            OptPay::NestedRes(as_i64) => {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let nested_ptr = self
                    .builder
                    .build_int_to_ptr(as_i64, i8_ptr, "opt_res_from_i64")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let res_sty = self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(self.context.bool_type()),
                        BasicTypeEnum::IntType(i64_ty),
                        BasicTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                let loaded = self
                    .builder
                    .build_load(
                        BasicTypeEnum::StructType(res_sty),
                        nested_ptr,
                        "opt_res_ld",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    .into_struct_value();
                let res_ty = Self::strip_first_type_arg(arg_type, "Option")
                    .unwrap_or_else(|| "Result".to_string());
                let nested = self.emit_result_to_string_typed(loaded, None, &res_ty)?;
                let some_fmt = self
                    .builder
                    .build_global_string_ptr("Some(%s)", "opt_res_sfmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(buf_size),
                        BasicMetadataValueEnum::PointerValue(some_fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(nested),
                    ],
                    "opt_res_snprintf",
                )?;
            }
            OptPay::Float(fv) => {
                let some_fmt = self
                    .builder
                    .build_global_string_ptr("Some(%g)", "opt_some_ffmt")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_call(
                    snprintf_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::IntValue(buf_size),
                        BasicMetadataValueEnum::PointerValue(some_fmt.as_pointer_value()),
                        BasicMetadataValueEnum::FloatValue(fv),
                    ],
                    "opt_some_snprintf_f",
                )?;
            }
        }
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(none_bb);
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(none_str.as_pointer_value()),
            ],
            "opt_none_strcpy",
        )?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.builder.position_at_end(merge_bb);
        Ok(buf)
    }

    /// Format a heterogeneous product/tuple (ints, bools, strings, nested structs).
    fn emit_product_tuple_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let fields = sv.get_type().get_field_types();
        let i64_ty = self.context.i64_type();
        let buf = self.malloc_or_abort(i64_ty.const_int(4096, false), "prod_tup_buf")?;
        let open = self
            .builder
            .build_global_string_ptr("(", "prod_tup_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        let snprintf_fn = self.get_runtime_fn("snprintf")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "prod_tup_open_cpy",
        )?;
        let buf_size = i64_ty.const_int(256, false);
        for (i, ft) in fields.iter().enumerate() {
            if i > 0 {
                let comma = self
                    .builder
                    .build_global_string_ptr(", ", "prod_tup_comma")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_call(
                    strcat_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
                    ],
                    "prod_tup_strcat_comma",
                )?;
            }
            let field_val =
                self.build_extract_value(sv.into(), i as u32, &format!("prod_tup_{}", i))?;
            let piece = self.malloc_or_abort(i64_ty.const_int(256, false), "prod_piece")?;
            match (ft, field_val) {
                (BasicTypeEnum::IntType(it), BasicValueEnum::IntValue(iv)) => {
                    if it.get_bit_width() == 1 {
                        let true_g = self
                            .builder
                            .build_global_string_ptr("true", "prod_true")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let false_g = self
                            .builder
                            .build_global_string_ptr("false", "prod_false")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let zero = iv.get_type().const_int(0, false);
                        let is_t = self
                            .builder
                            .build_int_compare(IntPredicate::NE, iv, zero, "prod_bool")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let sel = self
                            .builder
                            .build_select(
                                is_t,
                                true_g.as_pointer_value(),
                                false_g.as_pointer_value(),
                                "prod_bool_s",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(piece),
                                BasicMetadataValueEnum::PointerValue(sel.into_pointer_value()),
                            ],
                            "prod_bool_cpy",
                        )?;
                    } else {
                        let as_i64 = if iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(iv, i64_ty, "prod_sext")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            iv
                        };
                        let fmt = self
                            .builder
                            .build_global_string_ptr("%ld", "prod_ld")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(piece),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(as_i64),
                            ],
                            "prod_ld_sn",
                        )?;
                    }
                }
                (BasicTypeEnum::StructType(sty), BasicValueEnum::StructValue(fsv)) => {
                    let ffields = sty.get_field_types();
                    if ffields.len() >= 1
                        && matches!(ffields[0], BasicTypeEnum::PointerType(_))
                    {
                        // string {ptr,len}
                        let ptr = self
                            .build_extract_value(fsv.into(), 0, "prod_str_ptr")?
                            .into_pointer_value();
                        let fmt = self
                            .builder
                            .build_global_string_ptr("%s", "prod_s")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(piece),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(ptr),
                            ],
                            "prod_str_sn",
                        )?;
                    } else {
                        // Nested product / list-like — recurse product formatter.
                        let nested = self.emit_product_tuple_to_string(fsv)?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(piece),
                                BasicMetadataValueEnum::PointerValue(nested),
                            ],
                            "prod_nested_cpy",
                        )?;
                    }
                }
                (BasicTypeEnum::PointerType(_), BasicValueEnum::PointerValue(pv)) => {
                    let fmt = self
                        .builder
                        .build_global_string_ptr("%s", "prod_ptr_s")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(piece),
                            BasicMetadataValueEnum::IntValue(buf_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(pv),
                        ],
                        "prod_ptr_sn",
                    )?;
                }
                (BasicTypeEnum::FloatType(_), BasicValueEnum::FloatValue(fv)) => {
                    let fmt = self
                        .builder
                        .build_global_string_ptr("%g", "prod_f")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(piece),
                            BasicMetadataValueEnum::IntValue(buf_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::FloatValue(fv),
                        ],
                        "prod_f_sn",
                    )?;
                }
                _ => {
                    let q = self
                        .builder
                        .build_global_string_ptr("?", "prod_unk")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(piece),
                            BasicMetadataValueEnum::PointerValue(q.as_pointer_value()),
                        ],
                        "prod_unk_cpy",
                    )?;
                }
            }
            self.build_call(
                strcat_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(piece),
                ],
                "prod_strcat_piece",
            )?;
        }
        let close = self
            .builder
            .build_global_string_ptr(")", "prod_tup_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "prod_tup_close",
        )?;
        Ok(buf)
    }

    /// Serialize a product-tuple struct to a JSON array C string (compact,
    /// matching serde_json / interp `to_json` for `Value::Tuple`).
    pub(in crate::codegen) fn emit_product_tuple_to_json(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let fields = sv.get_type().get_field_types();
        let i64_ty = self.context.i64_type();
        let buf = self.malloc_or_abort(i64_ty.const_int(4096, false), "json_tup_buf")?;
        let open = self
            .builder
            .build_global_string_ptr("[", "json_tup_open")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let strcpy_fn = self.get_runtime_fn("strcpy")?;
        let strcat_fn = self.get_runtime_fn("strcat")?;
        let snprintf_fn = self.get_runtime_fn("snprintf")?;
        self.build_call(
            strcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(open.as_pointer_value()),
            ],
            "json_tup_open_cpy",
        )?;
        let buf_size = i64_ty.const_int(512, false);
        for (i, ft) in fields.iter().enumerate() {
            if i > 0 {
                let comma = self
                    .builder
                    .build_global_string_ptr(",", "json_tup_comma")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                self.build_call(
                    strcat_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::PointerValue(comma.as_pointer_value()),
                    ],
                    "json_tup_comma_cat",
                )?;
            }
            let field_val =
                self.build_extract_value(sv.into(), i as u32, &format!("json_tup_{}", i))?;
            let piece = self.malloc_or_abort(i64_ty.const_int(512, false), "json_tup_piece")?;
            match (ft, field_val) {
                (BasicTypeEnum::IntType(it), BasicValueEnum::IntValue(iv)) => {
                    if it.get_bit_width() == 1 {
                        let true_g = self
                            .builder
                            .build_global_string_ptr("true", "json_tup_true")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let false_g = self
                            .builder
                            .build_global_string_ptr("false", "json_tup_false")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let zero = iv.get_type().const_int(0, false);
                        let is_t = self
                            .builder
                            .build_int_compare(IntPredicate::NE, iv, zero, "json_tup_bool")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let sel = self
                            .builder
                            .build_select(
                                is_t,
                                true_g.as_pointer_value(),
                                false_g.as_pointer_value(),
                                "json_tup_bool_s",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(piece),
                                BasicMetadataValueEnum::PointerValue(sel.into_pointer_value()),
                            ],
                            "json_tup_bool_cpy",
                        )?;
                    } else {
                        let as_i64 = if iv.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(iv, i64_ty, "json_tup_sext")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            iv
                        };
                        let fmt = self
                            .builder
                            .build_global_string_ptr("%ld", "json_tup_ld")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(piece),
                                BasicMetadataValueEnum::IntValue(buf_size),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(as_i64),
                            ],
                            "json_tup_ld_sn",
                        )?;
                    }
                }
                (BasicTypeEnum::StructType(sty), BasicValueEnum::StructValue(fsv)) => {
                    let ffields = sty.get_field_types();
                    if ffields.len() == 2
                        && matches!(ffields[0], BasicTypeEnum::PointerType(_))
                        && matches!(
                            ffields[1],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                        )
                    {
                        // string {ptr,len} → JSON-escaped quoted string
                        // (mimi_json_escape_string already wraps with ").
                        let ptr = self
                            .build_extract_value(fsv.into(), 0, "json_tup_str_ptr")?
                            .into_pointer_value();
                        let esc_fn = self.get_runtime_fn("mimi_json_escape_string")?;
                        let escaped = self
                            .build_call(
                                esc_fn,
                                &[BasicMetadataValueEnum::PointerValue(ptr)],
                                "json_tup_esc",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_json_escape_string void")?
                            .into_pointer_value();
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(piece),
                                BasicMetadataValueEnum::PointerValue(escaped),
                            ],
                            "json_tup_esc_cpy",
                        )?;
                    } else {
                        // Nested product tuple.
                        let nested = self.emit_product_tuple_to_json(fsv)?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(piece),
                                BasicMetadataValueEnum::PointerValue(nested),
                            ],
                            "json_tup_nested_cpy",
                        )?;
                    }
                }
                (BasicTypeEnum::FloatType(_), BasicValueEnum::FloatValue(fv)) => {
                    let fmt = self
                        .builder
                        .build_global_string_ptr("%g", "json_tup_f")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        snprintf_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(piece),
                            BasicMetadataValueEnum::IntValue(buf_size),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::FloatValue(fv),
                        ],
                        "json_tup_f_sn",
                    )?;
                }
                _ => {
                    let n = self
                        .builder
                        .build_global_string_ptr("null", "json_tup_null")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    self.build_call(
                        strcpy_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(piece),
                            BasicMetadataValueEnum::PointerValue(n.as_pointer_value()),
                        ],
                        "json_tup_null_cpy",
                    )?;
                }
            }
            self.build_call(
                strcat_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(piece),
                ],
                "json_tup_piece_cat",
            )?;
        }
        let close = self
            .builder
            .build_global_string_ptr("]", "json_tup_close")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        self.build_call(
            strcat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::PointerValue(close.as_pointer_value()),
            ],
            "json_tup_close_cat",
        )?;
        Ok(buf)
    }

    /// Format an all-integer struct (tuple / map_get) as `(v0, v1, ...)`.
    fn emit_int_tuple_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let fields = sv.get_type().get_field_types();
        let i64_ty = self.context.i64_type();
        let mut fmt = String::from("(");
        let mut sprintf_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for (i, ft) in fields.iter().enumerate() {
            if i > 0 {
                fmt.push_str(", ");
            }
            let field_val = self.build_extract_value(sv.into(), i as u32, &format!("tup_{}", i))?;
            let iv = field_val.into_int_value();
            let bw = iv.get_type().get_bit_width();
            // Bool (i1/i8 disc-like small ints used as flags): print true/false
            // only when bit-width is 1; i32/i64 stay numeric.
            if bw == 1 {
                fmt.push_str("%s");
                let true_g = self
                    .builder
                    .build_global_string_ptr("true", "tup_true")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let false_g = self
                    .builder
                    .build_global_string_ptr("false", "tup_false")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let zero = iv.get_type().const_int(0, false);
                let is_true = self
                    .builder
                    .build_int_compare(IntPredicate::NE, iv, zero, "tup_bool")
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                let selected = self
                    .builder
                    .build_select(
                        is_true,
                        true_g.as_pointer_value(),
                        false_g.as_pointer_value(),
                        "tup_bool_str",
                    )
                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                sprintf_args.push(BasicMetadataValueEnum::PointerValue(
                    selected.into_pointer_value(),
                ));
            } else {
                fmt.push_str("%ld");
                let ext = if bw < 64 {
                    // i32 found flags from map_has_key: treat 0/1 as bool-like for display
                    // when field is i32 and value is 0 or 1? Keep numeric for generality.
                    self.builder
                        .build_int_s_extend(iv, i64_ty, "tup_sext")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                } else {
                    iv
                };
                sprintf_args.push(BasicMetadataValueEnum::IntValue(ext));
            }
            let _ = ft;
        }
        fmt.push(')');
        let est = (fmt.len() + fields.len() * 24 + 64) as u64;
        let buf_size = i64_ty.const_int(est, false);
        let buf = self.malloc_or_abort(buf_size, "tup_print_buf")?;
        let fmt_ptr = self
            .builder
            .build_global_string_ptr(&fmt, "tup_print_fmt")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let mut all_args = vec![
            BasicMetadataValueEnum::PointerValue(buf),
            BasicMetadataValueEnum::IntValue(buf_size),
            BasicMetadataValueEnum::PointerValue(fmt_ptr.as_pointer_value()),
        ];
        all_args.extend(sprintf_args);
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module.add_function(
                "snprintf",
                ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        self.build_call(snprintf_fn, &all_args, "tup_snprintf")?;
        Ok(buf)
    }

    /// Materialize a list struct value into an alloca and call the runtime
    /// helper `mimi_list_i32_to_string` to get a printable C string.
    fn emit_list_i32_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let list_struct_ty = self.list_struct_type();
        let alloca = self.build_alloca(list_struct_ty, "print_list_alloca")?;
        self.build_store(alloca, sv)?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
        let callee = self
            .module
            .get_function("mimi_list_i32_to_string")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_list_i32_to_string",
                    fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let raw = self
            .build_call(
                callee,
                &[BasicMetadataValueEnum::PointerValue(alloca)],
                "list_i32_to_str",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_list_i32_to_string returned void")?
            .into_pointer_value();
        Ok(raw)
    }

    /// Materialize a `List<List<T>>` struct value into an alloca and call
    /// `mimi_list_list_to_string` with the appropriate inner-list formatter.
    fn emit_list_list_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        inner_fn_name: &str,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let list_struct_ty = self.list_struct_type();
        let alloca = self.build_alloca(list_struct_ty, "print_list_list_alloca")?;
        self.build_store(alloca, sv)?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let callback_fn_ty =
            i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
        let inner_fn = self.module.get_function(inner_fn_name).unwrap_or_else(|| {
            self.module.add_function(
                inner_fn_name,
                callback_fn_ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        let callback = inner_fn.as_global_value().as_pointer_value();
        let fn_ty = i8_ptr_ty.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
            ],
            false,
        );
        let callee = self
            .module
            .get_function("mimi_list_list_to_string")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_list_list_to_string",
                    fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let raw = self
            .build_call(
                callee,
                &[
                    BasicMetadataValueEnum::PointerValue(alloca),
                    BasicMetadataValueEnum::PointerValue(callback),
                ],
                "list_list_to_str",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_list_list_to_string returned void")?
            .into_pointer_value();
        Ok(raw)
    }

    /// Materialize a list struct value into an alloca and call the runtime
    /// helper `mimi_list_to_string` to get a printable C string for string lists.
    fn emit_list_string_to_string(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let list_struct_ty = self.list_struct_type();
        let alloca = self.build_alloca(list_struct_ty, "print_str_list_alloca")?;
        self.build_store(alloca, sv)?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_ty = i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
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
                "list_to_str",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_list_to_string returned void")?
            .into_pointer_value();
        Ok(raw)
    }

    pub(super) fn compile_print(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "print expects at least 1 argument".to_string(),
            ));
        }
        let i64_ty = self.context.i64_type();
        let arg_type = self
            .pending_print_arg_types
            .first()
            .cloned()
            .unwrap_or_default();
        let (print_arg, fmt_spec) = self.extract_print_arg(&args[0], i64_ty, &arg_type)?;
        let fmt_global = self
            .builder
            .build_global_string_ptr(&fmt_spec, "fmt")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        let mut printf_args = vec![BasicMetadataValueEnum::PointerValue(
            fmt_global.as_pointer_value(),
        )];
        printf_args.push(print_arg);
        let printf = self.get_runtime_fn("printf")?;
        self.build_call(printf, &printf_args, "printf_call")?;
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_eprintln(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "eprintln expects at least 1 argument".to_string(),
            ));
        }
        let i64_ty = self.context.i64_type();
        let arg_type = self
            .pending_print_arg_types
            .first()
            .cloned()
            .unwrap_or_default();
        let (print_arg, mut fmt_spec) = self.extract_print_arg(&args[0], i64_ty, &arg_type)?;
        fmt_spec.push('\n');
        let fmt_global = self
            .builder
            .build_global_string_ptr(&fmt_spec, "efmt")
            .map_err(|e| CompileError::LlvmError(format!("efmt error: {}", e)))?;
        let mut printf_args = vec![BasicMetadataValueEnum::PointerValue(
            fmt_global.as_pointer_value(),
        )];
        printf_args.push(print_arg);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        self.build_call(printf, &printf_args, "eprintf_call")?;
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() || args.len() > 2 {
            return Err(CompileError::WrongArgCount(
                "assert expects 1 or 2 arguments (condition, optional message)".to_string(),
            ));
        }
        let cond = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "assert requires boolean/i64 argument".to_string(),
                ))
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "assert_ok");
        let fail_bb = self.context.append_basic_block(function, "assert_fail");
        self.build_cond_br(cond, ok_bb, fail_bb)?;

        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        if args.len() == 2 {
            // Use custom message
            let msg_ptr = self.extract_raw_str_ptr(&args[1]).map_err(|_| {
                CompileError::TypeMismatch(
                    "assert message argument must be a string pointer".to_string(),
                )
            })?;
            self.build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(msg_ptr)],
                "assert_printf",
            )?;
        } else {
            let fmt_global = self
                .builder
                .build_global_string_ptr("assertion failed\n", "assert_msg")
                .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
            self.build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(
                    fmt_global.as_pointer_value(),
                )],
                "assert_printf",
            )?;
        }
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.build_call(
            exit_fn,
            &[BasicMetadataValueEnum::IntValue(
                self.context.i32_type().const_int(1, false),
            )],
            "assert_exit",
        )?;
        // SAFETY: exit(1) is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert_eq(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "assert_eq expects 2 arguments".to_string(),
            ));
        }
        let a = args[0];
        let b = args[1];
        let eq = match (a, b) {
            (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => self
                .builder
                .build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                let strcmp_fn = self
                    .module
                    .get_function("strcmp")
                    .ok_or_else(|| "strcmp not declared".to_string())?;
                let cmp_result = self
                    .builder
                    .build_call(
                        strcmp_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ],
                        "strcmp_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strcmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                self.builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        cmp_result.into_int_value(),
                        zero,
                        "streq",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
            }
            _ => {
                let l_ptr = self.extract_raw_str_ptr(&a).ok();
                let r_ptr = self.extract_raw_str_ptr(&b).ok();
                if let (Some(l), Some(r)) = (l_ptr, r_ptr) {
                    let strcmp_fn = self
                        .module
                        .get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let cmp_result = self
                        .builder
                        .build_call(
                            strcmp_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(l),
                                BasicMetadataValueEnum::PointerValue(r),
                            ],
                            "strcmp_call",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("strcmp returned void")?;
                    let zero = self.context.i32_type().const_int(0, false);
                    self.builder
                        .build_int_compare(
                            inkwell::IntPredicate::EQ,
                            cmp_result.into_int_value(),
                            zero,
                            "streq",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                } else {
                    return Err(CompileError::TypeMismatch(
                        "assert_eq requires same types".to_string(),
                    ));
                }
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert_eq".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "aeq_ok");
        let fail_bb = self.context.append_basic_block(function, "aeq_fail");
        self.build_cond_br(eq, ok_bb, fail_bb)?;

        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;

        // Print "assertion failed: "
        let prefix = self
            .builder
            .build_global_string_ptr("assertion failed: ", "aeq_prefix")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(
                prefix.as_pointer_value(),
            )],
            "aeq_prefix_call",
        )?;

        // Print left value
        self.build_print_value(printf, &a)?;
        // Print " != "
        let sep = self
            .builder
            .build_global_string_ptr(" != ", "aeq_sep")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(sep.as_pointer_value())],
            "aeq_sep_call",
        )?;
        // Print right value
        self.build_print_value(printf, &b)?;
        // Print newline
        let nl = self
            .builder
            .build_global_string_ptr("\n", "aeq_nl")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(nl.as_pointer_value())],
            "aeq_nl_call",
        )?;

        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.build_call(
            exit_fn,
            &[BasicMetadataValueEnum::IntValue(
                self.context.i32_type().const_int(1, false),
            )],
            "aeq_exit",
        )?;
        // SAFETY: exit(1) is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert_ne(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "assert_ne expects 2 arguments".to_string(),
            ));
        }
        let a = args[0];
        let b = args[1];
        let ne = match (a, b) {
            (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => self
                .builder
                .build_int_compare(inkwell::IntPredicate::NE, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => self
                .builder
                .build_float_compare(inkwell::FloatPredicate::ONE, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                let strcmp_fn = self
                    .module
                    .get_function("strcmp")
                    .ok_or_else(|| "strcmp not declared".to_string())?;
                let cmp_result = self
                    .builder
                    .build_call(
                        strcmp_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ],
                        "strcmp_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strcmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                self.builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        cmp_result.into_int_value(),
                        zero,
                        "strne",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
            }
            _ => {
                let l_ptr = self.extract_raw_str_ptr(&a).ok();
                let r_ptr = self.extract_raw_str_ptr(&b).ok();
                if let (Some(l), Some(r)) = (l_ptr, r_ptr) {
                    let strcmp_fn = self
                        .module
                        .get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let cmp_result = self
                        .builder
                        .build_call(
                            strcmp_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(l),
                                BasicMetadataValueEnum::PointerValue(r),
                            ],
                            "strcmp_call",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("strcmp returned void")?;
                    let zero = self.context.i32_type().const_int(0, false);
                    self.builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            cmp_result.into_int_value(),
                            zero,
                            "strne",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                } else {
                    return Err(CompileError::TypeMismatch(
                        "assert_ne requires same types".to_string(),
                    ));
                }
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert_ne".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "ane_ok");
        let fail_bb = self.context.append_basic_block(function, "ane_fail");
        self.build_cond_br(ne, ok_bb, fail_bb)?;

        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        // Print "assertion failed: "
        let prefix = self
            .builder
            .build_global_string_ptr("assertion failed: ", "ane_prefix")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(
                prefix.as_pointer_value(),
            )],
            "ane_prefix_call",
        )?;
        // Print left value
        self.build_print_value(printf, &a)?;
        // Print " == "
        let sep = self
            .builder
            .build_global_string_ptr(" == ", "ane_sep")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(sep.as_pointer_value())],
            "ane_sep_call",
        )?;
        // Print right value
        self.build_print_value(printf, &b)?;
        // Print newline
        let nl = self
            .builder
            .build_global_string_ptr("\n", "ane_nl")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(nl.as_pointer_value())],
            "ane_nl_call",
        )?;
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.build_call(
            exit_fn,
            &[BasicMetadataValueEnum::IntValue(
                self.context.i32_type().const_int(1, false),
            )],
            "ane_exit",
        )?;
        // SAFETY: exit(1) is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert_approx_eq(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "assert_approx_eq expects 2 arguments".to_string(),
            ));
        }
        let a = args[0];
        let b = args[1];
        let eq = match (a, b) {
            (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                let diff = self
                    .builder
                    .build_float_sub(l, r, "diff")
                    .map_err(|e| CompileError::LlvmError(format!("fsub error: {}", e)))?;
                let fabs_fn = self.module.get_function("fabs").unwrap_or_else(|| {
                    let f64 = self.context.f64_type();
                    let ty = f64.fn_type(
                        &[inkwell::types::BasicMetadataTypeEnum::FloatType(f64)],
                        false,
                    );
                    self.module
                        .add_function("fabs", ty, Some(inkwell::module::Linkage::External))
                });
                let abs_diff = self
                    .builder
                    .build_call(
                        fabs_fn,
                        &[BasicMetadataValueEnum::FloatValue(diff)],
                        "fabs_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("fabs error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("fabs returned void")?
                    .into_float_value();
                let eps = self.context.f64_type().const_float(1e-6);
                self.builder
                    .build_float_compare(inkwell::FloatPredicate::OLT, abs_diff, eps, "approx")
                    .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?
            }
            _ => {
                return Err(CompileError::TypeMismatch(
                    "assert_approx_eq requires same numeric types".to_string(),
                ))
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert_approx_eq".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "aaeq_ok");
        let fail_bb = self.context.append_basic_block(function, "aaeq_fail");
        self.build_cond_br(eq, ok_bb, fail_bb)?;
        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        // Print "assertion failed: "
        let prefix = self
            .builder
            .build_global_string_ptr("assertion failed: ", "aaeq_prefix")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(
                prefix.as_pointer_value(),
            )],
            "aaeq_prefix_call",
        )?;
        // Print left value
        self.build_print_value(printf, &a)?;
        // Print " !≈ "
        let sep = self
            .builder
            .build_global_string_ptr(" !≈ ", "aaeq_sep")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(sep.as_pointer_value())],
            "aaeq_sep_call",
        )?;
        // Print right value
        self.build_print_value(printf, &b)?;
        // Print newline
        let nl = self
            .builder
            .build_global_string_ptr("\n", "aaeq_nl")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.build_call(
            printf,
            &[BasicMetadataValueEnum::PointerValue(nl.as_pointer_value())],
            "aaeq_nl_call",
        )?;
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.build_call(
            exit_fn,
            &[BasicMetadataValueEnum::IntValue(
                self.context.i32_type().const_int(1, false),
            )],
            "aaeq_exit",
        )?;
        // SAFETY: exit(1) is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_input(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() > 1 {
            return Err(CompileError::WrongArgCount(
                "input expects 0 or 1 argument".to_string(),
            ));
        }
        // Allocate buffer (4096 bytes)
        let buf_size = self.context.i64_type().const_int(4096, false);
        // B4: use malloc_or_abort for NULL check.
        let buf = self.malloc_or_abort(buf_size, "input_buf")?;
        // NOTE: not registered — returned value owns the allocation
        // fgets(buf, 4096, stdin)
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let stdin_global = self.module.add_global(i8_ptr_ty, None, "stdin");
        stdin_global.set_linkage(inkwell::module::Linkage::External);
        let stdin_val = self
            .builder
            .build_load(
                BasicTypeEnum::PointerType(i8_ptr_ty),
                stdin_global.as_pointer_value(),
                "stdin",
            )
            .map_err(|e| CompileError::LlvmError(format!("load stdin error: {}", e)))?
            .into_pointer_value();
        let fgets_fn = self.module.get_function("fgets").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i8_ptr.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fgets", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(
            fgets_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::IntValue(buf_size),
                BasicMetadataValueEnum::PointerValue(stdin_val),
            ],
            "fgets_call",
        )?;
        // strlen(buf) for string struct length
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let str_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(buf)],
                "strlen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?;
        // Build string struct { i8*, i64 }
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let str_alloca = self
            .builder
            .build_alloca(string_ty, "input_str")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ptr_gep, buf)?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(len_gep, str_len)?;
        Ok(str_alloca.into())
    }

    pub(super) fn compile_file_exists(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "file_exists expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        // access(path, F_OK) where F_OK = 0
        let i32_ty = self.context.i32_type();
        let access_fn = self.module.get_function("access").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i32_ty),
                ],
                false,
            );
            self.module
                .add_function("access", ty, Some(inkwell::module::Linkage::External))
        });
        let ret = self
            .builder
            .build_call(
                access_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(0, false)),
                ],
                "access_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("access error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("access returned void")?;
        let zero = i32_ty.const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                ret.into_int_value(),
                zero,
                "exists",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let ext: BasicValueEnum = self
            .builder
            .build_int_z_extend(cmp, self.context.i64_type(), "result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
            .into();
        Ok(ext)
    }

    pub(super) fn compile_read_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "read_file expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let bool_ty = self.context.bool_type();
        let i32_ty = self.context.i32_type();
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        // Result<string, string> layout: {i1 disc, string ok, i64 err}
        let result_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::StructType(string_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        let str_alloca = self
            .builder
            .build_alloca(string_ty, "read_str")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let result_alloca = self
            .builder
            .build_alloca(result_ty, "read_result")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;

        // Compute GEPs for string struct fields (used in Ok branch only)
        let str_ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let str_len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        // Compute GEPs for Result struct fields (used in both branches)
        let disc_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 0, "res_disc")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let ok_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 1, "res_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let err_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 2, "res_err")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;

        // fopen(path, "r")
        let mode_str = self
            .builder
            .build_global_string_ptr("r", "read_mode")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let fopen_fn = self.module.get_function("fopen").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i8_ptr.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fopen", ty, Some(inkwell::module::Linkage::External))
        });
        let file = self
            .builder
            .build_call(
                fopen_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ],
                "fopen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fopen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("fopen returned void")?
            .into_pointer_value();

        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function".to_string())?;
        let fopen_ok_bb = self.context.append_basic_block(function, "fopen_ok");
        let fopen_null_bb = self.context.append_basic_block(function, "fopen_null");
        let merge_bb = self.context.append_basic_block(function, "read_merge");

        let is_null = self
            .builder
            .build_int_compare(IntPredicate::EQ, file, i8_ptr_ty.const_zero(), "fopen_null")
            .map_err(|e| CompileError::LlvmError(format!("null compare error: {}", e)))?;
        self.build_cond_br(is_null, fopen_null_bb, fopen_ok_bb)?;

        // ── Ok branch: fopen succeeded ──
        self.builder.position_at_end(fopen_ok_bb);

        // fseek(file, 0, SEEK_END)
        let fseek_fn = self.module.get_function("fseek").unwrap_or_else(|| {
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::IntType(i32_ty),
                ],
                false,
            );
            self.module
                .add_function("fseek", ty, Some(inkwell::module::Linkage::External))
        });
        let fseek_result = self
            .builder
            .build_call(
                fseek_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(file),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(0, false)),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(2, false)), // SEEK_END
                ],
                "fseek_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fseek error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("fseek returned void")?
            .into_int_value();
        let fseek_ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                fseek_result,
                i32_ty.const_int(0, false),
                "fseek_ok",
            )
            .map_err(|e| CompileError::LlvmError(format!("fseek compare: {}", e)))?;
        // ftell(file) -> file size
        let ftell_fn = self.module.get_function("ftell").unwrap_or_else(|| {
            let ty = i64_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
            self.module
                .add_function("ftell", ty, Some(inkwell::module::Linkage::External))
        });
        let file_size = self
            .builder
            .build_call(
                ftell_fn,
                &[BasicMetadataValueEnum::PointerValue(file)],
                "ftell_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("ftell error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("ftell returned void")?
            .into_int_value();
        // Clamp negative file_size to 0
        let zero = i64_ty.const_int(0, false);
        let neg_one = i64_ty.const_int(u64::MAX, false);
        let is_neg_one = self
            .builder
            .build_int_compare(IntPredicate::EQ, file_size, neg_one, "is_neg_one")
            .map_err(|e| CompileError::LlvmError(format!("neg_one compare: {}", e)))?;
        let fseek_failed = self
            .builder
            .build_xor(fseek_ok, bool_ty.const_int(1, false), "fseek_failed")
            .map_err(|e| CompileError::LlvmError(format!("xor error: {}", e)))?;
        let clamp_cond = self
            .builder
            .build_or(fseek_failed, is_neg_one, "clamp_cond")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let file_size = self
            .builder
            .build_select(clamp_cond, zero, file_size, "file_size")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        // rewind(file)
        let rewind_fn = self.module.get_function("rewind").unwrap_or_else(|| {
            let ty = self
                .context
                .void_type()
                .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
            self.module
                .add_function("rewind", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(
            rewind_fn,
            &[BasicMetadataValueEnum::PointerValue(file)],
            "rewind_call",
        )?;
        // malloc(file_size + 1)
        let one = i64_ty.const_int(1, false);
        let alloc_size = self
            .builder
            .build_int_add(file_size, one, "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        // B4: use malloc_or_abort for NULL check.
        let buf = self.malloc_or_abort(alloc_size, "read_buf")?;
        // fread(buf, 1, file_size, file)
        let fread_fn = self.module.get_function("fread").unwrap_or_else(|| {
            let ty = i64_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                ],
                false,
            );
            self.module
                .add_function("fread", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(
            fread_fn,
            &[
                BasicMetadataValueEnum::PointerValue(buf),
                BasicMetadataValueEnum::IntValue(i64_ty.const_int(1, false)),
                BasicMetadataValueEnum::IntValue(file_size),
                BasicMetadataValueEnum::PointerValue(file),
            ],
            "fread_call",
        )?;
        // Null-terminate
        let null_gep = self
            .build_in_bounds_gep(
                BasicTypeEnum::IntType(self.context.i8_type()),
                buf,
                &[file_size],
                "null_byte",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(null_gep, self.context.i8_type().const_int(0, false))?;
        // fclose(file)
        let fclose_fn = self.module.get_function("fclose").unwrap_or_else(|| {
            let ty = i32_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
            self.module
                .add_function("fclose", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(
            fclose_fn,
            &[BasicMetadataValueEnum::PointerValue(file)],
            "fclose_call",
        )?;

        // Build string struct {i8*, i64} and store into Ok
        self.build_store(str_ptr_gep, buf)?;
        self.build_store(str_len_gep, file_size)?;

        self.build_store(disc_gep, bool_ty.const_int(1, false))?;
        let str_val = self.build_load(string_ty, str_alloca, "str_val")?;
        self.build_store(ok_gep, str_val)?;
        self.build_store(err_gep, i64_ty.const_int(0, false))?;
        self.build_br(merge_bb)?;

        // ── Err branch: fopen returned NULL ──
        self.builder.position_at_end(fopen_null_bb);
        let err_msg = self
            .builder
            .build_global_string_ptr("read_file: fopen failed", "read_file_err_msg")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        self.build_store(disc_gep, bool_ty.const_int(0, false))?;
        self.build_store(ok_gep, string_ty.const_zero())?;
        let err_ptr_int =
            self.build_ptr_to_int(err_msg.as_pointer_value(), i64_ty, "err_ptr_int")?;
        self.build_store(err_gep, err_ptr_int)?;
        self.build_br(merge_bb)?;

        // ── Merge ──
        self.builder.position_at_end(merge_bb);
        self.build_load(result_ty, result_alloca, "read_file_loaded")
    }

    pub(super) fn compile_write_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "write_file expects 2 arguments".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let content_ptr = self.extract_raw_str_ptr(&args[1])?;
        // fopen(path, "w")
        let mode_str = self
            .builder
            .build_global_string_ptr("w", "write_mode")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let fopen_fn = self.module.get_function("fopen").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i8_ptr.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fopen", ty, Some(inkwell::module::Linkage::External))
        });
        let file = self
            .builder
            .build_call(
                fopen_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ],
                "fopen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fopen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("fopen returned void")?
            .into_pointer_value();
        // CG-H11 (deep audit): check fopen result for NULL (file not writable).
        let null_check_bb = self.context.append_basic_block(
            self.current_function()
                .ok_or(CompileError::LlvmError("no current function".into()))?,
            "fopen_null_check",
        );
        let write_bb = self.context.append_basic_block(
            self.current_function()
                .ok_or(CompileError::LlvmError("no current function".into()))?,
            "fopen_not_null",
        );
        let is_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                file,
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .const_null(),
                "fopen_is_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(is_null, null_check_bb, write_bb)
            .map_err(|e| CompileError::LlvmError(format!("cbr error: {}", e)))?;
        self.builder.position_at_end(null_check_bb);
        // Return empty string on fopen failure
        let empty_str = self
            .builder
            .build_global_string_ptr("", "empty_str")
            .map_err(|e| CompileError::LlvmError(format!("gstr error: {}", e)))?;
        let ret_val: BasicValueEnum = empty_str.as_pointer_value().into();
        self.build_return(Some(&ret_val))?;
        self.builder.position_at_end(write_bb);
        // strlen(content) for length
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let content_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(content_ptr)],
                "strlen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?;
        // fwrite(content, 1, len, file)
        let fwrite_fn = self.module.get_function("fwrite").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = self.context.i64_type().fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fwrite", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(
            fwrite_fn,
            &[
                BasicMetadataValueEnum::PointerValue(content_ptr),
                BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(1, false)),
                BasicMetadataValueEnum::IntValue(content_len.into_int_value()),
                BasicMetadataValueEnum::PointerValue(file),
            ],
            "fwrite_call",
        )?;
        // fclose(file)
        let i32_ty = self.context.i32_type();
        let fclose_fn = self.module.get_function("fclose").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i32_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
            self.module
                .add_function("fclose", ty, Some(inkwell::module::Linkage::External))
        });
        self.build_call(
            fclose_fn,
            &[BasicMetadataValueEnum::PointerValue(file)],
            "fclose_call",
        )?;
        // Build Result<(), string> Ok struct: {i1 true, i64 0, i64 0}
        let bool_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let ok_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(BasicTypeEnum::StructType(ok_ty), "write_result")?;
        let disc_gep = self
            .gep()
            .build_struct_gep(ok_ty, alloca, 0, "wr_disc")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(disc_gep, bool_ty.const_int(1, false))?;
        let ok_gep = self
            .gep()
            .build_struct_gep(ok_ty, alloca, 1, "wr_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(ok_gep, i64_ty.const_int(0, false))?;
        let err_gep = self
            .gep()
            .build_struct_gep(ok_ty, alloca, 2, "wr_err")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(err_gep, i64_ty.const_int(0, false))?;
        self.build_load(
            BasicTypeEnum::StructType(ok_ty),
            alloca,
            "write_file_loaded",
        )
    }

    /// Print a single value to stdout for assert_eq diagnostics.
    fn build_print_value(
        &self,
        printf: FunctionValue<'ctx>,
        val: &BasicMetadataValueEnum<'ctx>,
    ) -> Result<(), CompileError> {
        match val {
            BasicMetadataValueEnum::IntValue(iv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%lld", "int_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                self.build_call(
                    printf,
                    &[
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::IntValue(*iv),
                    ],
                    "print_int",
                )
                .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%f", "float_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                self.build_call(
                    printf,
                    &[
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::FloatValue(*fv),
                    ],
                    "print_float",
                )
                .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
            }
            BasicMetadataValueEnum::PointerValue(pv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%s", "str_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                self.build_call(
                    printf,
                    &[
                        BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                        BasicMetadataValueEnum::PointerValue(*pv),
                    ],
                    "print_str",
                )
                .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                if let Ok(BasicValueEnum::PointerValue(pv)) =
                    self.build_extract_value((*sv).into(), 0, "str_field")
                {
                    let fmt = self
                        .builder
                        .build_global_string_ptr("%s", "struct_str_fmt")
                        .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                    self.build_call(
                        printf,
                        &[
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(pv),
                        ],
                        "print_struct_str",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // === Directory & path operations (codegen) ===

    fn call_runtime_str_to_bool(
        &self,
        runtime_fn_name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(format!(
                "{} expects 1 argument",
                runtime_fn_name
            )));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self
            .module
            .get_function(runtime_fn_name)
            .ok_or_else(|| CompileError::LlvmError(format!("{} not declared", runtime_fn_name)))?;
        let result = self
            .build_call(
                fn_val,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                &format!("{}_call", runtime_fn_name),
            )
            .map_err(|e| CompileError::LlvmError(format!("{}: {}", runtime_fn_name, e)))?;
        let ret = result
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError(format!("{} returned void", runtime_fn_name)))?;
        Ok(ret)
    }

    fn call_runtime_str_to_str(
        &self,
        runtime_fn_name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(format!(
                "{} expects 1 argument",
                runtime_fn_name
            )));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self
            .module
            .get_function(runtime_fn_name)
            .ok_or_else(|| CompileError::LlvmError(format!("{} not declared", runtime_fn_name)))?;
        let result = self
            .build_call(
                fn_val,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                &format!("{}_call", runtime_fn_name),
            )
            .map_err(|e| CompileError::LlvmError(format!("{}: {}", runtime_fn_name, e)))?;
        let raw_ptr = result
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError(format!("{} returned void", runtime_fn_name)))?;
        // Wrap raw C string into Mimi string struct {ptr, len}
        self.wrap_c_string(raw_ptr.into_pointer_value())
    }

    pub(super) fn compile_listdir(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "listdir expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self
            .module
            .get_function("mimi_listdir")
            .ok_or("mimi_listdir not declared")?;
        let result = self
            .build_call(
                fn_val,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                "listdir_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("listdir: {}", e)))?;
        let list_ptr = result
            .try_as_basic_value_opt()
            .ok_or("listdir returned void")?;
        // Return as opaque pointer (MimiList*)
        Ok(list_ptr)
    }

    pub(super) fn compile_is_dir(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_is_dir", args)
    }

    pub(super) fn compile_is_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_is_file", args)
    }

    pub(super) fn compile_path_join(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "path_join expects 2 arguments".to_string(),
            ));
        }
        let a_ptr = self.extract_raw_str_ptr(&args[0])?;
        let b_ptr = self.extract_raw_str_ptr(&args[1])?;
        let fn_val = self
            .module
            .get_function("mimi_path_join")
            .ok_or("mimi_path_join not declared")?;
        let result = self
            .build_call(
                fn_val,
                &[
                    BasicMetadataValueEnum::PointerValue(a_ptr),
                    BasicMetadataValueEnum::PointerValue(b_ptr),
                ],
                "path_join_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("path_join: {}", e)))?;
        let raw_ptr = result
            .try_as_basic_value_opt()
            .ok_or("path_join returned void")?;
        self.wrap_c_string(raw_ptr.into_pointer_value())
    }

    pub(super) fn compile_path_ext(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_path_ext", args)
    }

    pub(super) fn compile_path_basename(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_path_basename", args)
    }

    pub(super) fn compile_path_dirname(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_path_dirname", args)
    }

    pub(super) fn compile_walk_dir(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "walk_dir expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self
            .module
            .get_function("mimi_walk_dir")
            .ok_or("mimi_walk_dir not declared")?;
        let result = self
            .build_call(
                fn_val,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                "walk_dir_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("walk_dir: {}", e)))?;
        let list_ptr = result
            .try_as_basic_value_opt()
            .ok_or("walk_dir returned void")?;
        Ok(list_ptr)
    }

    pub(super) fn compile_mkdir_p(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_mkdir_p", args)
    }

    pub(super) fn compile_remove_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_remove_file", args)
    }

    // === Process & advanced file operations (codegen) ===

    pub(super) fn compile_exec(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "exec expects 1 argument".to_string(),
            ));
        }
        let cmd_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());

        // Call mimi_exec(cmd) -> MimiExecResult*
        let exec_fn = self.get_runtime_fn("mimi_exec")?;
        let res_ptr = self
            .build_call(
                exec_fn,
                &[BasicMetadataValueEnum::PointerValue(cmd_ptr)],
                "exec_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("exec error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_exec returned void")?
            .into_pointer_value();

        // MimiExecResult layout: { i64 exit_code, i8* stdout, i8* stderr }
        let res_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        );

        // Extract exit_code
        let exit_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 0, "exit_code_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let exit_code_raw = self
            .build_load(self.context.i64_type(), exit_gep, "exit_code_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // Truncate to i32 for ExecResult.exit_code field
        let exit_code = self
            .builder
            .build_int_truncate(exit_code_raw, self.context.i32_type(), "exit_code_i32")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;

        // Extract stdout
        let stdout_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 1, "stdout_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stdout_raw = self
            .build_load(i8_ptr, stdout_gep, "stdout_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stdout_str = self.wrap_c_string(stdout_raw)?;

        // Extract stderr
        let stderr_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 2, "stderr_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stderr_raw = self
            .build_load(i8_ptr, stderr_gep, "stderr_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stderr_str = self.wrap_c_string(stderr_raw)?;

        // Free the runtime struct (not the strings — they're owned by ExecResult)
        let free_struct_fn = self.get_runtime_fn("mimi_exec_free_struct")?;
        self.build_call(
            free_struct_fn,
            &[BasicMetadataValueEnum::PointerValue(res_ptr)],
            "exec_free_struct",
        )?;

        // Build ExecResult LLVM struct { i32, {i8*,i64}, {i8*,i64} }
        let string_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let exec_result_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i32_type()),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(exec_result_ty, "exec_result")?;

        // Store exit_code
        let f0 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 0, "f0")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(f0, exit_code)?;

        // Store stdout string
        let f1 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 1, "f1")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(f1, stdout_str)?;

        // Store stderr string
        let f2 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 2, "f2")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(f2, stderr_str)?;

        Ok(alloca.into())
    }

    pub(super) fn compile_file_stat(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "file_stat expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // Allocate err_out pointer
        let err_alloca = self.build_alloca(i8_ptr, "err_out")?;
        self.build_store(err_alloca, i8_ptr.const_null())?;

        // Call mimi_file_stat(path, &err_out)
        let stat_fn = self.get_runtime_fn("mimi_file_stat")?;
        let stat_ptr = self
            .build_call(
                stat_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(err_alloca),
                ],
                "stat_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("file_stat error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_file_stat returned void")?
            .into_pointer_value();

        // MimiStatResult layout: { i64 size, i64 modified, i64 is_file, i64 is_dir }
        let mimi_stat_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        // Check if stat_ptr is null (error case)
        let is_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                stat_ptr,
                i8_ptr.const_null(),
                "stat_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;

        // Build StatResult LLVM struct { i64, i64, i1, i1 }
        let bool_ty = self.context.bool_type();
        let stat_result_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(bool_ty),
                inkwell::types::BasicTypeEnum::IntType(bool_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(stat_result_ty, "stat_result")?;

        // MEM-C7 (deep audit): use conditional branch instead of select.
        // LLVM evaluates both sides of a select, so GEP+load on NULL would execute
        // even when is_null is true, causing UB. Branch to avoid the GEP entirely.
        let zero_i64 = i64_ty.const_int(0, false);
        let neg_one_i64 = i64_ty.const_int((-1i64) as u64, false);
        let false_val = bool_ty.const_int(0, false);

        let function = self.current_function().ok_or(CompileError::LlvmError(
            "no current function for file_stat".into(),
        ))?;
        let null_bb = self.context.append_basic_block(function, "stat_null_bb");
        let nonnull_bb = self.context.append_basic_block(function, "stat_nonnull_bb");
        let merge_bb = self.context.append_basic_block(function, "stat_merge_bb");

        self.builder
            .build_conditional_branch(is_null, null_bb, nonnull_bb)
            .map_err(|e| CompileError::LlvmError(format!("cbr error: {}", e)))?;

        // Null path: use default values
        self.builder.position_at_end(null_bb);
        let null_size = neg_one_i64;
        let null_mod = zero_i64;
        let null_isf = false_val;
        let null_isd = false_val;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;

        // Non-null path: load fields from stat_ptr
        self.builder.position_at_end(nonnull_bb);
        // size
        let size_gep = self
            .gep()
            .build_struct_gep(mimi_stat_ty, stat_ptr, 0, "size_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let nn_size = self
            .build_load(i64_ty, size_gep, "size_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // modified
        let mod_gep = self
            .gep()
            .build_struct_gep(mimi_stat_ty, stat_ptr, 1, "mod_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let nn_mod = self
            .build_load(i64_ty, mod_gep, "mod_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // is_file
        let isf_gep = self
            .gep()
            .build_struct_gep(mimi_stat_ty, stat_ptr, 2, "isf_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let isf_raw = self
            .build_load(i64_ty, isf_gep, "isf_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let nn_isf = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, isf_raw, zero_i64, "isf_cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        // is_dir
        let isd_gep = self
            .gep()
            .build_struct_gep(mimi_stat_ty, stat_ptr, 3, "isd_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let isd_raw = self
            .build_load(i64_ty, isd_gep, "isd_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let nn_isd = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, isd_raw, zero_i64, "isd_cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("br error: {}", e)))?;

        // Merge: phi nodes for each field
        self.builder.position_at_end(merge_bb);
        let size_phi = self
            .builder
            .build_phi(i64_ty, "size_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        size_phi.add_incoming(&[(&null_size, null_bb), (&nn_size, nonnull_bb)]);
        let size_val: BasicValueEnum = size_phi.as_basic_value();
        let mod_phi = self
            .builder
            .build_phi(i64_ty, "mod_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        mod_phi.add_incoming(&[(&null_mod, null_bb), (&nn_mod, nonnull_bb)]);
        let mod_val: BasicValueEnum = mod_phi.as_basic_value();
        let isf_phi = self
            .builder
            .build_phi(bool_ty, "isf_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        isf_phi.add_incoming(&[(&null_isf, null_bb), (&nn_isf, nonnull_bb)]);
        let isf_val: BasicValueEnum = isf_phi.as_basic_value();
        let isd_phi = self
            .builder
            .build_phi(bool_ty, "isd_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        isd_phi.add_incoming(&[(&null_isd, null_bb), (&nn_isd, nonnull_bb)]);
        let isd_val: BasicValueEnum = isd_phi.as_basic_value();

        // Store into StatResult struct
        let s0 = self
            .gep()
            .build_struct_gep(stat_result_ty, alloca, 0, "s0")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(s0, size_val)?;
        let s1 = self
            .gep()
            .build_struct_gep(stat_result_ty, alloca, 1, "s1")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(s1, mod_val)?;
        let s2 = self
            .gep()
            .build_struct_gep(stat_result_ty, alloca, 2, "s2")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(s2, isf_val)?;
        let s3 = self
            .gep()
            .build_struct_gep(stat_result_ty, alloca, 3, "s3")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(s3, isd_val)?;

        // Free the stat result (uses Rust allocator via Box::from_raw)
        let free_fn = self.get_runtime_fn("mimi_file_stat_free")?;
        self.build_call(
            free_fn,
            &[BasicMetadataValueEnum::PointerValue(stat_ptr)],
            "stat_free",
        )?;

        Ok(alloca.into())
    }

    pub(super) fn compile_append_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "append_file expects 2 arguments".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let content_ptr = self.extract_raw_str_ptr(&args[1])?;

        let append_fn = self.get_runtime_fn("mimi_append_file")?;
        let ret = self
            .build_call(
                append_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(content_ptr),
                ],
                "append_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("append_file error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_append_file returned void")?
            .into_int_value();

        // Convert i64 to bool (i64): ret != 0
        let zero = self.context.i64_type().const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, ret, zero, "append_ok")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let result = self
            .builder
            .build_int_z_extend(cmp, self.context.i64_type(), "append_result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(result.into())
    }

    pub(super) fn compile_set_env(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "set_env expects 2 arguments".to_string(),
            ));
        }
        let key_ptr = self.extract_raw_str_ptr(&args[0])?;
        let val_ptr = self.extract_raw_str_ptr(&args[1])?;

        let set_fn = self.get_runtime_fn("mimi_set_env")?;
        let ret = self
            .build_call(
                set_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                    BasicMetadataValueEnum::PointerValue(val_ptr),
                ],
                "set_env_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("set_env error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_set_env returned void")?
            .into_int_value();

        // Convert i64 to bool (i64): ret != 0
        let zero = self.context.i64_type().const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, ret, zero, "set_env_ok")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let result = self
            .builder
            .build_int_z_extend(cmp, self.context.i64_type(), "set_env_result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(result.into())
    }

    // === Crypto operations (codegen) ===

    pub(super) fn compile_sha256(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_sha256", args)
    }

    pub(super) fn compile_base64_encode(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_base64_encode", args)
    }

    pub(super) fn compile_base64_decode(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_base64_decode", args)
    }

    pub(super) fn compile_format(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "format expects at least 1 argument (template string)".to_string(),
            ));
        }
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // Convert all arguments to string pointers
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        // First arg: number of format arguments
        call_args.push(BasicMetadataValueEnum::IntValue(
            i64_ty.const_int((args.len() - 1) as u64, false),
        ));
        // Second arg: template string
        // Unwrap StructValue {i8*, i64} to PointerValue(i8*) if needed
        let template_val = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => *pv,
            BasicMetadataValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "template_ptr")
                .map_err(|e| {
                    CompileError::LlvmError(format!("extract format template ptr: {}", e))
                })?
                .into_pointer_value(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "format: first arg must be a string template".to_string(),
                ))
            }
        };
        call_args.push(BasicMetadataValueEnum::PointerValue(template_val));
        // Remaining args: convert to string pointers (up to 8)
        for i in 1..args.len().min(9) {
            match &args[i] {
                BasicMetadataValueEnum::PointerValue(pv) => {
                    call_args.push(BasicMetadataValueEnum::PointerValue(*pv));
                }
                BasicMetadataValueEnum::StructValue(sv) => {
                    let data_ptr = self
                        .builder
                        .build_extract_value(*sv, 0, "fmt_str_data")
                        .map_err(|e| {
                            CompileError::LlvmError(format!("format extract str data: {}", e))
                        })?
                        .into_pointer_value();
                    call_args.push(BasicMetadataValueEnum::PointerValue(data_ptr));
                }
                BasicMetadataValueEnum::IntValue(iv) => {
                    let to_i64_fn = self.get_runtime_fn("mimi_to_string_i64")?;
                    let str_result = self
                        .build_call(
                            to_i64_fn,
                            &[BasicMetadataValueEnum::IntValue(*iv)],
                            "to_str_i64",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("to_string error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("mimi_to_string_i64 returned void")?
                        .into_pointer_value();
                    call_args.push(BasicMetadataValueEnum::PointerValue(str_result));
                }
                BasicMetadataValueEnum::FloatValue(fv) => {
                    let to_f64_fn = self.get_runtime_fn("mimi_to_string_f64")?;
                    let str_result = self
                        .build_call(
                            to_f64_fn,
                            &[BasicMetadataValueEnum::FloatValue(*fv)],
                            "to_str_f64",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("to_string error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("mimi_to_string_f64 returned void")?
                        .into_pointer_value();
                    call_args.push(BasicMetadataValueEnum::PointerValue(str_result));
                }
                _ => {
                    call_args.push(BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()));
                }
            }
        }
        // Pad with null pointers if less than 8 args
        while call_args.len() < 10 {
            call_args.push(BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()));
        }
        let format_fn = self.get_runtime_fn("mimi_str_format")?;
        let result_ptr = self
            .build_call(format_fn, &call_args, "format_call")
            .map_err(|e| CompileError::LlvmError(format!("format error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_format returned void")?
            .into_pointer_value();
        // Wrap into canonical string struct {i8*, i64}
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(result_ptr)],
                "fmt_strlen",
            )
            .map_err(|e| CompileError::LlvmError(format!("format strlen: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        self.build_string_struct(result_ptr, len)
    }

    // === Binary I/O & streaming line reading (codegen) ===

    pub(super) fn compile_read_file_partial(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "read_file_partial expects 2 arguments".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let max_bytes = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "read_file_partial: max_bytes must be i64".into(),
                ))
            }
        };
        let func = self.get_runtime_fn("mimi_read_file_partial")?;
        let raw_ptr = self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::IntValue(max_bytes),
                ],
                "read_file_partial_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("read_file_partial error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_read_file_partial returned void")?
            .into_pointer_value();
        self.wrap_c_string(raw_ptr)
    }

    pub(super) fn compile_read_file_bytes(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "read_file_bytes expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let func = self.get_runtime_fn("mimi_read_file_bytes")?;
        let raw_ptr = self
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                "read_file_bytes_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("read_file_bytes error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_read_file_bytes returned void")?
            .into_pointer_value();
        self.wrap_c_string(raw_ptr)
    }

    pub(super) fn compile_write_file_bytes(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "write_file_bytes expects 2 arguments".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let data_ptr = self.extract_raw_str_ptr(&args[1])?;
        let func = self.get_runtime_fn("mimi_write_file_bytes")?;
        let result = self
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                ],
                "write_file_bytes_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("write_file_bytes error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_write_file_bytes returned void")?
            .into_int_value();
        let zero = self.context.i64_type().const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                result,
                zero,
                "write_file_bytes_ok",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        Ok(cmp.into())
    }

    pub(super) fn compile_read_lines_json(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "read_lines_json expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let func = self.get_runtime_fn("mimi_read_lines_json")?;
        let raw_ptr = self
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(path_ptr)],
                "read_lines_json_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("read_lines_json error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_read_lines_json returned void")?
            .into_pointer_value();
        self.wrap_c_string(raw_ptr)
    }

    pub(super) fn compile_exec_safe(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // exec_safe(prog, arg1, arg2, …) → mimi_exec_safe(prog, MimiList* argv).
        // Pack remaining string args into a temporary {len, data} list (null
        // when no extra args). Matches interpreter varargs semantics.
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "exec_safe expects at least 1 argument (program)".to_string(),
            ));
        }
        let prog_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let args_list = if args.len() == 1 {
            i8_ptr.const_null()
        } else {
            // Pack argv[1..] as C-string pointers into a MimiList on the stack.
            let n = (args.len() - 1) as u64;
            let n_iv = i64_ty.const_int(n, false);
            let ptr_size = i64_ty.const_int(8, false);
            let data_bytes = self
                .builder
                .build_int_mul(n_iv, ptr_size, "exec_argv_bytes")
                .map_err(|e| CompileError::LlvmError(format!("mul: {}", e)))?;
            let data_raw = self.malloc_or_abort(data_bytes, "exec_argv_data")?;
            for (i, arg) in args.iter().skip(1).enumerate() {
                let s_ptr = self.extract_raw_str_ptr(arg)?;
                let idx = i64_ty.const_int(i as u64, false);
                let slot = self
                    .gep()
                    .build_in_bounds_gep(
                        BasicTypeEnum::PointerType(i8_ptr),
                        data_raw,
                        &[idx],
                        &format!("exec_argv_{}", i),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.build_store(slot, s_ptr)?;
            }
            let list_ty = self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            let list_alloca = self.build_alloca(BasicTypeEnum::StructType(list_ty), "exec_argv")?;
            self.build_store(
                self.gep()
                    .build_struct_gep(BasicTypeEnum::StructType(list_ty), list_alloca, 0, "alen")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
                n_iv,
            )?;
            self.build_store(
                self.gep()
                    .build_struct_gep(BasicTypeEnum::StructType(list_ty), list_alloca, 1, "adata")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
                data_raw,
            )?;
            list_alloca
        };
        let exec_fn = self.get_runtime_fn("mimi_exec_safe")?;
        let res_ptr = self
            .build_call(
                exec_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(prog_ptr),
                    BasicMetadataValueEnum::PointerValue(args_list),
                ],
                "exec_safe_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("exec_safe error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_exec_safe returned void")?
            .into_pointer_value();

        // Reuse the same MimiExecResult → ExecResult lowering as compile_exec.
        let res_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        );
        let exit_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 0, "exit_code_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let exit_code_raw = self
            .build_load(self.context.i64_type(), exit_gep, "exit_code_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let exit_code = self
            .builder
            .build_int_truncate(exit_code_raw, self.context.i32_type(), "exit_code_i32")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
        let stdout_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 1, "stdout_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stdout_raw = self
            .build_load(i8_ptr, stdout_gep, "stdout_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stdout_str = self.wrap_c_string(stdout_raw)?;
        let stderr_gep = self
            .gep()
            .build_struct_gep(res_ty, res_ptr, 2, "stderr_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stderr_raw = self
            .build_load(i8_ptr, stderr_gep, "stderr_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stderr_str = self.wrap_c_string(stderr_raw)?;
        let free_struct_fn = self.get_runtime_fn("mimi_exec_free_struct")?;
        self.build_call(
            free_struct_fn,
            &[BasicMetadataValueEnum::PointerValue(res_ptr)],
            "exec_safe_free_struct",
        )?;
        let string_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let exec_result_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i32_type()),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(exec_result_ty, "exec_safe_result")?;
        let f0 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 0, "es_f0")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(f0, exit_code)?;
        let f1 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 1, "es_f1")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(f1, stdout_str)?;
        let f2 = self
            .gep()
            .build_struct_gep(exec_result_ty, alloca, 2, "es_f2")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(f2, stderr_str)?;
        self.build_load(exec_result_ty, alloca, "exec_safe_val")
    }

    pub(super) fn compile_exec_pipe(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "exec_pipe expects 1 argument".to_string(),
            ));
        }
        let cmd_ptr = self.extract_raw_str_ptr(&args[0])?;
        let func = self.get_runtime_fn("mimi_exec_pipe")?;
        let raw_ptr = self
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(cmd_ptr)],
                "exec_pipe_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("exec_pipe error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_exec_pipe returned void")?
            .into_pointer_value();
        self.wrap_c_string(raw_ptr)
    }
}
