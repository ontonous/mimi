use crate::codegen::{call_try_basic_value, CallbackThunkEntry, CallSiteValueExt, CodeGenerator, VarEntry};
use crate::codegen::types;
use crate::error::{CompileError, MimiResult};
use inkwell::ThreadLocalMode;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};
use std::collections::HashMap;
use super::helpers::elem_type_tag;

impl<'ctx> CodeGenerator<'ctx> {
    fn type_to_llvm_for_extern(&self, ty: &crate::ast::Type) -> BasicTypeEnum<'ctx> {
        // For user-defined types (Type::Name), prefer the registered type_llvm entry
        // which has the correct layout (e.g. i32 for #[repr(C)] enums).
        if let crate::ast::Type::Name(name, _) = ty {
            if let Some(&registered) = self.type_llvm.get(name.as_str()) {
                return registered;
            }
        }
        // G1b: Closure types (Type::Func) cross FFI as raw function pointers (i8*),
        // not as {fn_ptr, env_ptr} structs. The conversion is done at the call site
        // via get_or_create_callback_thunk + TLS globals.
        if matches!(ty, crate::ast::Type::Func(_, _)) {
            let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
            return BasicTypeEnum::PointerType(i8_ptr);
        }
        types::mimi_type_to_llvm_extern(self.context, ty)
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
    }

    pub(in crate::codegen) fn compile_impl_methods(&mut self) -> MimiResult<()> {
        for (type_name, trait_impls) in self.type_impls.clone() {
            for (trait_name, methods) in &trait_impls {
                for method in methods {
                    // Mangle name: {type_name}__{trait_name}__{method_name}
                    let mangled = format!("{}__{}__{}", type_name, trait_name, method.name);
                    // Build function: prepend self: &type_name as first param
                    let mut impl_method = method.clone();
                    impl_method.name = mangled;
                    // Prepend self param: self: &type_name (or self: type_name for value types)
                    let self_ty = match type_name.as_str() {
                        "i32" | "i64" | "f64" | "bool" => {
                            // Value types: pass self by value so arithmetic works
                            crate::ast::Type::Name(type_name.clone(), vec![])
                        }
                        _ => {
                            // Compound types: pass self by reference
                            crate::ast::Type::Ref(None, Box::new(
                                crate::ast::Type::Name(type_name.clone(), vec![])
                            ))
                        }
                    };
                    impl_method.params.insert(0, crate::ast::Param {
                        name: "self".into(),
                        ty: self_ty,
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
    pub(in crate::codegen) fn compile_vtables(&mut self) -> MimiResult<()> {
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
                            ).map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
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



    /// G1b: Get or create a callback thunk for a given callback signature.
    /// The thunk is a small LLVM function that:
    /// 1. Reads fn_ptr and env_ptr from module-level globals
    /// 2. Calls fn_ptr(env_ptr, args...) with the correct C calling convention
    /// Returns the thunk function value and its two global slots.
    pub(in crate::codegen) fn get_or_create_callback_thunk(
        &mut self,
        cb_params: &[crate::ast::Type],
        cb_ret: &crate::ast::Type,
    ) -> MimiResult<CallbackThunkEntry<'ctx>> {
        // Build fingerprint from signature
        let ret_str = format!("{:?}", cb_ret);
        let params_str: Vec<String> = cb_params.iter().map(|t| format!("{:?}", t)).collect();
        let fingerprint = format!("{}_{}", ret_str, params_str.join("_"));

        // Check cache
        if let Some(entry) = self.callback_thunks.get(&fingerprint) {
            return Ok(CallbackThunkEntry {
                thunk_fn: entry.thunk_fn,
                fn_ptr_global: entry.fn_ptr_global,
                env_ptr_global: entry.env_ptr_global,
            });
        }

        let i8_type = self.context.i8_type();
        let i8_ptr = i8_type.ptr_type(inkwell::AddressSpace::default());
        let id = self.callback_thunk_counter;
        self.callback_thunk_counter += 1;

        // Create global slots for fn_ptr and env_ptr
        // F1: Use thread-local storage so concurrent parasteps threads each
        // see their own fn_ptr/env_ptr, preventing data races on the globals.
        let fn_ptr_global = self.module.add_global(i8_ptr, None, &format!("__mimi_cb_fnptr_{}", id));
        fn_ptr_global.set_initializer(&i8_ptr.const_null());
        fn_ptr_global.set_thread_local(true);
        fn_ptr_global.set_thread_local_mode(Some(ThreadLocalMode::GeneralDynamicTLSModel));
        let env_ptr_global = self.module.add_global(i8_ptr, None, &format!("__mimi_cb_envptr_{}", id));
        env_ptr_global.set_initializer(&i8_ptr.const_null());
        env_ptr_global.set_thread_local(true);
        env_ptr_global.set_thread_local_mode(Some(ThreadLocalMode::GeneralDynamicTLSModel));

        // Build thunk function type matching callback signature
        let mut thunk_param_tys = Vec::new();
        for pt in cb_params {
            let llvm_ty = types::mimi_type_to_llvm_extern(self.context, pt)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            thunk_param_tys.push(types::basic_to_metadata(self.context, llvm_ty));
        }
        let thunk_ret_llvm = types::mimi_type_to_llvm_extern(self.context, cb_ret)
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));

        let thunk_fn_type = match thunk_ret_llvm {
            BasicTypeEnum::IntType(t) => t.fn_type(&thunk_param_tys, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&thunk_param_tys, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&thunk_param_tys, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&thunk_param_tys, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&thunk_param_tys, false),
            _ => self.context.i64_type().fn_type(&thunk_param_tys, false),
        };

        let thunk_name = format!("__mimi_thunk_{}", id);
        let thunk_fn = self.module.add_function(&thunk_name, thunk_fn_type, Some(inkwell::module::Linkage::Internal));

        let saved_block = self.builder.get_insert_block();
        let entry_bb = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry_bb);

        // Load fn_ptr and env_ptr from global slots
        let fn_ptr_val = self.builder.build_load(i8_ptr, fn_ptr_global.as_pointer_value(), "tls_fn_ptr")
            .map_err(|e| CompileError::LlvmError(format!("load fn_ptr: {}", e)))?;
        let env_ptr_val = self.builder.build_load(i8_ptr, env_ptr_global.as_pointer_value(), "tls_env_ptr")
            .map_err(|e| CompileError::LlvmError(format!("load env_ptr: {}", e)))?;
        let fn_ptr_pv = fn_ptr_val.into_pointer_value();
        let env_ptr_pv = env_ptr_val.into_pointer_value();

        // Build the Mimi closure function signature: fn(env_ptr: i8*, params...) -> ret
        let mut mimi_param_meta = vec![BasicMetadataTypeEnum::PointerType(i8_ptr)];
        mimi_param_meta.extend(thunk_param_tys.iter().cloned());

        let mimi_fn_ty = match thunk_ret_llvm {
            BasicTypeEnum::IntType(t) => t.fn_type(&mimi_param_meta, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&mimi_param_meta, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&mimi_param_meta, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&mimi_param_meta, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&mimi_param_meta, false),
            _ => self.context.i64_type().fn_type(&mimi_param_meta, false),
        };

        // Cast fn_ptr to the correct type: fn(env_ptr, params...) -> ret
        let fn_ptr_typed = self.builder.build_pointer_cast(
            fn_ptr_pv,
            mimi_fn_ty.ptr_type(inkwell::AddressSpace::default()),
            "fn_typed",
        ).map_err(|e| CompileError::LlvmError(format!("pointer cast: {}", e)))?;

        // Build call args: env_ptr + thunk params
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![
            BasicMetadataValueEnum::PointerValue(env_ptr_pv),
        ];
        for i in 0..thunk_param_tys.len() {
            let param = thunk_fn.get_nth_param(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("thunk param {} not found", i)))?;
            call_args.push(match param {
                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(iv),
                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(fv),
                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(pv),
                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(sv),
                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(av),
                _ => return Err(CompileError::LlvmError(format!("unsupported thunk param type at {}", i))),
            });
        }

        let cb_call = self.builder.build_indirect_call(
            mimi_fn_ty, fn_ptr_typed, &call_args, "cb_call",
        ).map_err(|e| CompileError::LlvmError(format!("thunk callback call: {}", e)))?;
        if thunk_fn_type.get_return_type().is_some() {
            let ret_val = call_try_basic_value(&cb_call)
                .ok_or_else(|| CompileError::LlvmError("thunk call returned void but expected value".to_string()))?;
            self.builder.build_return(Some(&ret_val))
                .map_err(|e| CompileError::LlvmError(format!("thunk return: {}", e)))?;
        } else {
            self.builder.build_return(None)
                .map_err(|e| CompileError::LlvmError(format!("thunk void return: {}", e)))?;
        }

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        let entry = CallbackThunkEntry { thunk_fn, fn_ptr_global, env_ptr_global };
        self.callback_thunks.insert(fingerprint, entry);

        Ok(CallbackThunkEntry { thunk_fn, fn_ptr_global, env_ptr_global })
    }


    pub(in crate::codegen) fn register_extern_block(&mut self, block: &crate::ast::ExternBlock) -> MimiResult<()> {
        for ef in &block.funcs {
            // Store param types for later closure→thunk conversion in compile_call
            let param_types: Vec<crate::ast::Type> = ef.params.iter().map(|p| p.ty.clone()).collect();
            self.extern_param_types.insert(ef.name.clone(), param_types);

            // F7: Tuple is allowed via JSON serialization (same path as List/Record).
            // The interpreter path uses FfiArgContract::Json; the codegen path
            // uses mimi_tuple_serialize/mimi_tuple_deserialize runtime functions.

            // D: Reject Unsupported FFI contract types at codegen time with a
            // readable error rather than silently passing void* to C (UB risk).
            let contract = crate::ffi::FfiContract::from_extern_with_caps(
                ef, &self.cap_type_names, &self.record_type_names);
            for (i, arg_contract) in contract.args.iter().enumerate() {
                if let crate::ffi::contract::FfiArgContract::Unsupported(ty) = arg_contract {
                    return Err(CompileError::LlvmError(format!(
                        "codegen does not yet support extern parameter type '{}' for '{}' \
                         (param {}). Use `mimi run` (interpreter) for JSON-serialized FFI, \
                         or convert to a #[repr(C)] record type.",
                        ty, ef.name, i
                    )));
                }
            }
            if let crate::ffi::contract::FfiRetContract::Unsupported(ty) = &contract.ret {
                return Err(CompileError::LlvmError(format!(
                    "codegen does not yet support extern return type '{}' for '{}'. \
                     Use `mimi run` (interpreter) for JSON-serialized FFI, \
                     or convert to a #[repr(C)] record type.",
                    ty, ef.name
                )));
            }

            // Store extern func definition + ABI for lazy code generation
            self.extern_func_defs.insert(ef.name.clone(), ef.clone());
            self.extern_block_abis.insert(ef.name.clone(), block.abi.clone());
        }
        Ok(())
    }

    /// Lazily generate the LLVM wrapper function and extern declaration for
    /// an extern function that was previously registered by register_extern_block.
    /// Called from compile_call when an extern function is actually invoked.
    pub(in crate::codegen) fn generate_extern_fn(&mut self, name: &str) -> MimiResult<()> {
        let ef = self.extern_func_defs.get(name)
            .ok_or_else(|| CompileError::LlvmError(format!("extern function '{}' not registered", name)))?
            .clone();
        let abi = self.extern_block_abis.get(name)
            .ok_or_else(|| CompileError::LlvmError(format!("extern ABI for '{}' not found", name)))?
            .clone();

        let list_struct_sty = self.context.struct_type(&[
            BasicTypeEnum::IntType(self.context.i64_type()),
            BasicTypeEnum::PointerType(self.context.i8_type().ptr_type(inkwell::AddressSpace::default())),
        ], false);
        let list_struct_ty = BasicTypeEnum::StructType(list_struct_sty);
        let list_ptr_ty = BasicTypeEnum::PointerType(list_struct_sty.ptr_type(inkwell::AddressSpace::default()));
        let mut param_tys = Vec::new();
        for p in &ef.params {
            let ty = match &p.ty {
                crate::ast::Type::Name(n, _) if n == "List" || (self.record_type_names.contains(n.as_str()) && !self.repr_c_record_names.contains(n.as_str())) => {
                    list_ptr_ty
                }
                _ => self.type_to_llvm_for_extern(&p.ty),
            };
            param_tys.push(types::basic_to_metadata(self.context, ty));
        }
        let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let mut extern_param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        for p in &ef.params {
            let llvm_ty = match &p.ty {
                crate::ast::Type::Name(n, _) if n == "string" || n == "List" || (self.record_type_names.contains(n.as_str()) && !self.repr_c_record_names.contains(n.as_str())) => {
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty)
                }
                // F7: Tuple extern params are serialized to JSON, passed as i8*
                crate::ast::Type::Tuple(_) => {
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty)
                }
                _ => {
                    let ty = self.type_to_llvm_for_extern(&p.ty);
                    types::basic_to_metadata(self.context, ty)
                }
            };
            extern_param_tys.push(llvm_ty);
        }
        let wrapper_ret_ty = match &ef.ret {
            Some(ty) => {
                if matches!(ty, crate::ast::Type::Name(n, _) if n == "List" || (self.record_type_names.contains(n.as_str()) && !self.repr_c_record_names.contains(n.as_str()))) {
                    list_struct_ty
                // F7: Tuple return — wrapper returns the LLVM struct type
                } else if matches!(ty, crate::ast::Type::Tuple(_)) {
                    types::mimi_type_to_llvm(self.context, ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
                } else {
                    self.type_to_llvm_for_extern(ty)
                }
            }
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };
        let extern_ret_ty = match &ef.ret {
            Some(ty) => {
                if matches!(ty, crate::ast::Type::Name(n, _) if n == "List" || (self.record_type_names.contains(n.as_str()) && !self.repr_c_record_names.contains(n.as_str()))) {
                    BasicTypeEnum::PointerType(i8_ptr_ty)
                // F7: Tuple return — C returns JSON string (i8*)
                } else if matches!(ty, crate::ast::Type::Tuple(_)) {
                    BasicTypeEnum::PointerType(i8_ptr_ty)
                // BUG 1 fix: C functions return string as char* (i8*)
                } else if matches!(ty, crate::ast::Type::Name(n, _) if n == "string") {
                    BasicTypeEnum::PointerType(i8_ptr_ty)
                } else {
                    self.type_to_llvm_for_extern(ty)
                }
            }
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };
        let is_void_return = ef.ret.is_none();
        let is_variadic = ef.variadic;
        let wrapper_fn_type = if is_void_return {
            self.context.void_type().fn_type(&param_tys, is_variadic)
        } else {
            match wrapper_ret_ty {
                BasicTypeEnum::IntType(t) => t.fn_type(&param_tys, is_variadic),
                BasicTypeEnum::FloatType(t) => t.fn_type(&param_tys, is_variadic),
                BasicTypeEnum::PointerType(t) => t.fn_type(&param_tys, is_variadic),
                BasicTypeEnum::StructType(t) => t.fn_type(&param_tys, is_variadic),
                BasicTypeEnum::ArrayType(t) => t.fn_type(&param_tys, is_variadic),
                _ => self.context.i64_type().fn_type(&param_tys, is_variadic),
            }
        };
        let extern_fn_type = if is_void_return {
            self.context.void_type().fn_type(&extern_param_tys, is_variadic)
        } else {
            match extern_ret_ty {
                BasicTypeEnum::IntType(t) => t.fn_type(&extern_param_tys, is_variadic),
                BasicTypeEnum::FloatType(t) => t.fn_type(&extern_param_tys, is_variadic),
                BasicTypeEnum::PointerType(t) => t.fn_type(&extern_param_tys, is_variadic),
                BasicTypeEnum::StructType(t) => t.fn_type(&extern_param_tys, is_variadic),
                BasicTypeEnum::ArrayType(t) => t.fn_type(&extern_param_tys, is_variadic),
                _ => self.context.i64_type().fn_type(&extern_param_tys, is_variadic),
            }
        };
        let extern_name = format!("__mimi_extern_{}", ef.name);
        let extern_fn = self.module.add_function(&extern_name, extern_fn_type, Some(inkwell::module::Linkage::External));
        // Set calling convention on the external function based on block ABI
        let cc = crate::ffi::abi_to_llvm_call_conv(&abi);
        extern_fn.set_call_conventions(cc);
        let wrapper_fn = self.module.add_function(&ef.name, wrapper_fn_type, Some(inkwell::module::Linkage::Internal));

        let entry = self.context.append_basic_block(wrapper_fn, "entry");
        let previous_block = self.builder.get_insert_block();
        self.builder.position_at_end(entry);

        let i64_ty = self.context.i64_type();

        // Phase 1: Retain c_shared/c_borrow/c_borrow_mut params before C call
        // F2: c_borrow and c_borrow_mut need retain/release just like c_shared
        // to ensure the handle stays alive during the C call.
        let mut shared_params: Vec<(usize, BasicValueEnum<'ctx>)> = Vec::new();
        for (i, p) in ef.params.iter().enumerate() {
            if matches!(p.ty, crate::ast::Type::CShared(_))
                || matches!(p.ty, crate::ast::Type::CBorrow(_))
                || matches!(p.ty, crate::ast::Type::CBorrowMut(_))
            {
                let param = wrapper_fn.get_nth_param(i as u32)
                    .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
                if let Some(retain_fn) = self.module.get_function("mimi_shared_retain") {
                    // These passport types are compiled as i8*, need to bitcast to i64 for the runtime call
                    let param_i64 = match param {
                        BasicValueEnum::IntValue(iv) => iv,
                        BasicValueEnum::PointerValue(pv) => {
                            self.builder.build_bit_cast(pv, i64_ty, &format!("ptr_to_i64_{}", i))
                                .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                                .into_int_value()
                        }
                        _ => return Err(CompileError::TypeMismatch(format!("c_shared/c_borrow param {} must be pointer or int", i))),
                    };
                    self.builder.build_call(retain_fn, &[
                        BasicMetadataValueEnum::IntValue(param_i64),
                    ], &format!("retain_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("retain error: {}", e)))?;
                }
                shared_params.push((i, param));
            }
        }

        // Phase 2: Check cap params
        for (i, p) in ef.params.iter().enumerate() {
            if let crate::ast::Type::Cap(cap_name) = &p.ty {
                let param = wrapper_fn.get_nth_param(i as u32)
                    .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
                if let Some(check_fn) = self.module.get_function("mimi_cap_check") {
                    let cap_name_global = self.builder.build_global_string_ptr(
                        &format!("{}\0", cap_name), &format!("cap_name_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("string global error: {}", e)))?;
                    let cap_name_ptr = cap_name_global.as_pointer_value();
                let check_result = self.builder.build_call(check_fn, &[
                    BasicMetadataValueEnum::IntValue(param.into_int_value()),
                    BasicMetadataValueEnum::PointerValue(cap_name_ptr),
                ], &format!("cap_check_{}", i))
                    .map_err(|e| CompileError::LlvmError(format!("cap_check error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("cap_check returned void".to_string()))?
                    .into_int_value();
                // If cap_check returns false (0), abort
                let zero_i32 = self.context.i32_type().const_int(0, false);
                let is_valid = self.builder.build_int_compare(
                    inkwell::IntPredicate::NE, check_result, zero_i32,
                    "cap_valid")
                    .map_err(|e| CompileError::LlvmError(format!("compare error: {}", e)))?;
                    let function = self.current_function()
                        .ok_or_else(|| CompileError::LlvmError("codegen: no current function for cap check in extern block".to_string()))?;
                    let ok_bb = self.context.append_basic_block(function, &format!("cap_ok_{}", i));
                    let fail_bb = self.context.append_basic_block(function, &format!("cap_fail_{}", i));
                    self.builder.build_conditional_branch(is_valid, ok_bb, fail_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    self.builder.position_at_end(fail_bb);
                    if let Some(exit_fn) = self.module.get_function("exit") {
                        self.builder.build_call(exit_fn, &[
                            BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                        ], "cap_fail_exit")
                            .map_err(|e| CompileError::LlvmError(format!("exit error: {}", e)))?;
                    }
                    self.builder.build_unconditional_branch(ok_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    self.builder.position_at_end(ok_bb);
                }
            }
        }

        // Phase 3: Check requires contract before C call
        // NOTE: Unlike the interpreter path (interp/ffi_call.rs) which returns
        // Errno::Generic on contract violation, the codegen path aborts the
        // process via mimi_runtime_abort. This is a deliberate design choice:
        // compiled code has no Result-returning convention for extern calls,
        // so graceful error propagation would require a sweeping API change.
        // The interp path's `verify_ffi` flag and the codegen path's
        // `verify_contracts` flag are the respective gates (both default true).
        if self.verify_contracts {
            if let Some(req_expr) = &ef.requires {
                let mut contract_vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
                for (i, p) in ef.params.iter().enumerate() {
                    let param = wrapper_fn.get_nth_param(i as u32)
                        .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
                    let llvm_ty = param.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, &p.name)
                        .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                    self.builder.build_store(alloca, param)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    contract_vars.insert(p.name.clone(), (alloca, llvm_ty));
                }
                self.compile_contract_assert(req_expr, &contract_vars, &format!("requires violation in extern '{}'", ef.name))?;
            }
        }

        // Phase 4: Build wrapper args, converting Mimi types to C ABI.
        // string {i8*, i64} → i8*, List/Record {i64, i8*} → JSON char*
        let mut wrapper_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        let mut json_strings: Vec<PointerValue<'ctx>> = Vec::new();
        for (i, p) in ef.params.iter().enumerate() {
            let param = wrapper_fn.get_nth_param(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
            match &p.ty {
                crate::ast::Type::Name(n, _) if n == "string" => {
                    let struct_val = param.into_struct_value();
                    let ptr = self.builder.build_extract_value(struct_val, 0, &format!("str_ptr_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("extract value error: {}", e)))?;
                    wrapper_args.push(match ptr {
                        BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(pv),
                        _ => return Err(CompileError::TypeMismatch(
                            format!("string param {}: expected pointer from struct field 0", i))),
                    });
                }
                crate::ast::Type::Name(n, _) if n == "List" || (self.record_type_names.contains(n.as_str()) && !self.repr_c_record_names.contains(n.as_str())) => {
                    let list_ptr = param.into_pointer_value();
                    let len_gep = self.builder.build_struct_gep(
                        list_struct_sty, list_ptr, 0, &format!("list_len_gep_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    let data_gep = self.builder.build_struct_gep(
                        list_struct_sty, list_ptr, 1, &format!("list_data_gep_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    let len_val = self.builder.build_load(self.context.i64_type(), len_gep, &format!("list_len_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                    let data_ptr_val = self.builder.build_load(
                        self.context.i8_type().ptr_type(inkwell::AddressSpace::default()),
                        data_gep, &format!("list_data_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                    let data_pv = match data_ptr_val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        _ => return Err(CompileError::TypeMismatch(
                            format!("list data ptr {}: expected pointer", i))),
                    };
                    let len_iv = match len_val {
                        BasicValueEnum::IntValue(iv) => iv,
                        _ => return Err(CompileError::TypeMismatch(
                            format!("list len {}: expected int", i))),
                    };
                    let elem_tag = elem_type_tag(&p.ty);
                    let elem_tag_iv = self.context.i64_type().const_int(elem_tag as u64, false);
                    let serialize_fn = if let Some(f) = self.module.get_function("mimi_json_serialize") {
                        f
                    } else {
                        let fn_ty = i8_ptr_ty.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                        ], false);
                        self.module.add_function("mimi_json_serialize", fn_ty, Some(inkwell::module::Linkage::External))
                    };
                    let json_val = self.builder.build_call(serialize_fn, &[
                        BasicMetadataValueEnum::PointerValue(data_pv),
                        BasicMetadataValueEnum::IntValue(len_iv),
                        BasicMetadataValueEnum::IntValue(elem_tag_iv),
                    ], &format!("json_ser_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("json serialize error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| CompileError::LlvmError("json serialize returned void".to_string()))?
                        .into_pointer_value();
                    json_strings.push(json_val);
                    wrapper_args.push(BasicMetadataValueEnum::PointerValue(json_val));
                }
                // F7: Tuple params — serialize heterogeneous tuple to JSON string
                crate::ast::Type::Tuple(elems) => {
                    let n = elems.len();
                    let i32_ty = self.context.i32_type();
                    let zero = i32_ty.const_int(0, false);
                    let i64_ty = self.context.i64_type();
                    // Store the tuple struct to an alloca for field extraction
                    let struct_val = param.into_struct_value();
                    let struct_ty = struct_val.get_type();
                    let alloca = self.builder.build_alloca(
                        BasicTypeEnum::StructType(struct_ty),
                        &format!("tuple_alloca_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                    self.builder.build_store(alloca, param)
                        .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                    // Allocate values and elem_types arrays
                    let arr_ty = i64_ty.array_type(n as u32);
                    let vals_alloca = self.builder.build_alloca(
                        BasicTypeEnum::ArrayType(arr_ty),
                        &format!("tuple_vals_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                    let tys_alloca = self.builder.build_alloca(
                        BasicTypeEnum::ArrayType(arr_ty),
                        &format!("tuple_tys_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                    for (ei, elem_ty) in elems.iter().enumerate() {
                        let idx = i32_ty.const_int(ei as u64, false);
                        // Extract element from struct
                        let elem_raw = self.builder.build_extract_value(struct_val, ei as u32,
                            &format!("tuple_elem_{}_{}", i, ei))
                            .map_err(|e| CompileError::LlvmError(format!("extract: {}", e)))?;
                        // Convert element to i64
                        let elem_i64 = match elem_raw {
                            BasicValueEnum::IntValue(iv) => iv,
                            BasicValueEnum::FloatValue(fv) => {
                                // Bitcast f64 bits to i64 via pointer; alloca is valid and freshly created.
                                let f_alloca = self.builder.build_alloca(
                                    BasicTypeEnum::FloatType(self.context.f64_type()),
                                    &format!("tf_{}_{}", i, ei))
                                    .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                                self.builder.build_store(f_alloca, fv)
                                    .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                                let cast = self.builder.build_pointer_cast(
                                    f_alloca,
                                    i64_ty.ptr_type(inkwell::AddressSpace::default()),
                                    &format!("f_cast_{}_{}", i, ei))
                                    .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?;
                                self.builder.build_load(i64_ty, cast,
                                    &format!("f_bits_{}_{}", i, ei))
                                    .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?
                                    .into_int_value()
                            },
                            BasicValueEnum::PointerValue(pv) => {
                                // Pointer to i64 (preserve address bits)
                                let i64_ptr_ty = i64_ty.ptr_type(inkwell::AddressSpace::default());
                                let cast = self.builder.build_pointer_cast(pv, i64_ptr_ty,
                                    &format!("p_cast_{}_{}", i, ei))
                                    .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?;
                                self.builder.build_load(i64_ty, cast,
                                    &format!("p_val_{}_{}", i, ei))
                                    .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?
                                    .into_int_value()
                            },
                            BasicValueEnum::StructValue(sv) => {
                                // Struct to i64 via alloca + pointer cast
                                let s_ty = sv.get_type();
                                let s_alloca = self.builder.build_alloca(
                                    BasicTypeEnum::StructType(s_ty),
                                    &format!("ts_{}_{}", i, ei))
                                    .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                                self.builder.build_store(s_alloca, sv)
                                    .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                                let cast = self.builder.build_pointer_cast(
                                    s_alloca,
                                    i64_ty.ptr_type(inkwell::AddressSpace::default()),
                                    &format!("s_cast_{}_{}", i, ei))
                                    .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?;
                                self.builder.build_load(i64_ty, cast,
                                    &format!("s_val_{}_{}", i, ei))
                                    .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?
                                    .into_int_value()
                            },
                            _ => {
                                let i64_ty = self.context.i64_type();
                                i64_ty.const_int(0, false)
                            }
                        };
                        // Store value to array via GEP
                        // SAFETY: vals_alloca is a valid alloca; indices are in-bounds constants.
                        let val_gep = unsafe { self.builder.build_gep(
                            i64_ty, vals_alloca, &[zero, idx],
                            &format!("tv_gep_{}_{}", i, ei))
                            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))? };
                        self.builder.build_store(val_gep, elem_i64)
                            .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                        // Store type tag
                        let tag_val = elem_type_tag(elem_ty);
                        let tag_i64 = i64_ty.const_int(tag_val as u64, false);
                        // SAFETY: tys_alloca is a valid alloca; indices are in-bounds constants.
                        let ty_gep = unsafe { self.builder.build_gep(
                            i64_ty, tys_alloca, &[zero, idx],
                            &format!("tt_gep_{}_{}", i, ei))
                            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))? };
                        self.builder.build_store(ty_gep, tag_i64)
                            .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                    }
                    let n_i64 = i64_ty.const_int(n as u64, false);
                    let serialize_fn = if let Some(f) = self.module.get_function("mimi_tuple_serialize") {
                        f
                    } else {
                        let i64_ptr_ty = i64_ty.ptr_type(inkwell::AddressSpace::default());
                        let fn_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default()).fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i64_ptr_ty),
                            BasicMetadataTypeEnum::IntType(i64_ty),
                            BasicMetadataTypeEnum::PointerType(i64_ptr_ty),
                        ], false);
                        self.module.add_function("mimi_tuple_serialize", fn_ty, Some(inkwell::module::Linkage::External))
                    };
                    let json_val = self.builder.build_call(serialize_fn, &[
                        BasicMetadataValueEnum::PointerValue(vals_alloca),
                        BasicMetadataValueEnum::IntValue(n_i64),
                        BasicMetadataValueEnum::PointerValue(tys_alloca),
                    ], &format!("tuple_ser_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("tuple serialize: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| CompileError::LlvmError("tuple serialize returned void".to_string()))?
                        .into_pointer_value();
                    json_strings.push(json_val);
                    wrapper_args.push(BasicMetadataValueEnum::PointerValue(json_val));
                }
                _ => {
                    wrapper_args.push(match param {
                        BasicValueEnum::IntValue(v) => BasicMetadataValueEnum::IntValue(v),
                        BasicValueEnum::FloatValue(v) => BasicMetadataValueEnum::FloatValue(v),
                        BasicValueEnum::PointerValue(v) => BasicMetadataValueEnum::PointerValue(v),
                        BasicValueEnum::StructValue(v) => BasicMetadataValueEnum::StructValue(v),
                        BasicValueEnum::ArrayValue(v) => BasicMetadataValueEnum::ArrayValue(v),
                        BasicValueEnum::VectorValue(v) => BasicMetadataValueEnum::VectorValue(v),
                        BasicValueEnum::ScalableVectorValue(_) => BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
                    });
                }
            }
        }

        let call = self.builder
            .build_call(extern_fn, &wrapper_args, "extern_call")
            .map_err(|e| CompileError::LlvmError(format!("failed to build extern wrapper call: {}", e)))?;

        // Free JSON serialization strings after C call
        for (j, json_pv) in json_strings.iter().enumerate() {
            if let Some(free_fn) = self.module.get_function("free") {
                self.builder.build_call(free_fn, &[
                    BasicMetadataValueEnum::PointerValue(*json_pv),
                ], &format!("free_json_{}", j))
                    .map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;
            }
        }

        // Phase 4: Release c_shared/c_borrow/c_borrow_mut params after C call
        // F2: Matches Phase 1 — retain/release pairs for all shared passport types.
        for (i, _) in &shared_params {
            if let Some(release_fn) = self.module.get_function("mimi_shared_release") {
                let orig_param = wrapper_fn.get_nth_param(*i as u32)
                    .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
                let param_i64 = match orig_param {
                    BasicValueEnum::IntValue(iv) => iv,
                    BasicValueEnum::PointerValue(pv) => {
                        self.builder.build_bit_cast(pv, i64_ty, &format!("ptr_to_i64_rel_{}", i))
                            .map_err(|e| format!("bitcast error: {}", e))?
                            .into_int_value()
                    }
                    _ => return Err(CompileError::TypeMismatch(format!("c_shared/c_borrow param {} must be pointer or int", i))),
                };
                self.builder.build_call(release_fn, &[
                    BasicMetadataValueEnum::IntValue(param_i64),
                ], &format!("release_{}", i))
                    .map_err(|e| CompileError::LlvmError(format!("release error: {}", e)))?;
            }
        }

        // Phase 5: Check ensures contract after C call
        if self.verify_contracts {
            if let Some(ens_expr) = &ef.ensures {
                let ret_val = call_try_basic_value(&call).ok_or_else(|| CompileError::LlvmError("extern wrapper call did not return a value".to_string()))?;
                let mut contract_vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
                for (i, p) in ef.params.iter().enumerate() {
                    let param = wrapper_fn.get_nth_param(i as u32)
                        .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
                    let llvm_ty = param.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, &p.name)
                        .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                    self.builder.build_store(alloca, param)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    contract_vars.insert(p.name.clone(), (alloca, llvm_ty));
                }
                let result_ty = ret_val.get_type();
                let result_alloca = self.builder.build_alloca(result_ty, "result")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(result_alloca, ret_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                contract_vars.insert("result".to_string(), (result_alloca, result_ty));
                self.compile_contract_assert(ens_expr, &contract_vars, &format!("ensures violation in extern '{}'", ef.name))?;
            }
        }

        // Phase 6: Return
        let is_tuple_return = ef.ret.as_ref().map_or(false, |ret_ty| {
            matches!(ret_ty, crate::ast::Type::Tuple(_))
        });
        let needs_json_deserialize = ef.ret.as_ref().map_or(false, |ret_ty| {
            matches!(ret_ty, crate::ast::Type::Name(n, _) if n == "List" || (self.record_type_names.contains(n.as_str()) && !self.repr_c_record_names.contains(n.as_str())))
        }) || is_tuple_return;
        if needs_json_deserialize {
            let ret = call_try_basic_value(&call)
                .ok_or_else(|| CompileError::LlvmError("extern call returned void".to_string()))?;
            let json_pv = match ret {
                BasicValueEnum::PointerValue(pv) => pv,
                _ => return Err(CompileError::LlvmError("Json return must be pointer".to_string())),
            };
            if is_tuple_return {
                // F7: Tuple return — deserialize JSON array back to LLVM struct
                let tuple_ty = ef.ret.as_ref().ok_or_else(|| CompileError::LlvmError("expected tuple return type for extern function".to_string()))?;
                let elems = match tuple_ty {
                    crate::ast::Type::Tuple(e) => e,
                    _ => return Err(CompileError::LlvmError("expected tuple type".to_string())),
                };
                let n_elems = elems.len();
                let i64_ty = self.context.i64_type();
                let i32_ty = self.context.i32_type();
                let zero = i32_ty.const_int(0, false);
                // Build elem_types array for deserialize
                let arr_ty = i64_ty.array_type(n_elems as u32);
                let tys_alloca = self.builder.build_alloca(
                    BasicTypeEnum::ArrayType(arr_ty),
                    "tuple_ret_tys")
                    .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                for (ei, elem_ty) in elems.iter().enumerate() {
                    let tag_val = elem_type_tag(elem_ty);
                    let tag_i64 = i64_ty.const_int(tag_val as u64, false);
                    let idx = i32_ty.const_int(ei as u64, false);
                    // SAFETY: GEP on struct pointer with correct field index 1 (type tag).
                    let ty_gep = unsafe { self.builder.build_gep(
                        i64_ty, tys_alloca, &[zero, idx],
                        &format!("tuple_ret_ty_gep_{}", ei))
                        .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))? };
                    self.builder.build_store(ty_gep, tag_i64)
                        .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                }
                // Allocate output values array
                let out_alloca = self.builder.build_alloca(
                    BasicTypeEnum::ArrayType(arr_ty),
                    "tuple_ret_vals")
                    .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                let n_i64 = i64_ty.const_int(n_elems as u64, false);
                let deser_fn = if let Some(f) = self.module.get_function("mimi_tuple_deserialize") {
                    f
                } else {
                    let i64_ptr_ty = i64_ty.ptr_type(inkwell::AddressSpace::default());
                    let i8_p_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                    let fn_ty = i64_ty.fn_type(&[
                        BasicMetadataTypeEnum::PointerType(i8_p_ty),
                        BasicMetadataTypeEnum::IntType(i64_ty),
                        BasicMetadataTypeEnum::PointerType(i64_ptr_ty),
                        BasicMetadataTypeEnum::PointerType(i64_ptr_ty),
                    ], false);
                    self.module.add_function("mimi_tuple_deserialize", fn_ty, Some(inkwell::module::Linkage::External))
                };
                let _parsed = self.builder.build_call(deser_fn, &[
                    BasicMetadataValueEnum::PointerValue(json_pv),
                    BasicMetadataValueEnum::IntValue(n_i64),
                    BasicMetadataValueEnum::PointerValue(tys_alloca),
                    BasicMetadataValueEnum::PointerValue(out_alloca),
                ], "tuple_deser")
                    .map_err(|e| CompileError::LlvmError(format!("tuple deserialize: {}", e)))?;
                if let Some(free_fn) = self.module.get_function("free") {
                    self.builder.build_call(free_fn, &[BasicMetadataValueEnum::PointerValue(json_pv)], "free_json_ret")
                        .map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;
                }
                // Build tuple struct from deserialized values
                let struct_ty = types::mimi_type_to_llvm(self.context, tuple_ty)
                    .ok_or_else(|| CompileError::LlvmError("unsupported tuple type".to_string()))?;
                let struct_alloca = self.builder.build_alloca(struct_ty, "tuple_ret")
                    .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                for (ei, elem_ty) in elems.iter().enumerate() {
                    let idx = i32_ty.const_int(ei as u64, false);
                    // SAFETY: out_alloca is a valid alloca; indices are in-bounds constants.
                    let val_gep = unsafe { self.builder.build_gep(
                        i64_ty, out_alloca, &[zero, idx],
                        &format!("tuple_ret_val_gep_{}", ei))
                        .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))? };
                    let raw_i64 = self.builder.build_load(i64_ty, val_gep,
                        &format!("tuple_ret_raw_{}", ei))
                        .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?
                        .into_int_value();
                    // Convert i64 back to the element type
                    let elem_llvm_ty = types::mimi_type_to_llvm(self.context, elem_ty)
                        .unwrap_or(BasicTypeEnum::IntType(i64_ty));
                    let field_val: BasicValueEnum = match elem_llvm_ty {
                        BasicTypeEnum::FloatType(ft) => {
                            // Convert i64 bits back to f64 via alloca+bitcast
                            let tmp_alloca = self.builder.build_alloca(
                                BasicTypeEnum::IntType(i64_ty),
                                &format!("f_tmp_{}", ei))
                                .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                            self.builder.build_store(tmp_alloca, raw_i64)
                                .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                            let cast = self.builder.build_pointer_cast(
                                tmp_alloca,
                                ft.ptr_type(inkwell::AddressSpace::default()),
                                &format!("f_cast_{}", ei))
                                .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?;
                            self.builder.build_load(BasicTypeEnum::FloatType(ft), cast,
                                &format!("f_val_{}", ei))
                                .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?
                        },
                        BasicTypeEnum::IntType(it) => {
                            BasicValueEnum::IntValue(
                                self.builder.build_int_cast(raw_i64, it,
                                    &format!("i_cast_{}", ei))
                                    .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?
                            )
                        },
                        BasicTypeEnum::PointerType(_pt) => {
                            let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                            let int_to_ptr = self.builder.build_int_to_ptr(raw_i64, i8_ptr_ty,
                                &format!("ptr_cast_{}", ei))
                                .map_err(|e| CompileError::LlvmError(format!("int to ptr: {}", e)))?;
                            BasicValueEnum::PointerValue(int_to_ptr)
                        },
                        _ => BasicValueEnum::IntValue(raw_i64),
                    };
                    let field_gep = self.builder.build_struct_gep(
                        struct_ty.into_struct_type(), struct_alloca, ei as u32,
                        &format!("tuple_ret_field_{}", ei))
                        .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                    self.builder.build_store(field_gep, field_val)
                        .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                }
                let ret_val = self.builder.build_load(struct_ty, struct_alloca, "tuple_ret_val")
                    .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?;
                self.builder.build_return(Some(&ret_val))
                    .map_err(|e| CompileError::LlvmError(format!("return: {}", e)))?;
            } else {
                // Original List/Record JSON deserialization
                let out_len_alloca = self.builder.build_alloca(self.context.i64_type(), "out_len")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let ret_tag = ef.ret.as_ref().map(|ret_ty| elem_type_tag(ret_ty)).unwrap_or(0);
                let ret_tag_iv = self.context.i64_type().const_int(ret_tag as u64, false);
                let deserialize_fn = if let Some(f) = self.module.get_function("mimi_json_deserialize") {
                    f
                } else {
                    let fn_ty = i8_ptr_ty.fn_type(&[
                        BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                        BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                        BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    ], false);
                    self.module.add_function("mimi_json_deserialize", fn_ty, Some(inkwell::module::Linkage::External))
                };
                let data_ptr_val = self.builder.build_call(deserialize_fn, &[
                    BasicMetadataValueEnum::PointerValue(json_pv),
                    BasicMetadataValueEnum::PointerValue(out_len_alloca),
                    BasicMetadataValueEnum::IntValue(ret_tag_iv),
                ], "json_deser")
                    .map_err(|e| CompileError::LlvmError(format!("json deserialize error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("json deserialize returned void".to_string()))?
                    .into_pointer_value();
                let len_val = self.builder.build_load(self.context.i64_type(), out_len_alloca, "list_len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                if let Some(free_fn) = self.module.get_function("free") {
                    self.builder.build_call(free_fn, &[BasicMetadataValueEnum::PointerValue(json_pv)], "free_json_ret")
                        .map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;
                }
                let list_alloca = self.builder.build_alloca(list_struct_ty, "list_ret")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_alloca, 0, "list_len_gep")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, len_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_alloca, 1, "list_data_gep")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(data_gep, data_ptr_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let ret_val = self.builder.build_load(list_struct_ty, list_alloca, "list_ret_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                self.builder.build_return(Some(&ret_val))
                    .map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
            }
        } else if ef.ret.as_ref().map_or(false, |t| matches!(t, crate::ast::Type::Name(n, _) if n == "string")) {
            // BUG 1 fix: convert char* (i8*) from C to {i8*, i64} Mimi string struct
            let ret = call_try_basic_value(&call)
                .ok_or_else(|| CompileError::LlvmError("extern call returned void".to_string()))?;
            let raw_ptr = match ret {
                BasicValueEnum::PointerValue(pv) => pv,
                _ => return Err(CompileError::LlvmError("string return must be pointer".to_string())),
            };
            let strlen_fn = self.module.get_function("strlen")
                .ok_or_else(|| CompileError::LlvmError("strlen not declared".to_string()))?;
            let len = self.builder.build_call(strlen_fn, &[BasicMetadataValueEnum::PointerValue(raw_ptr)], "strlen")
                .map_err(|e| CompileError::LlvmError(format!("strlen: {}", e)))?
                .try_as_basic_value_opt()
                .ok_or(CompileError::LlvmError("strlen returned void".to_string()))?
                .into_int_value();
            // Build {i8*, i64} string struct
            let struct_ty = wrapper_ret_ty;
            let alloca = self.builder.build_alloca(struct_ty, "string_ret")
                .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
            let ptr_gep = self.builder.build_struct_gep(struct_ty, alloca, 0, "str_ptr_gep")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            self.builder.build_store(ptr_gep, raw_ptr)
                .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
            let len_gep = self.builder.build_struct_gep(struct_ty, alloca, 1, "str_len_gep")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            self.builder.build_store(len_gep, len)
                .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
            let ret_val = self.builder.build_load(struct_ty, alloca, "string_ret_val")
                .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?;
            self.builder.build_return(Some(&ret_val))
                .map_err(|e| CompileError::LlvmError(format!("return: {}", e)))?;
        } else if wrapper_fn_type.get_return_type().is_some() {
            let ret = call_try_basic_value(&call).ok_or_else(|| CompileError::LlvmError("extern wrapper call did not return a value".to_string()))?;
            self.builder.build_return(Some(&ret))
                .map_err(|e| CompileError::LlvmError(format!("failed to build extern wrapper return: {}", e)))?;
        } else {
            self.builder.build_return(None)
                .map_err(|e| CompileError::LlvmError(format!("failed to build extern wrapper return: {}", e)))?;
        }

        if let Some(block) = previous_block {
            self.builder.position_at_end(block);
        }
        Ok(())
    }

}
