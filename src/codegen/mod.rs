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
        Self { context, module, builder, loop_break: None, loop_continue: None, type_defs: HashMap::new(), type_llvm: HashMap::new(), cap_vars: vec![HashMap::new()], type_map: HashMap::new(), func_defs: HashMap::new(), var_type_names: HashMap::new(), spawn_counter: 0, strict: false, compensation_blocks: Vec::new(), comp_scope_stack: Vec::new(), in_parasteps: false, parasteps_thread_ids: Vec::new(), trait_defs: HashMap::new(), type_impls: HashMap::new() }
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
        // First pass: collect type definitions and function definitions
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
            self.module.add_function(&ef.name, fn_type, Some(inkwell::module::Linkage::External));
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
                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");
                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, &mut vars)?;
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, &mut vars)?;
                    }
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::For { var, iterable, body } => {
                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
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
                Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Math(_) => {}
                _ => {}
            }
        }
        
        self.check_unconsumed_caps()?;
        self.pop_comp_scope();
        self.pop_cap_scope();
        
        if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
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

                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Then block
                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, &mut vars)?;
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    // Else block
                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, &mut vars)?;
                    }
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    // Continue at merge
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::While { cond, body } => {
                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.loop_break = old_break;
                    self.loop_continue = old_continue;

                    // Continue after loop
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::For { var, iterable, body } => {
                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                        let list_ptr = match iterable_val {
                            BasicValueEnum::PointerValue(pv) => pv,
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
                        let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                        let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                    self.compile_expr(expr, &vars)?;
                    // If the expression is a variable, mark it as consumed
                    if let Expr::Ident(name) = expr {
                        self.consume_cap(name)?;
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
                Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Math(_) => {
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

        if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
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

                    let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, vars)?;
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, vars)?;
                    }
                    if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
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
                Stmt::Desc(_) | Stmt::Requires(_) | Stmt::Ensures(_) | Stmt::Math(_) => {
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
                Lit::FString(_) => Ok(self.context.i64_type().const_int(0, false).into()),
            },
            Expr::Ident(name) => {
                if let Some(&(alloca, ty)) = vars.get(name) {
                    self.builder.build_load(ty, alloca, name)
                        .map_err(|e| format!("load error: {}", e))
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
                        self.compile_call(name, args, vars)
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

                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                let parent_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
            _ => Err(format!("unsupported expression in codegen: {:?}", expr)),
        }
    }

    /// Infer the type name of an object expression from the codegen's type definitions
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
                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                let function = self.builder.get_insert_block().unwrap().get_parent().unwrap();
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
                        Ok(call.try_as_basic_value().left().unwrap())
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
                Ok(call.try_as_basic_value().left().unwrap())
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
                // push(list, elem) - simplified: just return list pointer (no real mutation in codegen yet)
                if args.len() != 2 {
                    return Err("push expects 2 arguments".into());
                }
                // TODO: real push implementation with realloc
                match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => Ok(pv.into()),
                    _ => Err("push requires a list pointer".into()),
                }
            }
            "pop" => {
                if args.is_empty() {
                    return Err("pop expects 1 argument".into());
                }
                // TODO: real pop implementation
                Ok(self.context.i64_type().const_int(0, false).into())
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
                Ok(call.try_as_basic_value().left().unwrap())
            }
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
