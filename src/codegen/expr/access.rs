use crate::ast::*;
use crate::codegen::types;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    /// After loading a list element as i64, check if the element type is a
    /// compound type (stored as ptrtoint). If so, inttoptr + load the struct.
    fn convert_list_elem_from_i64(
        &self,
        elem_int: inkwell::values::IntValue<'ctx>,
        base_var: Option<&str>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        if let Some(var_name) = base_var {
            if let Some(&elem_llvm) = self.list_elem_llvm_types.get(var_name) {
                if let BasicTypeEnum::StructType(sty) = elem_llvm {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let elem_ptr = self.builder.build_int_to_ptr(elem_int, ptr_ty, "elem_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?;
                    let struct_val = self.builder.build_load(
                        BasicTypeEnum::StructType(sty), elem_ptr, "elem_struct",
                    ).map_err(|e| CompileError::LlvmError(format!("load struct elem: {}", e)))?;
                    return Ok(Some(struct_val));
                }
            }
        }
        Ok(None)
    }

    /// Try to convert a list element from i64 to its proper struct type by
    /// inferring the element type from the expression's type annotation.
    fn convert_list_elem_by_type(
        &self,
        elem_int: inkwell::values::IntValue<'ctx>,
        obj_expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        let obj_type = self.infer_object_type(obj_expr, vars);
        if obj_type.is_empty() {
            return Ok(None);
        }
        if let Some(elem_ty) = crate::codegen::extract_list_elem_type(&obj_type) {
            if let Some(llvm_elem) = types::mimi_type_to_llvm(self.context, &elem_ty) {
                if let BasicTypeEnum::StructType(sty) = llvm_elem {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let elem_ptr = self.builder.build_int_to_ptr(elem_int, ptr_ty, "elem_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?;
                    let struct_val = self.builder.build_load(
                        BasicTypeEnum::StructType(sty), elem_ptr, "elem_struct",
                    ).map_err(|e| CompileError::LlvmError(format!("load struct elem: {}", e)))?;
                    return Ok(Some(struct_val));
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
        if let Expr::Ident(name) = obj {
            if self.shared_var_names.contains(name.as_str()) {
                if let Some(&(alloca, _ty)) = vars.get(name.as_str()) {
                    let obj_type = self.infer_object_type(obj, vars);
                    if let Some(td) = self.type_defs.get(&obj_type) {
                        if let TypeDefKind::Record(fields) = &td.kind {
                            if let Some(idx) = fields.iter().position(|f| f.name == *field_name) {
                                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                                let heap_ptr = self.builder.build_load(
                                    BasicTypeEnum::PointerType(ptr_ty), alloca,
                                    &format!("{}_heap_ptr", name),
                                ).map_err(|e| CompileError::LlvmError(format!("shared heap ptr load: {}", e)))?.into_pointer_value();
                                let sty = self.type_llvm.get(&obj_type)
                                    .and_then(|bt| match bt { BasicTypeEnum::StructType(s) => Some(*s), _ => None })
                                    .ok_or_else(|| CompileError::Generic(format!("type '{}' is not a struct", obj_type)))?;
                                let gep = self.gep().build_struct_gep(sty, heap_ptr, idx as u32, field_name)
                                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                                let field_ty = types::mimi_type_to_llvm(self.context, &fields[idx].ty)
                                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                                return self.builder.build_load(field_ty, gep, field_name)
                                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)));
                            }
                        }
                    }
                }
            }
        }
        // Field access: obj.field
        let obj_val = self.compile_expr(obj, vars)?;
        let obj_type = self.infer_object_type(obj, vars);
        let field_ptr = match obj_val {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::StructValue(sv) => {
                if let Some(BasicTypeEnum::StructType(sty)) = self.type_llvm.get(&obj_type) {
                    let alloca = self.builder.build_alloca(*sty, "tmp")
                        .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                    self.builder.build_store(alloca, sv)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    alloca
                } else {
                    return Err(format!("[E0707] cannot access field on type '{}'", obj_type).into());
                }
            }
            _ => return Err(CompileError::Generic(format!("field access requires a struct or actor type, got {}", obj_val.get_type()))),
        };
        let sty = match self.type_llvm.get(&obj_type) {
            Some(BasicTypeEnum::StructType(s)) => *s,
            _ => return Err(format!("type '{}' is not a struct", obj_type).into()),
        };
        if let Some(td) = self.type_defs.get(&obj_type) {
            if let TypeDefKind::Record(fields) = &td.kind {
                if let Some(idx) = fields.iter().position(|f| f.name == *field_name) {
                    let gep = self.gep().build_struct_gep(sty, field_ptr, idx as u32, field_name)
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    let field_ty = types::mimi_type_to_llvm(self.context, &fields[idx].ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                    return self.builder.build_load(field_ty, gep, field_name)
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)));
                }
            }
        }
        // Fallback: numeric field index
        if let Ok(idx) = field_name.parse::<u32>() {
            let gep = self.gep().build_struct_gep(sty, field_ptr, idx, field_name)
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            return self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), gep, field_name)
                .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)));
        }
        Err(format!("field '{}' not found on type '{}'", field_name, obj_type).into())
    }


    pub(in crate::codegen) fn compile_index_expr(
        &mut self,
        obj: &Expr,
        idx_expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // list[i] or arr[i] - load from array/list
        let obj_val = self.compile_expr(obj, vars)?;
        let idx_val = self.compile_expr(idx_expr, vars)?;
        match obj_val {
            BasicValueEnum::PointerValue(pv) => {
                let idx_iv = match idx_val {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("index must be i64".into()),
                };
                // Try list struct first: { i64 len, i8* data }
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                // Check if this looks like a list struct by trying to GEP field 0 (len)
                if let Ok(_len_gep) = self.gep().build_struct_gep(list_ty, pv, 0, "list.len_check") {
                    // Bounds check
                    self.check_list_bounds(pv, idx_iv, "index read")?;
                    // It's a list struct - load data pointer and index into it
                    let data_gep = self.gep().build_struct_gep(list_ty, pv, 1, "list.data")
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    let data_ptr = self.builder.build_load(
                        BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        data_gep, "data")
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
                        .into_pointer_value();
                    let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "data_i64")
                        .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                        .into_pointer_value();
                                        let elem_ptr = {
                        self.gep().build_in_bounds_gep(self.context.i64_type(), data_ptr_i64, &[idx_iv], "elem")
                    }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    let elem_int = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, "elem_val")
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
                        .into_int_value();
                    // Convert i64→struct when the list element type is a compound type.
                    // Only use variable-name lookup for direct Ident expressions
                    // (avoids wrong type inference for nested lists).
                    if let Expr::Ident(var_name) = obj {
                        if let Some(converted) = self.convert_list_elem_from_i64(elem_int, Some(var_name.as_str()))? {
                            return Ok(converted);
                        }
                    }
                    if let Some(converted) = self.convert_list_elem_by_type(elem_int, obj, vars)? {
                        return Ok(converted);
                    }
                    return Ok(elem_int.into());
                }
                // Fallback: treat as raw pointer to i64 array
                                let elem_ptr = {
                    self.gep().build_in_bounds_gep(self.context.i64_type(), pv, &[idx_iv], "elem")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, "elem_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
            }
            BasicValueEnum::StructValue(sv) => {
                let sv_ty = sv.get_type();
                let list_alloca = self.builder.build_alloca(sv_ty, "list_tmp")
                    .map_err(|e| CompileError::LlvmError(format!("list alloca: {}", e)))?;
                self.builder.build_store(list_alloca, sv)
                    .map_err(|e| CompileError::LlvmError(format!("store list: {}", e)))?;
                let idx_iv = match idx_val {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("index must be i64".into()),
                };
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                self.check_list_bounds(list_alloca, idx_iv, "index read")?;
                let data_gep = self.gep().build_struct_gep(list_ty, list_alloca, 1, "list.data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_ptr = self.builder.build_load(
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                    data_gep, "data",
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "data_i64",
                ).map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?.into_pointer_value();
                let elem_ptr = {
                    self.gep().build_in_bounds_gep(self.context.i64_type(), data_ptr_i64, &[idx_iv], "elem")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let elem_int = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, "elem_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
                    .into_int_value();
                // For struct-valued lists (from chained indexing), use type
                // inference to determine if elements are structs or scalars.
                if let Expr::Ident(var_name) = obj {
                    if let Some(converted) = self.convert_list_elem_from_i64(elem_int, Some(var_name.as_str()))? {
                        return Ok(converted);
                    }
                }
                if let Some(converted) = self.convert_list_elem_by_type(elem_int, obj, vars)? {
                    return Ok(converted);
                }
                Ok(elem_int.into())
            }
            BasicValueEnum::ArrayValue(_av) => {
                // Direct LLVM array value: extract element by index
                let idx = match idx_val {
                    BasicValueEnum::IntValue(iv) => {
                        // Convert runtime i64 index to constant u32 for extractvalue
                        iv.get_zero_extended_constant()
                            .ok_or_else(|| "array index must be a compile-time constant".to_string())? as u32
                    }
                    _ => return Err("index must be i64".into()),
                };
                let elem = self.builder.build_extract_value(obj_val.into_array_value(), idx, "arr_elem")
                    .map_err(|e| CompileError::LlvmError(format!("extractvalue error: {}", e)))?;
                Ok(elem)
            }
            _ => Err("index requires a list/array pointer".into()),
        }
    }


    pub(in crate::codegen) fn compile_tuple_index_expr(
        &mut self,
        tuple_expr: &Expr,
        index: usize,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let tuple_val = self.compile_expr(tuple_expr, vars)?;
        match tuple_val {
            BasicValueEnum::PointerValue(pv) => {
                let struct_ty = self.tuple_type_stack.last()
                    .ok_or_else(|| "tuple type stack empty".to_string())?;
                let field_gep = self.gep().build_struct_gep(*struct_ty, pv, index as u32, &format!("tuple_field_{}", index))
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let field_types = struct_ty.get_field_types();
                let field_ty = field_types.get(index)
                    .ok_or_else(|| format!("tuple field {} out of bounds", index))?;
                let field_ty = *field_ty;
                self.builder.build_load(field_ty, field_gep, &format!("tuple_{}", index))
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
            }
            BasicValueEnum::StructValue(sv) => {
                self.builder.build_extract_value(sv, index as u32, &format!("tuple_{}", index))
                    .map_err(|e| CompileError::LlvmError(format!("extract tuple field {} error: {}", index, e)))
            }
            _ => Err(CompileError::Generic(format!("tuple index requires a tuple value, got {:?}", tuple_val))),
        }
    }

}
