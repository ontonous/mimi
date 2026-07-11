use crate::ast::*;
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};
use crate::span::Span;

use super::CodeGenerator;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{InitializationConfig, Target, TargetMachine};
use inkwell::OptimizationLevel;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn mangle_name(base: &str, type_map: &HashMap<String, crate::ast::Type>) -> String {
        if type_map.is_empty() {
            return base.to_string();
        }
        let mut parts: Vec<String> = type_map
            .iter()
            .map(|(k, v)| format!("{}_{}", k, crate::core::fmt_type(v)))
            .collect();
        parts.sort();
        format!("{}${}", base, parts.join("$"))
    }

    /// Resolve a type through the current type_map (substitute generic params)
    pub(super) fn resolve_type(&self, ty: &crate::ast::Type) -> crate::ast::Type {
        if self.type_map.is_empty() {
            return ty.clone();
        }
        let generics: Vec<crate::ast::GenericParam> = self
            .type_map
            .keys()
            .map(|k| crate::ast::GenericParam {
                name: k.clone(),
                bounds: vec![],
            })
            .collect();
        crate::core::subst_type_params(ty, &generics, &self.type_map)
    }

    /// Apply a handler to every item in `items`, recursing into modules.
    fn process_items<F>(items: &[Item], f: &mut F) -> MimiResult<()>
    where
        F: FnMut(&Item) -> MimiResult<()>,
    {
        for item in items {
            if let Item::Module(m) = item {
                for inner in &m.items {
                    f(inner)?;
                }
            } else {
                f(item)?;
            }
        }
        Ok(())
    }

    pub fn compile_file(&mut self, file: &File) -> MimiResult<()> {
        // Register built-in Record types used by builtins
        self.register_builtin_record_types()?;

        // v0.28.21 — Hold an owned copy of the file so `Expr::Comptime`
        // block folds can construct a fresh interpreter later, after
        // the original `&File` borrow has ended. The clone is shallow
        // w.r.t. String interning but acceptable at this scope.
        self.comptime_file = Some(std::rc::Rc::new(crate::ast::File {
            imports: file.imports.clone(),
            items: file.items.clone(),
        }));

        // v0.28.21 — Evaluate top-level `comptime func` and `const` items via the
        // interpreter and cache the results so `Expr::Comptime` blocks and
        // `comptime func name()` calls can fold to constants at codegen time.
        self.fold_comptime_items(file)?;

        // First pass: collect type definitions, function definitions, and cap definitions
        Self::process_items(&file.items, &mut |item| {
            match item {
                Item::Type(t) => {
                    self.register_type_def(t)?;
                }
                Item::Actor(actor) => {
                    self.register_actor_def(actor)?;
                }
                Item::Func(f) => {
                    self.func_defs.insert(f.name.clone(), f.clone());
                    if f.is_comptime {
                        self.comptime_func_names.insert(f.name.clone());
                    }
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
                    if !imp.type_args.is_empty() {
                        self.impl_type_args
                            .entry(imp.type_name.clone())
                            .or_insert_with(|| imp.type_args.clone());
                    }
                }
                Item::Const { name, value, .. } => {
                    // Store const for later reference
                    self.const_values.insert(name.clone(), value.clone());
                }
                Item::Flow(f) => {
                    // Register flow state payload types so record construction
                    // (e.g. `Zero { count: 0 }`) works in function codegen.
                    let qualified = format!("flow::{}", f.name);
                    for s in &f.states {
                        let type_name = format!("{}::{}", qualified, s.name);
                        let fields = s.payload.clone().unwrap_or_default();
                        let td = TypeDef {
                            name: type_name.clone(),
                            pub_: false,
                            kind: TypeDefKind::Record(fields),
                            generics: vec![],
                            derives: vec![],
                            attributes: vec![],
                        };
                        self.register_type_def(&td)?;
                        // Also register unqualified name (skip built-in names like "i32")
                        if !Self::is_builtin_type_name(&s.name)
                            && !self.type_defs.contains_key(&s.name)
                        {
                            let td = TypeDef {
                                name: s.name.clone(),
                                pub_: false,
                                kind: TypeDefKind::Record(s.payload.clone().unwrap_or_default()),
                                generics: vec![],
                                derives: vec![],
                                attributes: vec![],
                            };
                            self.register_type_def(&td)?;
                        }
                    }
                    // Cache the flow definition for transition compilation.
                    self.flow_defs.insert(f.name.clone(), f.clone());
                }
                _ => {}
            }
            Ok(())
        })?;
        // Second pass: register extern functions and external types
        Self::process_items(&file.items, &mut |item| {
            match item {
                Item::ExternBlock(block) => {
                    self.register_extern_block(block)?;
                }
                Item::Type(t) => {
                    self.register_type_def(t)?;
                }
                _ => {}
            }
            Ok(())
        })?;
        // v0.28.26 — Forward-declare all non-extern, non-async, non-comptime
        // user functions before any bodies are compiled. This lets functions
        // (including those in imported modules) call later-defined functions.
        // Iterate over file.items to keep declaration order deterministic and
        // match the order used for the rest of codegen.
        for item in &file.items {
            if let Item::Func(f) = item {
                if f.is_comptime || f.is_async || f.extern_abi.is_some() {
                    continue;
                }
                if matches!(f.ret, Some(Type::ImplTrait(_))) {
                    continue;
                }
                self.declare_func(f)?;
            }
        }
        // Forward-declare flow transitions so user functions can call them.
        {
            let flow_defs: Vec<FlowDef> = self.flow_defs.values().cloned().collect();
            for flow in &flow_defs {
                for t in &flow.transitions {
                    let func = Self::transition_to_func(flow, t);
                    self.func_defs.insert(func.name.clone(), func.clone());
                    self.declare_func(&func)?;
                }
            }
        }

        // Third pass: compile impl methods (needed before vtable construction)
        self.compile_impl_methods()?;
        // Fourth pass: compile vtables (needed before user function compilation)
        self.compile_vtables()?;
        // Fifth pass: compile user functions, actors, and flow transitions.
        // v0.28.21 — `comptime func` items are folded at codegen-start by
        // `fold_comptime_items` and intentionally NOT compiled to LLVM IR
        // (the caller resolves them via the cached `comptime_values` map,
        // so no runtime symbol is required for the function body).
        Self::process_items(&file.items, &mut |item| {
            match item {
                Item::Func(f) => {
                    if f.is_comptime {
                        // Skip — folded value lives in self.comptime_values.
                    } else {
                        self.compile_func(f).map_err(|e| e.at(Span::from(f.pos)))?;
                    }
                }
                Item::Actor(actor) => {
                    self.compile_actor(actor)?;
                }
                Item::Flow(f) => {
                    self.compile_flow(f)?;
                }
                _ => {}
            }
            Ok(())
        })?;
        // Warn about comptime functions that could not be compiled
        // (from external modules that were excluded)
        for item in &file.items {
            if let Item::Func(f) = item {
                if f.is_comptime {
                    eprintln!("warning: comptime function '{}' was not compiled", f.name);
                }
            }
        }
        Ok(())
    }

    /// Check if a name is a built-in Mimi type (should not be registered as a flow state type).
    fn is_builtin_type_name(name: &str) -> bool {
        matches!(
            name,
            "i32" | "i64" | "f32" | "f64" | "bool" | "string" | "unit" | "char" | "Int" | "Float"
                | "Bool" | "String" | "List" | "Option" | "Result" | "Set" | "Map"
        )
    }

    /// Mangle a flow transition into an ordinary function name.
    /// Format: `{FlowName}__{transition}__from_{FromState}`
    pub(super) fn transition_fn_name(flow: &str, transition: &str, from: &str) -> String {
        format!("{}__{}__from_{}", flow, transition, from)
    }

    /// Convert a flow transition into a synthetic FuncDef for codegen.
    ///
    /// Parameters: `self` (from-state payload) + event params.
    /// Return type: first declared to-state (multi-target uses first as nominal).
    /// Body: the transition body with outer `do { }` unwrapped (if present).
    pub(super) fn transition_to_func(flow: &FlowDef, t: &TransitionDef) -> FuncDef {
        let mut params = Vec::new();
        params.push(Param {
            name: "self".to_string(),
            ty: Type::Name(t.from_state.clone(), vec![]),
            mut_: false,
            default_value: None,
        });
        params.extend(t.params.iter().cloned());

        let ret_name = t
            .to_states
            .first()
            .cloned()
            .unwrap_or_else(|| "unit".to_string());

        // Unwrap a single outer `do { ... }` so compile_block sees normal stmts.
        let body: Block = match &t.body {
            Some(block) => {
                if block.len() == 1 {
                    if let Stmt::Do(inner) = &block[0] {
                        inner.clone()
                    } else {
                        block.clone()
                    }
                } else {
                    // Multiple top-level stmts: unwrap each Do, keep rest.
                    let mut out = Vec::new();
                    for stmt in block {
                        match stmt {
                            Stmt::Do(inner) => out.extend(inner.iter().cloned()),
                            other => out.push(other.clone()),
                        }
                    }
                    out
                }
            }
            None => Vec::new(),
        };

        FuncDef {
            name: Self::transition_fn_name(&flow.name, &t.name, &t.from_state),
            pub_: false,
            params,
            ret: Some(Type::Name(ret_name, vec![])),
            body,
            where_clause: vec![],
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            pos: t.pos,
        }
    }

    /// Compile all transitions of a flow as ordinary LLVM functions.
    pub(super) fn compile_flow(&mut self, flow: &FlowDef) -> MimiResult<()> {
        for t in &flow.transitions {
            if t.body.is_none() {
                continue; // abstract / protocol-style transition — no body
            }
            let func = Self::transition_to_func(flow, t);
            self.compile_func(&func)
                .map_err(|e| e.at(Span::from(t.pos)))?;
        }
        Ok(())
    }

    /// Register built-in Record types used by builtin functions (exec, file_stat, etc.)
    /// so that field access and struct construction work in codegen.
    fn register_builtin_record_types(&mut self) -> MimiResult<()> {
        use inkwell::types::BasicTypeEnum;
        let i32_ty = BasicTypeEnum::IntType(self.context.i32_type());
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
        let bool_ty = BasicTypeEnum::IntType(self.context.bool_type());
        let string_ty = {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            BasicTypeEnum::StructType(
                self.context
                    .struct_type(&[BasicTypeEnum::PointerType(i8_ptr), i64_ty], false),
            )
        };
        // ExecResult { exit_code: i32, stdout: string, stderr: string }
        if !self.type_defs.contains_key("ExecResult") {
            let exec_ty = crate::ast::TypeDef {
                name: "ExecResult".to_string(),
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        name: "exit_code".to_string(),
                        ty: crate::ast::Type::Name("i32".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "stdout".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "stderr".to_string(),
                        ty: crate::ast::Type::Name("string".to_string(), vec![]),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            let llvm_ty = BasicTypeEnum::StructType(
                self.context
                    .struct_type(&[i32_ty, string_ty, string_ty], false),
            );
            self.type_llvm.insert("ExecResult".to_string(), llvm_ty);
            self.type_defs.insert("ExecResult".to_string(), exec_ty);
        }
        // StatResult { size: i64, modified: i64, is_file: bool, is_dir: bool }
        if !self.type_defs.contains_key("StatResult") {
            let stat_ty = crate::ast::TypeDef {
                name: "StatResult".to_string(),
                pub_: false,
                kind: crate::ast::TypeDefKind::Record(vec![
                    crate::ast::Field {
                        name: "size".to_string(),
                        ty: crate::ast::Type::Name("i64".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "modified".to_string(),
                        ty: crate::ast::Type::Name("i64".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "is_file".to_string(),
                        ty: crate::ast::Type::Name("bool".to_string(), vec![]),
                    },
                    crate::ast::Field {
                        name: "is_dir".to_string(),
                        ty: crate::ast::Type::Name("bool".to_string(), vec![]),
                    },
                ]),
                generics: vec![],
                derives: vec![],
                attributes: vec![],
            };
            let llvm_ty = BasicTypeEnum::StructType(
                self.context
                    .struct_type(&[i64_ty, i64_ty, bool_ty, bool_ty], false),
            );
            self.type_llvm.insert("StatResult".to_string(), llvm_ty);
            self.type_defs.insert("StatResult".to_string(), stat_ty);
        }
        Ok(())
    }

    /// Run LLVM optimization passes on the module (O2).
    /// Called from compile_to_object during actual builds.
    pub fn optimize_module(&self) -> MimiResult<()> {
        if self.target_triple.is_some() {
            Target::initialize_all(&InitializationConfig::default());
        } else {
            Target::initialize_native(&InitializationConfig::default()).map_err(|e| {
                CompileError::LlvmError(format!("failed to initialize target: {}", e))
            })?;
        }
        let triple_str = self.target_triple.clone().unwrap_or_else(|| {
            TargetMachine::get_default_triple()
                .as_str()
                .to_string_lossy()
                .to_string()
        });
        let triple = inkwell::targets::TargetTriple::create(&triple_str);
        let target = Target::from_triple(&triple)
            .map_err(|e| CompileError::LlvmError(format!("failed to find target: {}", e)))?;
        let (cpu, features) = if self.target_triple.is_some() {
            (String::new(), String::new())
        } else {
            (
                TargetMachine::get_host_cpu_name().to_string(),
                TargetMachine::get_host_cpu_features().to_string(),
            )
        };
        let tm = target
            .create_target_machine(
                &triple,
                &cpu,
                &features,
                OptimizationLevel::Aggressive,
                inkwell::targets::RelocMode::Default,
                inkwell::targets::CodeModel::Default,
            )
            .ok_or_else(|| {
                CompileError::LlvmError("failed to create target machine".to_string())
            })?;
        let options = PassBuilderOptions::create();
        self.module
            .run_passes("default<O2>", &tm, options)
            .map_err(|e| CompileError::LlvmError(format!("optimization failed: {}", e)))
    }

    /// v0.28.21 — Walk top-level items and fold any `comptime func` or
    /// `const` declaration into `self.comptime_values` by running the
    /// interpreter. This is what allows `comptime { ... }` blocks and
    /// `comptime func name()` call sites in subsequent compilation to
    /// resolve to a constant value without re-evaluating the AST at
    /// codegen time.
    ///
    /// Errors from individual items are downgraded to `eprintln!`
    /// warnings so a single broken `comptime` declaration does not
    /// prevent the rest of the file from compiling. (This matches
    /// the v0.28.19 behaviour of warning-on-uncompilable-comptime.)
    fn fold_comptime_items(&mut self, _file: &File) -> MimiResult<()> {
        // Use the cloned file stored in self.comptime_file so the
        // interpreter can be created without re-borrowing the caller's
        // argument after `compile_file` has moved on.
        let file_ref = match &self.comptime_file {
            Some(rc) => rc.as_ref(),
            None => return Ok(()),
        };
        let mut interp = crate::interp::Interpreter::new(file_ref);
        // Drive the same `eval_comptime_funcs` step `Interpreter::run`
        // uses so we get a `comptime_results` map populated before any
        // user-level `Expr::Comptime` block is asked to fold.
        if let Err(e) = interp.eval_comptime_block(&Vec::new()) {
            eprintln!("warning: fold_comptime_items bootstrap: {}", e);
        }
        // Drain every pre-computed comptime result into the codegen cache.
        for (name, value) in interp.drain_comptime_results() {
            self.comptime_values.insert(name, value);
        }
        Ok(())
    }
}
