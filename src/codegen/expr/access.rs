use crate::ast::*;
use crate::codegen::types;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    /// Lower an assignable Mimi place to its actual backing address.
    ///
    /// Unlike `compile_expr` + spill, this preserves field/index write-back and
    /// gives view/mutate references the same ABI for nested projections.
    pub(in crate::codegen) fn compile_place_addr(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>), CompileError> {
        match expr.unlocated() {
            Expr::Ident(name) => {
                let (storage, storage_ty) = vars
                    .get(name)
                    .copied()
                    .ok_or_else(|| CompileError::Generic(format!("unknown place '{}'", name)))?;
                if matches!(storage_ty, BasicTypeEnum::PointerType(_)) {
                    let object_name = self.infer_object_type(expr, vars);
                    let base_name = Self::strip_generic_params(&object_name);
                    if let Some(BasicTypeEnum::StructType(struct_ty)) =
                        self.type_llvm.get(base_name).copied()
                    {
                        let pointer = self
                            .build_load(
                                BasicTypeEnum::PointerType(
                                    self.context.ptr_type(inkwell::AddressSpace::default()),
                                ),
                                storage,
                                &format!("{}_place", name),
                            )?
                            .into_pointer_value();
                        return Ok((pointer, BasicTypeEnum::StructType(struct_ty)));
                    }
                }
                Ok((storage, storage_ty))
            }
            Expr::Field(base, field) => {
                let (base_ptr, base_ty) = self.compile_place_addr(base, vars)?;
                let base_type_name = self.infer_object_type(base, vars);
                let base_name = Self::strip_generic_params(&base_type_name);
                let struct_ty = match base_ty {
                    BasicTypeEnum::StructType(struct_ty) => struct_ty,
                    BasicTypeEnum::PointerType(_) => self.expect_struct_type(base_name)?,
                    _ => {
                        return Err(CompileError::Generic(format!(
                            "field place '{}' has non-struct base",
                            field
                        )))
                    }
                };
                let (index, declared_ty) = if let Some(type_def) = self.type_defs.get(base_name) {
                    if let TypeDefKind::Record(fields) = &type_def.kind {
                        let index = fields
                            .iter()
                            .position(|candidate| candidate.name == *field)
                            .ok_or_else(|| {
                                CompileError::Generic(format!(
                                    "field '{}' not found on type '{}'",
                                    field, base_name
                                ))
                            })?;
                        (index as u32, Some(fields[index].ty.clone()))
                    } else {
                        return Err(format!("type '{}' is not a record", base_name).into());
                    }
                } else if let Ok(index) = field.parse::<u32>() {
                    struct_ty.get_field_type_at_index(index).ok_or_else(|| {
                        CompileError::Generic(format!("tuple field '{}' is out of bounds", field))
                    })?;
                    (index, None)
                } else {
                    return Err(
                        format!("field '{}' not found on type '{}'", field, base_name).into(),
                    );
                };
                let pointer = self
                    .gep()
                    .build_struct_gep(struct_ty, base_ptr, index, &format!("{}_addr", field))
                    .map_err(|error| CompileError::LlvmError(format!("gep error: {error}")))?;
                let storage_ty = struct_ty.get_field_type_at_index(index).ok_or_else(|| {
                    CompileError::Generic(format!("field '{}' storage type is missing", field))
                })?;
                if let Some(Type::Name(nested_name, _)) = declared_ty.as_ref().map(Type::unlocated)
                {
                    if let Some(BasicTypeEnum::StructType(nested_struct)) =
                        self.type_llvm.get(nested_name).copied()
                    {
                        if let BasicTypeEnum::IntType(storage_int) = storage_ty {
                            let encoded = self
                                .build_load(storage_int, pointer, "nested_place_ptr")?
                                .into_int_value();
                            let nested_ptr = self.build_int_to_ptr(
                                encoded,
                                self.context.ptr_type(inkwell::AddressSpace::default()),
                                "nested_place",
                            )?;
                            return Ok((nested_ptr, BasicTypeEnum::StructType(nested_struct)));
                        }
                    }
                }
                Ok((pointer, storage_ty))
            }
            Expr::TupleIndex(base, index) => {
                let (base_ptr, base_ty) = self.compile_place_addr(base, vars)?;
                let BasicTypeEnum::StructType(struct_ty) = base_ty else {
                    return Err("tuple projection requires an addressable tuple".into());
                };
                let field_ty = struct_ty
                    .get_field_type_at_index(*index as u32)
                    .ok_or_else(|| {
                        CompileError::Generic(format!("tuple index {} is out of bounds", index))
                    })?;
                let pointer = self
                    .gep()
                    .build_struct_gep(struct_ty, base_ptr, *index as u32, "tuple_addr")
                    .map_err(|error| CompileError::LlvmError(format!("gep error: {error}")))?;
                Ok((pointer, field_ty))
            }
            Expr::Index(base, index) => {
                let pointer = self.compile_index_addr(base, index, vars)?;
                Ok((pointer, BasicTypeEnum::IntType(self.context.i64_type())))
            }
            Expr::Unary(UnOp::Deref, pointer) => {
                let value = self.compile_expr(pointer, vars)?;
                let BasicValueEnum::PointerValue(pointer) = value else {
                    return Err("dereference place requires a pointer".into());
                };
                Ok((pointer, BasicTypeEnum::IntType(self.context.i64_type())))
            }
            _ => Err("expression is not an addressable place".into()),
        }
    }

    /// After loading a list element as i64, check if the element type is a
    /// compound type (stored as ptrtoint). If so, inttoptr + load the struct.
    fn convert_list_elem_from_i64(
        &self,
        elem_int: inkwell::values::IntValue<'ctx>,
        base_var: Option<&str>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        let Some(var_name) = base_var else {
            return Ok(None);
        };

        // Check if the list element type is a struct (stored as pointer to struct)
        if let Some(&BasicTypeEnum::StructType(sty)) = self.list_elem_llvm_types.get(var_name) {
            let fields = sty.get_field_types();
            // Mimi string is stored in list slots as the raw C-string pointer,
            // even though its LLVM type is the {i8*, i64} string struct.
            let is_string_struct = matches!(
                fields.as_slice(),
                [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(t)]
                    if t.get_bit_width() == 64
            );
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            if is_string_struct {
                let elem_ptr = self.build_int_to_ptr(elem_int, ptr_ty, "elem_str_ptr")?;
                return Ok(Some(BasicValueEnum::PointerValue(elem_ptr)));
            }
            let elem_ptr = self.build_int_to_ptr(elem_int, ptr_ty, "elem_ptr")?;
            let struct_val =
                self.build_load(BasicTypeEnum::StructType(sty), elem_ptr, "elem_struct")?;
            return Ok(Some(struct_val));
        }

        // Check if the list element type is string (stored as raw i8* pointer)
        if let Some(type_name) = self.var_type_names.get(var_name) {
            if type_name == "List<string>" {
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let elem_ptr = self.build_int_to_ptr(elem_int, ptr_ty, "elem_str_ptr")?;
                return Ok(Some(BasicValueEnum::PointerValue(elem_ptr)));
            }
        }

        Ok(None)
    }

    /// Try to convert a list element from i64 to its proper struct type by
    /// inferring the element type from the expression's type annotation.
    /// Arch-2: uses type-driven lookup instead of string parsing.
    fn convert_list_elem_by_type(
        &self,
        elem_int: inkwell::values::IntValue<'ctx>,
        obj_expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        // Arch-2: Try type-driven lookup first (no string parsing)
        if let Expr::Ident(name) = obj_expr.unlocated() {
            if let Some(Type::Name(n, args)) = self.var_types.get(name).map(Type::unlocated) {
                if n == "List" && args.len() == 1 {
                    let elem_ty = &args[0];
                    // List<string> stores raw C-string pointers in its data slots,
                    // not pointers to the {i8*, i64} string struct.
                    if let Type::Name(elem_name, _) = elem_ty.unlocated() {
                        if elem_name == "string" {
                            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                            let elem_ptr =
                                self.build_int_to_ptr(elem_int, ptr_ty, "elem_str_ptr")?;
                            return Ok(Some(BasicValueEnum::PointerValue(elem_ptr)));
                        }
                        if elem_name == "bool" {
                            // bool is stored as i64 0/1 — no conversion needed
                            return Ok(Some(BasicValueEnum::IntValue(elem_int)));
                        }
                        if elem_name == "f32" || elem_name == "f64" {
                            let fv = self
                                .build_bit_cast(
                                    BasicValueEnum::IntValue(elem_int),
                                    BasicTypeEnum::FloatType(self.context.f64_type()),
                                    "i64_to_f64",
                                )?
                                .into_float_value();
                            return Ok(Some(BasicValueEnum::FloatValue(fv)));
                        }
                    }
                    // Product tuples / records / nested structs: ptrtoint slots.
                    if matches!(elem_ty.unlocated(), Type::Tuple(_))
                        || matches!(
                            types::mimi_type_to_llvm(self.context, elem_ty),
                            Some(BasicTypeEnum::StructType(_))
                        )
                    {
                        if let Some(BasicTypeEnum::StructType(sty)) =
                            types::mimi_type_to_llvm(self.context, elem_ty)
                        {
                            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                            let elem_ptr = self.build_int_to_ptr(elem_int, ptr_ty, "elem_ptr")?;
                            let struct_val = self.build_load(
                                BasicTypeEnum::StructType(sty),
                                elem_ptr,
                                "elem_struct",
                            )?;
                            return Ok(Some(struct_val));
                        }
                    }
                }
            }
        }

        // Fallback: string-based parsing (for complex expressions not in var_types)
        let obj_type = self.infer_object_type(obj_expr, vars);
        if obj_type.is_empty() {
            return Ok(None);
        }

        // Handle List<string> — elements are raw i8* pointers
        if obj_type == "List<string>" {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let elem_ptr = self.build_int_to_ptr(elem_int, ptr_ty, "elem_str_ptr")?;
            return Ok(Some(BasicValueEnum::PointerValue(elem_ptr)));
        }

        // Handle bool — stored as i64 0/1, no conversion needed
        if obj_type == "bool" {
            return Ok(Some(BasicValueEnum::IntValue(elem_int)));
        }

        // Handle f32/f64 — stored as bitcast f64→i64, need to convert back
        if obj_type == "f64" || obj_type == "f32" {
            let fv = self
                .build_bit_cast(
                    BasicValueEnum::IntValue(elem_int),
                    BasicTypeEnum::FloatType(self.context.f64_type()),
                    "i64_to_f64",
                )?
                .into_float_value();
            return Ok(Some(BasicValueEnum::FloatValue(fv)));
        }

        if let Some(elem_ty) = crate::codegen::extract_list_elem_type(&obj_type) {
            let llvm_ty = if let Type::Name(name, _) = elem_ty.unlocated() {
                self.type_llvm
                    .get(name)
                    .cloned()
                    .or_else(|| types::mimi_type_to_llvm(self.context, &elem_ty))
            } else {
                types::mimi_type_to_llvm(self.context, &elem_ty)
            };
            match llvm_ty {
                Some(BasicTypeEnum::StructType(sty)) => {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let elem_ptr = self.build_int_to_ptr(elem_int, ptr_ty, "elem_ptr")?;
                    let struct_val =
                        self.build_load(BasicTypeEnum::StructType(sty), elem_ptr, "elem_struct")?;
                    return Ok(Some(struct_val));
                }
                Some(BasicTypeEnum::FloatType(_)) => {
                    let fv = self.build_bit_cast(
                        BasicValueEnum::IntValue(elem_int),
                        BasicTypeEnum::FloatType(self.context.f64_type()),
                        "i64_to_f64",
                    )?;
                    return Ok(Some(fv));
                }
                _ => {
                    // i32, bool, i64: stored directly as i64, no conversion needed
                    return Ok(Some(BasicValueEnum::IntValue(elem_int)));
                }
            }
        }

        Ok(None)
    }

    pub(in crate::codegen) fn compile_field_expr(
        &mut self,
        obj: &Expr,
        field_name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Shared variable fast path: obj is a shared var, load heap ptr directly
        if let Expr::Ident(name) = obj.unlocated() {
            if self.shared_var_names.contains(name.as_str()) {
                if let Some(&(alloca, _ty)) = vars.get(name.as_str()) {
                    if let Some(val) =
                        self.compile_shared_field_load(obj, name, alloca, field_name, vars)?
                    {
                        return Ok(val);
                    }
                }
            }
        }

        // Field access: obj.field
        let obj_val = self.compile_expr(obj, vars)?;
        let obj_type = self.infer_object_type(obj, vars);
        let base_type = Self::strip_generic_params(&obj_type);
        let field_ptr = self.materialize_field_base(obj_val, &obj_type)?;
        let sty = self.expect_struct_type(base_type)?;

        if let Some(td) = self.type_defs.get(base_type) {
            if let TypeDefKind::Record(fields) = &td.kind {
                if let Some(idx) = fields.iter().position(|f| f.name == *field_name) {
                    let gep = self
                        .gep()
                        .build_struct_gep(sty, field_ptr, idx as u32, field_name)
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    // i32 fields must be loaded as i32 then sign-extended to i64 to
                    // match Mimi's i64-uniform value convention; otherwise an i64
                    // load would over-read into the next struct field.
                    let (load_ty, ext) = match fields[idx].ty.unlocated() {
                        Type::Name(n, _) if n == "i32" => {
                            (BasicTypeEnum::IntType(self.context.i32_type()), true)
                        }
                        _ => (
                            self.llvm_type_for(&fields[idx].ty)
                                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
                            false,
                        ),
                    };
                    let loaded = self.build_load(load_ty, gep, field_name)?;
                    if ext {
                        if let BasicValueEnum::IntValue(iv) = loaded {
                            return Ok(self
                                .builder
                                .build_int_s_extend(iv, self.context.i64_type(), "i32_sext")
                                .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))?
                                .into());
                        }
                    }
                    return Ok(loaded);
                }
            }
        }

        // Fallback: numeric field index
        if let Ok(idx) = field_name.parse::<u32>() {
            let gep = self
                .gep()
                .build_struct_gep(sty, field_ptr, idx, field_name)
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            return self.build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                gep,
                field_name,
            );
        }

        Err(format!("field '{}' not found on type '{}'", field_name, obj_type).into())
    }

    /// Get a GEP pointer to a struct field without loading the value.
    /// Used by push/pop on actor self.field to get the field slot pointer.
    pub(in crate::codegen) fn compile_field_gep(
        &mut self,
        obj: &Expr,
        field_name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let obj_val = self.compile_expr(obj, vars)?;
        let obj_type = self.infer_object_type(obj, vars);
        let field_ptr = self.materialize_field_base(obj_val, &obj_type)?;
        let sty = self.expect_struct_type(&obj_type)?;
        if let Some(td) = self.type_defs.get(&obj_type) {
            if let TypeDefKind::Record(fields) = &td.kind {
                if let Some(idx) = fields.iter().position(|f| f.name == *field_name) {
                    let gep = self
                        .gep()
                        .build_struct_gep(sty, field_ptr, idx as u32, field_name)
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    return Ok(gep);
                }
            }
        }
        if let Ok(idx) = field_name.parse::<u32>() {
            let gep = self
                .gep()
                .build_struct_gep(sty, field_ptr, idx, field_name)
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            return Ok(gep);
        }
        Err(format!("field '{}' not found on type '{}'", field_name, obj_type).into())
    }

    /// Shared variable field access fast path.
    fn compile_shared_field_load(
        &mut self,
        obj: &Expr,
        name: &str,
        alloca: inkwell::values::PointerValue<'ctx>,
        field_name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        let obj_type = self.infer_object_type(obj, vars);
        let base = Self::strip_generic_params(&obj_type);
        let td = match self.type_defs.get(base) {
            Some(td) => td,
            None => return Ok(None),
        };
        let fields = match &td.kind {
            TypeDefKind::Record(fields) => fields,
            _ => return Ok(None),
        };
        let idx = match fields.iter().position(|f| f.name == *field_name) {
            Some(idx) => idx,
            None => return Ok(None),
        };

        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let heap_ptr = self
            .build_load(
                BasicTypeEnum::PointerType(ptr_ty),
                alloca,
                &format!("{}_heap_ptr", name),
            )?
            .into_pointer_value();
        let sty = self
            .type_llvm
            .get(base)
            .and_then(|bt| match bt {
                BasicTypeEnum::StructType(s) => Some(*s),
                _ => None,
            })
            .ok_or_else(|| CompileError::Generic(format!("type '{}' is not a struct", base)))?;
        let gep = self
            .gep()
            .build_struct_gep(sty, heap_ptr, idx as u32, field_name)
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let field_ty = types::mimi_type_to_llvm(self.context, &fields[idx].ty)
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
        self.build_load(field_ty, gep, field_name).map(Some)
    }

    /// Extract the base type name by stripping generic params (e.g. `Pair<i32>` → `Pair`).
    /// This lets field access work with fully-resolved type names from var_types.
    fn strip_generic_params(type_name: &str) -> &str {
        if let Some(lt_pos) = type_name.find('<') {
            &type_name[..lt_pos]
        } else {
            type_name
        }
    }

    /// Ensure a struct value is addressable (spill to stack if it is a value).
    fn materialize_field_base(
        &self,
        obj_val: BasicValueEnum<'ctx>,
        obj_type: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let base = Self::strip_generic_params(obj_type);
        match obj_val {
            BasicValueEnum::PointerValue(pv) => Ok(pv),
            BasicValueEnum::StructValue(sv) => {
                if let Some(BasicTypeEnum::StructType(sty)) = self.type_llvm.get(base) {
                    let alloca = self.build_alloca(*sty, "tmp")?;
                    self.build_store(alloca, sv)?;
                    Ok(alloca)
                } else {
                    Err(format!("[E0707] cannot access field on type '{}'", obj_type).into())
                }
            }
            _ => Err(CompileError::Generic(format!(
                "field access requires a struct or actor type, got {}",
                obj_val.get_type()
            ))),
        }
    }

    pub(in crate::codegen) fn expect_struct_type(
        &self,
        obj_type: &str,
    ) -> Result<inkwell::types::StructType<'ctx>, CompileError> {
        match self.type_llvm.get(obj_type) {
            Some(BasicTypeEnum::StructType(s)) => Ok(*s),
            _ => Err(format!("type '{}' is not a struct", obj_type).into()),
        }
    }

    pub(in crate::codegen) fn compile_index_expr(
        &mut self,
        obj: &Expr,
        idx_expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let obj_val = self.compile_expr(obj, vars)?;
        let idx_val = self.compile_expr(idx_expr, vars)?;
        match obj_val {
            BasicValueEnum::PointerValue(pv) => {
                self.compile_index_on_pointer(pv, obj, idx_val, vars)
            }
            BasicValueEnum::StructValue(sv) => self.compile_index_on_struct(sv, obj, idx_val, vars),
            BasicValueEnum::ArrayValue(_) => self.compile_index_on_array(obj_val, idx_val),
            BasicValueEnum::IntValue(iv) => {
                // Heap pointer stored as i64 (nested List<List<T>> indexing).
                // inttoptr recovers the list struct pointer for indexing.
                let pv = self
                    .builder
                    .build_int_to_ptr(
                        iv,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "list_ptr",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("int_to_ptr: {}", e)))?;
                self.compile_index_on_pointer(pv, obj, idx_val, vars)
            }
            _ => Err("index requires a list/array pointer".into()),
        }
    }

    fn compile_index_on_pointer(
        &mut self,
        pv: inkwell::values::PointerValue<'ctx>,
        obj: &Expr,
        idx_val: BasicValueEnum<'ctx>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let idx_iv = require_int_index(idx_val)?;
        let list_ty = self.standard_list_type();

        // Check if this looks like a list struct by trying to GEP field 0 (len)
        if self
            .gep()
            .build_struct_gep(list_ty, pv, 0, "list.len_check")
            .is_ok()
        {
            self.check_list_bounds(pv, idx_iv, "index read")?;
            let elem_int = self.load_list_element_i64(pv, idx_iv)?;
            if let Some(converted) = self.try_convert_list_element(elem_int, obj, vars)? {
                return Ok(converted);
            }
            return Ok(elem_int.into());
        }

        // Fallback: treat as raw pointer to i64 array
        let elem_ptr = self.build_in_bounds_gep(self.context.i64_type(), pv, &[idx_iv], "elem")?;
        self.build_load(
            BasicTypeEnum::IntType(self.context.i64_type()),
            elem_ptr,
            "elem_val",
        )
    }

    fn compile_index_on_struct(
        &mut self,
        sv: inkwell::values::StructValue<'ctx>,
        obj: &Expr,
        idx_val: BasicValueEnum<'ctx>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let sv_ty = sv.get_type();
        let list_alloca = self.build_alloca(sv_ty, "list_tmp")?;
        self.build_store(list_alloca, sv)?;
        let idx_iv = require_int_index(idx_val)?;
        self.check_list_bounds(list_alloca, idx_iv, "index read")?;
        let elem_int = self.load_list_element_i64(list_alloca, idx_iv)?;
        if let Some(converted) = self.try_convert_list_element(elem_int, obj, vars)? {
            return Ok(converted);
        }
        Ok(elem_int.into())
    }

    fn compile_index_on_array(
        &self,
        obj_val: BasicValueEnum<'ctx>,
        idx_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let idx = match idx_val {
            BasicValueEnum::IntValue(iv) => iv
                .get_zero_extended_constant()
                .ok_or_else(|| "array index must be a compile-time constant".to_string())?
                as u32,
            _ => return Err("index must be i64".into()),
        };
        self.build_extract_value(obj_val.into_array_value().into(), idx, "arr_elem")
    }

    fn standard_list_type(&self) -> inkwell::types::StructType<'ctx> {
        self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.i64_type()),
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            ],
            false,
        )
    }

    /// Compute the address of a list element as an i64* slot.
    /// Used for borrowed index expressions (`&xs[i]` / `&mut xs[i]`).
    pub(in crate::codegen) fn compile_index_addr(
        &mut self,
        obj: &Expr,
        idx_expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let idx_val = self.compile_expr(idx_expr, vars)?;
        let idx_iv = require_int_index(idx_val)?;

        let list_ptr = if let Expr::Ident(name) = obj.unlocated() {
            let (storage, storage_ty) = vars
                .get(name)
                .copied()
                .ok_or_else(|| CompileError::Generic(format!("unknown list place '{}'", name)))?;
            if matches!(storage_ty, BasicTypeEnum::PointerType(_)) {
                self.build_load(
                    BasicTypeEnum::PointerType(
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                    ),
                    storage,
                    &format!("{}_list_place", name),
                )?
                .into_pointer_value()
            } else {
                storage
            }
        } else {
            let obj_val = self.compile_expr(obj, vars)?;
            match obj_val {
                BasicValueEnum::PointerValue(pv) => pv,
                BasicValueEnum::StructValue(sv) => {
                    let list_ty = self.standard_list_type();
                    let list_alloca = self.build_alloca(list_ty, "list_tmp")?;
                    self.build_store(list_alloca, sv)?;
                    list_alloca
                }
                _ => return Err("borrowed index requires a list value".into()),
            }
        };

        self.check_list_bounds(list_ptr, idx_iv, "borrowed index")?;

        let list_ty = self.standard_list_type();
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_ptr, 1, "list.data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_ptr = self
            .build_load(
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                data_gep,
                "data",
            )?
            .into_pointer_value();
        let data_ptr_i64 = self
            .build_bit_cast(
                data_ptr.into(),
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .into(),
                "data_i64",
            )?
            .into_pointer_value();
        self.build_in_bounds_gep(
            self.context.i64_type(),
            data_ptr_i64,
            &[idx_iv],
            "elem_addr",
        )
    }

    /// Load a list element as i64 from `{ len, data }`.
    fn load_list_element_i64(
        &self,
        list_ptr: inkwell::values::PointerValue<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let list_ty = self.standard_list_type();
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_ptr, 1, "list.data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_ptr = self
            .build_load(
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                data_gep,
                "data",
            )?
            .into_pointer_value();
        let data_ptr_i64 = self
            .build_bit_cast(
                data_ptr.into(),
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .into(),
                "data_i64",
            )?
            .into_pointer_value();
        let elem_ptr =
            self.build_in_bounds_gep(self.context.i64_type(), data_ptr_i64, &[idx], "elem")?;
        Ok(self
            .build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                elem_ptr,
                "elem_val",
            )?
            .into_int_value())
    }

    /// Try to convert a loaded i64 list element into its real struct/string form.
    pub(in crate::codegen) fn try_convert_list_element(
        &self,
        elem_int: inkwell::values::IntValue<'ctx>,
        obj: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        if let Expr::Ident(var_name) = obj.unlocated() {
            if let Some(converted) =
                self.convert_list_elem_from_i64(elem_int, Some(var_name.as_str()))?
            {
                return Ok(Some(converted));
            }
        }
        self.convert_list_elem_by_type(elem_int, obj, vars)
    }

    pub(in crate::codegen) fn compile_tuple_index_expr(
        &mut self,
        tuple_expr: &Expr,
        index: usize,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // D4: newtype .0 — newtypes are transparent in codegen, .0 is identity
        if index == 0 {
            if let Expr::Ident(name) = tuple_expr.unlocated() {
                if let Some(type_name) = self.var_type_names.get(name) {
                    if let Some(td) = self.type_defs.get(type_name) {
                        if matches!(td.kind, crate::ast::TypeDefKind::Newtype(_)) {
                            return self.compile_expr(tuple_expr, vars);
                        }
                    }
                }
            }
        }

        let tuple_val = self.compile_expr(tuple_expr, vars)?;
        Ok(match tuple_val {
            BasicValueEnum::PointerValue(pv) => {
                let struct_ty = self
                    .tuple_type_stack
                    .last()
                    .ok_or_else(|| "tuple type stack empty".to_string())?;
                let field_gep = self
                    .gep()
                    .build_struct_gep(
                        *struct_ty,
                        pv,
                        index as u32,
                        &format!("tuple_field_{}", index),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let field_types = struct_ty.get_field_types();
                let field_ty = field_types
                    .get(index)
                    .ok_or_else(|| format!("tuple field {} out of bounds", index))?;
                self.build_load(*field_ty, field_gep, &format!("tuple_{}", index))?
            }
            BasicValueEnum::StructValue(sv) => {
                self.build_extract_value(sv.into(), index as u32, &format!("tuple_{}", index))?
            }
            _ => {
                return Err(CompileError::Generic(format!(
                    "tuple index requires a tuple value, got {:?}",
                    tuple_val
                )))
            }
        })
    }
}

fn require_int_index<'ctx>(
    idx_val: BasicValueEnum<'ctx>,
) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
    match idx_val {
        BasicValueEnum::IntValue(iv) => Ok(iv),
        _ => Err("index must be i64".into()),
    }
}
