#![allow(dead_code, deprecated)]

pub mod types;
pub mod builtins;

use crate::ast::*;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::OptimizationLevel;
use std::collections::HashMap;
use std::path::Path;

pub struct CodeGenerator<'ctx> {
    pub context: &'ctx Context,
    pub module: Module<'ctx>,
    pub builder: Builder<'ctx>,
    loop_break: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    loop_continue: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    type_defs: HashMap<String, crate::ast::TypeDef>,
    type_llvm: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Track linear capabilities in scope: name -> (pointer, consumed)
    cap_vars: Vec<HashMap<String, (inkwell::values::PointerValue<'ctx>, bool)>>,
    /// Known cap type names (from cap definitions)
    cap_type_names: std::collections::HashSet<String>,
    /// Generic type substitution map for current monomorphization
    type_map: HashMap<String, crate::ast::Type>,
    /// Store function definitions for monomorphization lookup
    func_defs: HashMap<String, FuncDef>,
    /// Track variable name -> Mimi type name for field access resolution
    var_type_names: HashMap<String, String>,
    /// Counter for generating unique spawn wrapper function names
    spawn_counter: u64,
    /// Strict mode: skip non-locked ($/$$) fragments during compilation
    pub strict: bool,
    /// no_std mode: compile without libc (freestanding target)
    pub no_std: bool,
    /// Verify contracts: compile requires/ensures as runtime asserts
    pub verify_contracts: bool,
    /// True when inside a parasteps block (enables thread-ID tracking for final join)
    in_parasteps: bool,
    /// Thread IDs created during parasteps that need joining at block end
    parasteps_thread_ids: Vec<inkwell::values::IntValue<'ctx>>,
    /// Compensation blocks registered via `on failure` (LIFO stack of scopes)
    compensation_blocks: Vec<Vec<Stmt>>,
    /// Stack of scope start indices into compensation_blocks
    comp_scope_stack: Vec<usize>,
    /// Trait definitions: trait_name -> TraitDef
    trait_defs: HashMap<String, crate::ast::TraitDef>,
    /// Trait implementations: type_name -> trait_name -> Vec<FuncDef> methods
    type_impls: HashMap<String, HashMap<String, Vec<FuncDef>>>,
    /// Vtable global variables per (type, trait): key = "{type}__{trait}"
    vtable_globals: HashMap<String, inkwell::values::GlobalValue<'ctx>>,
    /// Vtable struct types per trait: key = trait_name
    vtable_types: HashMap<String, inkwell::types::StructType<'ctx>>,
}

type VarEntry<'ctx> = (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>);

