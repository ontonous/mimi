//! C ABI wrapper generation for `extern "C"` exported Mimi functions.
//!
//! When a Mimi function is declared as `extern "C" func foo(...) -> T`, the
//! compiled symbol that C callers see must obey the C calling convention and
//! use C-level types (`int32_t`, `char*`, packed structs, function pointers).
//! Mimi's internal codegen, however, uses its own value representation:
//! `i32` is stored as `i64`, `string` is `{ptr, len}`, closures are
//! `{fn_ptr, env_ptr}`, and `#[repr(C)]` records use an internal layout.
//!
//! To keep the internal representation unchanged while presenting a correct C
//! ABI, we compile the function body as an *internal* function
//! `foo__mimi_export_body` and emit an exported wrapper `foo` that converts
//! arguments from C to internal, calls the body, and converts the result back.

use crate::ast::{Field, FuncDef, Type, TypeDefKind};
use crate::codegen::types;
use crate::codegen::{CallSiteValueExt, CodeGenerator};
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValue, BasicValueEnum};
use inkwell::AddressSpace;
use std::collections::HashMap;

/// A `#[repr(C)]` record is "simple" if it fits in a single 64-bit integer
/// under the System V AMD64 ABI: all fields are `i32` and there are at most
/// two fields. Such records are passed/returned as a single `i64` in LLVM IR.
fn is_simple_reprc_record(fields: &[Field]) -> bool {
    if fields.len() > 2 {
        return false;
    }
    fields
        .iter()
        .all(|f| matches!(&f.ty, Type::Name(n, _) if n == "i32"))
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Compile an exported `extern "C"` function by emitting a C-ABI wrapper
    /// around an already-compiled internal body function.
    pub(super) fn compile_export_wrapper(
        &mut self,
        func: &FuncDef,
        body_name: &str,
    ) -> MimiResult<()> {
        let abi = func.extern_abi.as_deref().unwrap_or("C");

        // C ABI return type.
        let c_ret_ty = match &func.ret {
            Some(ty) => self.c_abi_llvm_type(ty)?,
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };

        // C ABI parameter types.
        let c_param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = func
            .params
            .iter()
            .map(|p| {
                let ty = self.c_abi_llvm_type(&p.ty)?;
                Ok(types::basic_to_metadata(self.context, ty))
            })
            .collect::<MimiResult<Vec<_>>>()?;

        let fn_type = fn_type_for_basic_type(c_ret_ty, &c_param_tys)?;
        let function = self.module.add_function(
            &func.name,
            fn_type,
            Some(inkwell::module::Linkage::External),
        );
        let cc = crate::ffi::abi_to_llvm_call_conv(abi);
        function.set_call_conventions(cc);

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.push_cap_scope();
        self.push_comp_scope();
        self.push_heap_scope();

        let body_fn = self.module.get_function(body_name).ok_or_else(|| {
            CompileError::LlvmError(format!("export body '{}' not found", body_name))
        })?;

        let mut vars: HashMap<String, (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)> =
            HashMap::new();
        let mut body_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();

        for (i, param) in func.params.iter().enumerate() {
            let c_val = function
                .get_nth_param(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("export param {} not found", i)))?;
            let internal_val = self.convert_c_arg_to_internal(c_val, &param.ty)?;
            let internal_ty = self
                .llvm_type_for(&param.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            let alloca = self.build_alloca(internal_ty, &param.name)?;
            self.build_store(alloca, internal_val)?;

            // Track type metadata for method dispatch etc.
            if let Type::Name(tn, args) = &param.ty {
                if tn == "List" && !args.is_empty() {
                    if let Some(full) = self.get_full_type_name(&param.ty) {
                        self.var_type_names.insert(param.name.clone(), full);
                    }
                } else {
                    self.var_type_names.insert(param.name.clone(), tn.clone());
                }
            }
            self.register_list_elem_type(&param.name, &param.ty);

            vars.insert(param.name.clone(), (alloca, internal_ty));
            let loaded = self.build_load(internal_ty, alloca, &format!("{}_load", param.name))?;
            body_args.push(basic_value_to_metadata_value(&loaded));
        }

        let body_ret = self
            .build_call(body_fn, &body_args, "export_body_call")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("export body returned void".into()))?;

        let c_ret_val = self.convert_internal_ret_to_c(body_ret, func.ret.as_ref())?;
        self.build_return(Some(&c_ret_val))?;

        self.pop_shared_scope()?;
        self.free_heap_allocs()?;
        self.pop_comp_scope();
        self.pop_cap_scope();

        Ok(())
    }

    /// Map a Mimi type to the LLVM type used at the C ABI boundary for
    /// exported functions.
    fn c_abi_llvm_type(&self, ty: &Type) -> MimiResult<BasicTypeEnum<'ctx>> {
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" => Ok(BasicTypeEnum::IntType(self.context.i32_type())),
                "i64" => Ok(BasicTypeEnum::IntType(self.context.i64_type())),
                "f64" => Ok(BasicTypeEnum::FloatType(self.context.f64_type())),
                "bool" => Ok(BasicTypeEnum::IntType(self.context.i8_type())),
                "string" => Ok(BasicTypeEnum::PointerType(
                    self.context.ptr_type(AddressSpace::default()),
                )),
                "unit" => Ok(BasicTypeEnum::IntType(self.context.i64_type())),
                _ => {
                    if self.repr_c_record_names.contains(name.as_str()) {
                        let td = self.type_defs.get(name.as_str()).ok_or_else(|| {
                            CompileError::LlvmError(format!("unknown repr(C) record '{}'", name))
                        })?;
                        if let TypeDefKind::Record(fields) = &td.kind {
                            if is_simple_reprc_record(fields) {
                                Ok(BasicTypeEnum::IntType(self.context.i64_type()))
                            } else {
                                Ok(BasicTypeEnum::PointerType(
                                    self.context.ptr_type(AddressSpace::default()),
                                ))
                            }
                        } else {
                            Err(CompileError::LlvmError(format!(
                                "'{}' is not a record",
                                name
                            )))
                        }
                    } else if self.record_type_names.contains(name.as_str()) || name == "List" {
                        // Non-repr(C) records and lists cross the boundary as JSON strings.
                        Ok(BasicTypeEnum::PointerType(
                            self.context.ptr_type(AddressSpace::default()),
                        ))
                    } else {
                        Err(CompileError::LlvmError(format!(
                            "type '{}' has no C ABI representation",
                            name
                        )))
                    }
                }
            },
            Type::Func(_, _) | Type::ExternFunc(_, _) => Ok(BasicTypeEnum::PointerType(
                self.context.ptr_type(AddressSpace::default()),
            )),
            Type::Tuple(_) => Ok(BasicTypeEnum::PointerType(
                self.context.ptr_type(AddressSpace::default()),
            )),
            _ => Err(CompileError::LlvmError(format!(
                "type '{}' has no C ABI representation",
                crate::core::fmt_type(ty)
            ))),
        }
    }

    /// Convert a value received from C into Mimi's internal representation.
    fn convert_c_arg_to_internal(
        &mut self,
        c_val: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" => {
                    let iv = c_val.into_int_value();
                    Ok(self
                        .builder
                        .build_int_s_extend(iv, self.context.i64_type(), "carg_i32_ext")
                        .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))?
                        .into())
                }
                "bool" => {
                    let iv = c_val.into_int_value();
                    let zero = self.context.i8_type().const_int(0, false);
                    let bool_val = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::NE, iv, zero, "carg_bool_cmp")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                    Ok(bool_val.into())
                }
                "i64" | "f64" | "unit" => Ok(c_val),
                "string" => self.wrap_c_string(c_val.into_pointer_value()),
                _ => {
                    if self.repr_c_record_names.contains(name.as_str()) {
                        self.convert_c_reprc_record_to_internal(c_val, name)
                    } else if self.record_type_names.contains(name.as_str()) || name == "List" {
                        // JSON-serialized types: not yet supported in export wrapper.
                        Err(CompileError::LlvmError(format!(
                            "export wrapper: JSON boundary for '{}' not implemented",
                            name
                        )))
                    } else {
                        Err(CompileError::LlvmError(format!(
                            "export wrapper: unsupported argument type '{}'",
                            name
                        )))
                    }
                }
            },
            Type::Func(params, ret) | Type::ExternFunc(params, ret) => {
                let fn_ptr = c_val.into_pointer_value();
                let trampoline =
                    self.get_or_create_export_callback_trampoline(params.as_slice(), ret.as_ref())?;
                let closure_ty = types::closure_struct_type(self.context);
                let alloca =
                    self.build_alloca(BasicTypeEnum::StructType(closure_ty), "cb_closure")?;
                let fn_gep = self
                    .gep()
                    .build_struct_gep(closure_ty, alloca, 0, "cb_fn_gep")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.build_store(fn_gep, trampoline)?;
                let env_gep = self
                    .gep()
                    .build_struct_gep(closure_ty, alloca, 1, "cb_env_gep")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.build_store(env_gep, fn_ptr)?;
                let loaded = self.build_load(
                    BasicTypeEnum::StructType(closure_ty),
                    alloca,
                    "cb_closure_load",
                )?;
                Ok(loaded)
            }
            _ => Err(CompileError::LlvmError(format!(
                "export wrapper: unsupported argument type '{}'",
                crate::core::fmt_type(ty)
            ))),
        }
    }

    /// Convert a Mimi internal return value to the C ABI return type.
    fn convert_internal_ret_to_c(
        &mut self,
        internal_val: BasicValueEnum<'ctx>,
        ty: Option<&Type>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let unit_ty = Type::Name("unit".to_string(), vec![]);
        let ty = ty.unwrap_or(&unit_ty);
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" => {
                    let iv = internal_val.into_int_value();
                    Ok(self
                        .builder
                        .build_int_truncate(iv, self.context.i32_type(), "cret_i32_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?
                        .into())
                }
                "bool" => {
                    let iv = internal_val.into_int_value();
                    Ok(self
                        .builder
                        .build_int_z_extend(iv, self.context.i8_type(), "cret_bool_ext")
                        .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
                        .into())
                }
                "i64" | "f64" | "unit" => Ok(internal_val),
                "string" => {
                    let sv = internal_val.into_struct_value();
                    let ptr = self
                        .builder
                        .build_extract_value(sv, 0, "cret_str_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
                    Ok(ptr)
                }
                _ => {
                    if self.repr_c_record_names.contains(name.as_str()) {
                        self.convert_internal_reprc_record_to_c(internal_val, name)
                    } else {
                        Err(CompileError::LlvmError(format!(
                            "export wrapper: unsupported return type '{}'",
                            name
                        )))
                    }
                }
            },
            _ => Err(CompileError::LlvmError(format!(
                "export wrapper: unsupported return type '{}'",
                crate::core::fmt_type(ty)
            ))),
        }
    }

    /// Convert a simple #[repr(C)] record from its C ABI representation
    /// (packed i64 or pointer) to Mimi's internal struct representation.
    fn convert_c_reprc_record_to_internal(
        &mut self,
        c_val: BasicValueEnum<'ctx>,
        name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let td = self.type_defs.get(name).ok_or_else(|| {
            CompileError::LlvmError(format!("repr(C) record '{}' not found", name))
        })?;
        let fields = match &td.kind {
            TypeDefKind::Record(fields) => fields.clone(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "'{}' is not a record",
                    name
                )))
            }
        };
        let internal_sty = self
            .type_llvm
            .get(name)
            .and_then(|t| match t {
                BasicTypeEnum::StructType(s) => Some(*s),
                _ => None,
            })
            .ok_or_else(|| {
                CompileError::LlvmError(format!("internal type for '{}' missing", name))
            })?;

        if is_simple_reprc_record(&fields) {
            let packed = c_val.into_int_value();
            let i64_ty = self.context.i64_type();
            let i32_ty = self.context.i32_type();
            let mut field_vals: Vec<BasicValueEnum<'ctx>> = Vec::new();
            for (fi, f) in fields.iter().enumerate() {
                let raw_i32 = if fi == 0 {
                    self.builder
                        .build_int_truncate(packed, i32_ty, &format!("{}_{}_lo", name, f.name))
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?
                } else {
                    let shifted = self
                        .builder
                        .build_right_shift(
                            packed,
                            i64_ty.const_int((fi * 32) as u64, false),
                            false,
                            &format!("{}_{}_shifted", name, f.name),
                        )
                        .map_err(|e| CompileError::LlvmError(format!("shift error: {}", e)))?;
                    self.builder
                        .build_int_truncate(shifted, i32_ty, &format!("{}_{}_hi", name, f.name))
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?
                };
                let ext = self
                    .builder
                    .build_int_s_extend(raw_i32, i64_ty, &format!("{}_{}_ext", name, f.name))
                    .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))?;
                field_vals.push(ext.into());
            }
            Ok(internal_sty
                .const_named_struct(&field_vals)
                .as_basic_value_enum())
        } else {
            // Complex records: C ABI passes a pointer to the C-layout struct.
            let c_ptr = c_val.into_pointer_value();
            let c_sty = self.c_layout_struct_type(&fields)?;
            let c_typed_ptr = self.build_pointer_cast(
                c_ptr,
                self.context.ptr_type(AddressSpace::default()),
                "crecord_ptr",
            )?;
            let loaded = self.build_load(
                BasicTypeEnum::StructType(c_sty),
                c_typed_ptr,
                "crecord_load",
            )?;
            let mut field_vals = Vec::new();
            for (fi, f) in fields.iter().enumerate() {
                let raw = self
                    .builder
                    .build_extract_value(
                        loaded.into_struct_value(),
                        fi as u32,
                        &format!("{}_field_{}", name, fi),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
                field_vals.push(self.convert_c_field_to_internal(raw, &f.ty)?);
            }
            Ok(internal_sty
                .const_named_struct(&field_vals)
                .as_basic_value_enum())
        }
    }

    /// Convert a simple #[repr(C)] record from Mimi's internal struct
    /// representation to its C ABI representation.
    fn convert_internal_reprc_record_to_c(
        &mut self,
        internal_val: BasicValueEnum<'ctx>,
        name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let td = self.type_defs.get(name).ok_or_else(|| {
            CompileError::LlvmError(format!("repr(C) record '{}' not found", name))
        })?;
        let fields = match &td.kind {
            TypeDefKind::Record(fields) => fields.clone(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "'{}' is not a record",
                    name
                )))
            }
        };

        if is_simple_reprc_record(&fields) {
            let sv = internal_val.into_struct_value();
            let i64_ty = self.context.i64_type();
            let i32_ty = self.context.i32_type();
            let mut packed = i64_ty.const_int(0, false);
            for (fi, f) in fields.iter().enumerate() {
                let raw = self
                    .builder
                    .build_extract_value(sv, fi as u32, &format!("{}_{}_raw", name, f.name))
                    .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
                let truncated = self
                    .builder
                    .build_int_truncate(
                        raw.into_int_value(),
                        i32_ty,
                        &format!("{}_{}_trunc", name, f.name),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
                let zext = self
                    .builder
                    .build_int_z_extend(truncated, i64_ty, &format!("{}_{}_zext", name, f.name))
                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
                if fi == 0 {
                    packed = zext;
                } else {
                    let shifted = self
                        .builder
                        .build_left_shift(
                            zext,
                            i64_ty.const_int((fi * 32) as u64, false),
                            &format!("{}_{}_shifted", name, f.name),
                        )
                        .map_err(|e| CompileError::LlvmError(format!("shift error: {}", e)))?;
                    packed = self
                        .builder
                        .build_or(packed, shifted, &format!("{}_{}_packed", name, f.name))
                        .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
                }
            }
            Ok(packed.into())
        } else {
            Err(CompileError::LlvmError(format!(
                "export wrapper: complex repr(C) record return '{}' not implemented",
                name
            )))
        }
    }

    /// Build a C-layout LLVM struct type for a list of record fields.
    fn c_layout_struct_type(
        &self,
        fields: &[Field],
    ) -> MimiResult<inkwell::types::StructType<'ctx>> {
        let mut field_tys = Vec::new();
        for f in fields {
            field_tys.push(self.c_abi_llvm_type(&f.ty)?);
        }
        Ok(self.context.struct_type(&field_tys, false))
    }

    /// Convert a single C-layout record field to its internal representation.
    fn convert_c_field_to_internal(
        &mut self,
        c_val: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" => Ok(self
                    .builder
                    .build_int_s_extend(
                        c_val.into_int_value(),
                        self.context.i64_type(),
                        "field_i32_ext",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))?
                    .into()),
                "bool" => {
                    let zero = self.context.i8_type().const_int(0, false);
                    let b = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            c_val.into_int_value(),
                            zero,
                            "field_bool_cmp",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                    Ok(b.into())
                }
                "i64" | "f64" => Ok(c_val),
                _ => Err(CompileError::LlvmError(format!(
                    "export wrapper: unsupported record field type '{}'",
                    name
                ))),
            },
            _ => Err(CompileError::LlvmError(format!(
                "export wrapper: unsupported record field type '{}'",
                crate::core::fmt_type(ty)
            ))),
        }
    }

    /// Get or create a trampoline that adapts a C function pointer into the
    /// Mimi closure ABI for callbacks passed *to* Mimi from C.
    ///
    /// The trampoline has the Mimi closure signature
    /// `fn(env: i8*, internal_args...) -> i64` and internally calls the C
    /// function pointer stored in `env` with the correctly narrowed C ABI
    /// argument types.
    fn get_or_create_export_callback_trampoline(
        &mut self,
        cb_params: &[Type],
        cb_ret: &Type,
    ) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let fingerprint = format!("{:?}_{:?}", cb_params, cb_ret);
        if let Some(ptr) = self.export_callback_trampolines.get(&fingerprint) {
            return Ok(*ptr);
        }

        let id = self.export_callback_thunk_counter;
        self.export_callback_thunk_counter += 1;
        let i8_ptr = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // Internal closure ABI: env + internal params -> i64 (or f64 for float returns).
        let mut internal_param_meta = vec![BasicMetadataTypeEnum::PointerType(i8_ptr)];
        for p in cb_params {
            let resolved = self.resolve_type(p);
            let ty = self
                .llvm_type_for(&resolved)
                .unwrap_or(BasicTypeEnum::IntType(i64_ty));
            internal_param_meta.push(types::basic_to_metadata(self.context, ty));
        }

        let internal_ret_ty: BasicTypeEnum<'ctx> = match cb_ret {
            Type::Name(n, _) if n == "f64" => BasicTypeEnum::FloatType(self.context.f64_type()),
            _ => BasicTypeEnum::IntType(i64_ty),
        };

        let tramp_fn_type = fn_type_for_basic_type(internal_ret_ty, &internal_param_meta)?;
        let tramp_fn = self.module.add_function(
            &format!("__mimi_export_cb_trampoline_{}", id),
            tramp_fn_type,
            Some(inkwell::module::Linkage::Internal),
        );

        let saved_block = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(tramp_fn, "entry");
        self.builder.position_at_end(entry);

        let env_ptr = tramp_fn
            .get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("trampoline env missing".into()))?
            .into_pointer_value();

        // Build the C function pointer type.
        let c_ret_ty = self.c_abi_llvm_type(cb_ret)?;
        let c_param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = cb_params
            .iter()
            .map(|p| {
                let ty = self.c_abi_llvm_type(p)?;
                Ok(types::basic_to_metadata(self.context, ty))
            })
            .collect::<MimiResult<Vec<_>>>()?;
        let c_fn_type = fn_type_for_basic_type(c_ret_ty, &c_param_tys)?;
        let i8_ptr_ty = self.context.ptr_type(AddressSpace::default());
        let c_fn_ptr = self.build_pointer_cast(env_ptr, i8_ptr_ty, "cb_c_fn")?;

        let mut c_args = Vec::new();
        for (i, p) in cb_params.iter().enumerate() {
            let internal_val = tramp_fn
                .get_nth_param((i + 1) as u32)
                .ok_or_else(|| CompileError::LlvmError("trampoline param missing".into()))?;
            c_args.push(self.convert_internal_arg_to_c_callback_arg(internal_val, p)?);
        }

        let c_ret = self
            .builder
            .build_indirect_call(c_fn_type, c_fn_ptr, &c_args, "cb_call")
            .map_err(|e| CompileError::LlvmError(format!("indirect call error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("callback call returned void".into()))?;
        let internal_ret = self.convert_c_callback_ret_to_internal(c_ret, cb_ret)?;
        self.build_return(Some(&internal_ret))?;

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        let ptr = tramp_fn.as_global_value().as_pointer_value();
        self.export_callback_trampolines.insert(fingerprint, ptr);
        Ok(ptr)
    }

    /// Narrow an internal closure argument to the C ABI type expected by a
    /// callback function pointer.
    fn convert_internal_arg_to_c_callback_arg(
        &mut self,
        internal_val: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> MimiResult<BasicMetadataValueEnum<'ctx>> {
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" => {
                    let truncated = self
                        .builder
                        .build_int_truncate(
                            internal_val.into_int_value(),
                            self.context.i32_type(),
                            "cb_arg_i32",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
                    Ok(BasicMetadataValueEnum::IntValue(truncated))
                }
                "bool" => {
                    let truncated = self
                        .builder
                        .build_int_truncate(
                            internal_val.into_int_value(),
                            self.context.i8_type(),
                            "cb_arg_bool",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
                    Ok(BasicMetadataValueEnum::IntValue(truncated))
                }
                "i64" | "f64" => Ok(basic_value_to_metadata_value(&internal_val)),
                _ => Err(CompileError::LlvmError(format!(
                    "callback arg type '{}' not supported",
                    name
                ))),
            },
            _ => Err(CompileError::LlvmError(format!(
                "callback arg type '{}' not supported",
                crate::core::fmt_type(ty)
            ))),
        }
    }

    /// Widen a callback return value from C ABI type back to the internal
    /// closure return type (i64 for scalar/bool, f64 for float).
    fn convert_c_callback_ret_to_internal(
        &mut self,
        c_val: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" => Ok(self
                    .builder
                    .build_int_s_extend(
                        c_val.into_int_value(),
                        self.context.i64_type(),
                        "cb_ret_i32_ext",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))?
                    .into()),
                "bool" => Ok(self
                    .builder
                    .build_int_z_extend(
                        c_val.into_int_value(),
                        self.context.i64_type(),
                        "cb_ret_bool_ext",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
                    .into()),
                "i64" | "f64" => Ok(c_val),
                _ => Err(CompileError::LlvmError(format!(
                    "callback ret type '{}' not supported",
                    name
                ))),
            },
            _ => Err(CompileError::LlvmError(format!(
                "callback ret type '{}' not supported",
                crate::core::fmt_type(ty)
            ))),
        }
    }
}

/// Build an LLVM function type from a basic return type and parameter types.
fn fn_type_for_basic_type<'ctx>(
    ret: BasicTypeEnum<'ctx>,
    params: &[BasicMetadataTypeEnum<'ctx>],
) -> MimiResult<inkwell::types::FunctionType<'ctx>> {
    match ret {
        BasicTypeEnum::IntType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::FloatType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::PointerType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::StructType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::ArrayType(t) => Ok(t.fn_type(params, false)),
        _ => Err(CompileError::LlvmError(
            "unsupported function return type".into(),
        )),
    }
}

/// Convert a BasicValueEnum to its metadata value for calls.
fn basic_value_to_metadata_value<'ctx>(val: &BasicValueEnum<'ctx>) -> BasicMetadataValueEnum<'ctx> {
    match val {
        BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
        BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
        BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
        BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
        BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
        BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
        BasicValueEnum::ScalableVectorValue(_) => unreachable!(),
    }
}
