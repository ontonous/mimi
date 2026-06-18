#![allow(dead_code, deprecated)]

use crate::ast::*;
use crate::codegen::types;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_impl_methods(&mut self) -> MimiResult<()> {
        for (type_name, trait_impls) in self.type_impls.clone() {
            for (trait_name, methods) in &trait_impls {
                for method in methods {
                    // Skip non-committed methods
                    if !self.is_committed(&method.commitment) {
                        continue;
                    }
                    // Mangle name: {type_name}__{trait_name}__{method_name}
                    let mangled = format!("{}__{}__{}", type_name, trait_name, method.name);
                    // Build function: prepend self: &type_name as first param
                    let mut impl_method = method.clone();
                    impl_method.name = mangled;
                    // Prepend self param: self: &type_name
                    impl_method.params.insert(0, crate::ast::Param {
                        name: "self".into(),
                        ty: crate::ast::Type::Ref(None, Box::new(
                            crate::ast::Type::Name(type_name.clone(), vec![])
                        )),
                        mut_: false,
                    });
                    self.compile_func(&impl_method)?;
                }
            }
        }
        Ok(())
    }

    /// Build vtable struct types and global vtable instances for all trait impls.
    /// Called after compile_impl_methods so mangled functions exist.
    pub(super) fn compile_vtables(&mut self) -> MimiResult<()> {
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        // Phase 1: define vtable struct type per trait
        let mut trait_method_list: HashMap<String, Vec<String>> = HashMap::new();
        for (trait_name, trait_def) in &self.trait_defs {
            let method_names: Vec<String> = trait_def.methods.iter().map(|m| m.name.clone()).collect();
            if method_names.is_empty() {
                continue;
            }
            // Vtable struct: one i8* (function pointer) per method
            let field_tys: Vec<BasicTypeEnum> = (0..method_names.len())
                .map(|_| BasicTypeEnum::PointerType(i8_ptr))
                .collect();
            let vtable_ty = self.context.struct_type(&field_tys, false);
            self.vtable_types.insert(trait_name.clone(), vtable_ty);
            trait_method_list.insert(trait_name.clone(), method_names);
        }

        // Phase 2: emit a global vtable constant for each (type, trait) impl pair
        for (type_name, trait_impls) in &self.type_impls {
            for (trait_name, methods) in trait_impls {
                let Some(vtable_ty) = self.vtable_types.get(trait_name) else { continue };
                let Some(expected_methods) = trait_method_list.get(trait_name) else { continue };

                // Build initializer: one bitcast(function) per method slot
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let mut fn_ptrs: Vec<BasicValueEnum> = Vec::new();
                for method_name in expected_methods {
                    if methods.iter().any(|m| &m.name == method_name) {
                        let mangled = format!("{}__{}__{}", type_name, trait_name, method_name);
                        if let Some(f) = self.module.get_function(&mangled) {
                            let ptr = self.builder.build_bit_cast(
                                f.as_global_value().as_pointer_value(),
                                i8_ptr,
                                &format!("{}_{}_cast", trait_name, method_name),
                            ).map_err(|e| CompileError::Generic(format!("bitcast error: {}", e)))?;
                            fn_ptrs.push(ptr.into());
                            continue;
                        }
                    }
                    fn_ptrs.push(i8_ptr.const_null().into());
                }
                if fn_ptrs.is_empty() {
                    continue;
                }
                let init_val = vtable_ty.const_named_struct(&fn_ptrs);
                let gv_name = format!("{}_{}_vtable", type_name, trait_name);
                let gv = self.module.add_global(*vtable_ty, None, &gv_name);
                gv.set_initializer(&init_val);
                gv.set_constant(true);
                self.vtable_globals.insert(format!("{}__{}", type_name, trait_name), gv);
            }
        }
        Ok(())
    }

    pub(super) fn register_extern_block(&mut self, block: &crate::ast::ExternBlock) -> MimiResult<()> {
        for ef in &block.funcs {
            let mut param_tys = Vec::new();
            for p in &ef.params {
                let ty = types::mimi_type_to_llvm(self.context, &p.ty)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                param_tys.push(types::basic_to_metadata(self.context, ty));
            }
            let ret_ty = match &ef.ret {
                Some(ty) => types::mimi_type_to_llvm(self.context, ty)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
                None => BasicTypeEnum::IntType(self.context.i64_type()),
            };
            let fn_type = match ret_ty {
                BasicTypeEnum::IntType(t) => t.fn_type(&param_tys, false),
                BasicTypeEnum::FloatType(t) => t.fn_type(&param_tys, false),
                BasicTypeEnum::PointerType(t) => t.fn_type(&param_tys, false),
                BasicTypeEnum::StructType(t) => t.fn_type(&param_tys, false),
                BasicTypeEnum::ArrayType(t) => t.fn_type(&param_tys, false),
                _ => self.context.i64_type().fn_type(&param_tys, false),
            };
            let extern_name = format!("__mimi_extern_{}", ef.name);
            let extern_fn = self.module.add_function(&extern_name, fn_type, Some(inkwell::module::Linkage::External));
            let wrapper_fn = self.module.add_function(&ef.name, fn_type, Some(inkwell::module::Linkage::Internal));

            let entry = self.context.append_basic_block(wrapper_fn, "entry");
            let previous_block = self.builder.get_insert_block();
            self.builder.position_at_end(entry);

            let i64_ty = self.context.i64_type();
            let _i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());

            // Phase 1: Retain c_shared params before C call
            let mut shared_params: Vec<(usize, BasicValueEnum<'ctx>)> = Vec::new();
            for (i, p) in ef.params.iter().enumerate() {
                if matches!(p.ty, crate::ast::Type::CShared(_)) {
                    let param = wrapper_fn.get_nth_param(i as u32)
                        .ok_or_else(|| CompileError::Generic(format!("missing param {}", i)))?;
                    if let Some(retain_fn) = self.module.get_function("mimi_shared_retain") {
                        // c_shared is compiled as i8*, need to bitcast to i64 for the runtime call
                        let param_i64 = match param {
                            BasicValueEnum::IntValue(iv) => iv,
                            BasicValueEnum::PointerValue(pv) => {
                                self.builder.build_bit_cast(pv, i64_ty, &format!("ptr_to_i64_{}", i))
                                    .map_err(|e| CompileError::Generic(format!("bitcast error: {}", e)))?
                                    .into_int_value()
                            }
                            _ => return Err(CompileError::TypeMismatch(format!("c_shared param {} must be pointer or int", i))),
                        };
                        self.builder.build_call(retain_fn, &[
                            BasicMetadataValueEnum::IntValue(param_i64),
                        ], &format!("retain_{}", i))
                            .map_err(|e| CompileError::Generic(format!("retain error: {}", e)))?;
                    }
                    shared_params.push((i, param));
                }
            }

            // Phase 2: Check cap params
            for (i, p) in ef.params.iter().enumerate() {
                if let crate::ast::Type::Cap(cap_name) = &p.ty {
                    let param = wrapper_fn.get_nth_param(i as u32)
                        .ok_or_else(|| CompileError::Generic(format!("missing param {}", i)))?;
                    if let Some(check_fn) = self.module.get_function("mimi_cap_check") {
                        let cap_name_global = self.builder.build_global_string_ptr(
                            &format!("{}\0", cap_name), &format!("cap_name_{}", i))
                            .map_err(|e| CompileError::Generic(format!("string global error: {}", e)))?;
                        let cap_name_ptr = cap_name_global.as_pointer_value();
                        let check_result = self.builder.build_call(check_fn, &[
                            BasicMetadataValueEnum::IntValue(param.into_int_value()),
                            BasicMetadataValueEnum::PointerValue(cap_name_ptr),
                        ], &format!("cap_check_{}", i))
                            .map_err(|e| CompileError::Generic(format!("cap_check error: {}", e)))?
                            .try_as_basic_value().left()
                            .ok_or_else(|| CompileError::Generic("cap_check returned void".to_string()))?
                            .into_int_value();
                        // If cap_check returns false (0), abort
                        let is_valid = self.builder.build_int_compare(
                            inkwell::IntPredicate::NE, check_result,
                            self.context.bool_type().const_int(0, false),
                            "cap_valid")
                            .map_err(|e| CompileError::Generic(format!("compare error: {}", e)))?;
                        let function = self.current_function()
                            .ok_or_else(|| CompileError::Generic("codegen: no current function for cap check in extern block".to_string()))?;
                        let ok_bb = self.context.append_basic_block(function, &format!("cap_ok_{}", i));
                        let fail_bb = self.context.append_basic_block(function, &format!("cap_fail_{}", i));
                        self.builder.build_conditional_branch(is_valid, ok_bb, fail_bb)
                            .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                        self.builder.position_at_end(fail_bb);
                        if let Some(exit_fn) = self.module.get_function("exit") {
                            self.builder.build_call(exit_fn, &[
                                BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                            ], "cap_fail_exit")
                                .map_err(|e| CompileError::Generic(format!("exit error: {}", e)))?;
                        }
                        self.builder.build_unconditional_branch(ok_bb)
                            .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                        self.builder.position_at_end(ok_bb);
                    }
                }
            }

            // Phase 3: Build wrapper args and call extern function
            let wrapper_args: Vec<BasicMetadataValueEnum<'ctx>> = wrapper_fn
                .get_param_iter()
                .map(|p| match p {
                    BasicValueEnum::IntValue(v) => BasicMetadataValueEnum::IntValue(v),
                    BasicValueEnum::FloatValue(v) => BasicMetadataValueEnum::FloatValue(v),
                    BasicValueEnum::PointerValue(v) => BasicMetadataValueEnum::PointerValue(v),
                    BasicValueEnum::StructValue(v) => BasicMetadataValueEnum::StructValue(v),
                    BasicValueEnum::ArrayValue(v) => BasicMetadataValueEnum::ArrayValue(v),
                    BasicValueEnum::VectorValue(v) => BasicMetadataValueEnum::VectorValue(v),
                })
                .collect();

            let call = self.builder
                .build_call(extern_fn, &wrapper_args, "extern_call")
                .map_err(|e| CompileError::Generic(format!("failed to build extern wrapper call: {}", e)))?;

            // Phase 4: Release c_shared params after C call
            for (i, _param) in &shared_params {
                if let Some(release_fn) = self.module.get_function("mimi_shared_release") {
                    let orig_param = wrapper_fn.get_nth_param(*i as u32)
                        .ok_or_else(|| CompileError::Generic(format!("missing param {}", i)))?;
                    let param_i64 = match orig_param {
                        BasicValueEnum::IntValue(iv) => iv,
                        BasicValueEnum::PointerValue(pv) => {
                            self.builder.build_bit_cast(pv, i64_ty, &format!("ptr_to_i64_rel_{}", i))
                                .map_err(|e| format!("bitcast error: {}", e))?
                                .into_int_value()
                        }
                        _ => return Err(CompileError::TypeMismatch(format!("c_shared param {} must be pointer or int", i))),
                    };
                    self.builder.build_call(release_fn, &[
                        BasicMetadataValueEnum::IntValue(param_i64),
                    ], &format!("release_{}", i))
                        .map_err(|e| CompileError::Generic(format!("release error: {}", e)))?;
                }
            }

            // Phase 5: Return
            if fn_type.get_return_type().is_some() {
                let ret = call.try_as_basic_value().left().ok_or_else(|| CompileError::Generic("extern wrapper call did not return a value".to_string()))?;
                self.builder.build_return(Some(&ret))
                    .map_err(|e| CompileError::Generic(format!("failed to build extern wrapper return: {}", e)))?;
            } else {
                self.builder.build_return(None)
                    .map_err(|e| CompileError::Generic(format!("failed to build extern wrapper return: {}", e)))?;
            }

            if let Some(block) = previous_block {
                self.builder.position_at_end(block);
            }
        }
        Ok(())
    }

    pub(super) fn register_type_def(&mut self, t: &crate::ast::TypeDef) -> MimiResult<()> {
        let llvm_ty = match &t.kind {
            crate::ast::TypeDefKind::Record(fields) => {
                let mut field_tys = Vec::new();
                for f in fields {
                    let ty = types::mimi_type_to_llvm(self.context, &f.ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                    field_tys.push(ty);
                }
                BasicTypeEnum::StructType(self.context.struct_type(&field_tys, false))
            }
            crate::ast::TypeDefKind::Enum(_variants) => {
                // Enum representation: i32 tag + union of largest variant payload
                let tag_ty = BasicTypeEnum::IntType(self.context.i32_type());
                let payload_ty = BasicTypeEnum::IntType(self.context.i64_type());
                // For simplicity, use i64 as payload storage
                BasicTypeEnum::StructType(self.context.struct_type(&[tag_ty, payload_ty], false))
            }
            crate::ast::TypeDefKind::Alias(ty) | crate::ast::TypeDefKind::Newtype(ty) => {
                types::mimi_type_to_llvm(self.context, ty)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
            }
        };
        self.type_llvm.insert(t.name.clone(), llvm_ty);
        self.type_defs.insert(t.name.clone(), t.clone());
        Ok(())
    }

    pub(super) fn register_actor_def(&mut self, actor: &crate::ast::ActorDef) -> MimiResult<()> {
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
            commitment: actor.commitment,
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
