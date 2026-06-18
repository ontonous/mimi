#![allow(dead_code, deprecated)]

pub mod types;
pub mod builtins;

use crate::ast::*;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
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
}

type VarEntry<'ctx> = (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>);

impl<'ctx> CodeGenerator<'ctx> {
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();
        builtins::register_runtime(&module, context);
        Self { context, module, builder, loop_break: None, loop_continue: None, type_defs: HashMap::new(), type_llvm: HashMap::new(), cap_vars: vec![HashMap::new()], cap_type_names: std::collections::HashSet::new(), type_map: HashMap::new(), func_defs: HashMap::new(), var_type_names: HashMap::new(), spawn_counter: 0, strict: false, compensation_blocks: Vec::new(), comp_scope_stack: Vec::new(), in_parasteps: false, parasteps_thread_ids: Vec::new(), trait_defs: HashMap::new(), type_impls: HashMap::new() }
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
        // Join all remaining threads (spawns not awaited within the parasteps block)
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
                    return Err(format!("capability '{}' has already been consumed", name));
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
        // Compile trait impl methods
        self.compile_impl_methods()?;
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
                        ty: crate::ast::Type::Ref(Box::new(
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
            let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());

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
                            _ => return Err(format!("c_shared param {} must be pointer or int", i)),
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
                        _ => return Err(format!("c_shared param {} must be pointer or int", i)),
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
        let actor_ty = self.type_llvm.get(&actor.name)
            .ok_or_else(|| format!("actor type '{}' not found", actor.name))?
            .clone();
        
        let fn_type = match actor_ty {
            BasicTypeEnum::StructType(sty) => sty.fn_type(&metadata_params, false),
            _ => return Err(format!("actor '{}' type is not a struct", actor.name)),
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
        let actor_ty = self.type_llvm.get(&actor.name)
            .ok_or_else(|| format!("actor type '{}' not found", actor.name))?
            .clone();
        
        // Method name: ActorName__methodName
        let mangled = format!("{}__{}__method", actor.name, method.name);
        
        // Build function type: self (ptr to actor struct) + params -> ret
        let actor_ptr_ty = match actor_ty {
            BasicTypeEnum::StructType(sty) => BasicTypeEnum::PointerType(sty.ptr_type(inkwell::AddressSpace::default())),
            _ => return Err(format!("actor '{}' type is not a struct", actor.name)),
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
                        return Err("if condition must be boolean".into());
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
                        let start_iv = if let BasicValueEnum::IntValue(iv) = start_val { iv } else { return Err("range start must be i64".into()); };
                        let end_iv = if let BasicValueEnum::IntValue(iv) = end_val { iv } else { return Err("range end must be i64".into()); };
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
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("idx must be i64".into()); };
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
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("idx must be i64".into()); };
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
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val { iv } else { return Err("while condition must be boolean".into()); };
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
                Stmt::Arena(block) | Stmt::Unsafe(block) | Stmt::Alloc { body: block, .. } => {
                    self.compile_block(block, &mut vars)?;
                }
                Stmt::SharedLet { init, .. } => {
                    self.compile_expr(init, &vars)?;
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
            commitment: func.commitment.clone(),
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
            commitment: func.commitment.clone(),
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
                
                // Track capability parameters
                if matches!(&param.ty, Type::Cap(_)) {
                    self.register_cap(&param.name, alloca);
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
                        return Err("if condition must be boolean".into());
                    };

                    let function = self.current_function().unwrap();
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Then block
                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, &mut vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    // Else block
                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, &mut vars)?;
                    }
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    // Continue at merge
                    self.builder.position_at_end(merge_bb);
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
                        return Err("while condition must be boolean".into());
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
                        let start_iv = if let BasicValueEnum::IntValue(iv) = start_val { iv } else { return Err("range start must be i64".into()); };
                        let end_iv = if let BasicValueEnum::IntValue(iv) = end_val { iv } else { return Err("range end must be i64".into()); };

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
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("index must be i64".into()); };
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
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("index must be i64".into()); };
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
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("index must be i64".into()); };
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
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("index must be i64".into()); };
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
                    let val = self.compile_expr(expr, &vars)?;
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
                Stmt::SharedLet { init, .. } => {
                    // SharedLet: evaluate init expression (simplified - no actual shared ownership in codegen)
                    self.compile_expr(init, &vars)?;
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    // Arena: execute block sequentially (simplified - no region-based memory in codegen)
                    self.compile_block(block, &mut vars)?;
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, &mut vars)?;
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
                        return Err("if condition must be boolean".into());
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
                Stmt::SharedLet { init, .. } => {
                    // SharedLet: evaluate init expression (simplified)
                    self.compile_expr(init, vars)?;
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    // Arena: execute block sequentially (simplified)
                    self.compile_block(block, vars)?;
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, vars)?;
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
                                .map_err(|e| format!("load error: {}", e))?.into())
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
                                    _ => return Err("type_fields: argument must be a type name string".into()),
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
                                    _ => return Err("type_variants: argument must be a type name string".into()),
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
                                            _ => return Err("values: expected record pointer".into()),
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
                                                    _ => return Err("values: unsupported field type".into()),
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
                        
                        // 3. Fallback: field access or error
                        if self.type_defs.contains_key(&obj_type) {
                            Err(format!("method '{}' not compiled for type '{}' (missing crate?)", method_name, obj_type))
                        } else {
                            Err(format!("cannot call method '{}' on unknown type '{}'", method_name, obj_type))
                        }
                    }
                    _ => Err(format!("only direct function calls and method calls supported in codegen")),
                }
            }
            Expr::Turbofish(name, type_args, args) => {
                // Monomorphized call: func::<Type>(args)
                // Build type_map from explicit type args
                let func = self.find_func_def(name)?;
                if func.generics.len() != type_args.len() {
                    return Err(format!("turbofish for '{}' expects {} type args, got {}", name, func.generics.len(), type_args.len()));
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
                        }
                        Pattern::Literal(lit) => {
                            self.builder.position_at_end(else_bb);
                            let lit_val = match lit {
                                Lit::Int(n) => self.context.i64_type().const_int(*n as u64, true),
                                Lit::Bool(b) => self.context.bool_type().const_int(*b as u64, false),
                                Lit::Unit => self.context.i64_type().const_int(0, false),
                                _ => return Err("unsupported match literal type".into()),
                            };
                            let cmp = self.builder.build_int_compare(
                                inkwell::IntPredicate::EQ,
                                scrutinee_iv,
                                lit_val,
                                "cmp",
                            ).map_err(|e| format!("cmp error: {}", e))?;
                            let next_bb = if i < arms.len() - 1 {
                                self.context.append_basic_block(function, &format!("next{}", i))
                            } else {
                                merge_bb
                            };
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
                            let next_bb = if i < arms.len() - 1 {
                                self.context.append_basic_block(function, &format!("next{}", i))
                            } else {
                                merge_bb
                            };
                            self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            else_bb = next_bb;
                        }
                        _ => return Err(format!("unsupported pattern in codegen: {:?}", arm.pat)),
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
                        _ => {}
                    }
                    let arm_val = self.compile_expr(&arm.body, &local_vars)?;
                    incoming_vals.push(arm_val);
                    incoming_bbs.push(arm_bb);
                    self.builder.build_unconditional_branch(merge_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                }

                // Unreachable else block (should not be reached if match is exhaustive)
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
                let phi_refs: Vec<_> = incoming_vals.iter().zip(incoming_bbs.iter())
                    .map(|(v, bb)| (v as &dyn inkwell::values::BasicValue, *bb))
                    .collect();
                phi.add_incoming(&phi_refs);
                Ok(phi.as_basic_value())
            }
            Expr::Record { ty, fields } => {
                // Create a record value
                let type_name = ty.as_deref().unwrap_or("unknown");
                let llvm_ty = self.type_llvm.get(type_name)
                    .ok_or_else(|| format!("unknown type '{}'", type_name))?
                    .clone();
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
                            return Err(format!("cannot access field on type '{}'", obj_type));
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
                // Store each element
                for (i, elem) in elems.iter().enumerate() {
                    let val = self.compile_expr(elem, vars)?;
                    let iv = match val {
                        BasicValueEnum::IntValue(iv) => iv,
                        _ => return Err("list elements must be i64 for now".into()),
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
                // list[i] - load from array
                let obj_val = self.compile_expr(obj, vars)?;
                let idx_val = self.compile_expr(idx_expr, vars)?;
                match obj_val {
                    BasicValueEnum::PointerValue(pv) => {
                        let idx_iv = match idx_val {
                            BasicValueEnum::IntValue(iv) => iv,
                            _ => return Err("index must be i64".into()),
                        };
                        // Assume it's a list struct and get data pointer
                        let list_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false);
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
                        self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, "elem_val")
                            .map_err(|e| format!("load error: {}", e))
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
                let byte_size = i64_ty.const_int(8, false); // 8 bytes for i64
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
                
                // Create thread: pthread_create(&thread, NULL, wrapper, NULL)
                let thread_alloca = self.builder.build_alloca(i64_ty, "thread")
                    .map_err(|e| format!("alloca error: {}", e))?;
                // Zero-initialize thread
                self.builder.build_store(thread_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                
                let pthread_create_fn = self.module.get_function("pthread_create")
                    .ok_or("pthread_create not declared")?;
                let wrapper_fn_ptr = self.builder.build_pointer_cast(
                    wrapper_fn.as_global_value().as_pointer_value(),
                    i8_ptr,
                    "wrapper_i8"
                ).map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_call(pthread_create_fn, &[
                    BasicMetadataValueEnum::PointerValue(thread_alloca),
                    BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
                    BasicMetadataValueEnum::PointerValue(wrapper_fn_ptr),
                    BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
                ], "pthread_create_call")
                    .map_err(|e| format!("pthread_create error: {}", e))?;
                
                // Load the thread ID
                let thread_id_val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), thread_alloca, "thread_id")
                    .map_err(|e| format!("load error: {}", e))?;
                let thread_id = if let BasicValueEnum::IntValue(iv) = thread_id_val {
                    iv
                } else {
                    return Err("expected i64 thread ID".into());
                };
                // Track in parasteps mode for joining at block end
                if self.in_parasteps {
                    self.parasteps_thread_ids.push(thread_id);
                }
                Ok(thread_id_val)
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

                        // Err path: print error message and exit(1)
                        self.builder.position_at_end(err_bb);
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
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
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
                _ => Err("lt requires integer types".into()),
            },
            BinOp::Gt => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGT, l, r, "gt").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("gt requires integer types".into()),
            },
            BinOp::Le => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SLE, l, r, "le").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("le requires integer types".into()),
            },
            BinOp::Ge => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGE, l, r, "ge").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("ge requires integer types".into()),
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
                // Range is primarily used in for loops, which handle it specially
                // For standalone range expressions, we return an error for now
                Err("range expression not supported in codegen, use in for loop".into())
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

    fn compile_builtin_call(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match name {
            "println" => {
                if args.is_empty() {
                    return Err("println expects at least 1 argument".into());
                }
                // For string args: call puts
                // For integer args: call printf with "%ld\n"
                let fmt_str = match args[0] {
                    BasicMetadataValueEnum::PointerValue(_) => {
                        // String arg - use puts
                        let puts = self.module.get_function("puts")
                            .ok_or_else(|| "puts not declared".to_string())?;
                        self.builder.build_call(puts, args, "puts_call")
                            .map_err(|e| format!("puts error: {}", e))?;
                        return Ok(self.context.i64_type().const_int(0, false).into());
                    }
                    BasicMetadataValueEnum::IntValue(_) => "%ld\n",
                    BasicMetadataValueEnum::FloatValue(_) => "%f\n",
                    _ => "%p\n",
                };
                let fmt_global = self.builder.build_global_string_ptr(fmt_str, "fmt")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend_from_slice(args);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "printf_call")
                    .map_err(|e| format!("printf error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "print" => {
                if args.is_empty() {
                    return Err("print expects at least 1 argument".into());
                }
                let fmt_str = match args[0] {
                    BasicMetadataValueEnum::PointerValue(_) => "%s",
                    BasicMetadataValueEnum::IntValue(_) => "%ld",
                    BasicMetadataValueEnum::FloatValue(_) => "%f",
                    _ => "%p",
                };
                let fmt_global = self.builder.build_global_string_ptr(fmt_str, "fmt")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend_from_slice(args);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "printf_call")
                    .map_err(|e| format!("printf error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "eprintln" => {
                if args.is_empty() {
                    return Err("eprintln expects at least 1 argument".into());
                }
                let fmt_str = match args[0] {
                    BasicMetadataValueEnum::PointerValue(_) => "%s\n",
                    BasicMetadataValueEnum::IntValue(_) => "%ld\n",
                    BasicMetadataValueEnum::FloatValue(_) => "%f\n",
                    _ => "%p\n",
                };
                let fmt_global = self.builder.build_global_string_ptr(fmt_str, "efmt")
                    .map_err(|e| format!("efmt error: {}", e))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend_from_slice(args);
                // Use fprintf(stderr, ...)
                let _stderr = self.module.get_global("stderr")
                    .map(|g| g.as_pointer_value())
                    .unwrap_or_else(|| {
                        // Fallback: just use printf
                        self.module.get_function("printf").unwrap().as_global_value().as_pointer_value()
                    });
                // For simplicity, use printf for stderr too (not ideal but functional)
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "eprintf_call")
                    .map_err(|e| format!("eprintf error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "assert" => {
                if args.len() != 1 {
                    return Err("assert expects 1 argument".into());
                }
                let cond = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("assert requires boolean/i64 argument".into()),
                };
                let function = self.current_function().unwrap();
                let ok_bb = self.context.append_basic_block(function, "assert_ok");
                let fail_bb = self.context.append_basic_block(function, "assert_fail");
                self.builder.build_conditional_branch(cond, ok_bb, fail_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed\n", "assert_msg")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "assert_printf")
                    .map_err(|e| format!("printf error: {}", e))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "assert_exit")
                    .map_err(|e| format!("exit error: {}", e))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "assert_eq" => {
                if args.len() != 2 {
                    return Err("assert_eq expects 2 arguments".into());
                }
                let a = args[0];
                let b = args[1];
                let eq = match (a, b) {
                    (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?
                    }
                    _ => return Err("assert_eq requires same types".into()),
                };
                let function = self.current_function().unwrap();
                let ok_bb = self.context.append_basic_block(function, "aeq_ok");
                let fail_bb = self.context.append_basic_block(function, "aeq_fail");
                self.builder.build_conditional_branch(eq, ok_bb, fail_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values not equal\n", "aeq_msg")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "aeq_printf")
                    .map_err(|e| format!("printf error: {}", e))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "aeq_exit")
                    .map_err(|e| format!("exit error: {}", e))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "assert_ne" => {
                if args.len() != 2 {
                    return Err("assert_ne expects 2 arguments".into());
                }
                let a = args[0];
                let b = args[1];
                let ne = match (a, b) {
                    (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::NE, l, r, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        self.builder.build_float_compare(inkwell::FloatPredicate::ONE, l, r, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?
                    }
                    _ => return Err("assert_ne requires same types".into()),
                };
                let function = self.current_function().unwrap();
                let ok_bb = self.context.append_basic_block(function, "ane_ok");
                let fail_bb = self.context.append_basic_block(function, "ane_fail");
                self.builder.build_conditional_branch(ne, ok_bb, fail_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values are equal\n", "ane_msg")
                    .map_err(|e| format!("fmt error: {}", e))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "ane_printf")
                    .map_err(|e| format!("printf error: {}", e))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "ane_exit")
                    .map_err(|e| format!("exit error: {}", e))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "range" => {
                if args.len() != 2 {
                    return Err("range expects 2 arguments".into());
                }
                let start = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("range start must be i64".into()),
                };
                let end = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("range end must be i64".into()),
                };
                // Create a list struct: { i64 len, i64* data }
                // For simplicity in codegen, we use a runtime-allocated array
                let len_val = self.builder.build_int_sub(end, start, "range_len")
                    .map_err(|e| format!("sub error: {}", e))?;
                // Allocate array: len * sizeof(i64)
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
                // Fill the array: for i in 0..len: data[i] = start + i
                let i64_ty = self.context.i64_type();
                let idx_alloca = self.builder.build_alloca(i64_ty, "idx")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "range_loop");
                let body_bb = self.context.append_basic_block(function, "range_body");
                let exit_bb = self.context.append_basic_block(function, "range_exit");
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Loop condition
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, len_val, "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, exit_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Body: data[idx] = start + idx
                self.builder.position_at_end(body_bb);
                let elem_val = self.builder.build_int_add(start, idx, "elem_val")
                    .map_err(|e| format!("add error: {}", e))?;
                let elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr_i64, &[idx], "elem_ptr")
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(elem_ptr, elem_val)
                    .map_err(|e| format!("store error: {}", e))?;
                // idx++
                let next_idx = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next_idx")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next_idx)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Exit: create list struct { len, data* }
                self.builder.position_at_end(exit_bb);
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
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
            "len" => {
                if args.len() != 1 {
                    return Err("len expects 1 argument".into());
                }
                match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => {
                        // Could be a string or list. Assume list struct { len, data* }
                        let list_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false);
                        let len_gep = self.builder.build_struct_gep(list_ty, pv, 0, "list.len")
                            .map_err(|e| format!("gep error: {}", e))?;
                        let len = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), len_gep, "len")
                            .map_err(|e| format!("load error: {}", e))?;
                        Ok(len)
                    }
                    _ => Err("len expects a list or string pointer".into()),
                }
            }
            "to_string" | "int_to_string" => {
                if args.len() != 1 {
                    return Err("to_string expects 1 argument".into());
                }
                match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => {
                        // Allocate 21 bytes for i64 string representation
                        let alloc_size = self.context.i64_type().const_int(21, false);
                        let malloc_fn = self.module.get_function("malloc")
                            .ok_or_else(|| "malloc not declared".to_string())?;
                        let buf = self.builder.build_call(malloc_fn, &[
                            BasicMetadataValueEnum::IntValue(alloc_size),
                        ], "malloc_call")
                            .map_err(|e| format!("malloc error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt_global = self.builder.build_global_string_ptr("%ld", "int_fmt")
                            .map_err(|e| format!("fmt error: {}", e))?;
                        let sprintf_fn = self.module.get_function("sprintf")
                            .ok_or_else(|| "sprintf not declared".to_string())?;
                        self.builder.build_call(sprintf_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(iv),
                        ], "sprintf_call")
                            .map_err(|e| format!("sprintf error: {}", e))?;
                        // Return as string struct { ptr, len }
                        let strlen_fn = self.module.get_function("strlen")
                            .ok_or_else(|| "strlen not declared".to_string())?;
                        let str_len = self.builder.build_call(strlen_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                        ], "strlen_call")
                            .map_err(|e| format!("strlen error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("strlen returned void")?;
                        let string_ty = self.context.struct_type(&[
                            BasicTypeEnum::PointerType(self.context.i8_type().ptr_type(inkwell::AddressSpace::default())),
                            BasicTypeEnum::IntType(self.context.i64_type()),
                        ], false);
                        let str_alloca = self.builder.build_alloca(string_ty, "str")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(ptr_gep, buf)
                            .map_err(|e| format!("store error: {}", e))?;
                        let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                            .map_err(|e| format!("store error: {}", e))?;
                        self.builder.build_store(len_gep, str_len)
                            .map_err(|e| format!("store error: {}", e))?;
                        Ok(str_alloca.into())
                    }
                    _ => Err("to_string: unsupported type".into()),
                }
            }
            "abs" => {
                if args.len() != 1 {
                    return Err("abs expects 1 argument".into());
                }
                match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => {
                        // abs(x) = x < 0 ? -x : x
                        let zero = self.context.i64_type().const_int(0, true);
                        let neg = self.builder.build_int_sub(zero, iv, "neg")
                            .map_err(|e| format!("neg error: {}", e))?;
                        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, iv, self.context.i64_type().const_int(0, false), "is_neg")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        let result = self.builder.build_select(cmp, neg, iv, "abs_val")
                            .map_err(|e| format!("select error: {}", e))?;
                        Ok(result)
                    }
                    BasicMetadataValueEnum::FloatValue(_fv) => {
                        // Use fabs
                        let fabs_fn = self.module.get_function("fabs")
                            .or_else(|| {
                                // Declare fabs if not present
                                let fabs_ty = self.context.f64_type().fn_type(
                                    &[inkwell::types::BasicMetadataTypeEnum::FloatType(self.context.f64_type())], false);
                                Some(self.module.add_function("fabs", fabs_ty, Some(inkwell::module::Linkage::External)))
                            }).unwrap();
                        let call = self.builder.build_call(fabs_fn, args, "fabs_call")
                            .map_err(|e| format!("fabs error: {}", e))?;
                        Ok(self.expect_basic_value(&call, "fabs")?)
                    }
                    _ => Err("abs requires numeric type".into()),
                }
            }
            "sqrt" => {
                if args.len() != 1 {
                    return Err("sqrt expects 1 argument".into());
                }
                let sqrt_fn = self.module.get_function("sqrt")
                    .or_else(|| {
                        let sqrt_ty = self.context.f64_type().fn_type(
                            &[inkwell::types::BasicMetadataTypeEnum::FloatType(self.context.f64_type())], false);
                        Some(self.module.add_function("sqrt", sqrt_ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let call = self.builder.build_call(sqrt_fn, args, "sqrt_call")
                    .map_err(|e| format!("sqrt error: {}", e))?;
                Ok(self.expect_basic_value(&call, "sqrt")?)
            }
            "min" | "max" => {
                if args.len() != 2 {
                    return Err("min/max expects 2 arguments".into());
                }
                let a = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("min/max requires integer types".into()),
                };
                let b = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("min/max requires integer types".into()),
                };
                let pred = if name == "min" {
                    inkwell::IntPredicate::SLT
                } else {
                    inkwell::IntPredicate::SGT
                };
                let cmp = self.builder.build_int_compare(pred, a, b, "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let result = self.builder.build_select(cmp, a, b, "minmax")
                    .map_err(|e| format!("select error: {}", e))?;
                Ok(result)
            }
            "exit" => {
                if args.len() != 1 {
                    return Err("exit expects 1 argument".into());
                }
                let code = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("exit code must be integer".into()),
                };
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(code),
                ], "exit_call")
                    .map_err(|e| format!("exit error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "push" => {
                // push(list, elem) - resize data array and append element
                if args.len() != 2 {
                    return Err("push expects 2 arguments".into());
                }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("push requires a list pointer".into()),
                };
                let elem = args[1];

                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ], false);

                // Load current len and data
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "push_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "push_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let old_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "old_len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let old_data = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "old_data")
                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();

                // new_len = old_len + 1
                let new_len = self.builder.build_int_add(old_len, i64_ty.const_int(1, false), "new_len")
                    .map_err(|e| format!("add error: {}", e))?;

                // new_alloc_size = new_len * 8
                let elem_size = i64_ty.const_int(8, false);
                let new_alloc_size = self.builder.build_int_mul(new_len, elem_size, "new_alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;

                // realloc(old_data, new_alloc_size)
                let realloc_fn = self.module.get_function("realloc")
                    .ok_or("realloc not declared")?;
                let realloc_result = self.builder.build_call(realloc_fn, &[
                    BasicMetadataValueEnum::PointerValue(old_data),
                    BasicMetadataValueEnum::IntValue(new_alloc_size),
                ], "realloc_result")
                    .map_err(|e| format!("realloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("realloc returned void")?
                    .into_pointer_value();

                // Store new data pointer
                self.builder.build_store(data_gep, realloc_result)
                    .map_err(|e| format!("store error: {}", e))?;

                // Store new element at data[old_len]: *(new_data + old_len*8) = elem
                let idx_ptr = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(i64_ty),
                        realloc_result,
                        &[old_len],
                        "elem_ptr",
                    ).map_err(|e| format!("gep error: {}", e))?
                };
                // Bitcast i8* to i64* for store
                let idx_ptr_i64 = self.builder.build_bit_cast(
                    idx_ptr,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()),
                    "idx_ptr_i64",
                ).map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();

                // Get the element value
                let elem_val = match elem {
                    BasicMetadataValueEnum::IntValue(iv) => BasicValueEnum::IntValue(iv),
                    BasicMetadataValueEnum::FloatValue(fv) => BasicValueEnum::FloatValue(fv),
                    BasicMetadataValueEnum::PointerValue(pv) => BasicValueEnum::PointerValue(pv),
                    _ => return Err("push: unsupported element type".into()),
                };
                self.builder.build_store(idx_ptr_i64, elem_val)
                    .map_err(|e| format!("store error: {}", e))?;

                // Store new length
                self.builder.build_store(len_gep, new_len)
                    .map_err(|e| format!("store error: {}", e))?;

                // Return the list pointer (unchanged)
                Ok(BasicValueEnum::PointerValue(list_ptr))
            }
            "pop" => {
                // pop(list) - remove and return last element
                if args.len() != 1 {
                    return Err("pop expects 1 argument".into());
                }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("pop requires a list pointer".into()),
                };

                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ], false);

                // Load current len and data
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "pop_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "pop_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let old_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "old_len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let old_data = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "old_data")
                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();

                // Check if empty (len == 0)
                let is_empty = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ, old_len,
                    i64_ty.const_int(0, false), "is_empty")
                    .map_err(|e| format!("compare error: {}", e))?;

                let function = self.current_function().unwrap();
                let nonempty_bb = self.context.append_basic_block(function, "pop_nonempty");
                let empty_bb = self.context.append_basic_block(function, "pop_empty");
                let merge_bb = self.context.append_basic_block(function, "pop_merge");

                self.builder.build_conditional_branch(is_empty, empty_bb, nonempty_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                // Empty path: return 0
                self.builder.position_at_end(empty_bb);
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                // Non-empty path: get last element, decrement len
                self.builder.position_at_end(nonempty_bb);
                let last_idx = self.builder.build_int_sub(old_len, i64_ty.const_int(1, false), "last_idx")
                    .map_err(|e| format!("sub error: {}", e))?;
                let elem_ptr = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(i64_ty),
                        old_data,
                        &[last_idx],
                        "elem_ptr",
                    ).map_err(|e| format!("gep error: {}", e))?
                };
                let elem_ptr_i64 = self.builder.build_bit_cast(
                    elem_ptr,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()),
                    "elem_ptr_i64",
                ).map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();
                let elem_val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr_i64, "elem_val")
                    .map_err(|e| format!("load error: {}", e))?;

                // new_len = old_len - 1
                let new_len = self.builder.build_int_sub(old_len, i64_ty.const_int(1, false), "new_len")
                    .map_err(|e| format!("sub error: {}", e))?;
                self.builder.build_store(len_gep, new_len)
                    .map_err(|e| format!("store error: {}", e))?;

                // realloc to shrink (optional, but good practice)
                let new_alloc_size = self.builder.build_int_mul(new_len, i64_ty.const_int(8, false), "new_alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;
                let realloc_fn = self.module.get_function("realloc")
                    .ok_or("realloc not declared")?;
                let realloc_result = self.builder.build_call(realloc_fn, &[
                    BasicMetadataValueEnum::PointerValue(old_data),
                    BasicMetadataValueEnum::IntValue(new_alloc_size),
                ], "realloc_result")
                    .map_err(|e| format!("realloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("realloc returned void")?
                    .into_pointer_value();
                self.builder.build_store(data_gep, realloc_result)
                    .map_err(|e| format!("store error: {}", e))?;

                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                // Merge: phi node for the returned element
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(BasicTypeEnum::IntType(i64_ty), "pop_result")
                    .map_err(|e| format!("phi error: {}", e))?;
                let zero = i64_ty.const_int(0, false);
                phi.add_incoming(&[
                    (&BasicValueEnum::IntValue(zero), empty_bb),
                    (&elem_val, nonempty_bb),
                ]);
                Ok(phi.as_basic_value())
            }
            "floor" | "ceil" | "round" => {
                if args.len() != 1 {
                    return Err("floor/ceil/round expects 1 argument".into());
                }
                let fn_name = match name {
                    "floor" => "floor",
                    "ceil" => "ceil",
                    _ => "round",
                };
                let c_fn = self.module.get_function(fn_name)
                    .or_else(|| {
                        let ty = self.context.f64_type().fn_type(
                            &[inkwell::types::BasicMetadataTypeEnum::FloatType(self.context.f64_type())], false);
                        Some(self.module.add_function(fn_name, ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let call = self.builder.build_call(c_fn, args, &format!("{}_call", fn_name))
                    .map_err(|e| format!("{} error: {}", fn_name, e))?;
                Ok(self.expect_basic_value(&call, fn_name)?)
            }
            // ========== P0 gap fixes: I/O and file builtins via FFI ==========
            "input" => {
                if args.len() > 1 { return Err("input expects 0 or 1 argument".into()); }
                // Allocate buffer (4096 bytes)
                let buf_size = self.context.i64_type().const_int(4096, false);
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(buf_size),
                ], "input_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // fgets(buf, 4096, stdin)
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let stdin_global = self.module.add_global(
                    i8_ptr_ty, None, "stdin"
                );
                stdin_global.set_linkage(inkwell::module::Linkage::External);
                let stdin_val = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    stdin_global.as_pointer_value(),
                    "stdin"
                ).map_err(|e| format!("load stdin error: {}", e))?.into_pointer_value();
                let fgets_fn = self.module.get_function("fgets")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("fgets", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                self.builder.build_call(fgets_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(buf_size),
                    BasicMetadataValueEnum::PointerValue(stdin_val),
                ], "fgets_call")
                    .map_err(|e| format!("fgets error: {}", e))?;
                // strlen(buf) for string struct length
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let str_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                ], "strlen_call")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "input_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, str_len)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())
            }
            "file_exists" => {
                if args.len() != 1 { return Err("file_exists expects 1 argument".into()); }
                let path_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("file_exists expects a string".into()),
                };
                // access(path, F_OK) where F_OK = 0
                let i32_ty = self.context.i32_type();
                let access_fn = self.module.get_function("access")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i32_ty.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i32_ty),
                        ], false);
                        Some(self.module.add_function("access", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let ret = self.builder.build_call(access_fn, &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(0, false)),
                ], "access_call")
                    .map_err(|e| format!("access error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("access returned void")?;
                let zero = i32_ty.const_int(0, false);
                let cmp = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ,
                    ret.into_int_value(),
                    zero,
                    "exists"
                ).map_err(|e| format!("cmp error: {}", e))?;
                let ext: BasicValueEnum = self.builder.build_int_z_extend(cmp, self.context.i64_type(), "result")
                    .map_err(|e| format!("zext error: {}", e))?.into();
                Ok(ext)
            }
            "read_file" => {
                if args.len() != 1 { return Err("read_file expects 1 argument".into()); }
                let path_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("read_file expects a string path".into()),
                };
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // fopen(path, "r")
                let mode_str = self.builder.build_global_string_ptr("r", "read_mode")
                    .map_err(|e| format!("global string error: {}", e))?;
                let fopen_fn = self.module.get_function("fopen")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("fopen", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let file = self.builder.build_call(fopen_fn, &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ], "fopen_call")
                    .map_err(|e| format!("fopen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("fopen returned void")?
                    .into_pointer_value();
                // fseek(file, 0, SEEK_END)
                let i32_ty = self.context.i32_type();
                let fseek_fn = self.module.get_function("fseek")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i32_ty.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::IntType(i32_ty),
                        ], false);
                        Some(self.module.add_function("fseek", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                self.builder.build_call(fseek_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(2, false)), // SEEK_END
                ], "fseek_call")
                    .map_err(|e| format!("fseek error: {}", e))?;
                // ftell(file) -> file size
                let ftell_fn = self.module.get_function("ftell")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = self.context.i64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("ftell", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let file_size = self.builder.build_call(ftell_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                ], "ftell_call")
                    .map_err(|e| format!("ftell error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("ftell returned void")?
                    .into_int_value();
                // rewind(file)
                let rewind_fn = self.module.get_function("rewind")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = self.context.void_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("rewind", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                self.builder.build_call(rewind_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                ], "rewind_call")
                    .map_err(|e| format!("rewind error: {}", e))?;
                // malloc(file_size + 1)
                let one = self.context.i64_type().const_int(1, false);
                let alloc_size = self.builder.build_int_add(file_size, one, "alloc_size")
                    .map_err(|e| format!("add error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "read_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // fread(buf, 1, file_size, file)
                let fread_fn = self.module.get_function("fread")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = self.context.i64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("fread", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                self.builder.build_call(fread_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(1, false)),
                    BasicMetadataValueEnum::IntValue(file_size),
                    BasicMetadataValueEnum::PointerValue(file),
                ], "fread_call")
                    .map_err(|e| format!("fread error: {}", e))?;
                // Null-terminate
                let null_gep = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(self.context.i8_type()),
                        buf,
                        &[file_size],
                        "null_byte"
                    )
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(null_gep, self.context.i8_type().const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                // fclose(file)
                let fclose_fn = self.module.get_function("fclose")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i32_ty.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("fclose", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                self.builder.build_call(fclose_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                ], "fclose_call")
                    .map_err(|e| format!("fclose error: {}", e))?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "read_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, file_size)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())
            }
            "write_file" => {
                if args.len() != 2 { return Err("write_file expects 2 arguments".into()); }
                let path_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("write_file: first arg must be string path".into()),
                };
                let content_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("write_file: second arg must be string content".into()),
                };
                // fopen(path, "w")
                let mode_str = self.builder.build_global_string_ptr("w", "write_mode")
                    .map_err(|e| format!("global string error: {}", e))?;
                let fopen_fn = self.module.get_function("fopen")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("fopen", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let file = self.builder.build_call(fopen_fn, &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ], "fopen_call")
                    .map_err(|e| format!("fopen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("fopen returned void")?
                    .into_pointer_value();
                // strlen(content) for length
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let content_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(content_ptr),
                ], "strlen_call")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?;
                // fwrite(content, 1, len, file)
                let fwrite_fn = self.module.get_function("fwrite")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = self.context.i64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("fwrite", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                self.builder.build_call(fwrite_fn, &[
                    BasicMetadataValueEnum::PointerValue(content_ptr),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(1, false)),
                    BasicMetadataValueEnum::IntValue(content_len.into_int_value()),
                    BasicMetadataValueEnum::PointerValue(file),
                ], "fwrite_call")
                    .map_err(|e| format!("fwrite error: {}", e))?;
                // fclose(file)
                let i32_ty = self.context.i32_type();
                let fclose_fn = self.module.get_function("fclose")
                    .or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i32_ty.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("fclose", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                self.builder.build_call(fclose_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                ], "fclose_call")
                    .map_err(|e| format!("fclose error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "str_char_at" => {
                if args.len() != 2 { return Err("str_char_at expects 2 arguments".into()); }
                let str_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_char_at: first arg must be string".into()),
                };
                let index = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("str_char_at: second arg must be integer index".into()),
                };
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // Allocate 2 bytes: char + null terminator
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(2, false)),
                ], "char_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // gep str_ptr + index (indexing into string struct { ptr, len })
                let data_ptr_gep = self.builder.build_struct_gep(
                    self.context.struct_type(&[
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ], false),
                    str_ptr, 0, "str_data_ptr"
                ).map_err(|e| format!("gep error: {}", e))?;
                let data_ptr = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    data_ptr_gep,
                    "data_ptr"
                ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                // char = data_ptr[index]
                let char_ptr = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(self.context.i8_type()),
                        data_ptr,
                        &[index],
                        "char_ptr"
                    )
                }.map_err(|e| format!("gep error: {}", e))?;
                let char_val = self.builder.build_load(
                    BasicTypeEnum::IntType(self.context.i8_type()),
                    char_ptr,
                    "char_val"
                ).map_err(|e| format!("load error: {}", e))?;
                // Store char + null
                self.builder.build_store(buf, char_val)
                    .map_err(|e| format!("store error: {}", e))?;
                let null_gep = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(self.context.i8_type()),
                        buf,
                        &[self.context.i64_type().const_int(1, false)],
                        "null_byte"
                    )
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(null_gep, self.context.i8_type().const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "char_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, self.context.i64_type().const_int(1, false))
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())
            }
            "str_contains" => {
                if args.len() != 2 { return Err("str_contains expects 2 arguments".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_contains: first arg must be string".into()),
                };
                let sub_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_contains: second arg must be string".into()),
                };
                // strstr(s, sub) -> i8* (or NULL if not found)
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let strstr_fn = self.module.get_function("strstr")
                    .or_else(|| {
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("strstr", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let result = self.builder.build_call(strstr_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(sub_ptr),
                ], "strstr_call")
                    .map_err(|e| format!("strstr error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strstr returned void")?;
                let cmp = self.builder.build_is_not_null(result.into_pointer_value(), "found")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let ext: BasicValueEnum = self.builder.build_int_z_extend(cmp, self.context.i64_type(), "result")
                    .map_err(|e| format!("zext error: {}", e))?.into();
                Ok(ext)
            }
            "str_starts_with" => {
                if args.len() != 2 { return Err("str_starts_with expects 2 arguments".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_starts_with: first arg must be string".into()),
                };
                let prefix_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_starts_with: second arg must be string".into()),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                // Call C helper: strncmp(s, prefix, strlen(prefix)) == 0
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let prefix_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(prefix_ptr),
                ], "prefix_len")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let strncmp_fn = self.module.get_function("strncmp")
                    .or_else(|| {
                        let ty = self.context.i32_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                        ], false);
                        Some(self.module.add_function("strncmp", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let cmp_result = self.builder.build_call(strncmp_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(prefix_ptr),
                    BasicMetadataValueEnum::IntValue(prefix_len),
                ], "strncmp_call")
                    .map_err(|e| format!("strncmp error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strncmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                let eq = self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp_result.into_int_value(), zero, "starts_with")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let ext: BasicValueEnum = self.builder.build_int_z_extend(eq, self.context.i64_type(), "result")
                    .map_err(|e| format!("zext error: {}", e))?.into();
                Ok(ext)
            }
            "str_ends_with" => {
                if args.len() != 2 { return Err("str_ends_with expects 2 arguments".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_ends_with: first arg must be string".into()),
                };
                let suffix_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_ends_with: second arg must be string".into()),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // s_len = strlen(s), suffix_len = strlen(suffix)
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "s_len")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let suffix_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(suffix_ptr),
                ], "suffix_len")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                // If suffix_len > s_len, return false
                let gt = self.builder.build_int_compare(inkwell::IntPredicate::SGT, suffix_len, s_len, "gt")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let function = self.current_function().unwrap();
                let check_bb = self.context.append_basic_block(function, "check_suffix");
                let false_bb = self.context.append_basic_block(function, "suffix_false");
                let merge_bb = self.context.append_basic_block(function, "suffix_done");
                self.builder.build_conditional_branch(gt, false_bb, check_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Compare s + (s_len - suffix_len) with suffix
                self.builder.position_at_end(check_bb);
                let start_pos = self.builder.build_int_sub(s_len, suffix_len, "start_pos")
                    .map_err(|e| format!("sub error: {}", e))?;
                let s_suffix_ptr = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[start_pos], "s_suffix")
                }.map_err(|e| format!("gep error: {}", e))?;
                let strncmp_fn = self.module.get_function("strncmp")
                    .or_else(|| {
                        let ty = self.context.i32_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i64_ty),
                        ], false);
                        Some(self.module.add_function("strncmp", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let cmp_result = self.builder.build_call(strncmp_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_suffix_ptr),
                    BasicMetadataValueEnum::PointerValue(suffix_ptr),
                    BasicMetadataValueEnum::IntValue(suffix_len),
                ], "strncmp_call")
                    .map_err(|e| format!("strncmp error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strncmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                let eq = self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp_result.into_int_value(), zero, "ends_with")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let eq_ext = self.builder.build_int_z_extend(eq, i64_ty, "ext")
                    .map_err(|e| format!("zext error: {}", e))?;
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // False path
                self.builder.position_at_end(false_bb);
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Merge
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(i64_ty, "result")
                    .map_err(|e| format!("phi error: {}", e))?;
                phi.add_incoming(&[
                    (&self.context.i64_type().const_int(0, false), false_bb),
                    (&eq_ext, check_bb),
                ]);
                Ok(phi.as_basic_value().into())
            }
            // ========== Math builtins ==========
            "pow" => {
                if args.len() != 2 { return Err("pow expects 2 arguments".into()); }
                let f64_ty = self.context.f64_type();
                let a = match args[0] {
                    BasicMetadataValueEnum::FloatValue(fv) => fv,
                    BasicMetadataValueEnum::IntValue(iv) => {
                        self.builder.build_signed_int_to_float(iv, f64_ty, "a_f64")
                            .map_err(|e| format!("int_to_float error: {}", e))?
                    }
                    _ => return Err("pow requires numeric arguments".into()),
                };
                let b = match args[1] {
                    BasicMetadataValueEnum::FloatValue(fv) => fv,
                    BasicMetadataValueEnum::IntValue(iv) => {
                        self.builder.build_signed_int_to_float(iv, f64_ty, "b_f64")
                            .map_err(|e| format!("int_to_float error: {}", e))?
                    }
                    _ => return Err("pow requires numeric arguments".into()),
                };
                let pow_fn = self.module.get_function("pow")
                    .or_else(|| {
                        let ty = f64_ty.fn_type(&[
                            BasicMetadataTypeEnum::FloatType(f64_ty),
                            BasicMetadataTypeEnum::FloatType(f64_ty),
                        ], false);
                        Some(self.module.add_function("pow", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let call = self.builder.build_call(pow_fn, &[
                    BasicMetadataValueEnum::FloatValue(a),
                    BasicMetadataValueEnum::FloatValue(b),
                ], "pow_call")
                    .map_err(|e| format!("pow error: {}", e))?;
                Ok(self.expect_basic_value(&call, "pow")?)
            }
            "random" => {
                // Use random() from libc (returns long, we use i64)
                let random_fn = self.module.get_function("random")
                    .or_else(|| {
                        let ty = self.context.i64_type().fn_type(&[], false);
                        Some(self.module.add_function("random", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let call = self.builder.build_call(random_fn, &[], "random_call")
                    .map_err(|e| format!("random error: {}", e))?;
                Ok(self.expect_basic_value(&call, "random")?)
            }
            "pi" => {
                // Return constant pi as f64
                Ok(self.context.f64_type().const_float(std::f64::consts::PI).into())
            }
            // ========== String parsing ==========
            "str_parse_int" | "to_int" => {
                if args.len() != 1 { return Err("str_parse_int/to_int expects 1 argument".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_parse_int: first arg must be string".into()),
                };
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // strtol(s, NULL, 10)
                let strtol_fn = self.module.get_function("strtol")
                    .or_else(|| {
                        let ty = self.context.i64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr.ptr_type(inkwell::AddressSpace::default())),
                            BasicMetadataTypeEnum::IntType(self.context.i32_type()),
                        ], false);
                        Some(self.module.add_function("strtol", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let null_ptr = i8_ptr.const_null();
                let call = self.builder.build_call(strtol_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(null_ptr),
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(10, false)),
                ], "strtol_call")
                    .map_err(|e| format!("strtol error: {}", e))?;
                Ok(self.expect_basic_value(&call, "strtol")?)
            }
            "str_parse_float" | "to_float" => {
                if args.len() != 1 { return Err("str_parse_float/to_float expects 1 argument".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_parse_float: first arg must be string".into()),
                };
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // strtod(s, NULL)
                let strtod_fn = self.module.get_function("strtod")
                    .or_else(|| {
                        let ty = self.context.f64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr.ptr_type(inkwell::AddressSpace::default())),
                        ], false);
                        Some(self.module.add_function("strtod", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let null_ptr = i8_ptr.const_null();
                let call = self.builder.build_call(strtod_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(null_ptr),
                ], "strtod_call")
                    .map_err(|e| format!("strtod error: {}", e))?;
                Ok(self.expect_basic_value(&call, "strtod")?)
            }
            "str_index_of" => {
                if args.len() != 2 { return Err("str_index_of expects 2 arguments".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_index_of: first arg must be string".into()),
                };
                let sub_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_index_of: second arg must be string".into()),
                };
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // strstr(s, sub) -> pointer or NULL
                let strstr_fn = self.module.get_function("strstr")
                    .or_else(|| {
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        Some(self.module.add_function("strstr", ty, Some(inkwell::module::Linkage::External)))
                    }).unwrap();
                let found = self.builder.build_call(strstr_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(sub_ptr),
                ], "strstr_call")
                    .map_err(|e| format!("strstr error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strstr returned void")?
                    .into_pointer_value();
                // found - s = index
                let found_int = self.builder.build_ptr_to_int(found, i64_ty, "found_int")
                    .map_err(|e| format!("ptr_to_int error: {}", e))?;
                let s_int = self.builder.build_ptr_to_int(s_ptr, i64_ty, "s_int")
                    .map_err(|e| format!("ptr_to_int error: {}", e))?;
                let idx = self.builder.build_int_sub(found_int, s_int, "index")
                    .map_err(|e| format!("sub error: {}", e))?;
                Ok(idx.into())
            }
            "str_repeat" => {
                if args.len() != 2 { return Err("str_repeat expects 2 arguments".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_repeat: first arg must be string".into()),
                };
                let n = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("str_repeat: second arg must be integer count".into()),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // strlen(s)
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "s_len")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                // total = s_len * n + 1 (null)
                let total = self.builder.build_int_mul(s_len, n, "total")
                    .map_err(|e| format!("mul error: {}", e))?;
                let one = i64_ty.const_int(1, false);
                let alloc_size = self.builder.build_int_add(total, one, "alloc_size")
                    .map_err(|e| format!("add error: {}", e))?;
                // malloc(total)
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // memcpy loop (simplified: one copy + multiple memcpy)
                // First copy: memcpy(buf, s, s_len)
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(s_len),
                ], "memcpy_first")
                    .map_err(|e| format!("memcpy error: {}", e))?;
                // For remaining repeats, copy from buf to buf+(i*s_len)
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "repeat_loop");
                let body_bb = self.context.append_basic_block(function, "repeat_body");
                let done_bb = self.context.append_basic_block(function, "repeat_done");
                // i = 1 (first copy already done)
                let i_alloca = self.builder.build_alloca(i64_ty, "ri")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(i_alloca, i64_ty.const_int(1, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let i = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "i")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, i, n, "repeat_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                // dst = buf + i * s_len
                let offset = self.builder.build_int_mul(i, s_len, "offset")
                    .map_err(|e| format!("mul error: {}", e))?;
                let dst = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[offset], "dst")
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(dst),
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(s_len),
                ], "memcpy_loop")
                    .map_err(|e| format!("memcpy error: {}", e))?;
                // i++
                let next = self.builder.build_int_add(i, i64_ty.const_int(1, false), "next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(i_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                // Null-terminate
                let null_pos = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[total], "null_pos")
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(null_pos, i8_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                // Return string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "repeat_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, total)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())
            }
            // ========== String transformation ==========
            "str_trim" => {
                if args.len() != 1 { return Err("str_trim expects 1 argument".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_trim: first arg must be string".into()),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // strlen(s)
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "strlen_call")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let zero = i64_ty.const_int(0, false);
                // Scan forward for first non-space
                let function = self.current_function().unwrap();
                let fwd_loop = self.context.append_basic_block(function, "trim_fwd");
                let fwd_body = self.context.append_basic_block(function, "trim_fwd_body");
                let fwd_done = self.context.append_basic_block(function, "trim_fwd_done");
                let start_alloca = self.builder.build_alloca(i64_ty, "start")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(start_alloca, zero)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(fwd_loop)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(fwd_loop);
                let start = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), start_alloca, "start")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let fwd_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, start, s_len, "fwd_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(fwd_cmp, fwd_body, fwd_done)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(fwd_body);
                let ch_ptr = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[start], "ch")
                }.map_err(|e| format!("gep error: {}", e))?;
                let ch = self.builder.build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr, "ch_val")
                    .map_err(|e| format!("load error: {}", e))?;
                // isspace check: ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r'
                let space = i8_ty.const_int(b' ' as u64, false);
                let tab = i8_ty.const_int(b'\t' as u64, false);
                let nl = i8_ty.const_int(b'\n' as u64, false);
                let cr = i8_ty.const_int(b'\r' as u64, false);
                let is_space = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch.into_int_value(), space, "is_space")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_tab = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch.into_int_value(), tab, "is_tab")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_nl = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch.into_int_value(), nl, "is_nl")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_cr = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch.into_int_value(), cr, "is_cr")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_ws1 = self.builder.build_or(is_space, is_tab, "is_ws1")
                    .map_err(|e| format!("or error: {}", e))?;
                let is_ws2 = self.builder.build_or(is_nl, is_cr, "is_ws2")
                    .map_err(|e| format!("or error: {}", e))?;
                let is_ws = self.builder.build_or(is_ws1, is_ws2, "is_ws")
                    .map_err(|e| format!("or error: {}", e))?;
                let next = self.builder.build_int_add(start, i64_ty.const_int(1, false), "next")
                    .map_err(|e| format!("add error: {}", e))?;
                // if is_ws: continue; else: done
                self.builder.build_store(start_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_conditional_branch(is_ws, fwd_loop, fwd_done)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(fwd_done);
                // Scan backward for last non-space
                let bwd_loop = self.context.append_basic_block(function, "trim_bwd");
                let bwd_body = self.context.append_basic_block(function, "trim_bwd_body");
                let bwd_done = self.context.append_basic_block(function, "trim_bwd_done");
                let end_alloca = self.builder.build_alloca(i64_ty, "end")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(end_alloca, s_len)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(bwd_loop)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(bwd_loop);
                let end = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), end_alloca, "end")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let bwd_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SGT, end, zero, "bwd_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(bwd_cmp, bwd_body, bwd_done)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(bwd_body);
                let prev = self.builder.build_int_sub(end, i64_ty.const_int(1, false), "prev")
                    .map_err(|e| format!("sub error: {}", e))?;
                let ch_ptr2 = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[prev], "ch")
                }.map_err(|e| format!("gep error: {}", e))?;
                let ch2 = self.builder.build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr2, "ch_val")
                    .map_err(|e| format!("load error: {}", e))?;
                let is_ws2_1 = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch2.into_int_value(), space, "is_space")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_ws2_2 = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch2.into_int_value(), tab, "is_tab")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_ws2_3 = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch2.into_int_value(), nl, "is_nl")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_ws2_4 = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch2.into_int_value(), cr, "is_cr")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_ws2a = self.builder.build_or(is_ws2_1, is_ws2_2, "is_ws_a")
                    .map_err(|e| format!("or error: {}", e))?;
                let is_ws2b = self.builder.build_or(is_ws2_3, is_ws2_4, "is_ws_b")
                    .map_err(|e| format!("or error: {}", e))?;
                let is_ws2 = self.builder.build_or(is_ws2a, is_ws2b, "is_ws")
                    .map_err(|e| format!("or error: {}", e))?;
                self.builder.build_store(end_alloca, prev)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_conditional_branch(is_ws2, bwd_loop, bwd_done)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(bwd_done);
                // result = substr(start, end - start)
                let trimmed_len = self.builder.build_int_sub(end, start, "trimmed_len")
                    .map_err(|e| format!("sub error: {}", e))?;
                // malloc + memcpy
                let alloc_size = self.builder.build_int_add(trimmed_len, i64_ty.const_int(1, false), "alloc_size")
                    .map_err(|e| format!("add error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let src = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[start], "src")
                }.map_err(|e| format!("gep error: {}", e))?;
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(src),
                    BasicMetadataValueEnum::IntValue(trimmed_len),
                ], "memcpy_call")
                    .map_err(|e| format!("memcpy error: {}", e))?;
                let null_pos = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[trimmed_len], "null")
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(null_pos, i8_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "trim_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, trimmed_len)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())
            }
            "str_to_upper" => {
                if args.len() != 1 { return Err("str_to_upper expects 1 argument".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_to_upper: first arg must be string".into()),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // strlen, malloc copy + toupper each char
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "strlen_call")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let alloc_size = self.builder.build_int_add(s_len, i64_ty.const_int(1, false), "alloc_size")
                    .map_err(|e| format!("add error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // Copy s to buf first, then transform
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "memcpy_call")
                    .map_err(|e| format!("memcpy error: {}", e))?;
                // Loop: for i = 0..s_len: if buf[i] in 'a'..'z', buf[i] -= 32
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "upper_loop");
                let body_bb = self.context.append_basic_block(function, "upper_body");
                let done_bb = self.context.append_basic_block(function, "upper_done");
                let i_alloca = self.builder.build_alloca(i64_ty, "ui")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(i_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let i = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "i")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, i, s_len, "upper_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                let ch_ptr = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[i], "ch")
                }.map_err(|e| format!("gep error: {}", e))?;
                let ch = self.builder.build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr, "ch_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                // Check 'a' <= ch <= 'z'
                let a = i8_ty.const_int(b'a' as u64, false);
                let z = i8_ty.const_int(b'z' as u64, false);
                let is_lower1 = self.builder.build_int_compare(inkwell::IntPredicate::SGE, ch, a, "ge_a")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_lower2 = self.builder.build_int_compare(inkwell::IntPredicate::SLE, ch, z, "le_z")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_lower = self.builder.build_and(is_lower1, is_lower2, "is_lower")
                    .map_err(|e| format!("and error: {}", e))?;
                let upper_ch = self.builder.build_int_sub(ch, i8_ty.const_int(32, false), "upper")
                    .map_err(|e| format!("sub error: {}", e))?;
                let result_ch = self.builder.build_select(is_lower, upper_ch, ch, "result_ch")
                    .map_err(|e| format!("select error: {}", e))?;
                self.builder.build_store(ch_ptr, result_ch)
                    .map_err(|e| format!("store error: {}", e))?;
                let next = self.builder.build_int_add(i, i64_ty.const_int(1, false), "next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(i_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                // Return string struct
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "upper_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, s_len)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())
            }
            "str_to_lower" => {
                if args.len() != 1 { return Err("str_to_lower expects 1 argument".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_to_lower: first arg must be string".into()),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let s_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ], "strlen_call")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let alloc_size = self.builder.build_int_add(s_len, i64_ty.const_int(1, false), "alloc_size")
                    .map_err(|e| format!("add error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "memcpy_call")
                    .map_err(|e| format!("memcpy error: {}", e))?;
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "lower_loop");
                let body_bb = self.context.append_basic_block(function, "lower_body");
                let done_bb = self.context.append_basic_block(function, "lower_done");
                let i_alloca = self.builder.build_alloca(i64_ty, "li")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(i_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let i = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "i")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, i, s_len, "lower_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                let ch_ptr = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[i], "ch")
                }.map_err(|e| format!("gep error: {}", e))?;
                let ch = self.builder.build_load(BasicTypeEnum::IntType(i8_ty), ch_ptr, "ch_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let a_up = i8_ty.const_int(b'A' as u64, false);
                let z_up = i8_ty.const_int(b'Z' as u64, false);
                let is_upper1 = self.builder.build_int_compare(inkwell::IntPredicate::SGE, ch, a_up, "ge_A")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_upper2 = self.builder.build_int_compare(inkwell::IntPredicate::SLE, ch, z_up, "le_Z")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let is_upper = self.builder.build_and(is_upper1, is_upper2, "is_upper")
                    .map_err(|e| format!("and error: {}", e))?;
                let lower_ch = self.builder.build_int_add(ch, i8_ty.const_int(32, false), "lower")
                    .map_err(|e| format!("add error: {}", e))?;
                let result_ch = self.builder.build_select(is_upper, lower_ch, ch, "result_ch")
                    .map_err(|e| format!("select error: {}", e))?;
                self.builder.build_store(ch_ptr, result_ch)
                    .map_err(|e| format!("store error: {}", e))?;
                let next = self.builder.build_int_add(i, i64_ty.const_int(1, false), "next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(i_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "lower_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, s_len)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())
            }
            "str_substring" => {
                if args.len() != 3 { return Err("str_substring expects 3 arguments (s, start, end)".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_substring: first arg must be string".into()),
                };
                let start = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("str_substring: second arg must be integer start".into()),
                };
                let end = match args[2] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("str_substring: third arg must be integer end".into()),
                };
                let i8_ty = self.context.i8_type();
                let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // len = end - start
                let sub_len = self.builder.build_int_sub(end, start, "sub_len")
                    .map_err(|e| format!("sub error: {}", e))?;
                let alloc_size = self.builder.build_int_add(sub_len, i64_ty.const_int(1, false), "alloc_size")
                    .map_err(|e| format!("add error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // src = s + start
                let src = unsafe {
                    self.builder.build_gep(i8_ty, s_ptr, &[start], "src")
                }.map_err(|e| format!("gep error: {}", e))?;
                // memcpy(buf, src, sub_len)
                let memcpy_fn = self.module.get_function("memcpy")
                    .ok_or_else(|| "memcpy not declared".to_string())?;
                self.builder.build_call(memcpy_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::PointerValue(src),
                    BasicMetadataValueEnum::IntValue(sub_len),
                ], "memcpy_call")
                    .map_err(|e| format!("memcpy error: {}", e))?;
                // Null-terminate
                let null_pos = unsafe {
                    self.builder.build_gep(i8_ty, buf, &[sub_len], "null")
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(null_pos, i8_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                // Build string struct
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "sub_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(len_gep, sub_len)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())
            }
            "contains" => {
                if args.len() != 2 { return Err("contains expects 2 arguments".into()); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("contains: first arg must be a list".into()),
                };
                let elem_val = args[1];
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // Get list length and data
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
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr), data_gep, "data"
                ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                // Loop through list elements
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "contains_loop");
                let body_bb = self.context.append_basic_block(function, "contains_body");
                let found_bb = self.context.append_basic_block(function, "contains_found");
                let done_bb = self.context.append_basic_block(function, "contains_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "ci")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len.into_int_value(), "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                let elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
                }.map_err(|e| format!("gep error: {}", e))?;
                let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
                    .map_err(|e| format!("load error: {}", e))?;
                let eq = match (elem, elem_val) {
                    (BasicValueEnum::IntValue(a), BasicMetadataValueEnum::IntValue(b)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, a, b, "eq")
                            .map_err(|e| format!("cmp error: {}", e))?
                    }
                    _ => return Err("contains: element comparison only supports i64 for now".into()),
                };
                self.builder.build_conditional_branch(eq, found_bb, loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Next iteration
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Found
                self.builder.position_at_end(found_bb);
                self.builder.build_unconditional_branch(done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Done: phi(true, false)
                self.builder.position_at_end(done_bb);
                let phi = self.builder.build_phi(i64_ty, "result")
                    .map_err(|e| format!("phi error: {}", e))?;
                phi.add_incoming(&[
                    (&i64_ty.const_int(1, false), found_bb),
                    (&i64_ty.const_int(0, false), loop_bb),
                ]);
                Ok(phi.as_basic_value().into())
            }
            "sum" => {
                if args.len() != 1 { return Err("sum expects 1 argument (list)".into()); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("sum: first arg must be a list".into()),
                };
                let i64_ty = self.context.i64_type();
                let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false));
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(self.context.i8_type().ptr_type(inkwell::AddressSpace::default())),
                    data_gep, "data"
                ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                // Loop through list elements and sum
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "sum_loop");
                let body_bb = self.context.append_basic_block(function, "sum_body");
                let done_bb = self.context.append_basic_block(function, "sum_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "si")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let sum_alloca = self.builder.build_alloca(i64_ty, "sum")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_store(sum_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                let elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
                }.map_err(|e| format!("gep error: {}", e))?;
                let elem = self.builder.build_load(i64_ty, elem_ptr, "elem_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let sum = self.builder.build_load(i64_ty, sum_alloca, "sum")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let new_sum = self.builder.build_int_add(sum, elem, "new_sum")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(sum_alloca, new_sum)
                    .map_err(|e| format!("store error: {}", e))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                let result = self.builder.build_load(i64_ty, sum_alloca, "result_sum")
                    .map_err(|e| format!("load error: {}", e))?;
                Ok(result.into())
            }
            "reverse" => {
                if args.len() != 1 { return Err("reverse expects 1 argument (list)".into()); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("reverse: first arg must be a list".into()),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false));
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr), data_gep, "data"
                ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                // Allocate new array
                let sizeof_i64 = i64_ty.const_int(8, false);
                let alloc_size = self.builder.build_int_mul(list_len, sizeof_i64, "alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let new_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let new_data_i64 = self.builder.build_bit_cast(new_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "new_data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                // Copy elements in reverse order
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "reverse_loop");
                let body_bb = self.context.append_basic_block(function, "reverse_body");
                let done_bb = self.context.append_basic_block(function, "reverse_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "ri")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                let idx_plus_1 = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "idx_plus_1")
                    .map_err(|e| format!("add error: {}", e))?;
                let src_idx = self.builder.build_int_sub(list_len, idx_plus_1, "src_idx")
                    .map_err(|e| format!("sub error: {}", e))?;
                let src_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr, &[src_idx], "src_elem")
                }.map_err(|e| format!("gep error: {}", e))?;
                let src_val = self.builder.build_load(i64_ty, src_ptr, "src_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let dst_ptr = unsafe {
                    self.builder.build_gep(i64_ty, new_data_i64, &[idx], "dst_elem")
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(dst_ptr, src_val)
                    .map_err(|e| format!("store error: {}", e))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                // Build result list struct
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "reversed_list")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "result_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(result_len_gep, list_len)
                    .map_err(|e| format!("store error: {}", e))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "result_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let new_data_void = self.builder.build_bit_cast(new_data,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "new_data_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(result_data_gep, new_data_void)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(result_alloca.into())
            }
            "flatten" => {
                if args.len() != 1 { return Err("flatten expects 1 argument (list of lists)".into()); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("flatten: first arg must be a list".into()),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty), list_ptr, 0, "outer_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let outer_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "outer_len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "outer_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr), data_gep, "outer_data"
                ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    list_struct_ty.ptr_type(inkwell::AddressSpace::default()), "data_list_ptr")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                // First pass: count total elements
                let function = self.current_function().unwrap();
                let count_loop_bb = self.context.append_basic_block(function, "flatten_count_loop");
                let count_body_bb = self.context.append_basic_block(function, "flatten_count_body");
                let count_done_bb = self.context.append_basic_block(function, "flatten_count_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "fi")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let total_alloca = self.builder.build_alloca(i64_ty, "total")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_store(total_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(count_loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(count_loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, outer_len, "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, count_body_bb, count_done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(count_body_bb);
                let inner_list_ptr = unsafe {
                    self.builder.build_gep(list_struct_ty, data_ptr, &[idx], "inner_list")
                }.map_err(|e| format!("gep error: {}", e))?;
                let inner_len_gep = self.builder.build_struct_gep(list_struct_ty, inner_list_ptr, 0, "inner_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let inner_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), inner_len_gep, "inner_len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let total = self.builder.build_load(i64_ty, total_alloca, "total")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let new_total = self.builder.build_int_add(total, inner_len, "new_total")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(total_alloca, new_total)
                    .map_err(|e| format!("store error: {}", e))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(count_loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(count_done_bb);
                let total_len = self.builder.build_load(i64_ty, total_alloca, "total_len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                // Allocate new array
                let sizeof_i64 = i64_ty.const_int(8, false);
                let alloc_size = self.builder.build_int_mul(total_len, sizeof_i64, "alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let new_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let new_data_i64 = self.builder.build_bit_cast(new_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "new_data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                // Second pass: copy elements
                let copy_outer_bb = self.context.append_basic_block(function, "flatten_copy_outer");
                let copy_outer_body_bb = self.context.append_basic_block(function, "flatten_copy_outer_body");
                let copy_inner_bb = self.context.append_basic_block(function, "flatten_copy_inner");
                let copy_inner_body_bb = self.context.append_basic_block(function, "flatten_copy_inner_body");
                let copy_done_bb = self.context.append_basic_block(function, "flatten_copy_done");
                let outer_idx_alloca = self.builder.build_alloca(i64_ty, "foi")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let inner_idx_alloca = self.builder.build_alloca(i64_ty, "fii")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let dest_idx_alloca = self.builder.build_alloca(i64_ty, "fdi")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(outer_idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_store(dest_idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(copy_outer_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(copy_outer_bb);
                let outer_idx = self.builder.build_load(i64_ty, outer_idx_alloca, "outer_idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let outer_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, outer_idx, outer_len, "outer_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(outer_cmp, copy_outer_body_bb, copy_done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(copy_outer_body_bb);
                let inner_list_ptr = unsafe {
                    self.builder.build_gep(list_struct_ty, data_ptr, &[outer_idx], "inner_list")
                }.map_err(|e| format!("gep error: {}", e))?;
                let inner_len_gep = self.builder.build_struct_gep(list_struct_ty, inner_list_ptr, 0, "inner_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let inner_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), inner_len_gep, "inner_len")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let inner_data_gep = self.builder.build_struct_gep(list_struct_ty, inner_list_ptr, 1, "inner_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let inner_data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr), inner_data_gep, "inner_data"
                ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let inner_data_ptr = self.builder.build_bit_cast(inner_data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "inner_data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                self.builder.build_store(inner_idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(copy_inner_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(copy_inner_bb);
                let inner_idx = self.builder.build_load(i64_ty, inner_idx_alloca, "inner_idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let inner_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, inner_idx, inner_len, "inner_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(inner_cmp, copy_inner_body_bb, copy_outer_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(copy_inner_body_bb);
                let src_ptr = unsafe {
                    self.builder.build_gep(i64_ty, inner_data_ptr, &[inner_idx], "inner_elem")
                }.map_err(|e| format!("gep error: {}", e))?;
                let src_val = self.builder.build_load(i64_ty, src_ptr, "inner_elem_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let dest_idx = self.builder.build_load(i64_ty, dest_idx_alloca, "dest_idx")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let dest_ptr = unsafe {
                    self.builder.build_gep(i64_ty, new_data_i64, &[dest_idx], "dest_elem")
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(dest_ptr, src_val)
                    .map_err(|e| format!("store error: {}", e))?;
                let next_dest = self.builder.build_int_add(dest_idx, i64_ty.const_int(1, false), "next_dest")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(dest_idx_alloca, next_dest)
                    .map_err(|e| format!("store error: {}", e))?;
                let next_inner = self.builder.build_int_add(inner_idx, i64_ty.const_int(1, false), "next_inner")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(inner_idx_alloca, next_inner)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(copy_inner_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // After inner loop: increment outer_idx and continue
                self.builder.position_at_end(copy_outer_bb);
                let next_outer = self.builder.build_int_add(outer_idx, i64_ty.const_int(1, false), "next_outer")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(outer_idx_alloca, next_outer)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.position_at_end(copy_done_bb);
                // Build result list struct
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "flattened_list")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "result_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(result_len_gep, total_len)
                    .map_err(|e| format!("store error: {}", e))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "result_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let new_data_void = self.builder.build_bit_cast(new_data,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "new_data_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(result_data_gep, new_data_void)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(result_alloca.into())
            }
"sort" => {
                if args.len() != 1 { return Err("sort expects 1 argument (list)".into()); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("sort: first arg must be a list".into()),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty.clone()), list_ptr, 0, "sort_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "sort_len_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty.clone(), list_ptr, 1, "sort_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "sort_data_val")
                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "sort_data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let sizeof_i64 = i64_ty.const_int(8, false);
                let alloc_size = self.builder.build_int_mul(list_len, sizeof_i64, "sort_alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let new_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "sort_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let _new_data_i64 = self.builder.build_bit_cast(new_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "sort_new_data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let function = self.current_function().unwrap();
                let outer_loop_bb = self.context.append_basic_block(function, "sort_outer_loop");
                let outer_body_bb = self.context.append_basic_block(function, "sort_outer_body");
                let inner_loop_bb = self.context.append_basic_block(function, "sort_inner_loop");
                let inner_body_bb = self.context.append_basic_block(function, "sort_inner_body");
                let done_bb = self.context.append_basic_block(function, "sort_done");
                let i_alloca = self.builder.build_alloca(i64_ty, "sort_i")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let j_alloca = self.builder.build_alloca(i64_ty, "sort_j")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(i_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(outer_loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(outer_loop_bb);
                let i_val = self.builder.build_load(i64_ty, i_alloca, "sort_i_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let list_len_minus_1 = self.builder.build_int_sub(list_len, i64_ty.const_int(1, false), "sort_len_minus_1")
                    .map_err(|e| format!("sub error: {}", e))?;
                let outer_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, i_val, list_len_minus_1, "sort_outer_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(outer_cmp, outer_body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(outer_body_bb);
                // j = 0
                self.builder.build_store(j_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(inner_loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(inner_loop_bb);
                let i_val_now = self.builder.build_load(i64_ty, i_alloca, "sort_i_now")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let j_val = self.builder.build_load(i64_ty, j_alloca, "sort_j_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                // inner bound: n - i - 1
                let inner_bound = self.builder.build_int_sub(list_len, i_val_now, "sort_inner_bound")
                    .map_err(|e| format!("sub error: {}", e))?;
                let inner_bound_minus_1 = self.builder.build_int_sub(inner_bound, i64_ty.const_int(1, false), "sort_inner_bound_minus_1")
                    .map_err(|e| format!("sub error: {}", e))?;
                let inner_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, j_val, inner_bound_minus_1, "sort_inner_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(inner_cmp, inner_body_bb, outer_loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(inner_body_bb);
                // Load arr[j] and arr[j+1]
                let elem_j_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[j_val], "sort_elem_j") }
                    .map_err(|e| format!("gep error: {}", e))?;
                let elem_j = self.builder.build_load(i64_ty, elem_j_ptr, "sort_elem_j_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let j_plus_1 = self.builder.build_int_add(j_val, i64_ty.const_int(1, false), "sort_j_plus_1")
                    .map_err(|e| format!("add error: {}", e))?;
                let elem_j1_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[j_plus_1], "sort_elem_j1") }
                    .map_err(|e| format!("gep error: {}", e))?;
                let elem_j1 = self.builder.build_load(i64_ty, elem_j1_ptr, "sort_elem_j1_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                // if arr[j] > arr[j+1], swap
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SGT, elem_j, elem_j1, "sort_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let swap_bb = self.context.append_basic_block(function, "sort_swap");
                let skip_swap_bb = self.context.append_basic_block(function, "sort_skip_swap");
                self.builder.build_conditional_branch(cmp, swap_bb, skip_swap_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(swap_bb);
                // swap arr[j] and arr[j+1]
                self.builder.build_store(elem_j_ptr, elem_j1)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_store(elem_j1_ptr, elem_j)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(skip_swap_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(skip_swap_bb);
                // j++
                let next_j = self.builder.build_int_add(j_val, i64_ty.const_int(1, false), "sort_next_j")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(j_alloca, next_j)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(inner_loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // After inner loop ends (j >= n-i-1), increment i and continue outer
                self.builder.position_at_end(outer_loop_bb);
                let i_next = self.builder.build_int_add(i_val, i64_ty.const_int(1, false), "sort_i_next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(i_alloca, i_next)
                    .map_err(|e| format!("store error: {}", e))?;
                // Build result list (data is already sorted in-place via swaps)
                self.builder.position_at_end(done_bb);
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "sort_result")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "sort_result_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(result_len_gep, list_len)
                    .map_err(|e| format!("store error: {}", e))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "sort_result_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_void = self.builder.build_bit_cast(data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "sort_data_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(result_data_gep, data_void)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(result_alloca.into())
            }
            "enumerate" => {
                if args.len() != 1 { return Err("enumerate expects 1 argument (list)".into()); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("enumerate: first arg must be a list".into()),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty), list_ptr, 0, "enum_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "enum_len_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "enum_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "enum_data_val")
                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "enum_data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let sizeof_pair = i64_ty.const_int(16, false);
                let alloc_size = self.builder.build_int_mul(list_len, sizeof_pair, "enum_alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let result_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "enum_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let result_data_i64 = self.builder.build_bit_cast(result_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "enum_result_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "enum_loop");
                let body_bb = self.context.append_basic_block(function, "enum_body");
                let done_bb = self.context.append_basic_block(function, "enum_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "enum_idx")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "enum_idx_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "enum_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                let elem_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[idx], "enum_elem") }
                    .map_err(|e| format!("gep error: {}", e))?;
                let elem = self.builder.build_load(i64_ty, elem_ptr, "enum_elem_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let idx_2 = self.builder.build_int_add(idx, idx, "enum_idx_2")
                    .map_err(|e| format!("add error: {}", e))?;
                let pair_index_ptr = unsafe { self.builder.build_gep(i64_ty, result_data_i64, &[idx_2], "enum_pair_index") }
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(pair_index_ptr, idx)
                    .map_err(|e| format!("store error: {}", e))?;
                let pair_value_ptr = unsafe { self.builder.build_gep(i64_ty, result_data_i64, &[self.builder.build_int_add(idx_2, i64_ty.const_int(1, false), "enum_idx_2_plus_1").map_err(|e| format!("add error: {}", e))?], "enum_pair_value") }
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(pair_value_ptr, elem)
                    .map_err(|e| format!("store error: {}", e))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "enum_next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "enum_result")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "enum_result_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(result_len_gep, list_len)
                    .map_err(|e| format!("store error: {}", e))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "enum_result_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let result_data_void = self.builder.build_bit_cast(result_data,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "enum_result_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(result_data_gep, result_data_void)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(result_alloca.into())
            }
            "zip" => {
                if args.len() != 2 { return Err("zip expects 2 arguments (list, list)".into()); }
                let (list_ptr_a, list_ptr_b) = match (&args[0], &args[1]) {
                    (BasicMetadataValueEnum::PointerValue(pv_a), BasicMetadataValueEnum::PointerValue(pv_b)) => (pv_a, pv_b),
                    _ => return Err("zip: both args must be lists".into()),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep_a = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty.clone()), *list_ptr_a, 0, "zip_len_a")
                    .map_err(|e| format!("gep error: {}", e))?;
                let len_a = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep_a, "zip_len_a_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let len_gep_b = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty.clone()), *list_ptr_b, 0, "zip_len_b")
                    .map_err(|e| format!("gep error: {}", e))?;
                let len_b = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep_b, "zip_len_b_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let min_len = self.builder.build_int_compare(inkwell::IntPredicate::SLT, len_a, len_b, "zip_min")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let min_len = self.builder.build_select(min_len, len_a, len_b, "zip_min_len")
                    .map_err(|e| format!("select error: {}", e))?
                    .into_int_value();
                let data_gep_a = self.builder.build_struct_gep(list_struct_ty.clone(), *list_ptr_a, 1, "zip_data_a")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8_a = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep_a, "zip_data_a_val")
                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr_a = self.builder.build_bit_cast(data_i8_a,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "zip_data_a_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let data_gep_b = self.builder.build_struct_gep(list_struct_ty, *list_ptr_b, 1, "zip_data_b")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8_b = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep_b, "zip_data_b_val")
                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr_b = self.builder.build_bit_cast(data_i8_b,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "zip_data_b_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let sizeof_pair = i64_ty.const_int(16, false);
                let alloc_size = self.builder.build_int_mul(min_len, sizeof_pair, "zip_alloc_size")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let result_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "zip_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let result_data_i64 = self.builder.build_bit_cast(result_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "zip_result_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "zip_loop");
                let body_bb = self.context.append_basic_block(function, "zip_body");
                let done_bb = self.context.append_basic_block(function, "zip_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "zip_idx")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "zip_idx_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, min_len, "zip_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                let elem_a_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr_a, &[idx], "zip_elem_a") }
                    .map_err(|e| format!("gep error: {}", e))?;
                let elem_a = self.builder.build_load(i64_ty, elem_a_ptr, "zip_elem_a_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let elem_b_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr_b, &[idx], "zip_elem_b") }
                    .map_err(|e| format!("gep error: {}", e))?;
                let elem_b = self.builder.build_load(i64_ty, elem_b_ptr, "zip_elem_b_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let idx_2 = self.builder.build_int_add(idx, idx, "zip_idx_2")
                    .map_err(|e| format!("add error: {}", e))?;
                let pair_a_ptr = unsafe { self.builder.build_gep(i64_ty, result_data_i64, &[idx_2], "zip_pair_a") }
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(pair_a_ptr, elem_a)
                    .map_err(|e| format!("store error: {}", e))?;
                let pair_b_ptr = unsafe { self.builder.build_gep(i64_ty, result_data_i64, &[self.builder.build_int_add(idx_2, i64_ty.const_int(1, false), "zip_idx_2_plus_1").map_err(|e| format!("add error: {}", e))?], "zip_pair_b") }
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(pair_b_ptr, elem_b)
                    .map_err(|e| format!("store error: {}", e))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "zip_next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "zip_result")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "zip_result_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(result_len_gep, min_len)
                    .map_err(|e| format!("store error: {}", e))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "zip_result_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let result_data_void = self.builder.build_bit_cast(result_data,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "zip_result_void")
                    .map_err(|e| format!("bitcast error: {}", e))?;
                self.builder.build_store(result_data_gep, result_data_void)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(result_alloca.into())
            }
            "str_split" => {
                if args.len() != 2 { return Err("str_split expects 2 arguments (string, delimiter)".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_split: first arg must be string".into()),
                };
                let delim_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_split: second arg must be string".into()),
                };
                let func = self.module.get_function("mimi_str_split")
                    .ok_or("mimi_str_split not declared")?;
                let result_ptr = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(delim_ptr),
                ], "str_split_call")
                    .map_err(|e| format!("str_split error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_str_split returned void")?
                    .into_pointer_value();
                // MimiList* is {i64 len, const char** data} — same layout as our list struct
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ], false);
                let list_ptr = self.builder.build_bit_cast(result_ptr,
                    list_struct_ty.ptr_type(inkwell::AddressSpace::default()), "list_ptr")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let len_val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len_val")
                    .map_err(|e| format!("load error: {}", e))?;
                let data_val = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data_val")
                    .map_err(|e| format!("load error: {}", e))?;
                let result_struct = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ], false);
                let result_alloca = self.builder.build_alloca(result_struct, "str_split_result")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let r_len_gep = self.builder.build_struct_gep(result_struct, result_alloca, 0, "r_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let r_data_gep = self.builder.build_struct_gep(result_struct, result_alloca, 1, "r_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(r_len_gep, len_val)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_store(r_data_gep, data_val)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(result_alloca.into())
            }
            "str_join" => {
                if args.len() != 2 { return Err("str_join expects 2 arguments (list, separator)".into()); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_join: first arg must be list".into()),
                };
                let sep_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_join: second arg must be string".into()),
                };
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // Bitcast list pointer to i8* for C function
                let c_list_ptr = self.builder.build_bit_cast(list_ptr,
                    i8_ptr, "c_list_ptr")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let func = self.module.get_function("mimi_str_join")
                    .ok_or("mimi_str_join not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(c_list_ptr),
                    BasicMetadataValueEnum::PointerValue(sep_ptr),
                ], "str_join_call")
                    .map_err(|e| format!("str_join error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_str_join returned void")?;
                Ok(result)
            }
            "str_replace" => {
                if args.len() != 3 { return Err("str_replace expects 3 arguments".into()); }
                let s_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_replace: first arg must be string".into()),
                };
                let from_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_replace: second arg must be string".into()),
                };
                let to_ptr = match args[2] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("str_replace: third arg must be string".into()),
                };
                let func = self.module.get_function("mimi_str_replace")
                    .ok_or("mimi_str_replace not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(from_ptr),
                    BasicMetadataValueEnum::PointerValue(to_ptr),
                ], "str_replace_call")
                    .map_err(|e| format!("str_replace error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_str_replace returned void")?;
                Ok(result)
            }
            // ========== Map/Record runtime functions ==========
            "map_new" => {
                let func = self.module.get_function("mimi_map_new")
                    .ok_or("mimi_map_new not declared")?;
                let result = self.builder.build_call(func, &[], "map_new_call")
                    .map_err(|e| format!("map_new error: {}", e))?;
                result.try_as_basic_value().left()
                    .ok_or("mimi_map_new returned void".to_string())
            }
            "map_size" => {
                if args.len() != 1 { return Err("map_size expects 1 argument".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_size: first arg must be i64 map handle".into()),
                };
                let func = self.module.get_function("mimi_map_size")
                    .ok_or("mimi_map_size not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                ], "map_size_call")
                    .map_err(|e| format!("map_size error: {}", e))?;
                result.try_as_basic_value().left()
                    .ok_or("mimi_map_size returned void".to_string())
            }
            "has_key" => {
                if args.len() != 2 { return Err("has_key expects 2 arguments (map, key)".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("has_key: first arg must be i64 map handle".into()),
                };
                let key_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("has_key: second arg must be string pointer".into()),
                };
                let func = self.module.get_function("mimi_map_has_key")
                    .ok_or("mimi_map_has_key not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "has_key_call")
                    .map_err(|e| format!("has_key error: {}", e))?;
                let int_val = result.try_as_basic_value().left()
                    .ok_or("mimi_map_has_key returned void".to_string())?
                    .into_int_value();
                let const_val = int_val.get_zero_extended_constant().unwrap_or(0);
                Ok(self.context.bool_type().const_int(const_val, false).into())
            }
            "map_get" => {
                if args.len() != 2 { return Err("map_get expects 2 arguments (map, key)".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_get: first arg must be i64 map handle".into()),
                };
                let key_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("map_get: second arg must be string pointer".into()),
                };
                let func = self.module.get_function("mimi_map_get")
                    .ok_or("mimi_map_get not declared")?;
                let value_handle = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "map_get_call")
                    .map_err(|e| format!("map_get error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_map_get returned void".to_string())?
                    .into_int_value();
                let has_key_func = self.module.get_function("mimi_map_has_key")
                    .ok_or("mimi_map_has_key not declared")?;
                let found_int = self.builder.build_call(has_key_func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "has_key_check")
                    .map_err(|e| format!("has_key error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_map_has_key returned void".to_string())?
                    .into_int_value();
                let i64_ty = self.context.i64_type();
                let tuple_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.bool_type()),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let tuple_alloca = self.builder.build_alloca(tuple_ty, "map_get_result")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let found_gep = self.builder.build_struct_gep(tuple_ty, tuple_alloca, 0, "found_field")
                    .map_err(|e| format!("gep error: {}", e))?;
                let found_val = self.builder.build_int_z_extend(found_int,
                    self.context.bool_type(), "found_ext")
                    .map_err(|e| format!("zext error: {}", e))?;
                self.builder.build_store(found_gep, found_val)
                    .map_err(|e| format!("store error: {}", e))?;
                let value_gep = self.builder.build_struct_gep(tuple_ty, tuple_alloca, 1, "value_field")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(value_gep, value_handle)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(tuple_alloca.into())
            }
            "map_set" => {
                if args.len() != 3 { return Err("map_set expects 3 arguments (map, key, value)".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_set: first arg must be i64 map handle".into()),
                };
                let key_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("map_set: second arg must be string pointer".into()),
                };
                let value_handle = match args[2] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_set: third arg must be i64 value handle".into()),
                };
                let func = self.module.get_function("mimi_map_set")
                    .ok_or("mimi_map_set not declared")?;
                self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                    BasicMetadataValueEnum::IntValue(value_handle),
                ], "map_set_call")
                    .map_err(|e| format!("map_set error: {}", e))?;
                let const_val = map_handle.get_zero_extended_constant().unwrap_or(0);
                Ok(self.context.i64_type().const_int(const_val, false).into())
            }
            "map_remove" => {
                if args.len() != 2 { return Err("map_remove expects 2 arguments (map, key)".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_remove: first arg must be i64 map handle".into()),
                };
                let key_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("map_remove: second arg must be string pointer".into()),
                };
                let func = self.module.get_function("mimi_map_remove")
                    .ok_or("mimi_map_remove not declared")?;
                self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "map_remove_call")
                    .map_err(|e| format!("map_remove error: {}", e))?;
                let const_val = map_handle.get_zero_extended_constant().unwrap_or(0);
                Ok(self.context.i64_type().const_int(const_val, false).into())
            }
            "map_from_list" => {
                if args.len() != 1 { return Err("map_from_list expects 1 argument".into()); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("map_from_list: first arg must be list pointer".into()),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "map_from_list_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "map_from_list_len_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "map_from_list_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "map_from_list_data_val")
                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "map_from_list_data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let sizeof_pair = i64_ty.const_int(16, false);
                let alloc_size = self.builder.build_int_mul(list_len, sizeof_pair, "map_from_list_alloc")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let keys_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "map_from_list_keys_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let values_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "map_from_list_values_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let keys_ptr = self.builder.build_bit_cast(keys_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "keys_ptr_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let values_ptr = self.builder.build_bit_cast(values_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "values_ptr_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "map_from_list_loop");
                let body_bb = self.context.append_basic_block(function, "map_from_list_body");
                let done_bb = self.context.append_basic_block(function, "map_from_list_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "map_from_list_idx")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "map_from_list_idx_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "map_from_list_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                let idx_2 = self.builder.build_int_add(idx, idx, "map_from_list_idx_2")
                    .map_err(|e| format!("add error: {}", e))?;
                let key_ptr_elem = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[idx_2], "map_from_list_key_elem") }
                    .map_err(|e| format!("gep error: {}", e))?;
                let key_handle = self.builder.build_load(i64_ty, key_ptr_elem, "map_from_list_key_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let key_dest = unsafe { self.builder.build_gep(i64_ty, keys_ptr, &[idx], "map_from_list_key_dest") }
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(key_dest, key_handle)
                    .map_err(|e| format!("store error: {}", e))?;
                let idx_2_plus_1 = self.builder.build_int_add(idx_2, i64_ty.const_int(1, false), "map_from_list_idx_2_plus_1")
                    .map_err(|e| format!("add error: {}", e))?;
                let val_ptr_elem = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[idx_2_plus_1], "map_from_list_val_elem") }
                    .map_err(|e| format!("gep error: {}", e))?;
                let val_handle = self.builder.build_load(i64_ty, val_ptr_elem, "map_from_list_val_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let val_dest = unsafe { self.builder.build_gep(i64_ty, values_ptr, &[idx], "map_from_list_val_dest") }
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(val_dest, val_handle)
                    .map_err(|e| format!("store error: {}", e))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "map_from_list_next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                let func = self.module.get_function("mimi_map_from_list")
                    .ok_or("mimi_map_from_list not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(keys_ptr),
                    BasicMetadataValueEnum::PointerValue(values_ptr),
                    BasicMetadataValueEnum::IntValue(list_len),
                ], "map_from_list_call")
                    .map_err(|e| format!("map_from_list error: {}", e))?;
                result.try_as_basic_value().left()
                    .ok_or("mimi_map_from_list returned void".to_string())
            }
            // ========== map/filter/reduce handled in compile_expr (compile-time) ==========
            "lexer" | "parse" => {
                // lexer/parse are runtime-only functions - generate a call to external runtime
                // These functions are not available in pure LLVM codegen
                Err(format!("'{}' is a runtime-only function, not available in codegen", name))
            }
            _ => Err(format!("builtin '{}' not yet implemented in codegen", name)),
        }
    }

    pub fn emit_ir(&self) -> String {
        self.module.print_to_string().to_string()
    }

    pub fn compile_to_object(&self, output_path: &Path) -> Result<(), String> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|e| format!("failed to initialize target: {}", e))?;
        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple)
            .map_err(|e| format!("failed to find target for triple '{}': {}", triple, e))?;
        let cpu = TargetMachine::get_host_cpu_name().to_string();
        let features = TargetMachine::get_host_cpu_features().to_string();
        let tm = target.create_target_machine(
            &triple,
            &cpu,
            &features,
            OptimizationLevel::Aggressive,
            RelocMode::Default,
            CodeModel::Default,
        ).ok_or_else(|| format!("failed to create target machine for triple '{}'", triple))?;

        tm.write_to_file(&self.module, inkwell::targets::FileType::Object, output_path)
            .map_err(|e| format!("failed to write object file: {}", e))
    }
}
