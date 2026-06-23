use crate::ast::*;
use crate::codegen::types;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::BasicMetadataValueEnum;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn register_type_def(&mut self, t: &crate::ast::TypeDef) -> MimiResult<()> {
        let llvm_ty = match &t.kind {
            crate::ast::TypeDefKind::Record(fields) => {
                let mut field_tys = Vec::new();
                for f in fields {
                    let ty = types::mimi_type_to_llvm(self.context, &f.ty)
                        .ok_or_else(|| CompileError::LlvmError(format!(
                            "cannot map record field '{}' type to LLVM", crate::core::fmt_type(&f.ty)
                        )))?;
                    field_tys.push(ty);
                }
                BasicTypeEnum::StructType(self.context.struct_type(&field_tys, false))
            }
            crate::ast::TypeDefKind::Enum(variants) => {
                // Sort variants by name so ordinals are deterministic (not dependent on
                // declaration order). Matches the interp's string-based tag comparison.
                let mut sorted_variants: Vec<&crate::ast::Variant> = variants.iter().collect();
                sorted_variants.sort_by_key(|v| &v.name);
                if t.attributes.contains(&TypeAttribute::ReprC) {
                    // #[repr(C)] enums are plain i32 (matching C int / enum)
                    let enum_ty = BasicTypeEnum::IntType(self.context.i32_type());
                    // Register constructor functions for each variant
                    for (ordinal, v) in sorted_variants.iter().enumerate() {
                        let ctor_name = format!("{}_{}", t.name, v.name);
                        if self.module.get_function(&ctor_name).is_none() {
                            let fn_type = self.context.i32_type().fn_type(&[], false);
                            let ctor = self.module.add_function(&ctor_name, fn_type, Some(inkwell::module::Linkage::Internal));
                            let entry = self.context.append_basic_block(ctor, "entry");
                            let prev_block = self.builder.get_insert_block();
                            self.builder.position_at_end(entry);
                            self.builder.build_return(Some(&self.context.i32_type().const_int(ordinal as u64, false)))
                                .map_err(|e| CompileError::LlvmError(format!("ctor return error: {}", e)))?;
                            if let Some(prev) = prev_block { self.builder.position_at_end(prev); }
                        }
                    }
                    enum_ty
                } else {
                    // Internal enum representation: i32 tag + i64 payload (uniform)
                    // Struct-typed payloads are ptrtoint-encoded into the i64 slot.
                    let payload_ty = BasicTypeEnum::IntType(self.context.i64_type());
                    let tag_ty = BasicTypeEnum::IntType(self.context.i32_type());
                    let enum_ty = BasicTypeEnum::StructType(self.context.struct_type(&[tag_ty, payload_ty], false));
                    // Register constructor functions for each variant
                    let struct_ty = self.context.struct_type(&[
                        BasicTypeEnum::IntType(self.context.i32_type()),
                        payload_ty,
                    ], false);
                    // Metadata type for constructor parameter
                    let meta_payload_ty = match payload_ty {
                        BasicTypeEnum::IntType(t) => BasicMetadataTypeEnum::IntType(t),
                        BasicTypeEnum::FloatType(t) => BasicMetadataTypeEnum::FloatType(t),
                        BasicTypeEnum::PointerType(t) => BasicMetadataTypeEnum::PointerType(t),
                        BasicTypeEnum::StructType(t) => BasicMetadataTypeEnum::StructType(t),
                        BasicTypeEnum::ArrayType(t) => BasicMetadataTypeEnum::ArrayType(t),
                        _ => BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    };
                    for (ordinal, v) in sorted_variants.iter().enumerate() {
                        let ctor_name = format!("{}_{}", t.name, v.name);
                        if self.module.get_function(&ctor_name).is_none() {
                            // Determine payload LLVM type for this variant
                            let payload_llvm = v.payload.as_ref().and_then(|p| match p {
                                crate::ast::VariantPayload::Tuple(types) if types.len() == 1 => {
                                    self.llvm_type_for(&types[0])
                                }
                                _ => None,
                            });
                            let payload_is_struct = matches!(payload_llvm, Some(BasicTypeEnum::StructType(_)));
                            let fn_type = if v.payload.is_some() {
                                if payload_is_struct {
                                    struct_ty.fn_type(&[types::basic_to_metadata(self.context, payload_llvm.expect("payload_llvm is Some when payload_is_struct"))], false)
                                } else {
                                    struct_ty.fn_type(&[meta_payload_ty], false)
                                }
                            } else {
                                struct_ty.fn_type(&[], false)
                            };
                            let ctor = self.module.add_function(&ctor_name, fn_type, Some(inkwell::module::Linkage::Internal));
                            let entry = self.context.append_basic_block(ctor, "entry");
                            let prev_block = self.builder.get_insert_block();
                            self.builder.position_at_end(entry);
                            let alloca = self.builder.build_alloca(struct_ty, &ctor_name)
                                .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                            let tag_gep = self.gep().build_struct_gep(struct_ty, alloca, 0, "tag")
                                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                            self.builder.build_store(tag_gep, self.context.i32_type().const_int(ordinal as u64, false))
                                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                            if v.payload.is_some() {
                                let payload_arg = ctor.get_nth_param(0).ok_or_else(|| CompileError::LlvmError("missing payload param".to_string()))?;
                                let payload_gep = self.gep().build_struct_gep(struct_ty, alloca, 1, "payload")
                                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                                if payload_is_struct {
                                    // Struct-typed payload: malloc, store, ptrtoint to i64
                                    let payload_struct = payload_arg.into_struct_value();
                                    let payload_struct_ty = payload_llvm.expect("payload_llvm is Some when payload_is_struct");
                                    let struct_size = payload_struct_ty.size_of()
                                        .ok_or_else(|| CompileError::LlvmError("cannot get payload struct size".to_string()))?;
                                    let malloc_fn = self.module.get_function("malloc")
                                        .ok_or_else(|| "malloc not declared".to_string())?;
                                    let size_val = self.context.i64_type().const_int(
                                        struct_size.get_zero_extended_constant().unwrap_or(16), false
                                    );
                                    let malloc_call = self.builder.build_call(malloc_fn, &[
                                        BasicMetadataValueEnum::IntValue(size_val),
                                    ], "payload_malloc")
                                        .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?;
                                    let malloc_result = crate::codegen::call_try_basic_value(&malloc_call)
                                        .ok_or_else(|| "malloc returned void")?
                                        .into_pointer_value();
                                    let typed_ptr = self.builder.build_pointer_cast(
                                        malloc_result,
                                        payload_struct_ty.ptr_type(inkwell::AddressSpace::default()),
                                        "typed_ptr",
                                    ).map_err(|e| CompileError::LlvmError(format!("ptr cast: {}", e)))?;
                                    self.builder.build_store(typed_ptr, payload_struct)
                                        .map_err(|e| CompileError::LlvmError(format!("store struct: {}", e)))?;
                                    let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                                    let ptr_to_i8 = self.builder.build_pointer_cast(
                                        malloc_result, i8_ptr, "ptr_i8"
                                    ).map_err(|e| CompileError::LlvmError(format!("ptr cast: {}", e)))?;
                                    let int_val = self.builder.build_ptr_to_int(ptr_to_i8, self.context.i64_type(), "payload_int")
                                        .map_err(|e| CompileError::LlvmError(format!("ptr2int: {}", e)))?;
                                    self.builder.build_store(payload_gep, int_val)
                                        .map_err(|e| CompileError::LlvmError(format!("store int: {}", e)))?;
                                } else {
                                    self.builder.build_store(payload_gep, payload_arg)
                                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                                }
                            }
                            let loaded = self.builder.build_load(struct_ty, alloca, &ctor_name)
                                .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                            self.builder.build_return(Some(&loaded))
                                .map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                            if let Some(prev) = prev_block { self.builder.position_at_end(prev); }
                        }
                    }
                    enum_ty
                }
            }
            crate::ast::TypeDefKind::Alias(ty) | crate::ast::TypeDefKind::Newtype(ty) => {
                types::mimi_type_to_llvm(self.context, ty)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
            }
            crate::ast::TypeDefKind::Union(fields) => {
                // Represent union as a byte array large enough to hold the largest field
                let max_size = fields.iter().map(|f| {
                    let llvm_ty = types::mimi_type_to_llvm(self.context, &f.ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                    llvm_ty.size_of()
                        .and_then(|s| s.get_zero_extended_constant())
                        .unwrap_or(8)
                }).max().unwrap_or(8);
                let array_ty = self.context.i8_type().array_type(max_size as u32);
                BasicTypeEnum::ArrayType(array_ty)
            }
        };
        self.type_llvm.insert(t.name.clone(), llvm_ty);
        self.type_defs.insert(t.name.clone(), t.clone());
        // Track record types for FFI serialization
        if matches!(t.kind, crate::ast::TypeDefKind::Record(_)) {
            self.record_type_names.insert(t.name.clone());
            if t.attributes.contains(&TypeAttribute::ReprC) {
                self.repr_c_record_names.insert(t.name.clone());
            }
        }
        Ok(())
    }


    pub(in crate::codegen) fn register_actor_def(&mut self, actor: &crate::ast::ActorDef) -> MimiResult<()> {
        // Represent actor as a struct with fields
        let mut field_tys = Vec::new();
        for f in &actor.fields {
            let ty = types::mimi_type_to_llvm(self.context, &f.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            field_tys.push(ty);
        }
        let llvm_ty = BasicTypeEnum::StructType(self.context.struct_type(&field_tys, false));
        self.type_llvm.insert(actor.name.clone(), llvm_ty);
        
        // Also register as a type definition for field access
        let type_def = crate::ast::TypeDef {
            name: actor.name.clone(),
            pub_: actor.pub_,
            kind: crate::ast::TypeDefKind::Record(actor.fields.iter().map(|f| crate::ast::Field {
                name: f.name.clone(),
                ty: f.ty.clone(),
            }).collect()),
            generics: Vec::new(),
            derives: Vec::new(),
            attributes: Vec::new(),
        };
        self.type_defs.insert(actor.name.clone(), type_def);
        Ok(())
    }
}