impl<'ctx> CodeGenerator<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();
        builtins::register_runtime(&module, context);
        Self { context, module, builder, loop_break: None, loop_continue: None, type_defs: HashMap::new(), type_llvm: HashMap::new(), cap_vars: vec![HashMap::new()], cap_type_names: std::collections::HashSet::new(), type_map: HashMap::new(), func_defs: HashMap::new(), var_type_names: HashMap::new(), spawn_counter: 0, strict: false, no_std: false, verify_contracts: false, compensation_blocks: Vec::new(), comp_scope_stack: Vec::new(), in_parasteps: false, parasteps_thread_ids: Vec::new(), trait_defs: HashMap::new(), type_impls: HashMap::new(), vtable_globals: HashMap::new(), vtable_types: HashMap::new() }
    }

    /// Get the current LLVM function, or None if no insert block.
    fn current_function(&self) -> Option<inkwell::values::FunctionValue<'ctx>> {
        self.builder.get_insert_block()?.get_parent()
    }

    /// Check if the current insert block has a terminator.
    fn block_has_terminator(&self) -> bool {
        self.builder.get_insert_block().and_then(|b| b.get_terminator()).is_some()
    }

    /// Extract a basic value from a call result, or return an error.
    fn expect_basic_value(&self, call: &inkwell::values::CallSiteValue<'ctx>, name: &str) -> Result<BasicValueEnum<'ctx>, String> {
        call.try_as_basic_value().left().ok_or_else(|| format!("codegen: expected basic value from {}", name))
    }

    /// Create a structured codegen error with an error code.
    fn codegen_err(&self, code: &str, msg: String) -> String {
        format!("[{}] {}", code, msg)
    }

    /// Shorthand: return Err with codegen error code E07xx.
    fn cg_err<T>(&self, code: &str, msg: impl Into<String>) -> Result<T, String> {
        Err(self.codegen_err(code, msg.into()))
    }

    /// Enter parallel parasteps mode: track thread IDs for joining at block end
    fn enter_parasteps(&mut self) {
        self.in_parasteps = true;
        self.parasteps_thread_ids.clear();
    }

    /// Leave parallel parasteps mode: join all spawned threads
    fn leave_parasteps(&mut self) -> Result<(), String> {
        if !self.in_parasteps {
            return Ok(());
        }
        if self.parasteps_thread_ids.is_empty() {
            // Pool-based parasteps: wait for all pool tasks to complete
            let join_all_fn = self.module.get_function("mimi_pool_join_all")
                .ok_or("mimi_pool_join_all not declared")?;
            self.builder.build_call(join_all_fn, &[], "pool_join_all")
                .map_err(|e| format!("pool_join_all error: {}", e))?;
        } else {
            // Legacy pthread-based parasteps: join individual threads
            let i8_type = self.context.i8_type();
            let i8_ptr = i8_type.ptr_type(inkwell::AddressSpace::default());
            let join_fn = self.module.get_function("pthread_join")
                .ok_or("pthread_join not declared")?;
            for &thread_id in &self.parasteps_thread_ids {
                self.builder.build_call(join_fn, &[
                    BasicMetadataValueEnum::IntValue(thread_id),
                    BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
                ], "parasteps_join")
                    .map_err(|e| format!("parasteps join error: {}", e))?;
            }
        }
        self.parasteps_thread_ids.clear();
        self.in_parasteps = false;
        Ok(())
    }

    /// Push a new compensation scope
    fn push_comp_scope(&mut self) {
        self.comp_scope_stack.push(self.compensation_blocks.len());
    }

    /// Pop the current compensation scope (discard blocks registered in it — normal exit)
    fn pop_comp_scope(&mut self) {
        if let Some(start) = self.comp_scope_stack.pop() {
            self.compensation_blocks.truncate(start);
        }
    }

    /// Register a compensation block for LIFO execution on error exit
    fn register_comp(&mut self, stmts: &Block) {
        self.compensation_blocks.push(stmts.clone());
    }

    /// Compile all registered compensation blocks in LIFO order
    fn compile_compensations(
        &mut self,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), String> {
        let blocks: Vec<Block> = self.compensation_blocks.iter().rev().cloned().collect();
        for stmts in &blocks {
            self.compile_block(stmts, vars)?;
        }
        Ok(())
    }

    /// Compile a contract condition as a runtime assert (for --verify-contracts)
    fn compile_contract_assert(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
        msg: &str,
    ) -> Result<(), String> {
        let cond_val = self.compile_expr(expr, vars)?;
        let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
            iv
        } else {
            return Err(format!("contract condition must be boolean, got {:?}", cond_val.get_type()));
        };

        let function = self.current_function().unwrap();
        let pass_bb = self.context.append_basic_block(function, "contract_pass");
        let fail_bb = self.context.append_basic_block(function, "contract_fail");

        self.builder.build_conditional_branch(cond_bool, pass_bb, fail_bb)
            .map_err(|e| format!("branch error: {}", e))?;

        // Fail block: call abort/panic
        self.builder.position_at_end(fail_bb);
        let msg_ptr = self.builder.build_global_string_ptr(msg, "contract_msg")
            .map_err(|e| format!("string error: {}", e))?;
        let abort_fn = self.module.get_function("mimi_runtime_abort")
            .or_else(|| {
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let ty = self.context.void_type().fn_type(&[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ], false);
                Some(self.module.add_function("mimi_runtime_abort", ty, Some(inkwell::module::Linkage::External)))
            }).unwrap();
        self.builder.build_call(abort_fn, &[
            BasicMetadataValueEnum::PointerValue(msg_ptr.as_pointer_value()),
        ], "abort_call")
            .map_err(|e| format!("abort call error: {}", e))?;
        self.builder.build_unconditional_branch(pass_bb)
            .map_err(|e| format!("branch error: {}", e))?;

        // Continue at pass block
        self.builder.position_at_end(pass_bb);
        Ok(())
    }

    /// Push a new capability scope
    fn push_cap_scope(&mut self) {
        self.cap_vars.push(HashMap::new());
    }

    /// Pop the current capability scope
    fn pop_cap_scope(&mut self) {
        self.cap_vars.pop();
    }

    /// Register a capability variable in the current scope
    fn register_cap(&mut self, name: &str, ptr: inkwell::values::PointerValue<'ctx>) {
        if let Some(scope) = self.cap_vars.last_mut() {
            scope.insert(name.to_string(), (ptr, false));
        }
    }

    /// Mark a capability as consumed
    fn consume_cap(&mut self, name: &str) -> Result<(), String> {
        for scope in self.cap_vars.iter_mut().rev() {
            if let Some((_, consumed)) = scope.get_mut(name) {
                if *consumed {
                    return Err(format!("[E0718] capability '{}' has already been consumed", name));
                }
                *consumed = true;
                return Ok(());
            }
        }
        Ok(()) // Not a capability variable
    }

    /// Check if a variable is a consumed capability
    fn is_cap_consumed(&self, name: &str) -> bool {
        for scope in self.cap_vars.iter().rev() {
            if let Some((_, consumed)) = scope.get(name) {
                return *consumed;
            }
        }
        false
    }

    /// Check if a variable is a capability variable
    fn is_cap_var(&self, name: &str) -> bool {
        for scope in self.cap_vars.iter().rev() {
            if scope.contains_key(name) {
                return true;
            }
        }
        false
    }

    /// Check for unconsumed capabilities at scope exit
    fn check_unconsumed_caps(&self) -> Result<(), String> {
        if let Some(scope) = self.cap_vars.last() {
            for (name, (_, consumed)) in scope {
                if !consumed {
                    return Err(format!(
                        "linear capability '{}' must be consumed (via drop) before end of scope",
                        name
                    ));
                }
            }
        }
        Ok(())
    }

    /// Mangle a generic function name with concrete type arguments
    /// e.g., "identity" with type_map {T: i64} -> "identity__i64"
    fn mangle_name(base: &str, type_map: &HashMap<String, crate::ast::Type>) -> String {
        if type_map.is_empty() {
            return base.to_string();
        }
        let mut parts: Vec<String> = type_map.iter()
            .map(|(k, v)| format!("{}_{}", k, crate::core::fmt_type(v)))
            .collect();
        parts.sort();
        format!("{}__{}", base, parts.join("__"))
    }

    /// Resolve a type through the current type_map (substitute generic params)
    fn resolve_type(&self, ty: &crate::ast::Type) -> crate::ast::Type {
        if self.type_map.is_empty() {
            return ty.clone();
        }
        let generics: Vec<crate::ast::GenericParam> = self.type_map.keys()
            .map(|k| crate::ast::GenericParam { name: k.clone(), bounds: vec![] })
            .collect();
        crate::core::subst_type_params(ty, &generics, &self.type_map)
    }

    /// Resolve a type to its LLVM representation, applying generic substitution
    fn resolve_type_llvm(&self, ty: &crate::ast::Type) -> Option<BasicTypeEnum<'ctx>> {
        let resolved = self.resolve_type(ty);
        types::mimi_type_to_llvm(self.context, &resolved)
    }

    /// Check if an item is committed ($/$$) in strict mode.
    /// In loose mode (default), all items pass.
    /// In strict mode, only items with Locked/StrongLocked commitment compile.
    fn is_committed(&self, c: &Commitment) -> bool {
        if !self.strict { return true; }
        c.is_locked()
    }

    /// Get the commitment of a top-level item for strict-mode filtering.
    fn item_commitment(item: &Item) -> Commitment {
        match item {
            Item::Func(f) => f.commitment,
            Item::Type(t) => t.commitment,
            Item::Actor(a) => a.commitment,
            Item::Module(m) => m.commitment,
            _ => Commitment::None,
        }
    }

    pub fn compile_file(&mut self, file: &File) -> Result<(), String> {
        // First pass: collect type definitions, function definitions, and cap definitions
        for item in &file.items {
            match item {
                Item::Type(t) => {
                    self.register_type_def(t)?;
                }
                Item::Actor(actor) => {
                    self.register_actor_def(actor)?;
                }
                Item::Func(f) if !f.is_comptime => {
                    self.func_defs.insert(f.name.clone(), f.clone());
                }
                Item::Cap(cap) => {
                    self.cap_type_names.insert(cap.name.clone());
                }
                Item::Trait(t) => {
                    self.trait_defs.insert(t.name.clone(), t.clone());
                }
                Item::Impl(imp) => {
                    self.type_impls
                        .entry(imp.type_name.clone())
                        .or_default()
                        .insert(imp.trait_name.clone(), imp.methods.clone());
                }
                Item::Module(m) => {
                    for inner in &m.items {
                        match inner {
                            Item::Type(t) => {
                                self.register_type_def(t)?;
                            }
                            Item::Actor(actor) => {
                                self.register_actor_def(actor)?;
                            }
                            Item::Func(f) if !f.is_comptime => {
                                self.func_defs.insert(f.name.clone(), f.clone());
                            }
                            Item::Cap(cap) => {
                                self.cap_type_names.insert(cap.name.clone());
                            }
                            Item::Trait(t) => {
                                self.trait_defs.insert(t.name.clone(), t.clone());
                            }
                            Item::Impl(imp) => {
                                self.type_impls
                                    .entry(imp.type_name.clone())
                                    .or_default()
                                    .insert(imp.trait_name.clone(), imp.methods.clone());
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        // Second pass: register extern functions and compile user functions
        for item in &file.items {
            match item {
                Item::ExternBlock(block) => {
                    self.register_extern_block(block)?;
                }
                Item::Func(f) if !f.is_comptime && self.is_committed(&f.commitment) => {
                    self.compile_func(f)?;
                }
                Item::Actor(actor) if self.is_committed(&actor.commitment) => {
                    self.compile_actor(actor)?;
                }
                Item::Module(m) => {
                    for inner in &m.items {
                        match inner {
                            Item::ExternBlock(block) => {
                                self.register_extern_block(block)?;
                            }
                            Item::Func(f) if !f.is_comptime && self.is_committed(&f.commitment) => {
                                self.compile_func(f)?;
                            }
                            Item::Actor(actor) if self.is_committed(&actor.commitment) => {
                                self.compile_actor(actor)?;
                            }
                            Item::Type(t) if self.is_committed(&t.commitment) => {
                                self.register_type_def(t)?;
                            }
                            _ => {}
                        }
                    }
                }
                Item::Type(t) if self.is_committed(&t.commitment) => {
                    self.register_type_def(t)?;
                }
                _ => {}
            }
        }
        // Second pass: compile impl methods for committed trait implementations
        self.compile_impl_methods()?;
        self.compile_vtables()?;
        Ok(())
    }

    /// Compile all trait impl methods as standalone functions with mangled names
    fn compile_impl_methods(&mut self) -> Result<(), String> {
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
    fn compile_vtables(&mut self) -> Result<(), String> {
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
                            ).map_err(|e| format!("bitcast error: {}", e))?;
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

    fn register_extern_block(&mut self, block: &crate::ast::ExternBlock) -> Result<(), String> {
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
                        .ok_or(format!("missing param {}", i))?;
                    if let Some(retain_fn) = self.module.get_function("mimi_shared_retain") {
                        // c_shared is compiled as i8*, need to bitcast to i64 for the runtime call
                        let param_i64 = match param {
                            BasicValueEnum::IntValue(iv) => iv,
                            BasicValueEnum::PointerValue(pv) => {
                                self.builder.build_bit_cast(pv, i64_ty, &format!("ptr_to_i64_{}", i))
                                    .map_err(|e| format!("bitcast error: {}", e))?
                                    .into_int_value()
                            }
                            _ => return Err(format!("[E0712] c_shared param {} must be pointer or int", i)),
                        };
                        self.builder.build_call(retain_fn, &[
                            BasicMetadataValueEnum::IntValue(param_i64),
                        ], &format!("retain_{}", i))
                            .map_err(|e| format!("retain error: {}", e))?;
                    }
                    shared_params.push((i, param));
                }
            }

            // Phase 2: Check cap params
            for (i, p) in ef.params.iter().enumerate() {
                if let crate::ast::Type::Cap(cap_name) = &p.ty {
                    let param = wrapper_fn.get_nth_param(i as u32)
                        .ok_or(format!("missing param {}", i))?;
                    if let Some(check_fn) = self.module.get_function("mimi_cap_check") {
                        let cap_name_global = self.builder.build_global_string_ptr(
                            &format!("{}\0", cap_name), &format!("cap_name_{}", i))
                            .map_err(|e| format!("string global error: {}", e))?;
                        let cap_name_ptr = cap_name_global.as_pointer_value();
                        let check_result = self.builder.build_call(check_fn, &[
                            BasicMetadataValueEnum::IntValue(param.into_int_value()),
                            BasicMetadataValueEnum::PointerValue(cap_name_ptr),
                        ], &format!("cap_check_{}", i))
                            .map_err(|e| format!("cap_check error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("cap_check returned void")?
                            .into_int_value();
                        // If cap_check returns false (0), abort
                        let is_valid = self.builder.build_int_compare(
                            inkwell::IntPredicate::NE, check_result,
                            self.context.bool_type().const_int(0, false),
                            "cap_valid")
                            .map_err(|e| format!("compare error: {}", e))?;
                        let function = self.current_function().unwrap();
                        let ok_bb = self.context.append_basic_block(function, &format!("cap_ok_{}", i));
                        let fail_bb = self.context.append_basic_block(function, &format!("cap_fail_{}", i));
                        self.builder.build_conditional_branch(is_valid, ok_bb, fail_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(fail_bb);
                        if let Some(exit_fn) = self.module.get_function("exit") {
                            self.builder.build_call(exit_fn, &[
                                BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                            ], "cap_fail_exit")
                                .map_err(|e| format!("exit error: {}", e))?;
                        }
                        self.builder.build_unconditional_branch(ok_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
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
                .map_err(|e| format!("failed to build extern wrapper call: {}", e))?;

            // Phase 4: Release c_shared params after C call
            for (i, _param) in &shared_params {
                if let Some(release_fn) = self.module.get_function("mimi_shared_release") {
                    let orig_param = wrapper_fn.get_nth_param(*i as u32)
                        .ok_or(format!("missing param {}", i))?;
                    let param_i64 = match orig_param {
                        BasicValueEnum::IntValue(iv) => iv,
                        BasicValueEnum::PointerValue(pv) => {
                            self.builder.build_bit_cast(pv, i64_ty, &format!("ptr_to_i64_rel_{}", i))
                                .map_err(|e| format!("bitcast error: {}", e))?
                                .into_int_value()
                        }
                        _ => return Err(format!("[E0712] c_shared param {} must be pointer or int", i)),
                    };
                    self.builder.build_call(release_fn, &[
                        BasicMetadataValueEnum::IntValue(param_i64),
                    ], &format!("release_{}", i))
                        .map_err(|e| format!("release error: {}", e))?;
                }
            }

            // Phase 5: Return
            if fn_type.get_return_type().is_some() {
                let ret = call.try_as_basic_value().left().ok_or_else(|| {
                    "extern wrapper call did not return a value".to_string()
                })?;
                self.builder.build_return(Some(&ret))
                    .map_err(|e| format!("failed to build extern wrapper return: {}", e))?;
            } else {
                self.builder.build_return(None)
                    .map_err(|e| format!("failed to build extern wrapper return: {}", e))?;
            }

            if let Some(block) = previous_block {
                self.builder.position_at_end(block);
            }
        }
        Ok(())
    }

    fn register_type_def(&mut self, t: &crate::ast::TypeDef) -> Result<(), String> {
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

    fn register_actor_def(&mut self, actor: &crate::ast::ActorDef) -> Result<(), String> {
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

    fn compile_actor(&mut self, actor: &crate::ast::ActorDef) -> Result<(), String> {
        // Generate constructor function: ActorName(field1, field2, ...) -> Actor
        let mut param_types = Vec::new();
        for f in &actor.fields {
            let ty = types::mimi_type_to_llvm(self.context, &f.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            param_types.push(ty);
        }
        
        let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();
        
        // Return type is a pointer to the actor struct
        let actor_ty = *self.type_llvm.get(&actor.name)
            .ok_or_else(|| format!("actor type '{}' not found", actor.name))?;
        
        let fn_type = match actor_ty {
            BasicTypeEnum::StructType(sty) => sty.fn_type(&metadata_params, false),
            _ => return Err(format!("[E0703] actor '{}' type is not a struct", actor.name)),
        };
        
        let constructor_name = format!("{}_new", actor.name);
        let function = self.module.add_function(&constructor_name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        
        // Allocate actor struct
        let alloca = match actor_ty {
            BasicTypeEnum::StructType(sty) => self.builder.build_alloca(sty, &actor.name)
                .map_err(|e| format!("alloca error: {}", e))?,
            _ => return Err("actor type error".into()),
        };
        
        // Store field values
        for (i, param) in function.get_params().iter().enumerate() {
            if let Some(BasicTypeEnum::StructType(sty)) = self.type_llvm.get(&actor.name) {
                let gep = self.builder.build_struct_gep(*sty, alloca, i as u32, &actor.fields[i].name)
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(gep, *param)
                    .map_err(|e| format!("store error: {}", e))?;
            }
        }
        
        // Return the actor struct
        let ret_val = self.builder.build_load(actor_ty, alloca, &actor.name)
            .map_err(|e| format!("load error: {}", e))?;
        self.builder.build_return(Some(&ret_val))
            .map_err(|e| format!("return error: {}", e))?;
        
        // Compile all actor methods
        for method in &actor.methods {
            self.compile_actor_method(actor, method)?;
        }
        
        Ok(())
    }
    
    fn compile_actor_method(&mut self, actor: &crate::ast::ActorDef, method: &FuncDef) -> Result<(), String> {
        let actor_ty = *self.type_llvm.get(&actor.name)
            .ok_or_else(|| format!("actor type '{}' not found", actor.name))?;
        
        // Method name: ActorName__methodName
        let mangled = format!("{}__{}__method", actor.name, method.name);
        
        // Build function type: self (ptr to actor struct) + params -> ret
        let actor_ptr_ty = match actor_ty {
            BasicTypeEnum::StructType(sty) => BasicTypeEnum::PointerType(sty.ptr_type(inkwell::AddressSpace::default())),
            _ => return Err(format!("[E0703] actor '{}' type is not a struct", actor.name)),
        };
        
        let mut param_metadata = vec![types::basic_to_metadata(self.context, actor_ptr_ty)];
        let mut param_llvm = vec![actor_ptr_ty];
        for p in &method.params {
            let ty = types::mimi_type_to_llvm(self.context, &p.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            param_llvm.push(ty);
            param_metadata.push(types::basic_to_metadata(self.context, ty));
        }
        
        let ret_llvm = match &method.ret {
            Some(ty) => types::mimi_type_to_llvm(self.context, ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };
        
        let fn_type = match ret_llvm {
            BasicTypeEnum::IntType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&param_metadata, false),
            _ => self.context.i64_type().fn_type(&param_metadata, false),
        };
        
        let function = self.module.add_function(&mangled, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        
        self.push_cap_scope();
        self.push_comp_scope();
        
        let mut vars: HashMap<String, VarEntry> = HashMap::new();
        
        // Bind self: allocate space for actor struct and store pointer
        let self_alloca = self.builder.build_alloca(actor_ptr_ty, "self")
            .map_err(|e| format!("alloca error: {}", e))?;
        self.builder.build_store(self_alloca, function.get_nth_param(0).unwrap())
            .map_err(|e| format!("store error: {}", e))?;
        vars.insert("self".to_string(), (self_alloca, actor_ptr_ty));
        
        // Bind method params
        let param_offset = 1; // param 0 is self
        for (i, param) in method.params.iter().enumerate() {
            let ty = types::mimi_type_to_llvm(self.context, &param.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            let alloca = self.builder.build_alloca(ty, &param.name)
                .map_err(|e| format!("alloca error: {}", e))?;
            self.builder.build_store(alloca, function.get_nth_param((i + param_offset) as u32).unwrap())
                .map_err(|e| format!("store error: {}", e))?;
            vars.insert(param.name.clone(), (alloca, ty));
        }
        
        let mut last_val: BasicValueEnum = self.context.i64_type().const_int(0, false).into();
        for stmt in &method.body {
            // Run compensations before exit()
            if let Stmt::Expr(Expr::Call(callee, _)) = stmt {
                if let Expr::Ident(name) = &**callee {
                    if name == "exit" {
                        self.compile_compensations(&mut vars)?;
                    }
                }
            }
            match stmt {
                Stmt::Expr(expr) => {
                    last_val = self.compile_expr(expr, &vars)?;
                }
                Stmt::Return(Some(expr)) => {
                    self.pop_comp_scope();
                    let val = self.compile_expr(expr, &vars)?;
                    self.builder.build_return(Some(&val)).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    self.pop_comp_scope();
                    self.builder.build_return(None).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), .. } => {
                    let val = self.compile_expr(init, &vars)?;
                    let name = match pat {
                        Pattern::Variable(n) => n.clone(),
                        _ => continue,
                    };
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, &name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("store error: {}", e))?;
                    // Track type name from record expressions
                    if let Expr::Record { ty: Some(tn), .. } = init {
                        self.var_type_names.insert(name.clone(), tn.clone());
                    }
                    vars.insert(name, (alloca, llvm_ty));
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, &vars)?;
                    if let Some(&(alloca, _)) = vars.get(name) {
                        self.builder.build_store(alloca, val)
                            .map_err(|e| format!("store error: {}", e))?;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("[E0712] if condition must be boolean".into());
                    };
                    let function = self.current_function().unwrap();
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");
                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, &mut vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, &mut vars)?;
                    }
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::For { var, iterable, body } => {
                    let function = self.current_function().unwrap();
                    if let Expr::Binary(BinOp::Range, start_expr, end_expr) = iterable {
                        let start_val = self.compile_expr(start_expr, &vars)?;
                        let end_val = self.compile_expr(end_expr, &vars)?;
                        let start_iv = if let BasicValueEnum::IntValue(iv) = start_val { iv } else { return Err("[E0712] range start must be i64".into()); };
                        let end_iv = if let BasicValueEnum::IntValue(iv) = end_val { iv } else { return Err("[E0712] range end must be i64".into()); };
                        let idx_alloca = self.builder.build_alloca(self.context.i64_type(), "idx")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(idx_alloca, start_iv)
                            .map_err(|e| format!("store error: {}", e))?;
                        let loop_bb = self.context.append_basic_block(function, "forloop");
                        let body_bb = self.context.append_basic_block(function, "forbody");
                        let merge_bb = self.context.append_basic_block(function, "forcont");
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(loop_bb);
                        let idx_val = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), idx_alloca, "idx")
                            .map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] idx must be i64".into()); };
                        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx_iv, end_iv, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        self.builder.build_conditional_branch(cmp, body_bb, merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(body_bb);
                        let elem_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(self.context.i64_type()), var)
                            .map_err(|e| format!("alloca error: {}", e))?;
                        let old_break = self.loop_break.replace(merge_bb);
                        let old_continue = self.loop_continue.replace(loop_bb);
                        self.builder.build_store(elem_alloca, idx_val)
                            .map_err(|e| format!("store error: {}", e))?;
                        vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                        self.compile_block(body, &mut vars)?;
                        vars.remove(var);
                        self.loop_break = old_break;
                        self.loop_continue = old_continue;
                        let idx_val = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), idx_alloca, "idx")
                            .map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] idx must be i64".into()); };
                        let one = self.context.i64_type().const_int(1, false);
                        let next_idx = self.builder.build_int_add(idx_iv, one, "next_idx")
                            .map_err(|e| format!("add error: {}", e))?;
                        self.builder.build_store(idx_alloca, next_idx)
                            .map_err(|e| format!("store error: {}", e))?;
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(merge_bb);
                    } else {
                        return Err("for loop requires range in codegen".into());
                    }
                }
                Stmt::While { cond, body } => {
                    let function = self.current_function().unwrap();
                    let loop_bb = self.context.append_basic_block(function, "loop");
                    let body_bb = self.context.append_basic_block(function, "loopbody");
                    let merge_bb = self.context.append_basic_block(function, "loopcont");
                    self.builder.build_unconditional_branch(loop_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    self.builder.position_at_end(loop_bb);
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val { iv } else { return Err("[E0712] while condition must be boolean".into()); };
                    self.builder.build_conditional_branch(cond_bool, body_bb, merge_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    self.builder.position_at_end(body_bb);
                    let old_break = self.loop_break.replace(merge_bb);
                    let old_continue = self.loop_continue.replace(loop_bb);
                    self.compile_block(body, &mut vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.loop_break = old_break;
                    self.loop_continue = old_continue;
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::MmsBlock { .. } => {}
                Stmt::Parasteps(block) => {
                    // Parasteps: execute spawn statements in parallel, join at block end
                    self.enter_parasteps();
                    self.compile_block(block, &mut vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    self.compile_expr(expr, &vars)?;
                }
                Stmt::OnFailure(block) => {
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    let function = self.current_function().ok_or("arena outside function")?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| format!("branch to arena: {}", e))?;
                    }
                    self.builder.position_at_end(arena_body_bb);
                    let saved = self.build_stacksave()?;
                    let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
                    self.compile_block(block, &mut vars)?;
                    for k in vars.keys().cloned().collect::<Vec<_>>() {
                        if !vars_before.contains(&k) {
                            vars.remove(&k);
                        }
                    }
                    self.build_stackrestore(saved)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_cont_bb)
                            .map_err(|e| format!("branch after arena: {}", e))?;
                    }
                    self.builder.position_at_end(arena_cont_bb);
                }
                Stmt::Alloc { kind: AllocKind::Arena, body } => {
                    let function = self.current_function().ok_or("arena outside function")?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| format!("branch to alloc(Arena): {}", e))?;
                    }
                    self.builder.position_at_end(arena_body_bb);
                    let saved = self.build_stacksave()?;
                    let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
                    self.compile_block(body, &mut vars)?;
                    for k in vars.keys().cloned().collect::<Vec<_>>() {
                        if !vars_before.contains(&k) {
                            vars.remove(&k);
                        }
                    }
                    self.build_stackrestore(saved)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_cont_bb)
                            .map_err(|e| format!("branch after alloc(Arena): {}", e))?;
                    }
                    self.builder.position_at_end(arena_cont_bb);
                }
                Stmt::Unsafe(block) | Stmt::Alloc { body: block, .. } => {
                    self.compile_block(block, &mut vars)?;
                }
                Stmt::SharedLet { name, init, .. } => {
                    let val = self.compile_expr(init, &vars)?;
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, name)
                        .map_err(|e| format!("shared alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("shared store error: {}", e))?;
                    vars.insert(name.clone(), (alloca, llvm_ty));
                }
                Stmt::Desc(_) | Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Math(_) => {}
                _ => {}
            }
        }
        
        self.check_unconsumed_caps()?;
        self.pop_comp_scope();
        self.pop_cap_scope();
        
        if !self.block_has_terminator() {
            self.builder.build_return(Some(&last_val)).map_err(|e| format!("return error: {}", e))?;
        }
        Ok(())
    }

    /// Compile an async function: generate body + spawner
    fn compile_async_func(&mut self, func: &FuncDef) -> Result<(), String> {
        // 1. Compile the actual body as a hidden function
        let body_name = format!("{}__async_body", func.name);
        let body_func = FuncDef {
            name: body_name,
            commitment: func.commitment,
            pub_: false,
            params: func.params.clone(),
            ret: func.ret.clone(),
            body: func.body.clone(),
            where_clause: None,
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
        };
        self.compile_func(&body_func)?;

        // 2. Compile the public spawner: func name(args) -> i64 { spawn name__async_body(args) }
        // Build call args: name__async_body(arg1, arg2, ...)
        let call_args: Vec<Expr> = func.params.iter().map(|p| {
            Expr::Ident(p.name.clone())
        }).collect();
        let spawn_body = Expr::Spawn(Box::new(
            Expr::Call(
                Box::new(Expr::Ident(format!("{}__async_body", func.name))),
                call_args,
            )
        ));
        let spawner_func = FuncDef {
            name: func.name.clone(),
            commitment: func.commitment,
            pub_: func.pub_,
            params: func.params.clone(),
            ret: Some(Type::Name("i64".into(), vec![])),
            body: vec![Stmt::Expr(spawn_body)],
            where_clause: None,
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
        };
        self.compile_func(&spawner_func)?;
        Ok(())
    }

    fn compile_func(&mut self, func: &FuncDef) -> Result<(), String> {
        // Delegate async funcs to compile_async_func
        if func.is_async {
            return self.compile_async_func(func);
        }
        let ret_type = match &func.ret {
            Some(ty) => types::mimi_type_to_llvm(self.context, ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };

        let mut param_types = Vec::new();
        for param in &func.params {
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &param.ty) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();

        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        let function = self.module.add_function(&func.name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // Push scopes for function body
        self.push_cap_scope();
        self.push_comp_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &param.ty) {
                let alloca = self.builder.build_alloca(ty, &param.name)
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(alloca, function.get_nth_param(i as u32).expect("param index matches function signature"))
                    .map_err(|e| format!("store error: {}", e))?;
                vars.insert(param.name.clone(), (alloca, ty));
                
                // Track type name for method dispatch
                if let Type::Name(tn, _) = &param.ty {
                    self.var_type_names.insert(param.name.clone(), tn.clone());
                }
                if let Type::DynTrait(_) = &param.ty {
                    self.var_type_names.insert(param.name.clone(), crate::core::fmt_type(&param.ty));
                }
                if let Type::ImplTrait(_) = &param.ty {
                    self.var_type_names.insert(param.name.clone(), crate::core::fmt_type(&param.ty));
                }
                
                // Track capability parameters
                if matches!(&param.ty, Type::Cap(_)) {
                    self.register_cap(&param.name, alloca);
                }
            }
        }

        // Compile requires contracts as runtime asserts when verify_contracts is enabled
        if self.verify_contracts {
            for stmt in &func.body {
                if let Stmt::Requires(expr, _) = stmt {
                    self.compile_contract_assert(expr, &vars, &format!("requires violation in '{}'", func.name))?;
                }
            }
        }

        let mut last_val: BasicValueEnum = self.context.i64_type().const_int(0, false).into();
        for stmt in &func.body {
            // Run compensations before exit()
            if let Stmt::Expr(Expr::Call(callee, _)) = stmt {
                if let Expr::Ident(name) = &**callee {
                    if name == "exit" {
                        self.compile_compensations(&mut vars)?;
                    }
                }
            }
            match stmt {
                Stmt::Expr(expr) => {
                    last_val = self.compile_expr(expr, &vars)?;
                }
                Stmt::Return(Some(expr)) => {
                    self.pop_comp_scope();
                    let val = self.compile_expr(expr, &vars)?;
                    self.builder.build_return(Some(&val)).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    self.pop_comp_scope();
                    self.builder.build_return(None).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), ty, .. } => {
                    let val = self.compile_expr(init, &vars)?;
                    let name = match pat {
                        Pattern::Variable(n) => n.clone(),
                        _ => continue,
                    };
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, &name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("store error: {}", e))?;
                    // Track type name from explicit annotation or record expression
                    if let Some(Type::Name(tn, _)) = ty {
                        self.var_type_names.insert(name.clone(), tn.clone());
                    } else if let Expr::Record { ty: Some(tn), .. } = init {
                        self.var_type_names.insert(name.clone(), tn.clone());
                    }
                    vars.insert(name.clone(), (alloca, llvm_ty));
                    
                    // Track capability variables
                    if let Some(Type::Cap(_)) = &ty {
                        self.register_cap(&name, alloca);
                    }
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, &vars)?;
                    if let Some(&(alloca, _)) = vars.get(name) {
                        self.builder.build_store(alloca, val)
                            .map_err(|e| format!("store error: {}", e))?;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("[E0712] if condition must be boolean".into());
                    };

                    let function = self.current_function().unwrap();
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Then block
                    let then_val = {
                        self.builder.position_at_end(then_bb);
                        let mut then_vars = vars.clone();
                        let v = self.compile_block_last_val(then_, &mut then_vars)?;
                        let current = self.builder.get_insert_block().unwrap();
                        if current.get_terminator().is_none() {
                            self.builder.build_unconditional_branch(merge_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                        }
                        v
                    };
                    let then_bb_end = self.builder.get_insert_block().unwrap();

                    // Else block
                    let else_val = {
                        self.builder.position_at_end(else_bb);
                        if let Some(else_block) = else_ {
                            let mut else_vars = vars.clone();
                            let v = self.compile_block_last_val(else_block, &mut else_vars)?;
                            let current = self.builder.get_insert_block().unwrap();
                            if current.get_terminator().is_none() {
                                self.builder.build_unconditional_branch(merge_bb)
                                    .map_err(|e| format!("branch error: {}", e))?;
                            }
                            v
                        } else {
                            self.context.i64_type().const_int(0, false).into()
                        }
                    };
                    let else_bb_end = self.builder.get_insert_block().unwrap();
                    // No-else case: else_bb has no terminator yet — supply one
                    if else_bb_end.get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    // Continue at merge, produce phi if both branches have values
                    self.builder.position_at_end(merge_bb);
                    if then_val.get_type() == else_val.get_type() {
                        let phi = self.builder.build_phi(then_val.get_type(), "if_result")
                            .map_err(|e| format!("phi error: {}", e))?;
                        phi.add_incoming(&[
                            (&then_val as &dyn inkwell::values::BasicValue, then_bb_end),
                            (&else_val as &dyn inkwell::values::BasicValue, else_bb_end),
                        ]);
                        last_val = phi.as_basic_value();
                    }
                }
                Stmt::While { cond, body } => {
                    let function = self.current_function().unwrap();
                    let loop_bb = self.context.append_basic_block(function, "loop");
                    let body_bb = self.context.append_basic_block(function, "loopbody");
                    let merge_bb = self.context.append_basic_block(function, "loopcont");

                    // Jump to loop condition check
                    self.builder.build_unconditional_branch(loop_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Loop condition
                    self.builder.position_at_end(loop_bb);
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("[E0712] while condition must be boolean".into());
                    };
                    self.builder.build_conditional_branch(cond_bool, body_bb, merge_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Loop body
                    self.builder.position_at_end(body_bb);
                    let old_break = self.loop_break.take();
                    let old_continue = self.loop_continue.take();
                    self.loop_break = Some(merge_bb);
                    self.loop_continue = Some(loop_bb);
                    self.compile_block(body, &mut vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.loop_break = old_break;
                    self.loop_continue = old_continue;

                    // Continue after loop
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::For { var, iterable, body } => {
                    let function = self.current_function().unwrap();
                    let iterable_val = self.compile_expr(iterable, &vars)?;

                    if let Expr::Binary(BinOp::Range, start_expr, end_expr) = iterable {
                        let start_val = self.compile_expr(start_expr, &vars)?;
                        let end_val = self.compile_expr(end_expr, &vars)?;
                        let start_iv = if let BasicValueEnum::IntValue(iv) = start_val { iv } else { return Err("[E0712] range start must be i64".into()); };
                        let end_iv = if let BasicValueEnum::IntValue(iv) = end_val { iv } else { return Err("[E0712] range end must be i64".into()); };

                        let idx_alloca = self.builder.build_alloca(self.context.i64_type(), "idx")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(idx_alloca, start_iv)
                            .map_err(|e| format!("store error: {}", e))?;

                        let loop_bb = self.context.append_basic_block(function, "forloop");
                        let body_bb = self.context.append_basic_block(function, "forbody");
                        let merge_bb = self.context.append_basic_block(function, "forcont");

                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(loop_bb);
                        let idx_val = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            idx_alloca,
                            "idx"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] index must be i64".into()); };
                        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx_iv, end_iv, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        self.builder.build_conditional_branch(cmp, body_bb, merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(body_bb);
                        let old_break = self.loop_break.take();
                        let old_continue = self.loop_continue.take();
                        self.loop_break = Some(merge_bb);
                        self.loop_continue = Some(loop_bb);

                        let elem_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(self.context.i64_type()), var)
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(elem_alloca, idx_val)
                            .map_err(|e| format!("store error: {}", e))?;
                        vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(self.context.i64_type())));

                        self.compile_block(body, &mut vars)?;

                        vars.remove(var);
                        self.loop_break = old_break;
                        self.loop_continue = old_continue;

                        let idx_val = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            idx_alloca,
                            "idx"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] index must be i64".into()); };
                        let one = self.context.i64_type().const_int(1, false);
                        let next_idx = self.builder.build_int_add(idx_iv, one, "next_idx")
                            .map_err(|e| format!("add error: {}", e))?;
                        self.builder.build_store(idx_alloca, next_idx)
                            .map_err(|e| format!("store error: {}", e))?;

                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(merge_bb);
                    } else {
                        // Handle list iteration: accept both PointerValue (inline list)
                        // and IntValue (list parameter passed as opaque i64 pointer)
                        let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let list_ptr = match iterable_val {
                            BasicValueEnum::PointerValue(pv) => pv,
                            BasicValueEnum::IntValue(iv) => {
                                // Cast i64 (opaque pointer) to struct pointer
                                let int_ptr = self.builder.build_int_to_ptr(iv, i8_ptr_ty, "list_as_ptr")
                                    .map_err(|e| format!("int_to_ptr error: {}", e))?;
                                int_ptr
                            }
                            _ => return Err("for loop requires a list or range".into()),
                        };

                        let list_struct_ty = inkwell::types::BasicTypeEnum::StructType(
                            self.context.struct_type(&[
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                            ], false)
                        );
                        let list_len_gep = self.builder.build_struct_gep(
                            list_struct_ty,
                            list_ptr,
                            0,
                            "list.len"
                        ).map_err(|e| format!("gep error: {}", e))?;
                        let list_len = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            list_len_gep,
                            "len"
                        ).map_err(|e| format!("load error: {}", e))?;

                        let idx_alloca = self.builder.build_alloca(self.context.i64_type(), "idx")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(idx_alloca, self.context.i64_type().const_int(0, false))
                            .map_err(|e| format!("store error: {}", e))?;

                        let loop_bb = self.context.append_basic_block(function, "forloop");
                        let body_bb = self.context.append_basic_block(function, "forbody");
                        let merge_bb = self.context.append_basic_block(function, "forcont");

                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(loop_bb);
                        let idx_val = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            idx_alloca,
                            "idx"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] index must be i64".into()); };
                        let len_iv = if let BasicValueEnum::IntValue(iv) = list_len { iv } else { return Err("length must be i64".into()); };
                        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx_iv, len_iv, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        self.builder.build_conditional_branch(cmp, body_bb, merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(body_bb);
                        let old_break = self.loop_break.take();
                        let old_continue = self.loop_continue.take();
                        self.loop_break = Some(merge_bb);
                        self.loop_continue = Some(loop_bb);

                        let data_gep = self.builder.build_struct_gep(
                            list_struct_ty,
                            list_ptr,
                            1,
                            "list.data"
                        ).map_err(|e| format!("gep error: {}", e))?;
                        let data_ptr = self.builder.build_load(
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                            data_gep,
                            "data"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let data_pv = if let BasicValueEnum::PointerValue(pv) = data_ptr { pv } else { return Err("data must be pointer".into()); };

                        let elem_ptr = unsafe {
                            self.builder.build_gep(
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                data_pv,
                                &[idx_iv],
                                "elem"
                            )
                        }.map_err(|e| format!("gep error: {}", e))?;
                        let elem = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            elem_ptr,
                            "elem_val"
                        ).map_err(|e| format!("load error: {}", e))?;

                        let elem_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(self.context.i64_type()), var)
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(elem_alloca, elem)
                            .map_err(|e| format!("store error: {}", e))?;
                        vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(self.context.i64_type())));

                        self.compile_block(body, &mut vars)?;

                        vars.remove(var);
                        self.loop_break = old_break;
                        self.loop_continue = old_continue;

                        let idx_val = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            idx_alloca,
                            "idx"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] index must be i64".into()); };
                        let one = self.context.i64_type().const_int(1, false);
                        let next_idx = self.builder.build_int_add(idx_iv, one, "next_idx")
                            .map_err(|e| format!("add error: {}", e))?;
                        self.builder.build_store(idx_alloca, next_idx)
                            .map_err(|e| format!("store error: {}", e))?;

                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(merge_bb);
                    }
                }
                Stmt::Break(_) => {
                    if let Some(target) = self.loop_break {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| format!("break error: {}", e))?;
                        // Create unreachable block for subsequent statements
                        let function = self.current_function().unwrap();
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err("break outside of loop".into());
                    }
                }
                Stmt::Continue => {
                    if let Some(target) = self.loop_continue {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| format!("continue error: {}", e))?;
                        let function = self.current_function().unwrap();
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err("continue outside of loop".into());
                    }
                }
                Stmt::MmsBlock { .. } => {
                    // Skip MMS blocks in codegen (they're for documentation/contracts)
                }
                Stmt::Parasteps(block) => {
                    // Parasteps: execute spawn statements in parallel, join at block end
                    self.enter_parasteps();
                    self.compile_block(block, &mut vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    // Drop: evaluate expression and mark capability as consumed
                    let _val = self.compile_expr(expr, &vars)?;
                    // If the expression is a variable, mark it as consumed and call mimi_cap_consume
                    if let Expr::Ident(name) = expr {
                        self.consume_cap(name)?;
                        // Generate runtime cap consume call
                        if self.is_cap_var(name) {
                            if let Some(consume_fn) = self.module.get_function("mimi_cap_consume") {
                                if let Some(&(alloca, _)) = vars.get(name) {
                                    let cap_val = self.builder.build_load(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        alloca, &format!("cap_val_{}", name))
                                        .map_err(|e| format!("load error: {}", e))?;
                                    let name_global = self.builder.build_global_string_ptr(
                                        &format!("{}\0", name), &format!("cap_name_drop_{}", name))
                                        .map_err(|e| format!("string global error: {}", e))?;
                                    let name_ptr = name_global.as_pointer_value();
                                    self.builder.build_call(consume_fn, &[
                                        BasicMetadataValueEnum::IntValue(cap_val.into_int_value()),
                                        BasicMetadataValueEnum::PointerValue(name_ptr),
                                    ], &format!("cap_consume_{}", name))
                                        .map_err(|e| format!("cap_consume error: {}", e))?;
                                }
                            }
                        }
                    }
                }
                Stmt::SharedLet { name, init, .. } => {
                    let val = self.compile_expr(init, &vars)?;
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, name)
                        .map_err(|e| format!("shared alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("shared store error: {}", e))?;
                    vars.insert(name.clone(), (alloca, llvm_ty));
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    let function = self.current_function().ok_or("arena outside function")?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| format!("branch to arena: {}", e))?;
                    }
                    self.builder.position_at_end(arena_body_bb);
                    let saved = self.build_stacksave()?;
                    let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
                    self.compile_block(block, &mut vars)?;
                    for k in vars.keys().cloned().collect::<Vec<_>>() {
                        if !vars_before.contains(&k) {
                            vars.remove(&k);
                        }
                    }
                    self.build_stackrestore(saved)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_cont_bb)
                            .map_err(|e| format!("branch after arena: {}", e))?;
                    }
                    self.builder.position_at_end(arena_cont_bb);
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, &mut vars)?;
                }
                Stmt::Alloc { kind: AllocKind::Arena, body } => {
                    let function = self.current_function().ok_or("arena outside function")?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| format!("branch to alloc(Arena): {}", e))?;
                    }
                    self.builder.position_at_end(arena_body_bb);
                    let saved = self.build_stacksave()?;
                    let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
                    self.compile_block(body, &mut vars)?;
                    for k in vars.keys().cloned().collect::<Vec<_>>() {
                        if !vars_before.contains(&k) {
                            vars.remove(&k);
                        }
                    }
                    self.build_stackrestore(saved)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_cont_bb)
                            .map_err(|e| format!("branch after alloc(Arena): {}", e))?;
                    }
                    self.builder.position_at_end(arena_cont_bb);
                }
                Stmt::Alloc { body, .. } => {
                    // Alloc: execute body sequentially (simplified - no custom allocator in codegen)
                    self.compile_block(body, &mut vars)?;
                }
                Stmt::Desc(_) | Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Math(_) => {
                    // Skip contract-related statements in codegen
                }
                _ => {}
            }
        }

        // Check for unconsumed capabilities before returning
        self.check_unconsumed_caps()?;
        
        // Pop scopes (discard compensations on normal exit)
        self.pop_comp_scope();
        self.pop_cap_scope();

        self.builder.build_return(Some(&last_val)).map_err(|e| format!("return error: {}", e))?;
        Ok(())
    }

    /// Compile a generic function with concrete type arguments (monomorphization)
    fn compile_generic_func(&mut self, func: &FuncDef, type_map: &HashMap<String, crate::ast::Type>) -> Result<(), String> {
        // Save and set the type_map
        let prev_type_map = self.type_map.clone();
        self.type_map = type_map.clone();

        let mangled = Self::mangle_name(&func.name, type_map);

        // Skip if already compiled
        if self.module.get_function(&mangled).is_some() {
            self.type_map = prev_type_map;
            return Ok(());
        }

        // Substitute generic params in ret type and param types
        let ret_type = match &func.ret {
            Some(ty) => {
                let resolved = self.resolve_type(ty);
                types::mimi_type_to_llvm(self.context, &resolved)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
            }
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };

        let mut param_types = Vec::new();
        for param in &func.params {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &resolved) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();

        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        let function = self.module.add_function(&mangled, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.push_cap_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &resolved) {
                let alloca = self.builder.build_alloca(ty, &param.name)
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(alloca, function.get_nth_param(i as u32).expect("param index matches"))
                    .map_err(|e| format!("store error: {}", e))?;
                vars.insert(param.name.clone(), (alloca, ty));
                if matches!(&param.ty, Type::Cap(_)) {
                    self.register_cap(&param.name, alloca);
                }
            }
        }

        let last_val: BasicValueEnum = self.context.i64_type().const_int(0, false).into();
        self.compile_block(&func.body, &mut vars)?;

        self.check_unconsumed_caps()?;
        self.pop_cap_scope();

        if !self.block_has_terminator() {
            self.builder.build_return(Some(&last_val)).map_err(|e| format!("return error: {}", e))?;
        }
        self.type_map = prev_type_map;
        Ok(())
    }

    fn compile_block(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), String> {
        self.push_comp_scope();
        for stmt in block {
            // Run compensations before exit()
            if let Stmt::Expr(Expr::Call(callee, _)) = stmt {
                if let Expr::Ident(name) = &**callee {
                    if name == "exit" {
                        self.compile_compensations(vars)?;
                    }
                }
            }
            match stmt {
                Stmt::Expr(expr) => {
                    self.compile_expr(expr, vars)?;
                }
                Stmt::Return(Some(expr)) => {
                    let val = self.compile_expr(expr, vars)?;
                    self.builder.build_return(Some(&val)).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    self.builder.build_return(None).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), .. } => {
                    let val = self.compile_expr(init, vars)?;
                    let name = match pat {
                        Pattern::Variable(n) => n.clone(),
                        _ => continue,
                    };
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, &name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("store error: {}", e))?;
                    if let Expr::Record { ty: Some(tn), .. } = init {
                        self.var_type_names.insert(name.clone(), tn.clone());
                    }
                    vars.insert(name, (alloca, llvm_ty));
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, vars)?;
                    if let Some(&(alloca, _)) = vars.get(name) {
                        self.builder.build_store(alloca, val)
                            .map_err(|e| format!("store error: {}", e))?;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("[E0712] if condition must be boolean".into());
                    };

                    let function = self.current_function().unwrap();
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, vars)?;
                    }
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    self.builder.position_at_end(merge_bb);
                }
                Stmt::Break(_) | Stmt::Continue => {}
                Stmt::MmsBlock { .. } => {
                    // Skip MMS blocks in codegen (they're for documentation/contracts)
                }
                Stmt::Parasteps(block) => {
                    // Parasteps: execute spawn statements in parallel, join at block end
                    self.enter_parasteps();
                    self.compile_block(block, vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    // Drop: evaluate expression and discard result (for linear capabilities)
                    self.compile_expr(expr, vars)?;
                }
                Stmt::SharedLet { name, init, .. } => {
                    let val = self.compile_expr(init, vars)?;
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, name)
                        .map_err(|e| format!("shared alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("shared store error: {}", e))?;
                    vars.insert(name.clone(), (alloca, llvm_ty));
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    let function = self.current_function().ok_or("arena outside function")?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| format!("branch to arena: {}", e))?;
                    }
                    self.builder.position_at_end(arena_body_bb);
                    let saved = self.build_stacksave()?;
                    let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
                    self.compile_block(block, vars)?;
                    for k in vars.keys().cloned().collect::<Vec<_>>() {
                        if !vars_before.contains(&k) {
                            vars.remove(&k);
                        }
                    }
                    self.build_stackrestore(saved)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_cont_bb)
                            .map_err(|e| format!("branch after arena: {}", e))?;
                    }
                    self.builder.position_at_end(arena_cont_bb);
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, vars)?;
                }
                Stmt::Alloc { kind: AllocKind::Arena, body } => {
                    let function = self.current_function().ok_or("arena outside function")?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| format!("branch to alloc(Arena): {}", e))?;
                    }
                    self.builder.position_at_end(arena_body_bb);
                    let saved = self.build_stacksave()?;
                    let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
                    self.compile_block(body, vars)?;
                    for k in vars.keys().cloned().collect::<Vec<_>>() {
                        if !vars_before.contains(&k) {
                            vars.remove(&k);
                        }
                    }
                    self.build_stackrestore(saved)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_cont_bb)
                            .map_err(|e| format!("branch after alloc(Arena): {}", e))?;
                    }
                    self.builder.position_at_end(arena_cont_bb);
                }
                Stmt::Alloc { body, .. } => {
                    // Alloc: execute body sequentially (simplified)
                    self.compile_block(body, vars)?;
                }
                Stmt::Desc(_) | Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Math(_) => {
                    // Skip contract-related statements in codegen
                }
                _ => {}
            }
        }
        self.pop_comp_scope();
        Ok(())
    }

    /// Call @llvm.stacksave() to capture the current stack pointer for arena region management
    fn build_stacksave(&self) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let fn_type = i8_ptr.fn_type(&[], false);
        let fn_val = self.module.get_function("llvm.stacksave")
            .unwrap_or_else(|| self.module.add_function(
                "llvm.stacksave",
                fn_type,
                Some(inkwell::module::Linkage::External),
            ));
        let call = self.builder.build_call(fn_val, &[], "saved_stack")
            .map_err(|e| format!("stacksave: {}", e))?;
        let val = call.try_as_basic_value().left()
            .ok_or("stacksave returned void")?;
        match val {
            BasicValueEnum::PointerValue(ptr) => Ok(ptr),
            _ => Err(format!("stacksave didn't return pointer, got {:?}", val)),
        }
    }

    /// Call @llvm.stackrestore(i8*) to restore the stack pointer, freeing arena allocations
    fn build_stackrestore(&self, saved: inkwell::values::PointerValue<'ctx>) -> Result<(), String> {
        let i8_ptr_meta = BasicMetadataTypeEnum::PointerType(
            self.context.i8_type().ptr_type(inkwell::AddressSpace::default()),
        );
        let fn_type = self.context.void_type().fn_type(&[i8_ptr_meta], false);
        let fn_val = self.module.get_function("llvm.stackrestore")
            .unwrap_or_else(|| self.module.add_function(
                "llvm.stackrestore",
                fn_type,
                Some(inkwell::module::Linkage::External),
            ));
        self.builder.build_call(fn_val, &[BasicMetadataValueEnum::PointerValue(saved)], "")
            .map_err(|e| format!("stackrestore: {}", e))?;
        Ok(())
    }

    /// Compile a block and return the value of its last expression (for if-expressions)
    fn compile_block_last_val(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let mut last_val = self.context.i64_type().const_int(0, false).into();
        for stmt in block {
            match stmt {
                Stmt::Expr(e) => {
                    last_val = self.compile_expr(e, vars)?;
                }
                Stmt::Return(Some(e)) => {
                    return self.compile_expr(e, vars);
                }
                Stmt::Return(None) => {
                    return Ok(self.context.i64_type().const_int(0, false).into());
                }
                Stmt::Let { pat, init: Some(init), .. } => {
                    let val = self.compile_expr(init, vars)?;
                    let name = match pat {
                        Pattern::Variable(n) => n.clone(),
                        _ => continue,
                    };
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, &name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("store error: {}", e))?;
                    vars.insert(name, (alloca, llvm_ty));
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, vars)?;
                    if let Some(&(alloca, _)) = vars.get(name) {
                        self.builder.build_store(alloca, val)
                            .map_err(|e| format!("store error: {}", e))?;
                        last_val = val;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("[E0712] if condition must be boolean".into());
                    };
                    let function = self.current_function().unwrap();
                    let then_bb = self.context.append_basic_block(function, "blt_then");
                    let else_bb = self.context.append_basic_block(function, "blt_else");
                    let merge_bb = self.context.append_basic_block(function, "blt_merge");
                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    let then_val = {
                        self.builder.position_at_end(then_bb);
                        let mut then_vars = vars.clone();
                        let v = self.compile_block_last_val(then_, &mut then_vars)?;
                        if !self.block_has_terminator() {
                            self.builder.build_unconditional_branch(merge_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                        }
                        v
                    };
                    let then_bb_end = self.builder.get_insert_block().unwrap();
                    let else_val = {
                        self.builder.position_at_end(else_bb);
                        if let Some(eb) = else_ {
                            let mut else_vars = vars.clone();
                            let v = self.compile_block_last_val(eb, &mut else_vars)?;
                            if !self.block_has_terminator() {
                                self.builder.build_unconditional_branch(merge_bb)
                                    .map_err(|e| format!("branch error: {}", e))?;
                            }
                            v
                        } else {
                            self.context.i64_type().const_int(0, false).into()
                        }
                    };
                    let else_bb_end = self.builder.get_insert_block().unwrap();
                    // Ensure else_bb has a terminator (it's empty for no-else case)
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.builder.position_at_end(merge_bb);
                    // Create phi if both branches produce a value of the same type
                    if then_val.get_type() == else_val.get_type() {
                        let phi = self.builder.build_phi(then_val.get_type(), "if_lastval")
                            .map_err(|e| format!("phi error: {}", e))?;
                        phi.add_incoming(&[
                            (&then_val as &dyn inkwell::values::BasicValue, then_bb_end),
                            (&else_val as &dyn inkwell::values::BasicValue, else_bb_end),
                        ]);
                        last_val = phi.as_basic_value();
                    } else {
                        last_val = then_val;
                    }
                }
                _ => {}
            }
        }
        Ok(last_val)
    }

    fn compile_expr(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match expr {
            Expr::Literal(lit) => match lit {
                Lit::Int(n) => Ok(self.context.i64_type().const_int(*n as u64, true).into()),
                Lit::Float(f) => Ok(self.context.f64_type().const_float(*f).into()),
                Lit::Bool(b) => Ok(self.context.bool_type().const_int(*b as u64, false).into()),
                Lit::Unit => Ok(self.context.i64_type().const_int(0, false).into()),
                Lit::String(s) => {
                    let global = self.builder.build_global_string_ptr(s, "str")
                        .map_err(|e| format!("string error: {}", e))?;
                    Ok(global.as_pointer_value().into())
                }
                Lit::FString(parts) => Ok(self.compile_fstring(parts, vars)?),
            },
            Expr::Ident(name) => {
                if let Some(&(alloca, ty)) = vars.get(name) {
                    self.builder.build_load(ty, alloca, name)
                        .map_err(|e| format!("load error: {}", e))
                } else if self.cap_type_names.contains(name.as_str()) {
                    // Cap literal: call mimi_cap_register(name) to get handle
                    if let Some(register_fn) = self.module.get_function("mimi_cap_register") {
                        let name_global = self.builder.build_global_string_ptr(
                            &format!("{}\0", name), &format!("cap_name_{}", name))
                            .map_err(|e| format!("string global error: {}", e))?;
                        let name_ptr = name_global.as_pointer_value();
                        let handle = self.builder.build_call(register_fn, &[
                            BasicMetadataValueEnum::PointerValue(name_ptr),
                        ], &format!("cap_register_{}", name))
                            .map_err(|e| format!("cap_register error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("mimi_cap_register returned void")?;
                        Ok(handle)
                    } else {
                        Err(format!("cap literal '{}' requires mimi_cap_register runtime", name))
                    }
                } else {
                    Err(format!("undefined variable '{}'", name))
                }
            }
            Expr::Binary(op, lhs, rhs) => {
                let l = self.compile_expr(lhs, vars)?;
                let r = self.compile_expr(rhs, vars)?;
                self.compile_binop(*op, l, r)
            }
            Expr::Unary(op, inner) => {
                let v = self.compile_expr(inner, vars)?;
                match op {
                    UnOp::Neg => {
                        if let BasicValueEnum::IntValue(iv) = v {
                            let zero = self.context.i64_type().const_int(0, true);
                            Ok(self.builder.build_int_sub(zero, iv, "neg")
                                .map_err(|e| format!("neg error: {}", e))?.into())
                        } else if let BasicValueEnum::FloatValue(fv) = v {
                            let zero = self.context.f64_type().const_float(0.0);
                            Ok(self.builder.build_float_sub(zero, fv, "fneg")
                                .map_err(|e| format!("neg error: {}", e))?.into())
                        } else {
                            Err("negation requires numeric type".into())
                        }
                    }
                    UnOp::Not => {
                        if let BasicValueEnum::IntValue(iv) = v {
                            Ok(self.builder.build_not(iv, "not")
                                .map_err(|e| format!("not error: {}", e))?.into())
                        } else {
                            Err("not requires boolean type".into())
                        }
                    }
                    UnOp::Ref | UnOp::RefMut => {
                        let ty = v.get_type();
                        let alloca = self.builder.build_alloca(ty, "ref")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(alloca, v)
                            .map_err(|e| format!("store error: {}", e))?;
                        Ok(alloca.into())
                    }
                    UnOp::Deref => {
                        if let BasicValueEnum::PointerValue(ptr) = v {
                            // Try to determine the pointee type from the inner expression's variable entry
                            let pointee_ty = match inner.as_ref() {
                                Expr::Ident(name) => {
                                    if let Some(&(_, ty)) = vars.get(name) {
                                        ty
                                    } else {
                                        BasicTypeEnum::IntType(self.context.i64_type())
                                    }
                                }
                                _ => BasicTypeEnum::IntType(self.context.i64_type()),
                            };
                            Ok(self.builder.build_load(pointee_ty, ptr, "deref")
                                .map_err(|e| format!("load error: {}", e))?)
                        } else {
                            Err("deref requires pointer type".into())
                        }
                    }
                }
            }
            Expr::Call(callee, args) => {
                match callee.as_ref() {
                    Expr::Ident(name) => {
                        // Compile-time builtins: resolved at codegen time, not runtime
                        match name.as_str() {
                            "type_name" if args.len() == 1 => {
                                let type_str = match &args[0] {
                                    Expr::Ident(var_name) => self.var_type_names.get(var_name)
                                        .cloned().unwrap_or_else(|| "unknown".to_string()),
                                    Expr::Literal(Lit::String(s)) => s.clone(),
                                    _ => "unknown".to_string(),
                                };
                                // Build string literal: { i8*, i64 }
                                let global = self.builder.build_global_string_ptr(&type_str, "type_name")
                                    .map_err(|e| format!("global string error: {}", e))?;
                                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                                let string_ty = self.context.struct_type(&[
                                    BasicTypeEnum::PointerType(i8_ptr),
                                    BasicTypeEnum::IntType(self.context.i64_type()),
                                ], false);
                                let alloca = self.builder.build_alloca(string_ty, "type_str")
                                    .map_err(|e| format!("alloca error: {}", e))?;
                                let ptr_gep = self.builder.build_struct_gep(string_ty, alloca, 0, "ptr")
                                    .map_err(|e| format!("gep error: {}", e))?;
                                self.builder.build_store(ptr_gep, global.as_pointer_value())
                                    .map_err(|e| format!("store error: {}", e))?;
                                let len_gep = self.builder.build_struct_gep(string_ty, alloca, 1, "len")
                                    .map_err(|e| format!("gep error: {}", e))?;
                                let len = self.context.i64_type().const_int(type_str.len() as u64, false);
                                self.builder.build_store(len_gep, len)
                                    .map_err(|e| format!("store error: {}", e))?;
                                Ok(alloca.into())
                            }
                            "type_fields" if args.len() == 1 => {
                                let type_name_str = match &args[0] {
                                    Expr::Literal(Lit::String(s)) => s.clone(),
                                    Expr::Ident(var) => self.var_type_names.get(var)
                                        .cloned().unwrap_or_else(|| "unknown".to_string()),
                                    _ => return Err("[E0712] type_fields: argument must be a type name string".into()),
                                };
                                let field_names: Vec<String> = self.type_defs.get(&type_name_str)
                                    .map(|td| match &td.kind {
                                        TypeDefKind::Record(fields) => {
                                            fields.iter().map(|f| f.name.clone()).collect()
                                        }
                                        TypeDefKind::Enum(variants) => {
                                            variants.iter().map(|v| v.name.clone()).collect()
                                        }
                                        _ => vec![],
                                    })
                                    .unwrap_or_default();
                                // Build a List of field names
                                self.build_string_list(&field_names, vars)
                            }
                            "type_variants" if args.len() == 1 => {
                                let type_name_str = match &args[0] {
                                    Expr::Literal(Lit::String(s)) => s.clone(),
                                    Expr::Ident(var) => self.var_type_names.get(var)
                                        .cloned().unwrap_or_else(|| "unknown".to_string()),
                                    _ => return Err("[E0712] type_variants: argument must be a type name string".into()),
                                };
                                let variant_names: Vec<String> = self.type_defs.get(&type_name_str)
                                    .map(|td| match &td.kind {
                                        TypeDefKind::Enum(variants) => {
                                            variants.iter().map(|v| v.name.clone()).collect()
                                        }
                                        _ => vec![],
                                    })
                                    .unwrap_or_default();
                                self.build_string_list(&variant_names, vars)
                            }
                            "keys" | "values" if args.len() == 1 => {
                                let var_name = match &args[0] {
                                    Expr::Ident(n) => n.clone(),
                                    _ => return Err("keys/values: argument must be a variable name".into()),
                                };
                                let type_name = self.var_type_names.get(&var_name)
                                    .cloned().unwrap_or_else(|| "unknown".to_string());
                                let field_names: Vec<String> = self.type_defs.get(&type_name)
                                    .map(|td| match &td.kind {
                                        TypeDefKind::Record(fields) => {
                                            fields.iter().map(|f| f.name.clone()).collect()
                                        }
                                        _ => vec![],
                                    })
                                    .unwrap_or_default();
                                if name == "keys" {
                                    self.build_string_list(&field_names, vars)
                                } else {
                                    let field_count = field_names.len();
                                    let llvm_ty = self.type_llvm.get(&type_name).cloned();
                                    if let Some(BasicTypeEnum::StructType(_struct_ty)) = llvm_ty {
                                        let i64_ty = self.context.i64_type();
                                        let sizeof_i64 = i64_ty.const_int(8, false);
                                        let alloc_size = self.builder.build_int_mul(
                                            i64_ty.const_int(field_count as u64, false),
                                            sizeof_i64,
                                            "values_alloc_size"
                                        ).map_err(|e| format!("mul error: {}", e))?;
                                        let malloc_fn = self.module.get_function("malloc")
                                            .ok_or_else(|| "malloc not declared".to_string())?;
                                        let values_data = self.builder.build_call(malloc_fn, &[
                                            BasicMetadataValueEnum::IntValue(alloc_size),
                                        ], "values_malloc")
                                            .map_err(|e| format!("malloc error: {}", e))?
                                            .try_as_basic_value().left()
                                            .ok_or("malloc returned void")?
                                            .into_pointer_value();
                                        let values_data_i64 = self.builder.build_bit_cast(values_data,
                                            i64_ty.ptr_type(inkwell::AddressSpace::default()), "values_data_i64")
                                            .map_err(|e| format!("bitcast error: {}", e))?
                                            .into_pointer_value();
                                        let record_ptr = match self.compile_expr(&args[0], vars)? {
                                            BasicValueEnum::PointerValue(pv) => pv,
                                            _ => return Err("[E0712] values: expected record pointer".into()),
                                        };
                                        let td = self.type_defs.get(&type_name);
                                        if let Some(TypeDefKind::Record(fields)) = td.map(|t| &t.kind) {
                                            for (i, field) in fields.iter().enumerate() {
                                                let gep = self.builder.build_struct_gep(_struct_ty, record_ptr, i as u32, &field.name)
                                                    .map_err(|e| format!("gep error: {}", e))?;
                                                let field_ty = types::mimi_type_to_llvm(self.context, &field.ty)
                                                    .unwrap_or(BasicTypeEnum::IntType(i64_ty));
                                                let val = self.builder.build_load(field_ty, gep, &field.name)
                                                    .map_err(|e| format!("load error: {}", e))?;
                                                let val_i64 = match val {
                                                    BasicValueEnum::IntValue(iv) => iv,
                                                    BasicValueEnum::FloatValue(fv) => self.builder.build_float_to_unsigned_int(fv, i64_ty, "float_to_i64")
                                                        .map_err(|e| format!("fptosi error: {}", e))?,
                                                    BasicValueEnum::PointerValue(pv) => self.builder.build_ptr_to_int(pv, i64_ty, "ptr_to_i64")
                                                        .map_err(|e| format!("ptrtoint error: {}", e))?,
                                                    _ => return Err("[E0701] values: unsupported field type".into()),
                                                };
                                                let elem_ptr = unsafe { self.builder.build_gep(i64_ty, values_data_i64, &[i64_ty.const_int(i as u64, false)], "values_elem") }
                                                    .map_err(|e| format!("gep error: {}", e))?;
                                                self.builder.build_store(elem_ptr, val_i64)
                                                    .map_err(|e| format!("store error: {}", e))?;
                                            }
                                            let result_list_ty = self.context.struct_type(&[
                                                BasicTypeEnum::IntType(i64_ty),
                                                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                                            ], false);
                                            let result_alloca = self.builder.build_alloca(result_list_ty, "values_result")
                                                .map_err(|e| format!("alloca error: {}", e))?;
                                            let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "values_result_len")
                                                .map_err(|e| format!("gep error: {}", e))?;
                                            self.builder.build_store(result_len_gep, i64_ty.const_int(field_count as u64, false))
                                                .map_err(|e| format!("store error: {}", e))?;
                                            let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "values_result_data")
                                                .map_err(|e| format!("gep error: {}", e))?;
                                            let values_data_void = self.builder.build_bit_cast(values_data,
                                                self.context.ptr_type(inkwell::AddressSpace::default()), "values_data_void")
                                                .map_err(|e| format!("bitcast error: {}", e))?;
                                            self.builder.build_store(result_data_gep, values_data_void)
                                                .map_err(|e| format!("store error: {}", e))?;
                                            Ok(result_alloca.into())
                                        } else {
                                            Err("values: argument must be a record type".into())
                                        }
                                    } else {
                                        Err("values: type is not a struct".into())
                                    }
                                }
                            }
                            // map/list, fn_ref): compile-time list iteration + function call
                            "map" | "filter" if args.len() == 2 => {
                                let is_map = name == "map";
                                // Compile the list expression
                                let list_val = self.compile_expr(&args[0], vars)?;
                                let list_ptr = match list_val {
                                    BasicValueEnum::PointerValue(pv) => pv,
                                    _ => return Err("map/filter: first arg must be a list".into()),
                                };
                                // Resolve function name from second arg (must be an identifier)
                                let fn_name = match &args[1] {
                                    Expr::Ident(n) => n.clone(),
                                    _ => return Err("map/filter: second arg must be a function name (identifier)".into()),
                                };
                                let fn_llvm = self.module.get_function(&fn_name)
                                    .ok_or_else(|| format!("map/filter: function '{}' not compiled", fn_name))?;
                                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                                let i64_ty = self.context.i64_type();
                                let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                                    BasicTypeEnum::IntType(i64_ty),
                                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                                ], false));
                                // Read list length and data pointer
                                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                                    .map_err(|e| format!("gep error: {}", e))?;
                                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                                    .map_err(|e| format!("load error: {}", e))?;
                                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                                    .map_err(|e| format!("gep error: {}", e))?;
                                let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")
                                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                                let data_ptr = self.builder.build_bit_cast(data_i8,
                                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                                    .map_err(|e| format!("bitcast error: {}", e))?
                                    .into_pointer_value();
                                // Build result list: allocate {i64 len, i8* data}
                                let result_ty = self.context.struct_type(&[
                                    BasicTypeEnum::IntType(i64_ty),
                                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                                ], false);
                                let result_alloca = self.builder.build_alloca(result_ty, "map_result")
                                    .map_err(|e| format!("alloca error: {}", e))?;
                                // Allocate output data array (same len)
                                let elem_size = i64_ty.const_int(8, false);
                                let alloc_size = self.builder.build_int_mul(list_len.into_int_value(), elem_size, "alloc_size")
                                    .map_err(|e| format!("mul error: {}", e))?;
                                let malloc_fn = self.module.get_function("malloc")
                                    .ok_or_else(|| "malloc not declared".to_string())?;
                                let out_ptr = self.builder.build_call(malloc_fn, &[
                                    BasicMetadataValueEnum::IntValue(alloc_size),
                                ], "out_malloc")
                                    .map_err(|e| format!("malloc error: {}", e))?
                                    .try_as_basic_value().left()
                                    .ok_or("malloc returned void")?
                                    .into_pointer_value();
                                let out_i64 = self.builder.build_bit_cast(out_ptr,
                                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "out_i64")
                                    .map_err(|e| format!("bitcast error: {}", e))?
                                    .into_pointer_value();
                                // Loop: for i in 0..len
                                let function = self.current_function().unwrap();
                                let loop_bb = self.context.append_basic_block(function, "hof_loop");
                                let body_bb = self.context.append_basic_block(function, "hof_body");
                                let done_bb = self.context.append_basic_block(function, "hof_done");
                                let idx_alloca = self.builder.build_alloca(i64_ty, "hi")
                                    .map_err(|e| format!("alloca error: {}", e))?;
                                let write_idx = self.builder.build_alloca(i64_ty, "wi")
                                    .map_err(|e| format!("alloca error: {}", e))?;
                                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                                    .map_err(|e| format!("store error: {}", e))?;
                                self.builder.build_store(write_idx, i64_ty.const_int(0, false))
                                    .map_err(|e| format!("store error: {}", e))?;
                                self.builder.build_unconditional_branch(loop_bb)
                                    .map_err(|e| format!("branch error: {}", e))?;
                                self.builder.position_at_end(loop_bb);
                                let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
                                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                                let loop_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len.into_int_value(), "cmp")
                                    .map_err(|e| format!("cmp error: {}", e))?;
                                self.builder.build_conditional_branch(loop_cmp, body_bb, done_bb)
                                    .map_err(|e| format!("branch error: {}", e))?;
                                self.builder.position_at_end(body_bb);
                                // Load element
                                let elem_ptr = unsafe {
                                    self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
                                }.map_err(|e| format!("gep error: {}", e))?;
                                let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
                                    .map_err(|e| format!("load error: {}", e))?;
                                // Call the function: fn(elem)
                                let fn_call = self.builder.build_call(fn_llvm, &[
                                    BasicMetadataValueEnum::IntValue(elem.into_int_value()),
                                ], "fn_call")
                                    .map_err(|e| format!("call error: {}", e))?;
                                let result = fn_call.try_as_basic_value().left()
                                    .ok_or("function returned void")?;
                                if is_map {
                                    // For map: store result to output array
                                    let out_elem_ptr = unsafe {
                                        self.builder.build_gep(i64_ty, out_i64, &[idx], "out_elem")
                                    }.map_err(|e| format!("gep error: {}", e))?;
                                    self.builder.build_store(out_elem_ptr, result)
                                        .map_err(|e| format!("store error: {}", e))?;
                                } else {
                                    // For filter: if result is truthy (non-zero), store to output array
                                    let zero = i64_ty.const_int(0, false);
                                    // Zero-extend result to i64 for comparison (result may be i1 bool)
                                    let result_i64 = self.builder.build_int_z_extend(result.into_int_value(), i64_ty, "result_ext")
                                        .map_err(|e| format!("zext error: {}", e))?;
                                    let truthy = self.builder.build_int_compare(inkwell::IntPredicate::NE, result_i64, zero, "truthy")
                                        .map_err(|e| format!("cmp error: {}", e))?;
                                    let store_bb = self.context.append_basic_block(function, "filter_store");
                                    let next_bb = self.context.append_basic_block(function, "filter_next");
                                    self.builder.build_conditional_branch(truthy, store_bb, next_bb)
                                        .map_err(|e| format!("branch error: {}", e))?;
                                    self.builder.position_at_end(store_bb);
                                    let wi = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), write_idx, "wi")
                                        .map_err(|e| format!("load error: {}", e))?.into_int_value();
                                    let out_elem_ptr = unsafe {
                                        self.builder.build_gep(i64_ty, out_i64, &[wi], "out_elem")
                                    }.map_err(|e| format!("gep error: {}", e))?;
                                    self.builder.build_store(out_elem_ptr, elem)
                                        .map_err(|e| format!("store error: {}", e))?;
                                    let next_wi = self.builder.build_int_add(wi, i64_ty.const_int(1, false), "next_wi")
                                        .map_err(|e| format!("add error: {}", e))?;
                                    self.builder.build_store(write_idx, next_wi)
                                        .map_err(|e| format!("store error: {}", e))?;
                                    self.builder.build_unconditional_branch(next_bb)
                                        .map_err(|e| format!("branch error: {}", e))?;
                                    self.builder.position_at_end(next_bb);
                                }
                                // idx++
                                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                                    .map_err(|e| format!("add error: {}", e))?;
                                self.builder.build_store(idx_alloca, next)
                                    .map_err(|e| format!("store error: {}", e))?;
                                self.builder.build_unconditional_branch(loop_bb)
                                    .map_err(|e| format!("branch error: {}", e))?;
                                self.builder.position_at_end(done_bb);
                                // Store result list: len and data ptr
                                let out_len = if is_map {
                                    list_len
                                } else {
                                    self.builder.build_load(BasicTypeEnum::IntType(i64_ty), write_idx, "out_len")
                                        .map_err(|e| format!("load error: {}", e))?
                                };
                                let out_len_gep = self.builder.build_struct_gep(result_ty, result_alloca, 0, "out_len")
                                    .map_err(|e| format!("gep error: {}", e))?;
                                self.builder.build_store(out_len_gep, out_len)
                                    .map_err(|e| format!("store error: {}", e))?;
                                let out_data_gep = self.builder.build_struct_gep(result_ty, result_alloca, 1, "out_data")
                                    .map_err(|e| format!("gep error: {}", e))?;
                                let out_void = self.builder.build_pointer_cast(out_i64, i8_ptr, "out_void")
                                    .map_err(|e| format!("bitcast error: {}", e))?;
                                self.builder.build_store(out_data_gep, out_void)
                                    .map_err(|e| format!("store error: {}", e))?;
                                Ok(result_alloca.into())
                            }
                            "reduce" if args.len() == 3 => {
                                // reduce(list, fn, init) - function reference version
                                let list_val = self.compile_expr(&args[0], vars)?;
                                let list_ptr = match list_val {
                                    BasicValueEnum::PointerValue(pv) => pv,
                                    _ => return Err("reduce: first arg must be a list".into()),
                                };
                                let fn_name = match &args[1] {
                                    Expr::Ident(n) => n.clone(),
                                    _ => return Err("reduce: second arg must be a function name".into()),
                                };
                                let init_val = self.compile_expr(&args[2], vars)?;
                                let fn_llvm = self.module.get_function(&fn_name)
                                    .ok_or_else(|| format!("reduce: function '{}' not compiled", fn_name))?;
                                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                                let i64_ty = self.context.i64_type();
                                let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                                    BasicTypeEnum::IntType(i64_ty),
                                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                                ], false));
                                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                                    .map_err(|e| format!("gep error: {}", e))?;
                                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                                    .map_err(|e| format!("load error: {}", e))?;
                                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                                    .map_err(|e| format!("gep error: {}", e))?;
                                let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")
                                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                                let data_ptr = self.builder.build_bit_cast(data_i8,
                                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                                    .map_err(|e| format!("bitcast error: {}", e))?
                                    .into_pointer_value();
                                let acc_alloca = self.builder.build_alloca(i64_ty, "acc")
                                    .map_err(|e| format!("alloca error: {}", e))?;
                                self.builder.build_store(acc_alloca, init_val)
                                    .map_err(|e| format!("store error: {}", e))?;
                                let function = self.current_function().unwrap();
                                let loop_bb = self.context.append_basic_block(function, "reduce_loop");
                                let body_bb = self.context.append_basic_block(function, "reduce_body");
                                let done_bb = self.context.append_basic_block(function, "reduce_done");
                                let idx_alloca = self.builder.build_alloca(i64_ty, "ri")
                                    .map_err(|e| format!("alloca error: {}", e))?;
                                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                                    .map_err(|e| format!("store error: {}", e))?;
                                self.builder.build_unconditional_branch(loop_bb)
                                    .map_err(|e| format!("branch error: {}", e))?;
                                self.builder.position_at_end(loop_bb);
                                let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
                                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                                let loop_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len.into_int_value(), "cmp")
                                    .map_err(|e| format!("cmp error: {}", e))?;
                                self.builder.build_conditional_branch(loop_cmp, body_bb, done_bb)
                                    .map_err(|e| format!("branch error: {}", e))?;
                                self.builder.position_at_end(body_bb);
                                let elem_ptr = unsafe {
                                    self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
                                }.map_err(|e| format!("gep error: {}", e))?;
                                let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
                                    .map_err(|e| format!("load error: {}", e))?;
                                let acc = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), acc_alloca, "acc")
                                    .map_err(|e| format!("load error: {}", e))?;
                                let fn_result = self.builder.build_call(fn_llvm, &[
                                    BasicMetadataValueEnum::IntValue(acc.into_int_value()),
                                    BasicMetadataValueEnum::IntValue(elem.into_int_value()),
                                ], "reduce_call")
                                    .map_err(|e| format!("call error: {}", e))?
                                    .try_as_basic_value().left()
                                    .ok_or("function returned void")?;
                                self.builder.build_store(acc_alloca, fn_result)
                                    .map_err(|e| format!("store error: {}", e))?;
                                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                                    .map_err(|e| format!("add error: {}", e))?;
                                self.builder.build_store(idx_alloca, next)
                                    .map_err(|e| format!("store error: {}", e))?;
                                self.builder.build_unconditional_branch(loop_bb)
                                    .map_err(|e| format!("branch error: {}", e))?;
                                self.builder.position_at_end(done_bb);
                                let result = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), acc_alloca, "result")
                                    .map_err(|e| format!("load error: {}", e))?;
                                Ok(result)
                            }
                            _ => self.compile_call(name, args, vars)
                        }
                    }
                    Expr::Field(obj, method_name) => {
                        // Method call: obj.method(args)
                        // Determine the type of the object to find the actor/trait name
                        let obj_type = self.infer_object_type(obj, vars);
                        let actor_method = format!("{}__{}__method", obj_type, method_name);
                        
                        // 1. Try actor method dispatch
                        if let Some(function) = self.module.get_function(&actor_method) {
                            let obj_val = self.compile_expr(obj, vars)?;
                            let mut compiled_args = Vec::new();
                            compiled_args.push(obj_val);
                            for arg in args {
                                compiled_args.push(self.compile_expr(arg, vars)?);
                            }
                            let metadata_args: Vec<_> = compiled_args.iter().map(|v| match v {
                                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                                BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                            }).collect();
                            let call = self.builder.build_call(function, &metadata_args, "method_call")
                                .map_err(|e| format!("method call error: {}", e))?;
                            return Ok(call.try_as_basic_value().left().unwrap_or(
                                self.context.i64_type().const_int(0, false).into()
                            ));
                        }
                        
                        // 2. Try trait method dispatch: type_impls[type_name][trait_name][method_name]
                        if let Some(trait_impls) = self.type_impls.get(&obj_type) {
                            for (trait_name, methods) in trait_impls {
                                if methods.iter().any(|m| m.name == *method_name) {
                                    let mangled = format!("{}__{}__{}", obj_type, trait_name, method_name);
                                    if let Some(function) = self.module.get_function(&mangled) {
                                        let obj_val = self.compile_expr(obj, vars)?;
                                        let mut compiled_args = Vec::new();
                                        compiled_args.push(obj_val);
                                        for arg in args {
                                            compiled_args.push(self.compile_expr(arg, vars)?);
                                        }
                                        let metadata_args: Vec<_> = compiled_args.iter().map(|v| match v {
                                            BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                                            BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                                            BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                                            BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                                            BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                                            BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                                        }).collect();
                                        let call = self.builder.build_call(function, &metadata_args, "trait_call")
                                            .map_err(|e| format!("trait method call error: {}", e))?;
                                        return Ok(call.try_as_basic_value().left().unwrap_or(
                                            self.context.i64_type().const_int(0, false).into()
                                        ));
                                    }
                                }
                            }
                        }
                        
                        // 3. True vtable indirect dispatch for dyn Trait objects
                        if obj_type.starts_with("dyn ") {
                            let trait_name = obj_type.strip_prefix("dyn ").unwrap_or("");
                            if !trait_name.is_empty() && !trait_name.contains(' ') {
                                // Find method index within the trait definition
                                let method_idx = self.trait_defs.get(trait_name)
                                    .and_then(|tdef| tdef.methods.iter().position(|m| m.name == *method_name));
                                if let Some(idx) = method_idx {
                                    // Get the vtable struct type (clone to avoid borrow conflict)
                                    let vtable_ty = self.vtable_types.get(trait_name)
                                        .map(|s| *s).ok_or("no vtable type for trait")?;
                                    // Fat pointer layout: { i8* data, i8* vtable }
                                    let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                                    let fat_ty = self.context.struct_type(&[
                                        BasicTypeEnum::PointerType(i8_ptr_ty),
                                        BasicTypeEnum::PointerType(i8_ptr_ty),
                                    ], false);
                                    // The obj_val is a fat pointer struct { data: i8*, vtable: i8* }
                                    let obj_val = self.compile_expr(obj, vars)?;
                                    let fat_ptr = match obj_val {
                                            BasicValueEnum::StructValue(_) => {
                                                // Alloca the struct value so we can GEP into it
                                                let alloca = self.builder.build_alloca(
                                                    BasicTypeEnum::StructType(fat_ty), "fat_tmp"
                                                ).map_err(|e| format!("alloca error: {}", e))?;
                                                self.builder.build_store(alloca, obj_val)
                                                    .map_err(|e| format!("store error: {}", e))?;
                                                alloca
                                            }
                                            BasicValueEnum::PointerValue(pv) => pv,
                                            _ => return Err("dyn Trait value must be a struct or pointer".into()),
                                        };
                                        // Extract vtable pointer (field 1)
                                        let vtable_gep = self.builder.build_struct_gep(
                                            BasicTypeEnum::StructType(fat_ty), fat_ptr, 1, "vtable_gep"
                                        ).map_err(|e| format!("gep error: {}", e))?;
                                        let vtable_ptr = self.builder.build_load(
                                            BasicTypeEnum::PointerType(i8_ptr_ty), vtable_gep, "vtable_ptr"
                                        ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                                        // GEP into vtable at method index
                                        let method_gep = self.builder.build_struct_gep(
                                            BasicTypeEnum::StructType(vtable_ty), vtable_ptr, idx as u32, "method_gep"
                                        ).map_err(|e| format!("gep error: {}", e))?;
                                        // Load function pointer from vtable slot
                                        let fn_ptr = self.builder.build_load(
                                            BasicTypeEnum::PointerType(i8_ptr_ty), method_gep, "fn_ptr"
                                        ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                                        // Extract data pointer (field 0) for passing as self arg
                                        let data_gep = self.builder.build_struct_gep(
                                            BasicTypeEnum::StructType(fat_ty), fat_ptr, 0, "data_gep"
                                        ).map_err(|e| format!("gep error: {}", e))?;
                                        let data_ptr = self.builder.build_load(
                                            BasicTypeEnum::PointerType(i8_ptr_ty), data_gep, "data_ptr"
                                        ).map_err(|e| format!("load error: {}", e))?;
                                        // Get the mangled function's type for the indirect call signature
                                        // Find any matching mangled function to extract fn type
                                        let fn_sig = (|| -> Option<(inkwell::values::AnyValueEnum<'ctx>, String)> {
                                            for (tn, timpls) in &self.type_impls {
                                                if let Some(methods) = timpls.get(trait_name) {
                                                    if methods.iter().any(|m| m.name == *method_name) {
                                                        let mangled = format!("{}__{}__{}", tn, trait_name, method_name);
                                                        if let Some(f) = self.module.get_function(&mangled) {
                                                            return Some((inkwell::values::AnyValueEnum::FunctionValue(f), mangled));
                                                        }
                                                    }
                                                }
                                            }
                                            None
                                        })();
                                        if let Some((fn_val, _)) = fn_sig {
                                            let fn_llvm = fn_val.into_function_value();
                                            let fn_type = fn_llvm.get_type();
                                            // Cast fn_ptr i8* to the right function pointer type
                                            let fn_ptr_cast = self.builder.build_pointer_cast(
                                                fn_ptr,
                                                fn_type.ptr_type(inkwell::AddressSpace::default()),
                                                "fn_cast"
                                            ).map_err(|e| format!("cast error: {}", e))?;
                                            // Compile additional args (start with data ptr as self)
                                            let mut compiled_args = Vec::new();
                                            compiled_args.push(data_ptr);
                                            for arg in args {
                                                compiled_args.push(self.compile_expr(arg, vars)?);
                                            }
                                            let metadata_args: Vec<_> = compiled_args.iter().map(|v| match v {
                                                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                                                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                                                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                                                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                                                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                                                BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                                            }).collect();
                                            let call = self.builder.build_indirect_call(
                                                fn_type, fn_ptr_cast, &metadata_args, "dyn_call"
                                            ).map_err(|e| format!("dyn indirect call error: {}", e))?;
                                            return Ok(call.try_as_basic_value().left().unwrap_or(
                                                self.context.i64_type().const_int(0, false).into()
                                            ));
                                        }
                                }
                            }
                            return Err(format!("[E0708] cannot dispatch method '{}' on {}", method_name, obj_type));
                        }

                        // 3b. Try impl Trait dispatch (same logic as dyn Trait)
                        if obj_type.starts_with("impl ") {
                            let trait_name = obj_type.strip_prefix("impl ").unwrap_or("");
                            if !trait_name.is_empty() && !trait_name.contains(' ') {
                                for (type_name, trait_impls) in &self.type_impls {
                                    if let Some(methods) = trait_impls.get(trait_name) {
                                        if methods.iter().any(|m| m.name == *method_name) {
                                            let mangled = format!("{}__{}__{}", type_name, trait_name, method_name);
                                            if let Some(function) = self.module.get_function(&mangled) {
                                                let obj_val = self.compile_expr(obj, vars)?;
                                                let mut compiled_args = Vec::new();
                                                compiled_args.push(obj_val);
                                                for arg in args {
                                                    compiled_args.push(self.compile_expr(arg, vars)?);
                                                }
                                                let metadata_args: Vec<_> = compiled_args.iter().map(|v| match v {
                                                    BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                                                    BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                                                    BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                                                    BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                                                    BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                                                    BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                                                }).collect();
                                                let call = self.builder.build_call(function, &metadata_args, "impl_trait_call")
                                                    .map_err(|e| format!("impl trait call error: {}", e))?;
                                                return Ok(call.try_as_basic_value().left().unwrap_or(
                                                    self.context.i64_type().const_int(0, false).into()
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                            return Err(format!("[E0708] cannot dispatch method '{}' on {}", method_name, obj_type));
                        }

                        // 4. Fallback: field access or error
                        if self.type_defs.contains_key(&obj_type) {
                            Err(format!("method '{}' not compiled for type '{}' (missing crate?)", method_name, obj_type))
                        } else {
                            Err(format!("cannot call method '{}' on unknown type '{}'", method_name, obj_type))
                        }
                    }
                    _ => Err("only direct function calls and method calls supported in codegen".to_string()),
                }
            }
            Expr::Turbofish(name, type_args, args) => {
                // Monomorphized call: func::<Type>(args)
                // Build type_map from explicit type args
                let func = self.find_func_def(name)?;
                if func.generics.len() != type_args.len() {
                    return Err(format!("[E0720] turbofish for '{}' expects {} type args, got {}", name, func.generics.len(), type_args.len()));
                }
                let mut turbo_map: HashMap<String, crate::ast::Type> = HashMap::new();
                for (gp, ta) in func.generics.iter().zip(type_args.iter()) {
                    turbo_map.insert(gp.name.clone(), ta.clone());
                }
                // Merge with current type_map (for nested generics)
                let mut merged_map = self.type_map.clone();
                merged_map.extend(turbo_map);
                let mangled = Self::mangle_name(name, &merged_map);
                // Compile the specialized version if not yet compiled
                if self.module.get_function(&mangled).is_none() {
                    self.compile_generic_func(&func, &merged_map)?;
                }
                // Call the mangled function
                self.compile_call_mangled(&mangled, args, vars)
            }
            Expr::Match(scrutinee, arms) => {
                let scrutinee_val = self.compile_expr(scrutinee, vars)?;
                let scrutinee_iv = if let BasicValueEnum::IntValue(iv) = scrutinee_val {
                    iv
                } else {
                    return Err("match scrutinee must be integer (enum tag)".into());
                };

                let function = self.current_function().unwrap();
                let merge_bb = self.context.append_basic_block(function, "matchcont");
                let mut else_bb = self.context.append_basic_block(function, "matchelse");

                // Branch from current block to the dispatch (matchelse)
                self.builder.build_unconditional_branch(else_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(else_bb);

                let mut incoming_vals = Vec::new();
                let mut incoming_bbs = Vec::new();

                // Build if-else chain for each arm
                for (i, arm) in arms.iter().enumerate() {
                    let arm_bb = self.context.append_basic_block(function, &format!("arm{}", i));

                    match &arm.pat {
                        Pattern::Wildcard | Pattern::Variable(_) => {
                            // Always matches - jump to arm body
                            self.builder.position_at_end(else_bb);
                            self.builder.build_unconditional_branch(arm_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            // Create a fresh else_bb so the after-loop code doesn't
                            // double-terminate the block we just wrote to.
                            else_bb = self.context.append_basic_block(function, &format!("wccont{}", i));
                        }
                        Pattern::Literal(lit) => {
                            self.builder.position_at_end(else_bb);
                            let lit_val = match lit {
                                Lit::Int(n) => self.context.i64_type().const_int(*n as u64, true),
                                Lit::Bool(b) => self.context.bool_type().const_int(*b as u64, false),
                                Lit::Unit => self.context.i64_type().const_int(0, false),
                                _ => return Err("[E0709] unsupported match literal type".into()),
                            };
                            let cmp = self.builder.build_int_compare(
                                inkwell::IntPredicate::EQ,
                                scrutinee_iv,
                                lit_val,
                                "cmp",
                            ).map_err(|e| format!("cmp error: {}", e))?;
                            // Always create an intermediate next block so the else chain
                            // never points directly at merge_bb.  This keeps the phi's
                            // predecessor set clean and avoids corrupting merge_bb.
                            let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                            self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            else_bb = next_bb;
                        }
                        Pattern::Constructor(name, _) => {
                            // Constructor pattern: compare tag (name hash as i64 for now)
                            self.builder.position_at_end(else_bb);
                            let tag_val = self.context.i64_type().const_int(
                                name.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64)),
                                false,
                            );
                            let cmp = self.builder.build_int_compare(
                                inkwell::IntPredicate::EQ,
                                scrutinee_iv,
                                tag_val,
                                "cmp",
                            ).map_err(|e| format!("cmp error: {}", e))?;
                            let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                            self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            else_bb = next_bb;
                        }
                        Pattern::Tuple(_inner_pats) => {
                            // Tuple pattern: match each element of the tuple struct
                            // Treat as always-matching for now (full element-wise comparison is complex)
                            // but bind inner variables by loading from the struct
                            self.builder.position_at_end(else_bb);
                            let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                            self.builder.build_unconditional_branch(arm_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            else_bb = next_bb;
                        }
                        Pattern::Array(_inner_pats) => {
                            // Array pattern: match each element of the list
                            // Treat as always-matching for now, bind inner variables
                            self.builder.position_at_end(else_bb);
                            let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                            self.builder.build_unconditional_branch(arm_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            else_bb = next_bb;
                        }
                        Pattern::Slice(_inner_pats, _rest) => {
                            // Slice pattern: match prefix elements, bind rest
                            self.builder.position_at_end(else_bb);
                            let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                            self.builder.build_unconditional_branch(arm_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            else_bb = next_bb;
                        }
                    }

                    // Arm body
                    self.builder.position_at_end(arm_bb);
                    let mut local_vars = vars.clone();
                    // Bind variables from pattern
                    match &arm.pat {
                        Pattern::Variable(name) => {
                            let alloca = self.builder.build_alloca(
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                name,
                            ).map_err(|e| format!("alloca error: {}", e))?;
                            self.builder.build_store(alloca, scrutinee_iv)
                                .map_err(|e| format!("store error: {}", e))?;
                            local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                        }
                        Pattern::Constructor(_, inner_patterns) => {
                            // For constructor patterns, bind inner variables
                            // For now, assume single inner variable
                            for inner_pat in inner_patterns {
                                if let Pattern::Variable(name) = inner_pat {
                                    let alloca = self.builder.build_alloca(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        name,
                                    ).map_err(|e| format!("alloca error: {}", e))?;
                                    self.builder.build_store(alloca, scrutinee_iv)
                                        .map_err(|e| format!("store error: {}", e))?;
                                    local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                                }
                            }
                        }
                        Pattern::Tuple(inner_pats) => {
                            // For tuple patterns, bind inner variables by loading from struct
                            let scrutinee_ptr = match scrutinee_val {
                                BasicValueEnum::PointerValue(pv) => pv,
                                _ => continue,
                            };
                            // Determine tuple element types from the struct
                            let _elem_count = inner_pats.len();
                            for (j, inner_pat) in inner_pats.iter().enumerate() {
                                if let Pattern::Variable(name) = inner_pat {
                                    let gep = self.builder.build_struct_gep(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        scrutinee_ptr,
                                        j as u32,
                                        &format!("tuple_{}", j),
                                    ).map_err(|e| format!("gep error: {}", e))?;
                                    let val = self.builder.build_load(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        gep,
                                        &format!("tup_{}", j),
                                    ).map_err(|e| format!("load error: {}", e))?;
                                    let alloca = self.builder.build_alloca(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        name,
                                    ).map_err(|e| format!("alloca error: {}", e))?;
                                    self.builder.build_store(alloca, val)
                                        .map_err(|e| format!("store error: {}", e))?;
                                    local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                                }
                            }
                        }
                        Pattern::Array(inner_pats) => {
                            // For array patterns, bind inner variables by loading from list data
                            let scrutinee_ptr = match scrutinee_val {
                                BasicValueEnum::PointerValue(pv) => pv,
                                _ => continue,
                            };
                            // Load data pointer from list struct
                            let list_ty = self.context.struct_type(&[
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                            ], false);
                            let data_gep = self.builder.build_struct_gep(list_ty, scrutinee_ptr, 1, "list_data")
                                .map_err(|e| format!("gep error: {}", e))?;
                            let data_i8 = self.builder.build_load(
                                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                                data_gep, "data").map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                            let i64_ty = self.context.i64_type();
                            let data_ptr = self.builder.build_bit_cast(data_i8,
                                i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                                .map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();
                            for (j, inner_pat) in inner_pats.iter().enumerate() {
                                if let Pattern::Variable(name) = inner_pat {
                                    let idx = i64_ty.const_int(j as u64, false);
                                    let elem_ptr = unsafe {
                                        self.builder.build_gep(i64_ty, data_ptr, &[idx], &format!("arr_{}", j))
                                    }.map_err(|e| format!("gep error: {}", e))?;
                                    let val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, &format!("arrv_{}", j))
                                        .map_err(|e| format!("load error: {}", e))?;
                                    let alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), name)
                                        .map_err(|e| format!("alloca error: {}", e))?;
                                    self.builder.build_store(alloca, val)
                                        .map_err(|e| format!("store error: {}", e))?;
                                    local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(i64_ty)));
                                }
                            }
                        }
                        Pattern::Slice(inner_pats, rest) => {
                            // For slice patterns, bind prefix variables and rest as list
                            let scrutinee_ptr = match scrutinee_val {
                                BasicValueEnum::PointerValue(pv) => pv,
                                _ => continue,
                            };
                            let list_ty = self.context.struct_type(&[
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                            ], false);
                            let data_gep = self.builder.build_struct_gep(list_ty, scrutinee_ptr, 1, "list_data")
                                .map_err(|e| format!("gep error: {}", e))?;
                            let data_i8 = self.builder.build_load(
                                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                                data_gep, "data").map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                            let i64_ty = self.context.i64_type();
                            let data_ptr = self.builder.build_bit_cast(data_i8,
                                i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                                .map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();
                            // Bind prefix elements
                            for (j, inner_pat) in inner_pats.iter().enumerate() {
                                if let Pattern::Variable(name) = inner_pat {
                                    let idx = i64_ty.const_int(j as u64, false);
                                    let elem_ptr = unsafe {
                                        self.builder.build_gep(i64_ty, data_ptr, &[idx], &format!("slc_{}", j))
                                    }.map_err(|e| format!("gep error: {}", e))?;
                                    let val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, &format!("slcv_{}", j))
                                        .map_err(|e| format!("load error: {}", e))?;
                                    let alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), name)
                                        .map_err(|e| format!("alloca error: {}", e))?;
                                    self.builder.build_store(alloca, val)
                                        .map_err(|e| format!("store error: {}", e))?;
                                    local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(i64_ty)));
                                }
                            }
                            // Bind rest as remaining list (simplified: bind as empty list)
                            if let Some(rest_pat) = rest.as_ref() {
                                if let Pattern::Variable(name) = rest_pat.as_ref() {
                                    let i64_ty = self.context.i64_type();
                                    let empty_list: BasicValueEnum = i64_ty.const_int(0, false).into();
                                    let alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), name)
                                        .map_err(|e| format!("alloca error: {}", e))?;
                                    self.builder.build_store(alloca, empty_list)
                                        .map_err(|e| format!("store error: {}", e))?;
                                    local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(i64_ty)));
                                }
                            }
                        }
                        Pattern::Wildcard | Pattern::Literal(_) => {
                            // Wildcard and literal patterns: no variable binding needed
                        }
                    }
                    let arm_val = self.compile_expr(&arm.body, &local_vars)?;
                    incoming_vals.push(arm_val);
                    incoming_bbs.push(arm_bb);
                    self.builder.build_unconditional_branch(merge_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                }

                // Unreachable else block (should not be reached if match is exhaustive).
                // else_bb is a fresh next_N block (never merge_bb) thanks to the
                // unconditional intermediate-block creation above.
                self.builder.position_at_end(else_bb);
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                // Merge block - use phi to select the right value
                self.builder.position_at_end(merge_bb);
                if incoming_vals.is_empty() {
                    return Err("empty match expression".into());
                }
                let ty = incoming_vals[0].get_type();
                let phi = self.builder.build_phi(ty, "match.result")
                    .map_err(|e| format!("phi error: {}", e))?;
                let mut phi_incoming: Vec<_> = incoming_vals.iter().zip(incoming_bbs.iter())
                    .map(|(v, bb)| (v as &dyn inkwell::values::BasicValue, *bb))
                    .collect();
                // Add the unreachable else block with a dummy value so every
                // predecessor of merge_bb has a phi entry.
                let dummy_val = self.context.i64_type().const_int(0, false);
                phi_incoming.push((&dummy_val as &dyn inkwell::values::BasicValue, else_bb));
                phi.add_incoming(&phi_incoming);
                Ok(phi.as_basic_value())
            }
            Expr::Record { ty, fields } => {
                // Create a record value
                let type_name = ty.as_deref().unwrap_or("unknown");
                let llvm_ty = *self.type_llvm.get(type_name)
                    .ok_or_else(|| format!("unknown type '{}'", type_name))?;
                if let BasicTypeEnum::StructType(sty) = llvm_ty {
                    let alloca = self.builder.build_alloca(sty, type_name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    // Store field values
                    for (i, field) in fields.iter().enumerate() {
                        let val = self.compile_expr(&field.value, vars)?;
                        let gep = self.builder.build_struct_gep(sty, alloca, i as u32, &field.name)
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(gep, val)
                            .map_err(|e| format!("store error: {}", e))?;
                    }
                    Ok(alloca.into())
                } else {
                    Err(format!("type '{}' is not a struct", type_name))
                }
            }
            Expr::Field(obj, field_name) => {
                // Field access: obj.field
                let obj_val = self.compile_expr(obj, vars)?;
                let obj_type = self.infer_object_type(obj, vars);
                let field_ptr = match obj_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    BasicValueEnum::StructValue(sv) => {
                        if let Some(BasicTypeEnum::StructType(sty)) = self.type_llvm.get(&obj_type) {
                            let alloca = self.builder.build_alloca(*sty, "tmp")
                                .map_err(|e| format!("alloca error: {}", e))?;
                            self.builder.build_store(alloca, sv)
                                .map_err(|e| format!("store error: {}", e))?;
                            alloca
                        } else {
                            return Err(format!("[E0707] cannot access field on type '{}'", obj_type));
                        }
                    }
                    _ => return Err(format!("field access requires struct/actor type, got {:?}", obj_val.get_type())),
                };
                let sty = match self.type_llvm.get(&obj_type) {
                    Some(BasicTypeEnum::StructType(s)) => *s,
                    _ => return Err(format!("type '{}' is not a struct", obj_type)),
                };
                if let Some(td) = self.type_defs.get(&obj_type) {
                    if let TypeDefKind::Record(fields) = &td.kind {
                        if let Some(idx) = fields.iter().position(|f| f.name == *field_name) {
                            let gep = self.builder.build_struct_gep(sty, field_ptr, idx as u32, field_name)
                                .map_err(|e| format!("gep error: {}", e))?;
                            let field_ty = types::mimi_type_to_llvm(self.context, &fields[idx].ty)
                                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                            return self.builder.build_load(field_ty, gep, field_name)
                                .map_err(|e| format!("load error: {}", e));
                        }
                    }
                }
                // Fallback: numeric field index
                if let Ok(idx) = field_name.parse::<u32>() {
                    let gep = self.builder.build_struct_gep(sty, field_ptr, idx, field_name)
                        .map_err(|e| format!("gep error: {}", e))?;
                    return self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), gep, field_name)
                        .map_err(|e| format!("load error: {}", e));
                }
                Err(format!("field '{}' not found on type '{}'", field_name, obj_type))
            }
            Expr::List(elems) => {
                // Create a list struct: { i64 len, i64* data }
                let count = elems.len() as u64;
                let len_val = self.context.i64_type().const_int(count, false);
                // Allocate array
                let sizeof_i64 = self.context.i64_type().const_int(8, false);
                let alloc_size = self.builder.build_int_mul(len_val, sizeof_i64, "alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let data_ptr = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                    self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
                    "data_ptr_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                // Store each element (universal i64 representation)
                for (i, elem) in elems.iter().enumerate() {
                    let val = self.compile_expr(elem, vars)?;
                    let iv = match val {
                        BasicValueEnum::IntValue(iv) => iv,
                        BasicValueEnum::FloatValue(fv) => {
                            self.builder.build_bit_cast(fv, self.context.i64_type(), "f64_to_i64")
                                .map_err(|e| format!("bitcast error: {}", e))?
                                .into_int_value()
                        }
                        BasicValueEnum::PointerValue(pv) => {
                            self.builder.build_ptr_to_int(pv, self.context.i64_type(), "ptr_to_i64")
                                .map_err(|e| format!("ptr_to_int error: {}", e))?
                        }
                        _ => return Err("list elements must be scalar types (int, float, pointer) for now".into()),
                    };
                    let idx = self.context.i64_type().const_int(i as u64, false);
                    let elem_ptr = unsafe {
                        self.builder.build_gep(self.context.i64_type(), data_ptr_i64, &[idx], "elem")
                    }.map_err(|e| format!("gep error: {}", e))?;
                    self.builder.build_store(elem_ptr, iv)
                        .map_err(|e| format!("store error: {}", e))?;
                }
                // Create list struct
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let list_alloca = self.builder.build_alloca(list_ty, "list")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(list_ty, list_alloca, 0, "list_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, len_val)
                    .map_err(|e| format!("store error: {}", e))?;
                let data_gep = self.builder.build_struct_gep(list_ty, list_alloca, 1, "list_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_void_ptr = self.builder.build_bit_cast(data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "data_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(data_gep, data_void_ptr)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(list_alloca.into())
            }
            Expr::Index(obj, idx_expr) => {
                // list[i] or arr[i] - load from array/list
                let obj_val = self.compile_expr(obj, vars)?;
                let idx_val = self.compile_expr(idx_expr, vars)?;
                match obj_val {
                    BasicValueEnum::PointerValue(pv) => {
                        let idx_iv = match idx_val {
                            BasicValueEnum::IntValue(iv) => iv,
                            _ => return Err("[E0712] index must be i64".into()),
                        };
                        // Try list struct first: { i64 len, i8* data }
                        let list_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false);
                        // Check if this looks like a list struct by trying to GEP field 0 (len)
                        if let Ok(_len_gep) = self.builder.build_struct_gep(list_ty, pv, 0, "list.len_check") {
                            // It's a list struct - load data pointer and index into it
                            let data_gep = self.builder.build_struct_gep(list_ty, pv, 1, "list.data")
                                .map_err(|e| format!("gep error: {}", e))?;
                            let data_ptr = self.builder.build_load(
                                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                                data_gep, "data")
                                .map_err(|e| format!("load error: {}", e))?
                                .into_pointer_value();
                            let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                                self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
                                "data_i64")
                                .map_err(|e| format!("bitcast error: {}", e))?
                                .into_pointer_value();
                            let elem_ptr = unsafe {
                                self.builder.build_gep(self.context.i64_type(), data_ptr_i64, &[idx_iv], "elem")
                            }.map_err(|e| format!("gep error: {}", e))?;
                            return self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, "elem_val")
                                .map_err(|e| format!("load error: {}", e));
                        }
                        // Fallback: treat as raw pointer to i64 array
                        let elem_ptr = unsafe {
                            self.builder.build_gep(self.context.i64_type(), pv, &[idx_iv], "elem")
                        }.map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, "elem_val")
                            .map_err(|e| format!("load error: {}", e))
                    }
                    BasicValueEnum::ArrayValue(_av) => {
                        // Direct LLVM array value: extract element by index
                        let idx = match idx_val {
                            BasicValueEnum::IntValue(iv) => {
                                // Convert runtime i64 index to constant u32 for extractvalue
                                iv.get_zero_extended_constant()
                                    .ok_or_else(|| "[E0712] array index must be a compile-time constant".to_string())? as u32
                            }
                            _ => return Err("[E0712] index must be i64".into()),
                        };
                        let elem = self.builder.build_extract_value(obj_val.into_array_value(), idx, "arr_elem")
                            .map_err(|e| format!("extractvalue error: {}", e))?;
                        Ok(elem)
                    }
                    _ => Err("index requires a list/array pointer".into()),
                }
            }
            Expr::Spawn(expr) => {
                // Spawn: create a thread to execute the expression
                let parent_fn = self.current_function().unwrap();
                let parent_name = parent_fn.get_name().to_str().unwrap_or("unknown").to_string();
                let wrapper_name = format!("{}{}__spawn_wrapper", parent_name, self.spawn_counter).to_string();
                self.spawn_counter += 1;
                
                // Create wrapper function: i8* wrapper(i8*)
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let wrapper_fn_type = i8_ptr.fn_type(
                    &[BasicMetadataTypeEnum::PointerType(i8_ptr)], false
                );
                let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_fn_type, None);
                let wrapper_entry = self.context.append_basic_block(wrapper_fn, "entry");
                
                // Save current builder position and compile the spawn body into the wrapper
                let saved_block = self.builder.get_insert_block();
                self.builder.position_at_end(wrapper_entry);
                
                // Compile the spawn expression (the result is the return value)
                let result = self.compile_expr(expr, vars)?;
                
                // Allocate heap space for the return value using malloc (not alloca — 
                // heap memory survives the wrapper function's return)
                let i64_ty = self.context.i64_type();
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let result_llvm_ty_for_size = result.get_type();
                let byte_size_val = result_llvm_ty_for_size.size_of()
                    .and_then(|v: inkwell::values::IntValue<'ctx>| v.get_zero_extended_constant())
                    .unwrap_or(8) as u64;
                let byte_size = i64_ty.const_int(byte_size_val, false);
                let result_storage = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(byte_size),
                ], "malloc_result")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value()
                    .left()
                    .ok_or("malloc returned void")?;
                let result_storage_ptr = if let BasicValueEnum::PointerValue(pv) = result_storage {
                    pv
                } else {
                    return Err("malloc should return a pointer".into());
                };
                // Store the result
                            // Cast result_storage (i8*) to the correct type pointer for storing
                let result_llvm_ty = result.get_type();
                let result_ptr_ty = match result_llvm_ty {
                    BasicTypeEnum::IntType(t) => t.ptr_type(inkwell::AddressSpace::default()),
                    BasicTypeEnum::FloatType(t) => t.ptr_type(inkwell::AddressSpace::default()),
                    BasicTypeEnum::PointerType(t) => t.ptr_type(inkwell::AddressSpace::default()),
                    BasicTypeEnum::StructType(t) => t.ptr_type(inkwell::AddressSpace::default()),
                    BasicTypeEnum::ArrayType(t) => t.ptr_type(inkwell::AddressSpace::default()),
                    BasicTypeEnum::VectorType(t) => t.ptr_type(inkwell::AddressSpace::default()),
                };
                let result_typed_ptr = self.builder.build_pointer_cast(
                    result_storage_ptr,
                    result_ptr_ty,
                    "result_typed"
                ).map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(result_typed_ptr, result)
                    .map_err(|e| format!("store error: {}", e))?;
                // Return the i8* pointer
                self.builder.build_return(Some(&result_storage))
                    .map_err(|e| format!("return error: {}", e))?;
                
                // Restore builder position to original block
                if let Some(bb) = saved_block {
                    self.builder.position_at_end(bb);
                }
                
                let wrapper_fn_ptr = self.builder.build_pointer_cast(
                    wrapper_fn.as_global_value().as_pointer_value(),
                    i8_ptr,
                    "wrapper_i8"
                ).map_err(|e| format!("bitcast error: {}", e))?;

                if self.in_parasteps {
                    // Parasteps: submit to thread pool (avoids creating N OS threads)
                    let mimi_pool_submit_fn = self.module.get_function("mimi_pool_submit")
                        .ok_or("mimi_pool_submit not declared")?;
                    self.builder.build_call(mimi_pool_submit_fn, &[
                        BasicMetadataValueEnum::PointerValue(wrapper_fn_ptr),
                        BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
                    ], "pool_submit_call")
                        .map_err(|e| format!("pool_submit error: {}", e))?;
                    // Return 0 as placeholder (parasteps joins all at block end)
                    let placeholder = i64_ty.const_int(0, false);
                    Ok(BasicValueEnum::IntValue(placeholder))
                } else {
                    // Non-parasteps (single spawn+await): use raw pthread_create
                    let thread_alloca = self.builder.build_alloca(i64_ty, "thread")
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(thread_alloca, i64_ty.const_int(0, false))
                        .map_err(|e| format!("store error: {}", e))?;

                    let pthread_create_fn = self.module.get_function("pthread_create")
                        .ok_or("pthread_create not declared")?;
                    self.builder.build_call(pthread_create_fn, &[
                        BasicMetadataValueEnum::PointerValue(thread_alloca),
                        BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
                        BasicMetadataValueEnum::PointerValue(wrapper_fn_ptr),
                        BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
                    ], "pthread_create_call")
                        .map_err(|e| format!("pthread_create error: {}", e))?;

                    let thread_id_val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), thread_alloca, "thread_id")
                        .map_err(|e| format!("load error: {}", e))?;
                    Ok(thread_id_val)
                }
            }
            Expr::Await(expr) => {
                // Await: join the thread and get the result
                let thread_val = self.compile_expr(expr, vars)?;
                let thread_id = match thread_val {
                    BasicValueEnum::IntValue(iv) => iv,
                    BasicValueEnum::PointerValue(pv) => {
                        self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), pv, "thread")
                            .map_err(|e| format!("load error: {}", e))?.into_int_value()
                    }
                    _ => return Err("await requires a thread (i64) value".into()),
                };
                
                // Allocate space to receive the wrapper's return pointer (void**)
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let retval_storage = self.builder.build_alloca(i8_ptr, "retval_ptr")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(retval_storage, i8_ptr.const_null())
                    .map_err(|e| format!("store error: {}", e))?;
                
                // Remove from parasteps tracking (already awaited, avoid double-join at block end)
                self.parasteps_thread_ids.retain(|&id| id != thread_id);
                
                let pthread_join_fn = self.module.get_function("pthread_join")
                    .ok_or("pthread_join not declared")?;
                self.builder.build_call(pthread_join_fn, &[
                    BasicMetadataValueEnum::IntValue(thread_id),
                    BasicMetadataValueEnum::PointerValue(retval_storage),
                ], "pthread_join_call")
                    .map_err(|e| format!("pthread_join error: {}", e))?;
                
                // Load the returned pointer from the storage (it's the wrapper's malloc'd result)
                let result_i8_ptr = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr),
                    retval_storage,
                    "result_ptr"
                ).map_err(|e| format!("load error: {}", e))?;
                let result_ptr = if let BasicValueEnum::PointerValue(pv) = result_i8_ptr {
                    pv
                } else {
                    return Err("expected pointer from pthread_join".into());
                };
                
                // Cast from i8* to i64* and load the result value
                let i64_ty = self.context.i64_type();
                let result_typed = self.builder.build_pointer_cast(
                    result_ptr,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()),
                    "result_i64_ptr"
                ).map_err(|e| format!("bitcast error: {}", e))?;
                let result_val = self.builder.build_load(
                    BasicTypeEnum::IntType(i64_ty),
                    result_typed,
                    "spawn_result_val"
                ).map_err(|e| format!("load error: {}", e))?;
                
                // Free the malloc'd memory
                let free_fn = self.module.get_function("free")
                    .ok_or_else(|| "free not declared".to_string())?;
                self.builder.build_call(free_fn, &[
                    BasicMetadataValueEnum::PointerValue(result_ptr),
                ], "free_call")
                    .map_err(|e| format!("free error: {}", e))?;
                
                Ok(result_val)
            }
            Expr::Try(inner) => {
                // ? operator: compile inner expr as Result<T,E>{i1, T},
                // check discriminant, extract T on Ok, exit on Err
                let result_val = self.compile_expr(inner, vars)?;

                // The result should be a struct {i1, T}. Load it if it's a pointer.
                // Extract discriminant (field 0) via GEP+load if pointer, or extract_value if struct
                let i1_ty = self.context.bool_type();
                let i64_ty = self.context.i64_type();
                let function = self.current_function().unwrap();
                let ok_bb = self.context.append_basic_block(function, "try_ok");
                let err_bb = self.context.append_basic_block(function, "try_err");

                match result_val {
                    BasicValueEnum::PointerValue(pv) => {
                        // Access struct fields via GEP
                        let result_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(i1_ty),
                            BasicTypeEnum::IntType(i64_ty),
                        ], false);
                        let gep0 = self.builder.build_struct_gep(
                            BasicTypeEnum::StructType(result_ty), pv, 0, "disc_gep"
                        ).map_err(|e| format!("gep error: {}", e))?;
                        let disc = self.builder.build_load(
                            BasicTypeEnum::IntType(i1_ty), gep0, "discriminant"
                        ).map_err(|e| format!("load error: {}", e))?.into_int_value();
                        let gep1 = self.builder.build_struct_gep(
                            BasicTypeEnum::StructType(result_ty), pv, 1, "pay_gep"
                        ).map_err(|e| format!("gep error: {}", e))?;
                        let payload = self.builder.build_load(
                            BasicTypeEnum::IntType(i64_ty), gep1, "payload"
                        ).map_err(|e| format!("load error: {}", e))?;

                        self.builder.build_conditional_branch(disc, ok_bb, err_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        // Err path: run compensations, print error message, exit(1)
                        self.builder.position_at_end(err_bb);
                        let mut comp_vars = vars.clone();
                        self.compile_compensations(&mut comp_vars)?;
                        let try_exit_fn = self.module.get_function("mimi_try_exit")
                            .ok_or("mimi_try_exit not declared")?;
                        self.builder.build_call(try_exit_fn, &[
                            BasicMetadataValueEnum::IntValue(payload.into_int_value()),
                        ], "try_exit")
                            .map_err(|e| format!("try_exit error: {}", e))?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.build_unconditional_branch(unreachable)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(ok_bb);
                        Ok(payload)
                    }
                    BasicValueEnum::StructValue(sv) => {
                        // Extract via extract_value for struct values
                        let disc = self.builder.build_extract_value(sv, 0, "discriminant")
                            .map_err(|e| format!("extract_value error: {}", e))?;
                        let payload = self.builder.build_extract_value(sv, 1, "payload")
                            .map_err(|e| format!("extract_value error: {}", e))?;

                        self.builder.build_conditional_branch(disc.into_int_value(), ok_bb, err_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(err_bb);
                        let mut comp_vars = vars.clone();
                        self.compile_compensations(&mut comp_vars)?;
                        let try_exit_fn = self.module.get_function("mimi_try_exit")
                            .ok_or("mimi_try_exit not declared")?;
                        self.builder.build_call(try_exit_fn, &[
                            BasicMetadataValueEnum::IntValue(payload.into_int_value()),
                        ], "try_exit")
                            .map_err(|e| format!("try_exit error: {}", e))?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.build_unconditional_branch(unreachable)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(ok_bb);
                        Ok(payload)
                    }
                    _ => Err("? operator requires a Result/Option type (struct pointer or value)".into()),
                }
            }
            Expr::TypeOf(inner) => {
                // type_name(x): resolve type name at compile time
                let type_str = match inner.as_ref() {
                    Expr::Ident(var_name) => self.var_type_names.get(var_name)
                        .cloned().unwrap_or_else(|| "unknown".to_string()),
                    _ => "unknown".to_string(),
                };
                // Build string literal struct { i8*, i64 }
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                let global = self.builder.build_global_string_ptr(&type_str, "typename")
                    .map_err(|e| format!("global string error: {}", e))?;
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let alloca = self.builder.build_alloca(string_ty, "type_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, alloca, 0, "ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, global.as_pointer_value())
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, alloca, 1, "len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, i64_ty.const_int(type_str.len() as u64, false))
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(alloca.into())
            }
            Expr::TypeInfo(ty) => {
                // type_info(T): compile-time reflection on type (future)
                let _ = ty;
                Err("type_info is not available in codegen mode (compile-time reflection only)".into())
            }
            Expr::Old(inner) => {
                // old(expr): snapshot value at function entry
                // For codegen, old() is transparent — just compile the inner expression
                self.compile_expr(inner, vars)
            }
            Expr::Tuple(elems) => {
                let mut field_vals = Vec::new();
                for e in elems {
                    field_vals.push(self.compile_expr(e, vars)?);
                }
                let field_tys: Vec<BasicTypeEnum<'ctx>> = field_vals.iter().map(|v| v.get_type()).collect();
                let struct_ty = self.context.struct_type(&field_tys, false);
                let alloca = self.builder.build_alloca(struct_ty, "tuple")
                    .map_err(|e| format!("alloca error: {}", e))?;
                for (i, val) in field_vals.iter().enumerate() {
                    let gep = self.builder.build_struct_gep(struct_ty, alloca, i as u32, &format!("tuple_{}", i))
                        .map_err(|e| format!("gep error: {}", e))?;
                    self.builder.build_store(gep, *val)
                        .map_err(|e| format!("store error: {}", e))?;
                }
                Ok(alloca.into())
            }
            Expr::If { cond, then_, else_ } => {
                let cond_val = self.compile_expr(cond, vars)?;
                let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                    iv
                } else {
                    return Err("if expression condition must be boolean".into());
                };
                let function = self.current_function().unwrap();
                let then_bb = self.context.append_basic_block(function, "ifexpr_then");
                let else_bb = self.context.append_basic_block(function, "ifexpr_else");
                let merge_bb = self.context.append_basic_block(function, "ifexpr_merge");
                self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Then branch
                self.builder.position_at_end(then_bb);
                let mut then_vars = vars.clone();
                let then_val = self.compile_block_last_val(then_, &mut then_vars)?;
                if !self.block_has_terminator() {
                    self.builder.build_unconditional_branch(merge_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                }
                let then_bb_end = self.builder.get_insert_block().unwrap();
                // Else branch
                self.builder.position_at_end(else_bb);
                let else_val = if let Some(eb) = else_ {
                    let mut else_vars = vars.clone();
                    let v = self.compile_block_last_val(eb, &mut else_vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    Some(v)
                } else {
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    None
                };
                let else_bb_end = self.builder.get_insert_block().unwrap();
                // Merge with phi
                self.builder.position_at_end(merge_bb);
                let ty = then_val.get_type();
                let phi = self.builder.build_phi(ty, "ifexpr_result")
                    .map_err(|e| format!("phi error: {}", e))?;
                let else_v = else_val.unwrap_or(self.context.i64_type().const_int(0, false).into());
                phi.add_incoming(&[
                    (&then_val as &dyn inkwell::values::BasicValue, then_bb_end),
                    (&else_v as &dyn inkwell::values::BasicValue, else_bb_end),
                ]);
                Ok(phi.as_basic_value())
            }
            Expr::Range { start, end } => {
                let start_val = self.compile_expr(start, vars)?;
                let end_val = self.compile_expr(end, vars)?;
                let start_iv = match start_val {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("[E0712] range start must be i64".into()),
                };
                let end_iv = match end_val {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("[E0712] range end must be i64".into()),
                };
                // Create a range struct { start: i64, end: i64 }
                let range_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let alloca = self.builder.build_alloca(range_ty, "range")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let start_gep = self.builder.build_struct_gep(range_ty, alloca, 0, "range_start")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(start_gep, start_iv)
                    .map_err(|e| format!("store error: {}", e))?;
                let end_gep = self.builder.build_struct_gep(range_ty, alloca, 1, "range_end")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(end_gep, end_iv)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(alloca.into())
            }
            Expr::SliceExpr { target, start, end } => {
                // Slice: arr[start..end] — compile target, compute slice offset and length
                let target_val = self.compile_expr(target, vars)?;
                let target_ptr = match target_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Err("slice target must be a list/array pointer".into()),
                };
                // Get list length from struct field 0
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep = self.builder.build_struct_gep(list_ty, target_ptr, 0, "slice_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), len_gep, "len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_ty, target_ptr, 1, "slice_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_ptr = self.builder.build_load(
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                    data_gep, "data").map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                // Compute start index (default 0)
                let start_idx = match start {
                    Some(e) => self.compile_expr(e, vars)?.into_int_value(),
                    None => self.context.i64_type().const_int(0, false),
                };
                // Compute end index (default: list length)
                let end_idx = match end {
                    Some(e) => self.compile_expr(e, vars)?.into_int_value(),
                    None => list_len,
                };
                // Compute new length = end - start
                let new_len = self.builder.build_int_sub(end_idx, start_idx, "slice_len")
                    .map_err(|e| format!("sub error: {}", e))?;
                // Compute new data pointer: data + start * sizeof(i64)
                let i64_ty = self.context.i64_type();
                let elem_size = i64_ty.const_int(8, false);
                let byte_offset = self.builder.build_int_mul(start_idx, elem_size, "slice_offset")
                    .map_err(|e| format!("mul error: {}", e))?;
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let data_i8 = self.builder.build_pointer_cast(data_ptr, i8_ptr, "data_as_i8")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                let new_data_i8 = unsafe {
                    self.builder.build_gep(self.context.i8_type(), data_i8, &[byte_offset], "new_data")
                }.map_err(|e| format!("gep error: {}", e))?;
                let new_data_ptr = self.builder.build_pointer_cast(new_data_i8,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "new_data_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                // Build new list struct { new_len, new_data_ptr }
                let result_alloca = self.builder.build_alloca(list_ty, "slice_result")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let rlen_gep = self.builder.build_struct_gep(list_ty, result_alloca, 0, "rlen")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(rlen_gep, new_len)
                    .map_err(|e| format!("store error: {}", e))?;
                let rdata_gep = self.builder.build_struct_gep(list_ty, result_alloca, 1, "rdata")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(rdata_gep, new_data_ptr)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(result_alloca.into())
            }
            Expr::Lambda { params, ret, body } => {
                // Generate anonymous function with closure capture support
                let param_names: std::collections::HashSet<String> =
                    params.iter().map(|p| p.name.clone()).collect();
                let mut free_vars = HashMap::new();
                self.collect_free_vars(body, &param_names, vars, &mut free_vars);
                // Step 2: Build function type with captured variables as extra parameters
                let ret_type = match ret {
                    Some(ty) => types::mimi_type_to_llvm(self.context, ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
                    None => BasicTypeEnum::IntType(self.context.i64_type()),
                };
                let mut param_types = Vec::new();
                for p in params {
                    let ty = types::mimi_type_to_llvm(self.context, &p.ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                    param_types.push(ty);
                }
                // Add captured variables as extra parameters (all as i64 for simplicity)
                for _name in free_vars.keys() {
                    param_types.push(BasicTypeEnum::IntType(self.context.i64_type()));
                }
                let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();
                let fn_type = match ret_type {
                    BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
                    BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
                    BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
                    BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
                    BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
                    _ => self.context.i64_type().fn_type(&metadata_params, false),
                };
                let lambda_name = format!("__lambda_{}_{}", self.spawn_counter, body.len());
                self.spawn_counter += 1;
                let lambda_fn = self.module.add_function(&lambda_name, fn_type, None);
                let entry = self.context.append_basic_block(lambda_fn, "entry");
                let saved_block = self.builder.get_insert_block();
                self.builder.position_at_end(entry);
                let mut lambda_vars = vars.clone();
                let mut param_idx = 0;
                // Bind regular parameters
                for p in params.iter() {
                    let ty = types::mimi_type_to_llvm(self.context, &p.ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                    let alloca = self.builder.build_alloca(ty, &p.name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, lambda_fn.get_nth_param(param_idx as u32).unwrap())
                        .map_err(|e| format!("store error: {}", e))?;
                    lambda_vars.insert(p.name.clone(), (alloca, ty));
                    param_idx += 1;
                }
                // Bind captured variables
                for name in free_vars.keys() {
                    let alloca = self.builder.build_alloca(
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        &format!("cap_{}", name),
                    ).map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, lambda_fn.get_nth_param(param_idx as u32).unwrap())
                        .map_err(|e| format!("store error: {}", e))?;
                    lambda_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                    param_idx += 1;
                }
                // Compile body
                let mut last_val = self.context.i64_type().const_int(0, false).into();
                for stmt in body {
                    match stmt {
                        Stmt::Expr(e) => { last_val = self.compile_expr(e, &lambda_vars)?; }
                        Stmt::Return(Some(e)) => {
                            let v = self.compile_expr(e, &lambda_vars)?;
                            self.builder.build_return(Some(&v)).map_err(|e| format!("return error: {}", e))?;
                            break;
                        }
                        Stmt::Return(None) => {
                            self.builder.build_return(None).map_err(|e| format!("return error: {}", e))?;
                            break;
                        }
                        Stmt::Let { pat, init: Some(init), .. } => {
                            let val = self.compile_expr(init, &lambda_vars)?;
                            let name = match pat { Pattern::Variable(n) => n.clone(), _ => continue };
                            let llvm_ty = val.get_type();
                            let alloca = self.builder.build_alloca(llvm_ty, &name).map_err(|e| format!("alloca error: {}", e))?;
                            self.builder.build_store(alloca, val).map_err(|e| format!("store error: {}", e))?;
                            lambda_vars.insert(name, (alloca, llvm_ty));
                        }
                        _ => {}
                    }
                }
                if !self.block_has_terminator() {
                    self.builder.build_return(Some(&last_val)).map_err(|e| format!("return error: {}", e))?;
                }
                if let Some(bb) = saved_block {
                    self.builder.position_at_end(bb);
                }
                // For now, return the function pointer (closure capture requires runtime support)
                // TODO: return closure struct { fn_ptr, captured_env } when runtime supports it
                Ok(lambda_fn.as_global_value().as_pointer_value().into())
            }
            Expr::Comprehension { expr, var, iter, guard } => {
                // List comprehension: [expr for x in iter if guard]
                // Compile iter to get list pointer
                let iter_val = self.compile_expr(iter, vars)?;
                let list_ptr = match iter_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Err("comprehension iter must be a list pointer".into()),
                };
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false));
                // Read list length and data
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "comp_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "comp_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")
                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();
                // Allocate output array (same max size as input)
                let elem_size = i64_ty.const_int(8, false);
                let alloc_size = self.builder.build_int_mul(list_len, elem_size, "comp_alloc")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let out_ptr = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "comp_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?.into_pointer_value();
                let out_i64 = self.builder.build_bit_cast(out_ptr,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "out_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();
                // Loop: for i in 0..len
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "comp_loop");
                let body_bb = self.context.append_basic_block(function, "comp_body");
                let done_bb = self.context.append_basic_block(function, "comp_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "ci")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let wi_alloca = self.builder.build_alloca(i64_ty, "cw")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_store(wi_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                // Load element
                let elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
                }.map_err(|e| format!("gep error: {}", e))?;
                let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
                    .map_err(|e| format!("load error: {}", e))?;
                // Bind var
                let mut comp_vars = vars.clone();
                let elem_alloca = self.builder.build_alloca(i64_ty, var)
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(elem_alloca, elem)
                    .map_err(|e| format!("store error: {}", e))?;
                comp_vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(i64_ty)));
                // Check guard
                let include = if let Some(g) = guard {
                    let g_val = self.compile_expr(g, &comp_vars)?;
                    let g_bool = match g_val {
                        BasicValueEnum::IntValue(iv) => self.builder.build_int_z_extend(iv, i64_ty, "g_ext")
                            .map_err(|e| format!("zext error: {}", e))?,
                        _ => return Err("guard must be boolean".into()),
                    };
                    self.builder.build_int_compare(inkwell::IntPredicate::NE, g_bool, i64_ty.const_int(0, false), "g_truthy")
                        .map_err(|e| format!("cmp error: {}", e))?
                } else {
                    self.context.bool_type().const_int(1, false)
                };
                let store_bb = self.context.append_basic_block(function, "comp_store");
                let next_bb = self.context.append_basic_block(function, "comp_next");
                self.builder.build_conditional_branch(include, store_bb, next_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(store_bb);
                // Evaluate expression
                let result = self.compile_expr(expr, &comp_vars)?;
                let wi = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), wi_alloca, "wi")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let out_elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, out_i64, &[wi], "out_elem")
                }.map_err(|e| format!("gep error: {}", e))?;
                let result_i64 = match result {
                    BasicValueEnum::IntValue(iv) => iv,
                    BasicValueEnum::FloatValue(fv) => self.builder.build_float_to_signed_int(fv, i64_ty, "f_to_i")
                        .map_err(|e| format!("fptosi error: {}", e))?,
                    BasicValueEnum::PointerValue(pv) => self.builder.build_ptr_to_int(pv, i64_ty, "p_to_i")
                        .map_err(|e| format!("ptrtoint error: {}", e))?,
                    _ => return Err("comprehension expression must produce i64-compatible value".into()),
                };
                self.builder.build_store(out_elem_ptr, result_i64)
                    .map_err(|e| format!("store error: {}", e))?;
                let next_wi = self.builder.build_int_add(wi, i64_ty.const_int(1, false), "next_wi")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(wi_alloca, next_wi)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(next_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(next_bb);
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                // Build result list
                let result_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), wi_alloca, "result_len")
                    .map_err(|e| format!("load error: {}", e))?;
                let result_alloca = self.builder.build_alloca(list_struct_ty, "comp_result")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let rlen_gep = self.builder.build_struct_gep(list_struct_ty, result_alloca, 0, "rlen")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(rlen_gep, result_len)
                    .map_err(|e| format!("store error: {}", e))?;
                let rdata_gep = self.builder.build_struct_gep(list_struct_ty, result_alloca, 1, "rdata")
                    .map_err(|e| format!("gep error: {}", e))?;
                let out_void = self.builder.build_pointer_cast(out_i64, i8_ptr, "out_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(rdata_gep, out_void)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(result_alloca.into())
            }
            Expr::Quote(_) | Expr::QuoteInterpolate(_) | Expr::Comptime(_) => {
                Err("quote/comptime expressions must be resolved before codegen".into())
            }
            #[allow(unreachable_patterns)]
            _ => Err(format!("unsupported expression in codegen: {:?}", expr)),
        }
    }

    /// Infer the type name of an object expression from the codegen's type definitions
    /// Build a List<string> from a slice of string values (compile-time constant list)
    fn build_string_list(
        &self,
        strings: &[String],
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i8_ty = self.context.i8_type();
        let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let count = strings.len() as u64;

        // Allocate array of string structs: [ { i8*, i64 } x N ]
        let str_ty = self.context.struct_type(&[
            BasicTypeEnum::PointerType(i8_ptr),
            BasicTypeEnum::IntType(i64_ty),
        ], false);
        let arr_type = str_ty.array_type(count as u32);
        let arr_alloca = self.builder.build_alloca(BasicTypeEnum::ArrayType(arr_type), "str_arr")
            .map_err(|e| format!("alloca error: {}", e))?;

        for (i, s) in strings.iter().enumerate() {
            let global = self.builder.build_global_string_ptr(s, &format!("str_{}", i))
                .map_err(|e| format!("global string error: {}", e))?;
            let elem_ptr = self.builder.build_struct_gep(
                BasicTypeEnum::StructType(str_ty),
                arr_alloca,
                i as u32,
                &format!("elem_{}", i),
            ).map_err(|e| format!("gep error: {}", e))?;
            let ptr_gep = self.builder.build_struct_gep(str_ty, elem_ptr, 0, "ptr")
                .map_err(|e| format!("gep error: {}", e))?;
            self.builder.build_store(ptr_gep, global.as_pointer_value())
                .map_err(|e| format!("store error: {}", e))?;
            let len_gep = self.builder.build_struct_gep(str_ty, elem_ptr, 1, "len")
                .map_err(|e| format!("gep error: {}", e))?;
            self.builder.build_store(len_gep, i64_ty.const_int(s.len() as u64, false))
                .map_err(|e| format!("store error: {}", e))?;
        }

        // Build list struct: { i64 len, i8* data }
        let list_ty = self.context.struct_type(&[
            BasicTypeEnum::IntType(i64_ty),
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
        ], false);
        let list_alloca = self.builder.build_alloca(list_ty, "str_list")
            .map_err(|e| format!("alloca error: {}", e))?;
        let len_gep = self.builder.build_struct_gep(list_ty, list_alloca, 0, "len")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(len_gep, i64_ty.const_int(count, false))
            .map_err(|e| format!("store error: {}", e))?;
        let data_gep = self.builder.build_struct_gep(list_ty, list_alloca, 1, "data")
            .map_err(|e| format!("gep error: {}", e))?;
        let arr_void_ptr = self.builder.build_pointer_cast(
            arr_alloca,
            i8_ptr,
            "arr_void"
        ).map_err(|e| format!("bitcast error: {}", e))?;
        self.builder.build_store(data_gep, arr_void_ptr)
            .map_err(|e| format!("store error: {}", e))?;
        Ok(list_alloca.into())
    }

    /// Collect free variables used in a block that are defined in the enclosing scope
    fn collect_free_vars(
        &self,
        block: &Block,
        param_names: &std::collections::HashSet<String>,
        vars: &HashMap<String, VarEntry<'ctx>>,
        free_vars: &mut HashMap<String, (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    ) {
        let mut defined = param_names.clone();
        for stmt in block {
            match stmt {
                Stmt::Expr(e) => self.collect_free_vars_expr(e, &defined, vars, free_vars),
                Stmt::Let { pat, init: Some(init), .. } => {
                    self.collect_free_vars_expr(init, &defined, vars, free_vars);
                    if let Pattern::Variable(name) = pat {
                        defined.insert(name.clone());
                    }
                }
                Stmt::Return(Some(e)) => self.collect_free_vars_expr(e, &defined, vars, free_vars),
                Stmt::If { cond, then_, else_ } => {
                    self.collect_free_vars_expr(cond, &defined, vars, free_vars);
                    self.collect_free_vars(then_, &defined, vars, free_vars);
                    if let Some(eb) = else_ {
                        self.collect_free_vars(eb, &defined, vars, free_vars);
                    }
                }
                _ => {}
            }
        }
    }

    fn collect_free_vars_expr(
        &self,
        expr: &Expr,
        defined: &std::collections::HashSet<String>,
        vars: &HashMap<String, VarEntry<'ctx>>,
        free_vars: &mut HashMap<String, (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    ) {
        match expr {
            Expr::Ident(name) => {
                if !defined.contains(name.as_str()) {
                    if let Some(&(ptr, ty)) = vars.get(name.as_str()) {
                        free_vars.entry(name.clone()).or_insert((ptr, ty));
                    }
                }
            }
            Expr::Binary(_, l, r) => {
                self.collect_free_vars_expr(l, defined, vars, free_vars);
                self.collect_free_vars_expr(r, defined, vars, free_vars);
            }
            Expr::Unary(_, e) => self.collect_free_vars_expr(e, defined, vars, free_vars),
            Expr::Call(callee, args) => {
                self.collect_free_vars_expr(callee, defined, vars, free_vars);
                for arg in args {
                    self.collect_free_vars_expr(arg, defined, vars, free_vars);
                }
            }
            Expr::Field(obj, _) => self.collect_free_vars_expr(obj, defined, vars, free_vars),
            Expr::Index(obj, idx) => {
                self.collect_free_vars_expr(obj, defined, vars, free_vars);
                self.collect_free_vars_expr(idx, defined, vars, free_vars);
            }
            Expr::List(elems) | Expr::Tuple(elems) => {
                for e in elems {
                    self.collect_free_vars_expr(e, defined, vars, free_vars);
                }
            }
            Expr::If { cond, then_, else_ } => {
                self.collect_free_vars_expr(cond, defined, vars, free_vars);
                self.collect_free_vars(then_, defined, vars, free_vars);
                if let Some(eb) = else_ {
                    self.collect_free_vars(eb, defined, vars, free_vars);
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    fn infer_object_type(&self, expr: &Expr, vars: &HashMap<String, VarEntry<'ctx>>) -> String {
        match expr {
            Expr::Ident(name) => {
                // Look up variable's type name from our tracking map
                if let Some(ty_name) = self.var_type_names.get(name) {
                    ty_name.clone()
                } else {
                    name.clone()
                }
            }
            Expr::Record { ty: Some(name), .. } => name.clone(),
            Expr::Call(callee, _) => {
                // constructor call like ActorName(args) -> return type is the name
                if let Expr::Ident(name) = callee.as_ref() {
                    // Try to strip _new suffix used by our codegen constructors
                    if let Some(stripped) = name.strip_suffix("_new") {
                        stripped.to_string()
                    } else {
                        name.clone()
                    }
                } else {
                    String::new()
                }
            }
            Expr::Field(obj, _) => self.infer_object_type(obj, vars),
            _ => String::new(),
        }
    }

    fn compile_fstring(
        &mut self,
        parts: &[crate::ast::FStringPart],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let _i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        if parts.is_empty() {
            let global = self.builder.build_global_string_ptr("", "fstr_empty")
                .map_err(|e| format!("string error: {}", e))?;
            return Ok(global.as_pointer_value().into());
        }

        // Optimization: if all parts are text, return a single global string
        let all_text: Option<String> = parts.iter().map(|p| {
            match p {
                crate::ast::FStringPart::Text(t) => Some(t.as_str()),
                _ => None,
            }
        }).collect();
        if let Some(text) = all_text {
            let global = self.builder.build_global_string_ptr(&text, "fstr_literal")
                .map_err(|e| format!("string error: {}", e))?;
            return Ok(global.as_pointer_value().into());
        }

        // For f-strings with interpolation: use malloc + strcpy + strcat
        let malloc_fn = self.module.get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let strcpy_fn = self.module.get_function("strcpy")
            .ok_or_else(|| "strcpy not declared".to_string())?;
        let strcat_fn = self.module.get_function("strcat")
            .ok_or_else(|| "strcat not declared".to_string())?;
        let strlen_fn = self.module.get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let sprintf_fn = self.module.get_function("sprintf")
            .ok_or_else(|| "sprintf not declared".to_string())?;

        // Allocate a 1024-byte buffer for the result
        let buf_size = i64_ty.const_int(1024, false);
        let buf = self.builder.build_call(malloc_fn, &[
            BasicMetadataValueEnum::IntValue(buf_size),
        ], "fstr_buf")
            .map_err(|e| format!("malloc error: {}", e))?
            .try_as_basic_value().left()
            .ok_or("malloc returned void")?
            .into_pointer_value();

        // Initialize buffer with empty string
        let empty = self.builder.build_global_string_ptr("", "fstr_empty_init")
            .map_err(|e| format!("string error: {}", e))?;
        self.builder.build_call(strcpy_fn, &[
            BasicMetadataValueEnum::PointerValue(buf),
            BasicMetadataValueEnum::PointerValue(empty.as_pointer_value()),
        ], "fstr_init")
            .map_err(|e| format!("strcpy error: {}", e))?;

        // Append each part
        for (i, part) in parts.iter().enumerate() {
            match part {
                crate::ast::FStringPart::Text(t) => {
                    if t.is_empty() { continue; }
                    let global = self.builder.build_global_string_ptr(t, &format!("fstr_part_{}", i))
                        .map_err(|e| format!("string error: {}", e))?;
                    self.builder.build_call(strcat_fn, &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::PointerValue(global.as_pointer_value()),
                    ], &format!("fstr_cat_{}", i))
                        .map_err(|e| format!("strcat error: {}", e))?;
                }
                crate::ast::FStringPart::Interp(expr) => {
                    let val = self.compile_expr(expr, vars)?;
                    // Convert value to string based on type
                    match val {
                        BasicValueEnum::IntValue(iv) => {
                            let len = self.builder.build_call(strlen_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                            ], "fstr_strlen")
                                .map_err(|e| format!("strlen error: {}", e))?
                                .try_as_basic_value().left()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            let i8_type = self.context.i8_type();
                            let pos = unsafe { self.builder.build_gep(i8_type, buf, &[len], "fstr_pos") }
                                .map_err(|e| format!("gep error: {}", e))?;
                            let fmt = self.builder.build_global_string_ptr("%ld", &format!("fstr_fmt_{}", i))
                                .map_err(|e| format!("string error: {}", e))?;
                            self.builder.build_call(sprintf_fn, &[
                                BasicMetadataValueEnum::PointerValue(pos),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(iv),
                            ], &format!("fstr_sprintf_{}", i))
                                .map_err(|e| format!("sprintf error: {}", e))?;
                        }
                        BasicValueEnum::FloatValue(fv) => {
                            let len = self.builder.build_call(strlen_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                            ], "fstr_strlen")
                                .map_err(|e| format!("strlen error: {}", e))?
                                .try_as_basic_value().left()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            let i8_type = self.context.i8_type();
                            let pos = unsafe { self.builder.build_gep(i8_type, buf, &[len], "fstr_pos") }
                                .map_err(|e| format!("gep error: {}", e))?;
                            let fmt = self.builder.build_global_string_ptr("%f", &format!("fstr_fmt_{}", i))
                                .map_err(|e| format!("string error: {}", e))?;
                            self.builder.build_call(sprintf_fn, &[
                                BasicMetadataValueEnum::PointerValue(pos),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::FloatValue(fv),
                            ], &format!("fstr_sprintf_{}", i))
                                .map_err(|e| format!("sprintf error: {}", e))?;
                        }
                        BasicValueEnum::PointerValue(pv) => {
                            // String pointer: use strcat
                            self.builder.build_call(strcat_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::PointerValue(pv),
                            ], &format!("fstr_cat_{}", i))
                                .map_err(|e| format!("strcat error: {}", e))?;
                        }
                        _ => {
                            let unknown = self.builder.build_global_string_ptr("<unsupported>", &format!("fstr_unsup_{}", i))
                                .map_err(|e| format!("string error: {}", e))?;
                            self.builder.build_call(strcat_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::PointerValue(unknown.as_pointer_value()),
                            ], &format!("fstr_cat_unsup_{}", i))
                                .map_err(|e| format!("strcat error: {}", e))?;
                        }
                    }
                }
            }
        }

        Ok(buf.into())
    }

    fn compile_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match op {
            BinOp::Add => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_add(l, r, "add").map_err(|e| format!("add error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_add(l, r, "fadd").map_err(|e| format!("add error: {}", e))?.into()),
                _ => Err("add requires same numeric types".into()),
            },
            BinOp::Sub => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_sub(l, r, "sub").map_err(|e| format!("sub error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_sub(l, r, "fsub").map_err(|e| format!("sub error: {}", e))?.into()),
                _ => Err("sub requires same numeric types".into()),
            },
            BinOp::Mul => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_mul(l, r, "mul").map_err(|e| format!("mul error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_mul(l, r, "fmul").map_err(|e| format!("mul error: {}", e))?.into()),
                _ => Err("mul requires same numeric types".into()),
            },
            BinOp::Div => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_signed_div(l, r, "div").map_err(|e| format!("div error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_div(l, r, "fdiv").map_err(|e| format!("div error: {}", e))?.into()),
                _ => Err("div requires same numeric types".into()),
            },
            BinOp::Mod => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_signed_rem(l, r, "rem").map_err(|e| format!("rem error: {}", e))?.into()),
                _ => Err("mod requires integer types".into()),
            },
            BinOp::EqCmp => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::EQ, l, r, "eq").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "feq").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("eq requires same types".into()),
            },
            BinOp::NeCmp => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::NE, l, r, "ne").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::ONE, l, r, "fne").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("ne requires same types".into()),
            },
            BinOp::Lt => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SLT, l, r, "lt").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OLT, l, r, "flt").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("lt requires same numeric types".into()),
            },
            BinOp::Gt => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGT, l, r, "gt").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OGT, l, r, "fgt").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("gt requires same numeric types".into()),
            },
            BinOp::Le => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SLE, l, r, "le").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OLE, l, r, "fle").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("le requires same numeric types".into()),
            },
            BinOp::Ge => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGE, l, r, "ge").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OGE, l, r, "fge").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("ge requires same numeric types".into()),
            },
            BinOp::And => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_and(l, r, "and").map_err(|e| format!("and error: {}", e))?.into()),
                _ => Err("and requires boolean types".into()),
            },
            BinOp::Or => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_or(l, r, "or").map_err(|e| format!("or error: {}", e))?.into()),
                _ => Err("or requires boolean types".into()),
            },
            BinOp::Range => {
                let start_iv = match lhs {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("[E0712] range start must be i64".into()),
                };
                let end_iv = match rhs {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("[E0712] range end must be i64".into()),
                };
                // Create a range struct { start: i64, end: i64 }
                let i64_ty = self.context.i64_type();
                let range_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let alloca = self.builder.build_alloca(range_ty, "range")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let start_gep = self.builder.build_struct_gep(range_ty, alloca, 0, "range_start")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(start_gep, start_iv)
                    .map_err(|e| format!("store error: {}", e))?;
                let end_gep = self.builder.build_struct_gep(range_ty, alloca, 1, "range_end")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(end_gep, end_iv)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(alloca.into())
            }
            _ => Err(format!("unsupported binary operator {:?}", op)),
        }
    }

    fn compile_call(
        &mut self,
        name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let mut compiled_args = Vec::new();
        for arg in args {
            compiled_args.push(self.compile_expr(arg, vars)?);
        }

        let metadata_args: Vec<_> = compiled_args.iter().map(|v| {
            match v {
                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
            }
        }).collect();

        // Dispatch builtins
        if builtins::is_builtin(name) {
            return self.compile_builtin_call(name, &metadata_args);
        }

        // Handle built-in Option/Result constructors
        match name {
            "Ok" | "Some" => {
                if compiled_args.len() != 1 {
                    return Err(format!("[E0711] {} expects 1 argument", name));
                }
                let val = compiled_args[0];
                let bool_ty = self.context.bool_type();
                let disc = bool_ty.const_int(1, false);
                let inner_ty = val.get_type();
                let struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(bool_ty),
                    inner_ty,
                ], false);
                let alloca = self.builder.build_alloca(struct_ty, "result_val")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let disc_gep = self.builder.build_struct_gep(struct_ty, alloca, 0, "disc")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(disc_gep, disc)
                    .map_err(|e| format!("store error: {}", e))?;
                let val_gep = self.builder.build_struct_gep(struct_ty, alloca, 1, "payload")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(val_gep, val)
                    .map_err(|e| format!("store error: {}", e))?;
                let result = self.builder.build_load(struct_ty, alloca, "loaded")
                    .map_err(|e| format!("load error: {}", e))?;
                return Ok(result);
            }
            "Err" | "None" => {
                if name == "Err" && compiled_args.len() != 1 {
                    return Err("[E0711] Err expects 1 argument".into());
                }
                if name == "None" && compiled_args.len() != 0 {
                    return Err("[E0711] None expects 0 arguments".into());
                }
                let bool_ty = self.context.bool_type();
                let disc = bool_ty.const_int(0, false);
                let payload_ty = BasicTypeEnum::IntType(self.context.i64_type());
                let struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(bool_ty),
                    payload_ty,
                ], false);
                let alloca = self.builder.build_alloca(struct_ty, "result_val")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let disc_gep = self.builder.build_struct_gep(struct_ty, alloca, 0, "disc")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(disc_gep, disc)
                    .map_err(|e| format!("store error: {}", e))?;
                let val_gep = self.builder.build_struct_gep(struct_ty, alloca, 1, "payload")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(val_gep, self.context.i64_type().const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                let result = self.builder.build_load(struct_ty, alloca, "loaded")
                    .map_err(|e| format!("load error: {}", e))?;
                return Ok(result);
            }
            _ => {}
        }

        if let Some(function) = self.module.get_function(name) {
            let call = self.builder.build_call(function, &metadata_args, "call")
                .map_err(|e| format!("call error: {}", e))?;
            Ok(call.try_as_basic_value().left().unwrap_or(
                self.context.i64_type().const_int(0, false).into()
            ))
        } else {
            // Try mangled name with current type_map
            let mangled = Self::mangle_name(name, &self.type_map);
            if let Some(function) = self.module.get_function(&mangled) {
                let call = self.builder.build_call(function, &metadata_args, "call")
                    .map_err(|e| format!("call error: {}", e))?;
                Ok(call.try_as_basic_value().left().unwrap_or(
                    self.context.i64_type().const_int(0, false).into()
                ))
            } else {
                Err(format!("undefined function '{}' in codegen", name))
            }
        }
    }

    /// Call a function by its mangled name
    fn compile_call_mangled(
        &mut self,
        mangled: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let mut compiled_args = Vec::new();
        for arg in args {
            compiled_args.push(self.compile_expr(arg, vars)?);
        }

        let metadata_args: Vec<_> = compiled_args.iter().map(|v| {
            match v {
                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
            }
        }).collect();

        if let Some(function) = self.module.get_function(mangled) {
            let call = self.builder.build_call(function, &metadata_args, "call")
                .map_err(|e| format!("call error: {}", e))?;
            Ok(call.try_as_basic_value().left().unwrap_or(
                self.context.i64_type().const_int(0, false).into()
            ))
        } else {
            Err(format!("undefined function '{}' in codegen", mangled))
        }
    }

    /// Find a FuncDef by name from the codegen's stored func_defs
    fn find_func_def(&self, name: &str) -> Result<FuncDef, String> {
        self.func_defs.get(name)
            .cloned()
            .ok_or_else(|| format!("function '{}' definition not available for monomorphization", name))
    }

    /// Extract a raw C string pointer (i8*) from a Mimi string argument.
    /// Mimi strings are represented as either:
    ///   - An i8* raw C string (from string literals)
    ///   - A {i8*, i64} struct (from string variables)
    fn extract_raw_str_ptr(&self, arg: &BasicMetadataValueEnum<'ctx>) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => {
                // Could be a raw C string pointer OR a pointer to a Mimi string struct {i8*, i64}.
                // Try to detect: if it points to a struct with ptr+len, load field 0.
                // For now, assume it's a raw C string pointer (string literal case).
                // String variables may produce pointer-to-struct — handle below.
                Ok(*pv)
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                let extracted = self.builder.build_extract_value(*sv, 0, "str_ptr")
                    .map_err(|e| format!("extract str ptr error: {}", e))?;
                match extracted {
                    BasicValueEnum::PointerValue(pv) => Ok(pv),
                    _ => Err("[E0712] string struct field 0 is not a pointer".into()),
                }
            }
            _ => Err("[E0712] expected a string argument".into()),
        }
    }

    /// Return an error if running in no_std mode for a builtin that depends on libc.
    fn require_std(&self, builtin: &str) -> Result<(), String> {
        if self.no_std {
            Err(format!("[E0750] '{}' requires libc (not available in no_std mode)", builtin))
        } else {
            Ok(())
        }
    }

    pub fn emit_ir(&self) -> String {
        self.module.print_to_string().to_string()
    }

    pub fn compile_to_object(&self, output_path: &Path) -> Result<(), String> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|e| format!("failed to initialize target: {}", e))?;
        let triple = TargetMachine::get_default_triple();
        let triple_str = triple.as_str().to_string_lossy().to_string();
        let triple_ref = if self.no_std {
            // Use freestanding target triple
            // e.g., "x86_64-unknown-linux-gnu" → "x86_64-unknown-none"
            let parts: Vec<&str> = triple_str.split('-').collect();
            let freestanding = if parts.len() >= 3 {
                format!("{}-{}-none", parts[0], parts[1])
            } else {
                format!("{}-none", parts[0])
            };
            inkwell::targets::TargetTriple::create(&freestanding)
        } else {
            triple
        };
        let target = Target::from_triple(&triple_ref)
            .map_err(|e| format!("failed to find target for triple '{}': {}", triple_ref, e))?;
        let cpu = TargetMachine::get_host_cpu_name().to_string();
        let features = TargetMachine::get_host_cpu_features().to_string();
        let tm = target.create_target_machine(
            &triple_ref, &cpu, &features,
            OptimizationLevel::Aggressive,
            RelocMode::Default, CodeModel::Default,
        ).ok_or_else(|| format!("failed to create target machine for triple '{}'", triple_ref))?;

        tm.write_to_file(&self.module, inkwell::targets::FileType::Object, output_path)
            .map_err(|e| format!("failed to write object file: {}", e))
    }
}
