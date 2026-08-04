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
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::AddressSpace;
use std::collections::HashMap;

/// SysV x86-64 classification of a scalar-only `#[repr(C)]` record as it
/// crosses the C ABI boundary (0.34.35, M-010).
///
/// SysV passes structs <= 16 bytes in registers, classified per eightbyte:
/// each 8-byte chunk is INTEGER (any i32/i64/bool member) or SSE (f64-only
/// chunk). A C caller therefore passes {i32,i32} PACKED in one register
/// (rdi), {i64,i64} in rdi+rsi, and {i64,f64} in rdi+xmm0. Declaring the
/// wrapper parameter as the raw struct type is WRONG: LLVM does not apply
/// SysV merging/coercion to bare struct params and splits fields across
/// successive registers (verified by disassembly: {i32,i32} read from
/// edi+esi while the C caller packed both into rdi). The fix is to use the
/// "coerced" boundary type — one i64/double per eightbyte — which LLVM
/// splits/joins into exactly the SysV registers, then reconstruct the
/// C-layout struct through memory.
///
/// Structs > 16 bytes go through memory: C passes large PARAM structs by
/// hidden pointer (in a register) and returns large structs via sret
/// (caller-provided buffer as hidden first argument).
enum SysVCoerce<'ctx> {
    /// <= 16B: pass/return in registers. The boundary type is a scalar
    /// (i64 / double) for one eightbyte or a 2-element struct for two.
    Reg(BasicTypeEnum<'ctx>),
    /// > 16B: pass by hidden pointer (params) / sret buffer (returns).
    Mem,
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

        // 0.34.35 (M-010): SysV coercion for repr(C) records. Params/returns
        // cross the boundary in coerced eightbyte register types (or via
        // memory/sret for >16B), not as bare LLVM struct types — see
        // sysv_coerce_reprc_record for the ABI rationale.
        let ret_reprc = match &func.ret {
            Some(ty) => self.reprc_record_info(ty)?,
            None => None,
        };
        let sret = matches!(&ret_reprc, Some((_, _, SysVCoerce::Mem)));

        // C ABI return type.
        let void_ret = sret;
        let c_ret_ty = if sret {
            // sret: caller provides the result buffer; wrapper returns void.
            BasicTypeEnum::IntType(self.context.i64_type())
        } else if let Some((_, _, SysVCoerce::Reg(t))) = &ret_reprc {
            *t
        } else {
            match &func.ret {
                Some(ty) => self.c_abi_llvm_type(ty)?,
                None => BasicTypeEnum::IntType(self.context.i64_type()),
            }
        };

        // C ABI parameter types (sret buffer first when returning >16B).
        let ptr_ty = BasicTypeEnum::PointerType(self.context.ptr_type(AddressSpace::default()));
        let mut c_param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        if sret {
            c_param_tys.push(types::basic_to_metadata(self.context, ptr_ty));
        }
        for p in &func.params {
            let bty = if let Some((_, _, coerce)) = self.reprc_record_info(&p.ty)? {
                match coerce {
                    SysVCoerce::Reg(t) => t,
                    SysVCoerce::Mem => ptr_ty,
                }
            } else {
                self.c_abi_llvm_type(&p.ty)?
            };
            c_param_tys.push(types::basic_to_metadata(self.context, bty));
        }

        let fn_type = if void_ret {
            self.context.void_type().fn_type(&c_param_tys, false)
        } else {
            fn_type_for_basic_type(c_ret_ty, &c_param_tys)?
        };
        let function = self.module.add_function(
            &func.name,
            fn_type,
            Some(inkwell::module::Linkage::External),
        );
        let cc = crate::ffi::abi_to_llvm_call_conv(abi);
        function.set_call_conventions(cc);

        let arg_offset: u32 = if sret { 1 } else { 0 };

        // 0.34.35 (M-010): SysV passes MEMORY-class (>16B) struct PARAMS on
        // the stack, not via a register pointer — the LLVM encoding for that
        // is a `byval` pointer parameter (verified against gcc/clang output:
        // callers copy the struct to the stack top and pass no register; a
        // plain ptr param made us read rdi garbage). Large struct RETURNS use
        // the sret hidden buffer in param 0.
        {
            use inkwell::attributes::{Attribute, AttributeLoc};
            if sret {
                if let Some((rname, _, _)) = &ret_reprc {
                    let c_sty = self.c_sty_for_reprc_record(rname)?;
                    let kind = Attribute::get_named_enum_kind_id("sret");
                    let attr = self.context.create_type_attribute(kind, c_sty.into());
                    function.add_attribute(AttributeLoc::Param(0), attr);
                }
            }
            for (i, param) in func.params.iter().enumerate() {
                if let Some((rname, _, SysVCoerce::Mem)) = self.reprc_record_info(&param.ty)? {
                    let c_sty = self.c_sty_for_reprc_record(&rname)?;
                    let kind = Attribute::get_named_enum_kind_id("byval");
                    let attr = self.context.create_type_attribute(kind, c_sty.into());
                    function.add_attribute(AttributeLoc::Param(i as u32 + arg_offset), attr);
                }
            }
        }

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
                .get_nth_param(i as u32 + arg_offset)
                .ok_or_else(|| CompileError::LlvmError(format!("export param {} not found", i)))?;
            let internal_val =
                if let Some((rname, _, coerce)) = self.reprc_record_info(&param.ty)? {
                    // repr(C) param: undo SysV coercion to the C-layout struct,
                    // then map fields into the internal representation.
                    let c_struct_val = match coerce {
                        SysVCoerce::Reg(coerce_ty) => {
                            let c_sty = self.c_sty_for_reprc_record(&rname)?;
                            self.reinterpret_bytes(
                                c_val,
                                coerce_ty,
                                BasicTypeEnum::StructType(c_sty),
                                &format!("{}_arg_cast", rname),
                            )?
                        }
                        SysVCoerce::Mem => {
                            // C passes large struct params by hidden pointer.
                            let c_sty = self.c_sty_for_reprc_record(&rname)?;
                            let pv = c_val.into_pointer_value();
                            self.build_load(
                                BasicTypeEnum::StructType(c_sty),
                                pv,
                                &format!("{}_mem_load", rname),
                            )?
                        }
                    };
                    self.convert_c_reprc_record_to_internal(c_struct_val, &rname)?
                } else {
                    self.convert_c_arg_to_internal(c_val, &param.ty)?
                };
            let internal_ty = self
                .llvm_type_for(&param.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            let alloca = self.build_alloca(internal_ty, &param.name)?;
            self.build_store(alloca, internal_val)?;

            // Track type metadata for method dispatch etc.
            if let Type::Name(tn, args) = param.ty.unlocated() {
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
            body_args.push(types::basic_value_to_metadata_value(
                &loaded,
                self.context.i64_type(),
            ));
        }

        let body_ret = self
            .build_call(body_fn, &body_args, "export_body_call")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("export body returned void".into()))?;

        if sret {
            // Write the C-layout struct into the caller-provided buffer.
            let sret_ptr = function
                .get_nth_param(0)
                .ok_or_else(|| CompileError::LlvmError("sret param not found".into()))?
                .into_pointer_value();
            let c_struct_val = self.convert_internal_ret_to_c(body_ret, func.ret.as_ref())?;
            self.build_store(sret_ptr, c_struct_val)?;
            self.build_return(None)?;
        } else if let Some((rname, _, SysVCoerce::Reg(coerce_ty))) = &ret_reprc {
            // Return the coerced eightbyte aggregate; LLVM splits it into the
            // SysV registers (rax/rdx for INTEGER, xmm0/xmm1 for SSE).
            let c_struct_val = self.convert_internal_ret_to_c(body_ret, func.ret.as_ref())?;
            let c_sty = self.c_sty_for_reprc_record(rname)?;
            let coerce_val = self.reinterpret_bytes(
                c_struct_val,
                BasicTypeEnum::StructType(c_sty),
                *coerce_ty,
                &format!("{}_cret_cast", rname),
            )?;
            self.build_return(Some(&coerce_val))?;
        } else {
            let c_ret_val = self.convert_internal_ret_to_c(body_ret, func.ret.as_ref())?;
            self.build_return(Some(&c_ret_val))?;
        }

        self.pop_shared_scope()?;
        self.free_heap_allocs()?;
        self.pop_comp_scope();
        self.pop_cap_scope();

        Ok(())
    }

    /// If `ty` names a repr(C) record, return (name, fields, SysV coercion).
    fn reprc_record_info(
        &self,
        ty: &Type,
    ) -> MimiResult<Option<(String, Vec<Field>, SysVCoerce<'ctx>)>> {
        if let Type::Name(name, _) = ty.unlocated() {
            if self.repr_c_record_names.contains(name.as_str()) {
                if let Some(td) = self.type_defs.get(name.as_str()) {
                    if let TypeDefKind::Record(fields) = &td.kind {
                        let coerce = self.sysv_coerce_reprc_record(fields)?;
                        return Ok(Some((name.clone(), fields.clone(), coerce)));
                    }
                }
            }
        }
        Ok(None)
    }

    /// C-layout LLVM struct type for a repr(C) record.
    fn c_sty_for_reprc_record(&self, name: &str) -> MimiResult<inkwell::types::StructType<'ctx>> {
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
        self.c_layout_struct_type(&fields)
    }

    /// Reinterpret the bytes of `val` (typed `from_ty`) as `to_ty` through a
    /// stack slot. Both types must have identical size; used to move between
    /// the SysV-coerced boundary type and the C-layout struct type.
    fn reinterpret_bytes(
        &self,
        val: BasicValueEnum<'ctx>,
        from_ty: BasicTypeEnum<'ctx>,
        to_ty: BasicTypeEnum<'ctx>,
        tag: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let alloca = self.build_alloca(from_ty, &format!("{}_slot", tag))?;
        self.build_store(alloca, val)?;
        self.build_load(to_ty, alloca, tag)
    }

    /// Map a Mimi type to the LLVM type used at the C ABI boundary for
    /// exported functions.
    fn c_abi_llvm_type(&self, ty: &Type) -> MimiResult<BasicTypeEnum<'ctx>> {
        match ty.unlocated() {
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
                            // 0.34.35 (M-010/N-1, L3): repr(C) records cross the
                            // boundary BY VALUE as their C-layout LLVM struct.
                            // LLVM's x86-64 legalization implements the SysV
                            // classification rules (<=16B in registers by class,
                            // >16B passed/returned indirectly via hidden
                            // pointer/sret), exactly matching C callers. The old
                            // split (i64-packed for <=2 all-i32 fields, raw
                            // pointer otherwise) violated SysV for every other
                            // shape: {i64}/{i64,i64} parameters read a register
                            // value as a pointer (SIGSEGV), larger shapes
                            // returned garbage.
                            Ok(BasicTypeEnum::StructType(
                                self.c_layout_struct_type(fields)?,
                            ))
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
                    } else if name == "Map" || name == "Set" {
                        // Opaque runtime handles (i64).
                        Ok(BasicTypeEnum::IntType(self.context.i64_type()))
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
        match ty.unlocated() {
            Type::Name(name, _) => match name.as_str() {
                "i32" => {
                    // After A1 restoration, internal i32 uses i32 type.
                    // C ABI already provides i32, so pass through.
                    Ok(c_val)
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
                "Map" | "Set" => Ok(c_val), // opaque i64 handles
                _ => {
                    if self.repr_c_record_names.contains(name.as_str()) {
                        self.convert_c_reprc_record_to_internal(c_val, name)
                    } else if self.record_type_names.contains(name.as_str()) || name == "List" {
                        // C ABI passes a JSON C string pointer for non-repr(C)
                        // records and List; decode via the same from_json path.
                        let pv = c_val.into_pointer_value();
                        self.compile_from_json_raw(ty, pv)
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
        match ty.unlocated() {
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
                "i64" | "f64" | "unit" | "Map" | "Set" => Ok(internal_val),
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
                    } else if self.record_type_names.contains(name.as_str()) || name == "List" {
                        // Return as heap JSON C string (caller frees).
                        self.export_value_as_json_cstr(internal_val, ty)
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

    /// Serialize an internal List/Record value to a heap JSON C string for export.
    fn export_value_as_json_cstr(
        &mut self,
        internal_val: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        match ty.unlocated() {
            Type::Name(n, args) if n == "List" => {
                let list_struct_ty = self.list_struct_type();
                let alloca =
                    self.build_alloca(BasicTypeEnum::StructType(list_struct_ty), "exp_list")?;
                match internal_val {
                    BasicValueEnum::StructValue(sv) => self.build_store(alloca, sv)?,
                    BasicValueEnum::PointerValue(pv) => {
                        let loaded = self
                            .builder
                            .build_load(
                                BasicTypeEnum::StructType(list_struct_ty),
                                pv,
                                "exp_list_ld",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            .into_struct_value();
                        self.build_store(alloca, loaded)?;
                    }
                    _ => {
                        return Err(CompileError::LlvmError(
                            "export List return: unexpected value kind".into(),
                        ))
                    }
                }
                let elem = args.first().and_then(|t| match t.unlocated() {
                    Type::Name(en, _) => Some(en.as_str()),
                    _ => None,
                });
                let rt_fn = match elem {
                    Some("string") => "mimi_list_str_to_json",
                    Some("f64") | Some("f32") => "mimi_list_f64_to_json",
                    Some("bool") => "mimi_list_bool_to_json",
                    _ => "mimi_list_i64_to_json",
                };
                let func = self.get_runtime_fn(rt_fn)?;
                let raw = self
                    .build_call(
                        func,
                        &[BasicMetadataValueEnum::PointerValue(alloca)],
                        "export_list_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("list to_json void")?
                    .into_pointer_value();
                // Do not free on export return — C caller owns the buffer.
                Ok(BasicValueEnum::PointerValue(raw))
            }
            Type::Name(n, _) if self.record_type_names.contains(n.as_str()) => {
                let llvm_ty = *self.type_llvm.get(n).ok_or_else(|| {
                    CompileError::LlvmError(format!("no LLVM type for record {}", n))
                })?;
                let BasicTypeEnum::StructType(sty) = llvm_ty else {
                    return Err(CompileError::LlvmError(format!(
                        "record type {} is not a struct",
                        n
                    )));
                };
                let struct_ptr = match internal_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    BasicValueEnum::StructValue(sv) => {
                        let alloca =
                            self.build_alloca(BasicTypeEnum::StructType(sty), "exp_rec")?;
                        self.build_store(alloca, sv)?;
                        alloca
                    }
                    _ => {
                        return Err(CompileError::LlvmError(
                            "export record return: unexpected value kind".into(),
                        ))
                    }
                };
                let raw = self.compile_record_to_json_cstr(n, struct_ptr)?;
                // C caller owns the buffer — do not register for free_heap_allocs.
                Ok(BasicValueEnum::PointerValue(raw))
            }
            _ => Err(CompileError::LlvmError(
                "export_value_as_json_cstr: unsupported type".into(),
            )),
        }
    }

    /// Convert a #[repr(C)] record from its C ABI representation to Mimi's
    /// internal struct representation.
    ///
    /// 0.34.35 (M-010): the record now arrives BY VALUE as the C-layout
    /// struct (the wrapper declares a struct-typed parameter, and LLVM's
    /// legalization delivers it per SysV). Fields already carry C widths;
    /// each is adjusted to the internal field type for safety.
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

        let sv = c_val.into_struct_value();
        let mut field_vals: Vec<BasicValueEnum<'ctx>> = Vec::new();
        for (fi, _f) in fields.iter().enumerate() {
            let raw = self
                .builder
                .build_extract_value(sv, fi as u32, &format!("{}_field_{}", name, fi))
                .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
            // CG-C4: the internal struct uses extern field types (i32 for i32
            // fields), so adjust the C field value to the internal field type.
            let field_ty = internal_sty
                .get_field_type_at_index(fi as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("field {} type missing", fi)))?;
            field_vals.push(self.adjust_int_val(raw, field_ty)?);
        }
        self.build_struct_from_fields(internal_sty, &field_vals, name)
    }

    /// 0.34.35 (M-010/N-1): assemble a struct value from RUNTIME field
    /// values via insertvalue. `const_named_struct` is only legal when every
    /// operand is a compile-time constant; feeding it runtime SSA values
    /// produces malformed pseudo-constant IR that LLVM's new pass manager
    /// crashes on (it does not verify before optimizing; standalone `opt`
    /// rejects it with "invalid use of function-local name"). insertvalue is
    /// the correct construction for runtime operands.
    fn build_struct_from_fields(
        &self,
        sty: inkwell::types::StructType<'ctx>,
        field_vals: &[BasicValueEnum<'ctx>],
        tag: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let mut agg = sty.get_undef();
        for (fi, v) in field_vals.iter().enumerate() {
            agg = self
                .builder
                .build_insert_value(agg, *v, fi as u32, &format!("{}_ins_{}", tag, fi))
                .map_err(|e| CompileError::LlvmError(format!("insert error: {}", e)))?
                .into_struct_value();
        }
        Ok(BasicValueEnum::StructValue(agg))
    }

    /// Convert a #[repr(C)] record from Mimi's internal struct representation
    /// to its C ABI representation.
    ///
    /// 0.34.35 (M-010/N-1, L3): the record is RETURNED BY VALUE as the
    /// C-layout struct. LLVM legalizes the struct return per SysV (<=16B in
    /// rax/rdx/xmm by class, >16B via sret), exactly matching C callers. This
    /// replaces the old split (i64-packed for <=2 all-i32 fields; heap-malloc
    /// pointer otherwise), which violated SysV and also leaked a malloc on
    /// every call.
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

        let c_sty = self.c_layout_struct_type(&fields)?;
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
        let sv = match internal_val {
            BasicValueEnum::StructValue(s) => s,
            BasicValueEnum::PointerValue(pv) => self
                .build_load(
                    BasicTypeEnum::StructType(internal_sty),
                    pv,
                    &format!("{}_cret_load", name),
                )?
                .into_struct_value(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "export return of '{}': unexpected value kind",
                    name
                )))
            }
        };

        let mut field_vals: Vec<BasicValueEnum<'ctx>> = Vec::new();
        for (fi, f) in fields.iter().enumerate() {
            let raw = self
                .builder
                .build_extract_value(sv, fi as u32, &format!("{}_{}_raw", name, f.name))
                .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
            field_vals.push(self.convert_internal_field_to_c(raw, &f.ty)?);
        }
        self.build_struct_from_fields(c_sty, &field_vals, &format!("{}_cret", name))
    }

    /// Classify a scalar-only repr(C) record for the SysV x86-64 boundary.
    /// Scalar-only fields (i32/i64/f64/bool) are naturally aligned and never
    /// straddle an eightbyte boundary, so no eightbyte is ever MEMORY class.
    fn sysv_coerce_reprc_record(&self, fields: &[Field]) -> MimiResult<SysVCoerce<'ctx>> {
        let i64_ty = self.context.i64_type();
        let f64_ty = self.context.f64_type();

        let size = self.compute_c_struct_size(fields)?;
        if size > 16 {
            return Ok(SysVCoerce::Mem);
        }

        // Field byte spans (offset, size).
        let mut spans: Vec<(usize, usize, bool)> = Vec::new(); // (off, size, is_float)
        let mut offset = 0usize;
        let mut max_align = 1usize;
        for f in fields {
            let (sz, al) = self.field_c_size_align(&f.ty)?;
            max_align = max_align.max(al);
            offset = (offset + al - 1) & !(al - 1);
            let is_float = matches!(f.ty.unlocated(), Type::Name(n, _) if n == "f64");
            spans.push((offset, sz, is_float));
            offset += sz;
        }

        if size == 0 {
            // Empty record: pass as zero-width; represent as a single i64 slot.
            return Ok(SysVCoerce::Reg(BasicTypeEnum::IntType(i64_ty)));
        }

        let n_eightbytes = size.div_ceil(8);
        let mut elem_tys: Vec<BasicTypeEnum<'ctx>> = Vec::new();
        for eb in 0..n_eightbytes {
            let lo = eb * 8;
            let hi = lo + 8;
            // A byte is SSE only if every field byte in this eightbyte is f64;
            // any integer byte makes the eightbyte INTEGER (SysV merge rule).
            let mut has_int = false;
            let mut has_any = false;
            for &(off, sz, is_float) in &spans {
                let f_lo = off;
                let f_hi = off + sz;
                if f_hi <= lo || f_lo >= hi {
                    continue;
                }
                has_any = true;
                if !is_float {
                    has_int = true;
                }
            }
            let cls_is_sse = has_any && !has_int;
            elem_tys.push(if cls_is_sse {
                BasicTypeEnum::FloatType(f64_ty)
            } else {
                BasicTypeEnum::IntType(i64_ty)
            });
        }

        let ty = if elem_tys.len() == 1 {
            elem_tys[0]
        } else {
            BasicTypeEnum::StructType(self.context.struct_type(&elem_tys, false))
        };
        Ok(SysVCoerce::Reg(ty))
    }

    /// Compute the total size in bytes of a C-layout struct with the given fields,
    /// using standard C struct padding rules (natural alignment).
    fn compute_c_struct_size(&self, fields: &[Field]) -> MimiResult<usize> {
        let mut max_align = 1usize;
        let mut offset = 0usize;
        for f in fields {
            let (size, align) = self.field_c_size_align(&f.ty)?;
            max_align = max_align.max(align);
            let aligned = (offset + align - 1) & !(align - 1);
            offset = aligned + size;
        }
        let total = (offset + max_align - 1) & !(max_align - 1);
        Ok(total)
    }

    /// Get the C ABI size and alignment of a field type.
    fn field_c_size_align(&self, ty: &Type) -> MimiResult<(usize, usize)> {
        match ty.unlocated() {
            Type::Name(name, _) => match name.as_str() {
                "i32" => Ok((4, 4)),
                "i64" => Ok((8, 8)),
                "f64" => Ok((8, 8)),
                "bool" => Ok((1, 1)),
                _ => Err(CompileError::LlvmError(format!(
                    "export wrapper: unknown field type '{}' for C struct size",
                    name
                ))),
            },
            _ => Err(CompileError::LlvmError(format!(
                "export wrapper: unsupported field type for C struct size: {}",
                crate::core::fmt_type(ty)
            ))),
        }
    }

    /// Convert a Mimi internal field value to its C ABI representation.
    /// This is the reverse of `convert_c_field_to_internal`.
    fn convert_internal_field_to_c(
        &mut self,
        internal_val: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        match ty.unlocated() {
            Type::Name(name, _) => match name.as_str() {
                "i32" => {
                    let iv = internal_val.into_int_value();
                    Ok(self
                        .builder
                        .build_int_truncate(iv, self.context.i32_type(), "field_i32_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?
                        .into())
                }
                "bool" => {
                    let iv = internal_val.into_int_value();
                    Ok(self
                        .builder
                        .build_int_truncate(iv, self.context.i8_type(), "field_bool_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?
                        .into())
                }
                "i64" | "f64" => Ok(internal_val),
                _ => Err(CompileError::LlvmError(format!(
                    "export wrapper: unsupported field type '{}'",
                    name
                ))),
            },
            _ => Err(CompileError::LlvmError(format!(
                "export wrapper: unsupported field type '{}'",
                crate::core::fmt_type(ty)
            ))),
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
    #[allow(dead_code)]
    pub(crate) fn convert_c_field_to_internal(
        &mut self,
        c_val: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        match ty.unlocated() {
            Type::Name(name, _) => match name.as_str() {
                "i32" => {
                    // After A1 restoration, internal i32 fields use i32 type.
                    // Just truncate to i32 in case the C value came in as a wider type.
                    let iv = c_val.into_int_value();
                    let bw = iv.get_type().get_bit_width();
                    if bw == 32 {
                        Ok(iv.into())
                    } else {
                        Ok(self
                            .builder
                            .build_int_truncate(iv, self.context.i32_type(), "field_i32_trunc")
                            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?
                            .into())
                    }
                }
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

        let internal_ret_ty: BasicTypeEnum<'ctx> = match cb_ret.unlocated() {
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
        match ty.unlocated() {
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
                "i64" | "f64" => Ok(types::basic_value_to_metadata_value(
                    &internal_val,
                    self.context.i64_type(),
                )),
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
        match ty.unlocated() {
            Type::Name(name, _) => match name.as_str() {
                "i32" => {
                    // After A1 restoration, internal i32 is i32 — pass through.
                    let iv = c_val.into_int_value();
                    let bw = iv.get_type().get_bit_width();
                    if bw == 32 {
                        Ok(iv.into())
                    } else {
                        Ok(self
                            .builder
                            .build_int_truncate(iv, self.context.i32_type(), "cb_ret_i32_trunc")
                            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?
                            .into())
                    }
                }
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
