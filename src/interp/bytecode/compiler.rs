//! AST → Bytecode compiler.
//!
//! Compiles Mimi AST functions into `FunctionProto` bytecode.
//! Register allocation: variables are assigned registers at first use,
//! with free-list reuse on scope exit (register pressure reduction).

use super::instr::*;
use super::registry;
use crate::ast::*;
use crate::interp::error::InterpError;
use std::collections::HashMap;

/// Bytecode compiler: transforms AST functions into FunctionProto.
pub struct BytecodeCompiler {
    /// Function name → FuncIdx mapping (built during first pass).
    func_table: HashMap<String, FuncIdx>,
    /// Builtin name → BuiltinIdx mapping.
    builtin_table: HashMap<String, BuiltinIdx>,
    /// All compiled function prototypes.
    pub functions: Vec<FunctionProto>,
    /// Builtin names in index order.
    pub builtin_names: Vec<String>,
    /// Known enum variant names (for variant constructor resolution).
    variant_names: std::collections::HashSet<String>,
    /// Known newtype names (for newtype constructor resolution).
    newtype_names: std::collections::HashSet<String>,
    /// Known impl type names (for method resolution prefixes).
    impl_type_names: Vec<String>,
    /// Constant name → value expression (for Item::Const resolution).
    constants: HashMap<String, Expr>,
    /// Known flow names (for transition call resolution).
    flow_names: std::collections::HashSet<String>,
    /// Flow persistent field names: flow_name → fields (fault shadowing).
    flow_persistent: std::collections::HashMap<String, Vec<String>>,
    /// Flow typed-fault error type: flow_name → error type name (from `fault T`).
    flow_fault_type: std::collections::HashMap<String, String>,
    /// Type definitions (for type_fields / type_variants).
    type_defs: std::collections::HashMap<String, crate::ast::TypeDefKind>,
    /// Known actor names (for spawn resolution).
    actor_names: std::collections::HashSet<String>,
    /// Known extern function names (for clear error messages).
    extern_names: std::collections::HashSet<String>,
    /// Ordered extern names for Op::CallExtern indexing (0.33 Phase D).
    extern_name_order: Vec<String>,
    /// Known capability names (for cap value construction).
    cap_names: std::collections::HashSet<String>,
    /// Capability components: cap_name → [component_names] (for combined caps).
    cap_components: HashMap<String, Vec<String>>,
    /// Type hints from CheckedProgram: function name → parameter VarTypes.
    /// Eliminates expr_is_float heuristics for parameters (G1).
    type_hints: HashMap<String, Vec<VarType>>,
    /// Method resolution table from CheckedProgram impls:
    /// (type_name, method_name) → mangled function name.
    /// Eliminates string prefix guessing (G1).
    method_table: HashMap<(String, String), String>,
    /// Flow definitions (for transition compilation).
    flow_defs: HashMap<String, FlowDef>,
    /// Actor definitions (for runtime spawn).
    actor_defs: HashMap<String, ActorDef>,
    /// Flow transition function indices: (flow, transition, from_state) → FuncIdx.
    flow_transition_funcs: HashMap<(String, String, String), FuncIdx>,
    /// Transitions with `fails` clause.
    flow_fails_transitions: std::collections::HashSet<(String, String, String)>,
    /// Actor method function indices: (actor_name, method_name) → FuncIdx.
    actor_method_funcs: HashMap<(String, String), FuncIdx>,
    /// The original AST file (stored for actor worker threads).
    ast_file: Option<std::sync::Arc<File>>,
    /// Default parameter values: function name → per-param default expr (None = required).
    func_defaults: HashMap<String, Vec<Option<Expr>>>,
    /// Parameter names: function name → ordered param names (for named arg reordering).
    func_param_names: HashMap<String, Vec<String>>,
    /// Type aliases: alias name → aliased Type (for from_json::<T> resolution).
    type_aliases: HashMap<String, Type>,
    /// Record field types: type_name → [(field_name, field_type_str)].
    record_fields: HashMap<String, Vec<(String, String)>>,
}

/// Per-function compilation state.
struct FuncCompiler {
    /// The prototype being built.
    proto: FunctionProto,
    /// Variable name → register mapping (current scope chain).
    vars: Vec<HashMap<String, Reg>>,
    /// Variable name → known type tag (for int/float dispatch without CheckedProgram).
    var_types: HashMap<String, VarType>,
    /// Variable name → mutability (immutable by default; `let mut` and
    /// `mut` params are true). Mirrors tree-walker runtime check in
    /// scope_env::assign (compile-time here).
    var_mut: HashMap<String, bool>,
    /// Break jump sites for the current loop (patched on loop exit).
    break_jumps: Vec<Vec<usize>>,
    /// Continue jump sites for the current loop (patched to loop head/increment).
    continue_jumps: Vec<Vec<usize>>,
    /// Current source line (1-based) for line_table population (D12).
    current_line: u32,
    /// Free registers available for reuse (register pressure reduction).
    free_regs: Vec<Reg>,
    /// Registers allocated per scope (for reclaim on pop_scope).
    scope_regs: Vec<Vec<Reg>>,
    /// Deferred blocks per scope (LIFO execution at scope exit).
    defer_scopes: Vec<Vec<Block>>,
    /// OnFailure blocks per scope (LIFO execution on fault at scope exit).
    on_failure_scopes: Vec<Vec<Block>>,
    /// Instruction index of SetFaultPc for the current scope (for patching).
    set_fault_pc_idx: Option<usize>,
    /// Borrow aliases: variable name → the original place expression it borrows.
    /// Used to resolve `*alias = value` write-back through mutable references.
    borrow_aliases: HashMap<String, Expr>,
    /// Loop result registers: one per active loop (for break-with-value).
    /// `break expr` writes to the top register; the loop expression reads it.
    loop_result_regs: Vec<Reg>,
}

/// Lightweight type tag for register dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum VarType {
    Int,
    Float,
    Bool,
    String,
    /// User-defined type (named), prevents fallback to Int operations.
    User(String),
    /// `dyn Trait` receiver: method dispatch happens at runtime by the
    /// concrete record type (tree-walker Value::DynTrait semantics).
    Dyn(String),
    Unknown,
}

impl FuncCompiler {
    fn new(name: String, param_count: u16) -> Self {
        FuncCompiler {
            proto: FunctionProto::new(name, param_count),
            vars: vec![HashMap::new()],
            var_types: HashMap::new(),
            var_mut: HashMap::new(),
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            current_line: 0,
            free_regs: Vec::new(),
            scope_regs: vec![Vec::new()],
            defer_scopes: vec![Vec::new()],
            on_failure_scopes: vec![Vec::new()],
            set_fault_pc_idx: None,
            borrow_aliases: HashMap::new(),
            loop_result_regs: Vec::new(),
        }
    }

    fn push_scope(&mut self) {
        self.vars.push(HashMap::new());
        self.scope_regs.push(Vec::new());
        self.defer_scopes.push(Vec::new());
        self.on_failure_scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        self.vars.pop();
        if let Some(regs) = self.scope_regs.pop() {
            self.free_regs.extend(regs);
        }
        self.defer_scopes.pop();
        self.on_failure_scopes.pop();
    }

    /// Innermost variable scope — invariant: at least one scope is always
    /// active (pushed by `push_scope` / constructor, popped by `pop_scope`).
    #[inline]
    fn vars_mut(&mut self) -> &mut HashMap<String, Reg> {
        mimi_debug_assert!(!self.vars.is_empty(), "bytecode: no active variable scope");
        match self.vars.last_mut() {
            Some(v) => v,
            None => {
                unreachable!("bytecode: no active variable scope (compiler invariant violated)")
            }
        }
    }

    /// Break-jump sites for the innermost loop.
    #[inline]
    fn break_jumps_mut(&mut self) -> &mut Vec<usize> {
        mimi_debug_assert!(!self.break_jumps.is_empty(), "bytecode: break outside loop");
        match self.break_jumps.last_mut() {
            Some(v) => v,
            None => unreachable!("bytecode: break outside loop (compiler invariant violated)"),
        }
    }

    /// Continue-jump sites for the innermost loop.
    #[inline]
    fn continue_jumps_mut(&mut self) -> &mut Vec<usize> {
        mimi_debug_assert!(
            !self.continue_jumps.is_empty(),
            "bytecode: continue outside loop"
        );
        match self.continue_jumps.last_mut() {
            Some(v) => v,
            None => unreachable!("bytecode: continue outside loop (compiler invariant violated)"),
        }
    }

    /// Deferred blocks for the innermost scope.
    #[inline]
    fn defer_scopes_mut(&mut self) -> &mut Vec<Block> {
        mimi_debug_assert!(
            !self.defer_scopes.is_empty(),
            "bytecode: no active defer scope"
        );
        match self.defer_scopes.last_mut() {
            Some(v) => v,
            None => unreachable!("bytecode: no active defer scope (compiler invariant violated)"),
        }
    }

    /// OnFailure blocks for the innermost scope.
    #[inline]
    fn on_failure_scopes_mut(&mut self) -> &mut Vec<Block> {
        mimi_debug_assert!(
            !self.on_failure_scopes.is_empty(),
            "bytecode: no active OnFailure scope"
        );
        match self.on_failure_scopes.last_mut() {
            Some(v) => v,
            None => {
                unreachable!("bytecode: no active OnFailure scope (compiler invariant violated)")
            }
        }
    }

    /// Look up a variable's register, searching innermost → outermost.
    fn lookup_var(&self, name: &str) -> Option<Reg> {
        for scope in self.vars.iter().rev() {
            if let Some(&r) = scope.get(name) {
                return Some(r);
            }
        }
        None
    }

    /// Bind a variable name to a register (reuses free registers when available).
    fn bind_var(&mut self, name: &str) -> Reg {
        let r = if let Some(free) = self.free_regs.pop() {
            free
        } else {
            self.proto.alloc_reg()
        };
        self.vars_mut().insert(name.to_string(), r);
        // Track for scope-based reclaim.
        if let Some(scope) = self.scope_regs.last_mut() {
            scope.push(r);
        }
        r
    }

    /// Get or create a register for a variable.
    fn get_or_bind(&mut self, name: &str) -> Reg {
        if let Some(r) = self.lookup_var(name) {
            r
        } else {
            self.bind_var(name)
        }
    }

    fn emit(&mut self, op: Op) -> usize {
        self.proto.line_table.push(self.current_line);
        self.proto.emit(op)
    }

    /// Set the current source line from an AST node's span (D12).
    fn set_line_from_meta(&mut self, meta: Option<AstNodeMeta>) {
        if let Some(m) = meta {
            self.current_line = m.span.start_line as u32;
        }
    }

    /// Record the inferred type of a register for int/float dispatch.
    fn set_reg_type(&mut self, name: &str, ty: VarType) {
        self.var_types.insert(name.to_string(), ty);
    }

    /// Check if a register is known to hold a float.
    fn reg_is_float(&self, name: &str) -> bool {
        self.var_types.get(name) == Some(&VarType::Float)
    }

    /// Check if a register is known to hold a string.
    fn reg_is_string(&self, name: &str) -> bool {
        self.var_types.get(name) == Some(&VarType::String)
    }

    /// Record mutability of a bound variable (`let mut` or `mut` param).
    fn set_var_mut(&mut self, name: &str, is_mut: bool) {
        self.var_mut.insert(name.to_string(), is_mut);
    }

    /// True when the variable is known mutable. Unknown → immutable
    /// (tree-walker default is immutable unless explicitly `mut`).
    fn var_is_mut(&self, name: &str) -> bool {
        self.var_mut.get(name).copied().unwrap_or(false)
    }

    /// True when the variable holds a `dyn Trait` value. Method calls on
    /// such receivers must dispatch at runtime by concrete type, not by
    /// static impl-name prefix matching.
    fn var_is_dyn(&self, name: &str) -> bool {
        matches!(self.var_types.get(name), Some(VarType::Dyn(_)))
    }
}

impl BytecodeCompiler {
    pub fn new() -> Self {
        BytecodeCompiler {
            func_table: HashMap::new(),
            builtin_table: HashMap::new(),
            functions: Vec::new(),
            builtin_names: Vec::new(),
            variant_names: std::collections::HashSet::new(),
            newtype_names: std::collections::HashSet::new(),
            impl_type_names: Vec::new(),
            constants: HashMap::new(),
            flow_names: std::collections::HashSet::new(),
            actor_names: std::collections::HashSet::new(),
            extern_names: std::collections::HashSet::new(),
            extern_name_order: Vec::new(),
            cap_names: std::collections::HashSet::new(),
            cap_components: HashMap::new(),
            type_hints: HashMap::new(),
            method_table: HashMap::new(),
            flow_defs: HashMap::new(),
            actor_defs: HashMap::new(),
            flow_transition_funcs: HashMap::new(),
            flow_fails_transitions: std::collections::HashSet::new(),
            flow_persistent: HashMap::new(),
            flow_fault_type: HashMap::new(),
            type_defs: HashMap::new(),
            actor_method_funcs: HashMap::new(),
            ast_file: None,
            func_defaults: HashMap::new(),
            func_param_names: HashMap::new(),
            type_aliases: HashMap::new(),
            record_fields: HashMap::new(),
        }
    }

    /// Install type information from CheckedProgram (G1 integration).
    /// Populates `type_hints` (parameter types) and `method_table` (impl dispatch).
    /// Call this BEFORE `compile_file` to enable type-directed compilation.
    pub fn install_checked_program(&mut self, program: &crate::core::CheckedProgram) {
        // Extract parameter types for each function.
        for func in program.functions().values() {
            let param_types: Vec<VarType> = func
                .params
                .iter()
                .map(|(_, ty)| surface_type_to_var_type(ty))
                .collect();
            self.type_hints
                .insert(func.qualified_name.clone(), param_types);
        }

        // Build method resolution table from impls.
        for impl_def in program.impls().values() {
            for method_name in &impl_def.methods {
                let mangled = format!("{}_{}", impl_def.type_name, method_name);
                self.method_table
                    .insert((impl_def.type_name.clone(), method_name.clone()), mangled);
            }
        }
    }

    /// Compile a full AST file into a BytecodeProgram.
    pub fn compile_file(&mut self, file: &File) -> Result<BytecodeProgram, InterpError> {
        // Store AST for actor worker threads.
        self.ast_file = Some(std::sync::Arc::new(file.clone()));

        // Pass 1: register all function names + collect variant/actor/flow names.
        for item in &file.items {
            if let Item::Func(f) = item {
                let idx = self.functions.len() as FuncIdx;
                self.func_table.insert(f.name.clone(), idx);
                // Collect default parameter values and param names for call-site handling.
                let defaults: Vec<Option<Expr>> =
                    f.params.iter().map(|p| p.default_value.clone()).collect();
                if defaults.iter().any(|d| d.is_some()) {
                    self.func_defaults.insert(f.name.clone(), defaults);
                }
                let param_names: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
                self.func_param_names.insert(f.name.clone(), param_names);
                // Push placeholder.
                self.functions
                    .push(FunctionProto::new(f.name.clone(), f.params.len() as u16));
                // Fill mutate-param metadata now (call sites may reference
                // this function before its body is compiled).
                let proto = &mut self.functions[idx as usize];
                proto.has_mut_params = f.params.iter().any(|p| p.mut_);
                proto.mut_param_indices = f
                    .params
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.mut_)
                    .map(|(i, _)| i as u16)
                    .collect();
            }
            // Module functions are registered under their qualified path
            // (Module::sub::name) so `M::f(...)` calls resolve like top-level
            // functions (tree-walker `build_qualified_path` parity).
            if let Item::Module(m) = item {
                self.collect_module_funcs(m, "");
            }
            // Collect enum variant names and newtype names for constructor resolution.
            if let Item::Type(td) = item {
                self.type_defs.insert(td.name.clone(), td.kind.clone());
                match &td.kind {
                    TypeDefKind::Enum(variants) => {
                        for variant in variants {
                            self.variant_names.insert(variant.name.clone());
                        }
                    }
                    TypeDefKind::Newtype(_) => {
                        self.newtype_names.insert(td.name.clone());
                    }
                    TypeDefKind::Alias(ty) => {
                        self.type_aliases.insert(td.name.clone(), ty.clone());
                    }
                    TypeDefKind::Record(fields) => {
                        let field_types: Vec<(String, String)> = fields
                            .iter()
                            .map(|f| {
                                let resolved = self.resolve_type(&f.ty);
                                (f.name.clone(), crate::core::fmt_type(&resolved))
                            })
                            .collect();
                        self.record_fields.insert(td.name.clone(), field_types);
                    }
                    _ => {}
                }
            }
            // Collect constants for inline resolution.
            if let Item::Const { name, value, .. } = item {
                self.constants.insert(name.clone(), value.clone());
            }
            // Collect flow definitions.
            if let Item::Flow(f) = item {
                self.flow_names.insert(f.name.clone());
                self.flow_defs.insert(f.name.clone(), f.clone());
                // Fault shadowing metadata (v0.29.12/14).
                if !f.persistent_fields.is_empty() {
                    self.flow_persistent
                        .insert(f.name.clone(), f.persistent_fields.clone());
                }
                // Typed-fault error type (v0.34.18b): absorbed panics add a
                // defaulted `error` field matching the codegen backend.
                if let Some(ft) = &f.fault_type {
                    let resolved = self.resolve_type(ft);
                    self.flow_fault_type
                        .insert(f.name.clone(), crate::core::fmt_type(&resolved));
                }
            }
            // Collect actor definitions.
            if let Item::Actor(a) = item {
                self.actor_names.insert(a.name.clone());
                self.actor_defs.insert(a.name.clone(), a.clone());
            }
            // Collect extern function names (for call dispatch + indexing).
            if let Item::ExternBlock(block) = item {
                for f in &block.funcs {
                    if self.extern_names.insert(f.name.clone()) {
                        self.extern_name_order.push(f.name.clone());
                    }
                }
            }
            // Collect capability names (for cap value construction).
            if let Item::Cap(cap) = item {
                self.cap_names.insert(cap.name.clone());
                // Store components: combined caps have [part1, part2], simple caps have [name].
                let components = if let Some(ref combined) = cap.combined_with {
                    // Parse "A + B" format (matches tree-walker collect_caps).
                    let parts: Vec<String> = combined
                        .split(" + ")
                        .map(|s| s.trim().to_string())
                        .collect();
                    if parts.len() > 1 {
                        parts
                    } else {
                        vec![cap.name.clone(), combined.clone()]
                    }
                } else {
                    vec![cap.name.clone()]
                };
                self.cap_components.insert(cap.name.clone(), components);
            }
        }

        // Pass 1.5: register impl method names (must precede body compilation
        // so that method calls in function bodies can resolve mangled names).
        for item in &file.items {
            if let Item::Impl(impl_def) = item {
                // Collect impl type name for method resolution.
                if !self.impl_type_names.contains(&impl_def.type_name) {
                    self.impl_type_names.push(impl_def.type_name.clone());
                }
                for method in &impl_def.methods {
                    let mangled_name = format!("{}_{}", impl_def.type_name, method.name);
                    let idx = self.functions.len() as FuncIdx;
                    self.func_table.insert(mangled_name.clone(), idx);
                    // +1 for implicit `self` parameter.
                    self.functions.push(FunctionProto::new(
                        mangled_name,
                        method.params.len() as u16 + 1,
                    ));
                }
            }
        }

        // Register builtins from the canonical registry (D1: single source of truth).
        let reg = registry::create_registry();
        for name in reg.names() {
            self.register_builtin(&name);
        }

        // Pass 2: compile each function body.
        for item in &file.items {
            if let Item::Func(f) = item {
                let idx = self.func_table[&f.name];
                let proto = self.compile_func(f)?;
                self.functions[idx as usize] = proto;
            }
            if let Item::Module(m) = item {
                self.compile_module_funcs(m, "")?;
            }
        }

        // Pass 3: compile impl method bodies.
        for item in &file.items {
            if let Item::Impl(impl_def) = item {
                for method in &impl_def.methods {
                    let mangled_name = format!("{}_{}", impl_def.type_name, method.name);
                    if let Some(&idx) = self.func_table.get(&mangled_name) {
                        let mut proto = self.compile_func_impl(method)?;
                        // Runtime dyn dispatch looks methods up by mangled
                        // name ({Type}_{method}) via function_index.
                        proto.name = mangled_name;
                        self.functions[idx as usize] = proto;
                    }
                }
            }
        }

        // Pass 4: compile flow transition bodies as functions.
        // Each transition becomes a function: __flow_{Flow}_{transition}_{from_state}
        // Parameters: self (from-state value) + transition params.
        for item in &file.items {
            if let Item::Flow(flow) = item {
                for t in &flow.transitions {
                    if let Some(body) = &t.body {
                        let func_name = format!("__flow_{}_{}_{}", flow.name, t.name, t.from_state);
                        let param_count = 1 + t.params.len(); // self + params
                        let idx = self.functions.len() as FuncIdx;
                        self.func_table.insert(func_name.clone(), idx);
                        self.functions
                            .push(FunctionProto::new(func_name.clone(), param_count as u16));
                        self.flow_transition_funcs.insert(
                            (flow.name.clone(), t.name.clone(), t.from_state.clone()),
                            idx,
                        );
                        // Track transitions with `fails` clause.
                        if t.fails.is_some() {
                            self.flow_fails_transitions.insert((
                                flow.name.clone(),
                                t.name.clone(),
                                t.from_state.clone(),
                            ));
                        }
                        // Compile the transition body.
                        let mut fc = FuncCompiler::new(func_name, param_count as u16);
                        // Bind `self` to register 0 (direct insert, like compile_func).
                        fc.vars[0].insert("self".to_string(), 0);
                        // Bind transition params to registers 1..n.
                        for (i, p) in t.params.iter().enumerate() {
                            fc.vars[0].insert(p.name.clone(), (i + 1) as Reg);
                        }
                        let last_reg = self.compile_block(&mut fc, body)?;
                        // Return the last expression's value, or Unit if none
                        // (mirrors Pass-5 actor method pattern at lines 351-360).
                        if let Some(r) = last_reg {
                            fc.emit(Op::Ret { ra: r });
                        } else {
                            let r_unit = fc.proto.alloc_reg();
                            let unit_idx = fc.proto.add_const(ConstValue::Unit);
                            fc.emit(Op::LoadConst {
                                rd: r_unit,
                                idx: unit_idx,
                            });
                            fc.emit(Op::Ret { ra: r_unit });
                        }
                        let proto = fc.proto;
                        self.functions[idx as usize] = proto;
                    }
                }
            }
        }

        // Pass 5: compile actor method bodies as functions.
        // Each method becomes: __actor_{ActorName}_{method}
        // Parameters: implicit self (register 0) + explicit params.
        for item in &file.items {
            if let Item::Actor(actor) = item {
                for method in &actor.methods {
                    let func_name = format!("__actor_{}_{}", actor.name, method.name);
                    let param_count = 1 + method.params.len(); // self + params
                    let idx = self.functions.len() as FuncIdx;
                    self.func_table.insert(func_name.clone(), idx);
                    self.functions
                        .push(FunctionProto::new(func_name.clone(), param_count as u16));
                    self.actor_method_funcs
                        .insert((actor.name.clone(), method.name.clone()), idx);
                    // Compile the method body (like compile_func_impl).
                    let mut fc = FuncCompiler::new(func_name, param_count as u16);
                    fc.vars[0].insert("self".to_string(), 0);
                    for (i, p) in method.params.iter().enumerate() {
                        fc.vars[0].insert(p.name.clone(), (i + 1) as Reg);
                    }
                    let last_reg = self.compile_block(&mut fc, &method.body)?;
                    // Return the last expression's value, or Unit if none.
                    if let Some(r) = last_reg {
                        fc.emit(Op::Ret { ra: r });
                    } else {
                        let r_unit = fc.proto.alloc_reg();
                        let unit_idx = fc.proto.add_const(ConstValue::Unit);
                        fc.emit(Op::LoadConst {
                            rd: r_unit,
                            idx: unit_idx,
                        });
                        fc.emit(Op::Ret { ra: r_unit });
                    }
                    let proto = fc.proto;
                    self.functions[idx as usize] = proto;
                }
            }
        }

        let entry = self
            .func_table
            .get("main")
            .copied()
            .ok_or_else(|| InterpError::new("no main function found"))?;

        let max_children = self.flow_defs.values().find_map(|f| {
            f.annotations.iter().find_map(|a| match a.kind {
                crate::ast::FlowAnnotationKind::MaxChildren(n) => Some(n),
                _ => None,
            })
        });

        Ok(BytecodeProgram {
            functions: std::mem::take(&mut self.functions),
            entry,
            builtin_names: std::mem::take(&mut self.builtin_names),
            extern_names: std::mem::take(&mut self.extern_name_order),
            actor_defs: std::mem::take(&mut self.actor_defs),
            flow_defs: std::mem::take(&mut self.flow_defs),
            flow_transition_funcs: std::mem::take(&mut self.flow_transition_funcs),
            flow_fails_transitions: std::mem::take(&mut self.flow_fails_transitions),
            actor_method_funcs: std::mem::take(&mut self.actor_method_funcs),
            max_children,
            flow_persistent: std::mem::take(&mut self.flow_persistent),
            flow_fault_type: std::mem::take(&mut self.flow_fault_type),
            type_defs: std::mem::take(&mut self.type_defs),
            ast: self.ast_file.clone(),
            record_fields: std::mem::take(&mut self.record_fields),
        })
    }

    /// Resolve type aliases recursively: if `ty` is a Name that matches a
    /// known alias, replace it with the aliased type. Recurses into generic
    /// arguments and composite types.
    fn resolve_type(&self, ty: &Type) -> Type {
        match ty {
            Type::Name(n, args) if args.is_empty() => {
                if let Some(aliased) = self.type_aliases.get(n) {
                    self.resolve_type(aliased)
                } else {
                    ty.clone()
                }
            }
            Type::Name(n, args) => {
                let resolved_args: Vec<Type> = args.iter().map(|a| self.resolve_type(a)).collect();
                // Check if the outer name is also an alias (e.g., type MyMap = Map<string, i32>)
                if let Some(aliased) = self.type_aliases.get(n) {
                    // For generic aliases, we can't easily substitute type params,
                    // so just resolve the inner args.
                    let mut result = self.resolve_type(aliased);
                    if let Type::Name(_, ref mut inner_args) = result {
                        *inner_args = resolved_args;
                    }
                    result
                } else {
                    Type::Name(n.clone(), resolved_args)
                }
            }
            Type::Located { ty: inner, .. } => self.resolve_type(inner),
            Type::Option(inner) => Type::Option(Box::new(self.resolve_type(inner))),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.resolve_type(ok)),
                Box::new(self.resolve_type(err)),
            ),
            Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| self.resolve_type(e)).collect()),
            Type::Ref(lt, inner) => Type::Ref(lt.clone(), Box::new(self.resolve_type(inner))),
            Type::RefMut(lt, inner) => Type::RefMut(lt.clone(), Box::new(self.resolve_type(inner))),
            Type::Shared(inner) => Type::Shared(Box::new(self.resolve_type(inner))),
            Type::LocalShared(inner) => Type::LocalShared(Box::new(self.resolve_type(inner))),
            Type::Weak(inner) => Type::Weak(Box::new(self.resolve_type(inner))),
            Type::WeakLocal(inner) => Type::WeakLocal(Box::new(self.resolve_type(inner))),
            other => other.clone(),
        }
    }

    /// Compile a file for comptime evaluation only.
    /// Unlike `compile_file`, this does not require a `main` function.
    /// Used by codegen to evaluate `comptime func` declarations.
    pub fn compile_for_comptime(&mut self, file: &File) -> Result<BytecodeProgram, InterpError> {
        // Store AST for actor worker threads.
        self.ast_file = Some(std::sync::Arc::new(file.clone()));

        // Pass 1: register all function names + collect variant/actor/flow names.
        for item in &file.items {
            if let Item::Func(f) = item {
                let idx = self.functions.len() as FuncIdx;
                self.func_table.insert(f.name.clone(), idx);
                let defaults: Vec<Option<Expr>> =
                    f.params.iter().map(|p| p.default_value.clone()).collect();
                if defaults.iter().any(|d| d.is_some()) {
                    self.func_defaults.insert(f.name.clone(), defaults);
                }
                let param_names: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
                self.func_param_names.insert(f.name.clone(), param_names);
                self.functions
                    .push(FunctionProto::new(f.name.clone(), f.params.len() as u16));
            }
            if let Item::Type(td) = item {
                match &td.kind {
                    TypeDefKind::Enum(variants) => {
                        for variant in variants {
                            self.variant_names.insert(variant.name.clone());
                        }
                    }
                    TypeDefKind::Newtype(_) => {
                        self.newtype_names.insert(td.name.clone());
                    }
                    _ => {}
                }
            }
            if let Item::Const { name, value, .. } = item {
                self.constants.insert(name.clone(), value.clone());
            }
            if let Item::Cap(cap) = item {
                self.cap_names.insert(cap.name.clone());
                let components = if let Some(ref combined) = cap.combined_with {
                    let parts: Vec<String> = combined
                        .split(" + ")
                        .map(|s| s.trim().to_string())
                        .collect();
                    if parts.len() > 1 {
                        parts
                    } else {
                        vec![cap.name.clone(), combined.clone()]
                    }
                } else {
                    vec![cap.name.clone()]
                };
                self.cap_components.insert(cap.name.clone(), components);
            }
        }

        // Pass 1.5: register impl method names.
        for item in &file.items {
            if let Item::Impl(impl_def) = item {
                if !self.impl_type_names.contains(&impl_def.type_name) {
                    self.impl_type_names.push(impl_def.type_name.clone());
                }
            }
        }

        // Register builtins.
        let registry = crate::interp::bytecode::registry::create_registry();
        for name in registry.names() {
            self.register_builtin(&name);
        }

        // Pass 2: compile function bodies.
        for item in &file.items {
            if let Item::Func(f) = item {
                if let Some(&idx) = self.func_table.get(&f.name) {
                    let proto = self.compile_func(f)?;
                    self.functions[idx as usize] = proto;
                }
            }
        }

        // No main requirement for comptime compilation.
        let entry = self.func_table.get("main").copied().unwrap_or(0);

        Ok(BytecodeProgram {
            functions: std::mem::take(&mut self.functions),
            entry,
            builtin_names: std::mem::take(&mut self.builtin_names),
            extern_names: std::mem::take(&mut self.extern_name_order),
            actor_defs: std::mem::take(&mut self.actor_defs),
            flow_defs: std::mem::take(&mut self.flow_defs),
            flow_transition_funcs: std::mem::take(&mut self.flow_transition_funcs),
            flow_fails_transitions: std::mem::take(&mut self.flow_fails_transitions),
            actor_method_funcs: std::mem::take(&mut self.actor_method_funcs),
            max_children: None,
            flow_persistent: std::mem::take(&mut self.flow_persistent),
            flow_fault_type: std::mem::take(&mut self.flow_fault_type),
            type_defs: std::mem::take(&mut self.type_defs),
            ast: self.ast_file.clone(),
            record_fields: std::mem::take(&mut self.record_fields),
        })
    }

    fn register_builtin(&mut self, name: &str) {
        let idx = self.builtin_names.len() as BuiltinIdx;
        self.builtin_table.insert(name.to_string(), idx);
        self.builtin_names.push(name.to_string());
    }

    /// Build a qualified path from nested Field(Ident(...), ...) expressions
    /// (e.g. `Outer::Inner::f` → "Outer::Inner::f"). Mirrors the tree-walker
    /// `Interpreter::build_qualified_path`.
    fn build_qualified_path(obj: &Expr, field: &str) -> Option<String> {
        match obj.unlocated() {
            Expr::Ident(name) => Some(format!("{}::{}", name, field)),
            Expr::Field(inner_obj, inner_field) => {
                Self::build_qualified_path(inner_obj, inner_field)
                    .map(|base| format!("{}::{}", base, field))
            }
            _ => None,
        }
    }

    /// Register module functions under their qualified path (recursive).
    fn collect_module_funcs(&mut self, module: &crate::ast::ModuleDef, prefix: &str) {
        let current = if prefix.is_empty() {
            module.name.clone()
        } else {
            format!("{}::{}", prefix, module.name)
        };
        for inner in &module.items {
            match inner {
                crate::ast::Item::Func(f) => {
                    let qualified = format!("{}::{}", current, f.name);
                    let idx = self.functions.len() as FuncIdx;
                    self.func_table.insert(qualified.clone(), idx);
                    let defaults: Vec<Option<Expr>> =
                        f.params.iter().map(|p| p.default_value.clone()).collect();
                    if defaults.iter().any(|d| d.is_some()) {
                        self.func_defaults.insert(qualified.clone(), defaults);
                    }
                    let param_names: Vec<String> =
                        f.params.iter().map(|p| p.name.clone()).collect();
                    self.func_param_names.insert(qualified.clone(), param_names);
                    self.functions
                        .push(FunctionProto::new(qualified.clone(), f.params.len() as u16));
                    let proto = &mut self.functions[idx as usize];
                    proto.has_mut_params = f.params.iter().any(|p| p.mut_);
                    proto.mut_param_indices = f
                        .params
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.mut_)
                        .map(|(i, _)| i as u16)
                        .collect();
                }
                crate::ast::Item::Module(m) => self.collect_module_funcs(m, &current),
                _ => {}
            }
        }
    }

    /// Compile module function bodies (recursive), replacing placeholders.
    fn compile_module_funcs(
        &mut self,
        module: &crate::ast::ModuleDef,
        prefix: &str,
    ) -> Result<(), InterpError> {
        let current = if prefix.is_empty() {
            module.name.clone()
        } else {
            format!("{}::{}", prefix, module.name)
        };
        for inner in &module.items {
            match inner {
                crate::ast::Item::Func(f) => {
                    let qualified = format!("{}::{}", current, f.name);
                    if let Some(&idx) = self.func_table.get(&qualified) {
                        let proto = self.compile_func(f)?;
                        self.functions[idx as usize] = proto;
                    }
                }
                crate::ast::Item::Module(m) => self.compile_module_funcs(m, &current)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Compile a single function definition.
    fn compile_func(&mut self, f: &FuncDef) -> Result<FunctionProto, InterpError> {
        let mut fc = FuncCompiler::new(f.name.clone(), f.params.len() as u16);

        // Bind parameters to registers 0..param_count.
        for (i, param) in f.params.iter().enumerate() {
            fc.vars[0].insert(param.name.clone(), i as Reg);
            fc.set_var_mut(&param.name, param.mut_);
            // Track parameter types for int/float dispatch.
            let ty = match param.ty.unlocated() {
                Type::Name(n, _) if n == "f64" || n == "f32" => VarType::Float,
                Type::Name(n, _) if n == "i32" || n == "i64" => VarType::Int,
                Type::Name(n, _) if n == "bool" => VarType::Bool,
                Type::Name(n, _) if n == "string" => VarType::String,
                Type::DynTrait(names) => VarType::Dyn(names.join(" + ")),
                Type::Name(n, _) => VarType::User(n.clone()),
                _ => VarType::Unknown,
            };
            fc.set_reg_type(&param.name, ty);
        }
        // Ensure register_count accounts for params.
        while fc.proto.register_count < f.params.len() as u16 {
            fc.proto.alloc_reg();
        }

        fc.has_mut_params(f);

        // Contract metadata (0.33 Phase F: runtime contract checking).
        fc.proto.has_requires = f.has_requires;
        fc.proto.has_ensures = f.has_ensures;
        fc.proto.param_names = f.params.iter().map(|p| p.name.clone()).collect();

        // Compile body statements.
        let last_reg = self.compile_block(&mut fc, &f.body)?;

        // Return the last expression's value, or Unit if none.
        if let Some(r) = last_reg {
            fc.emit(Op::Ret { ra: r });
        } else {
            fc.emit(Op::RetUnit);
        }

        // Compile contract expressions as mini-functions (0.33 Phase F: native contract eval).
        if f.has_requires || f.has_ensures {
            self.compile_contract_funcs(&mut fc.proto, f)?;
        }

        Ok(fc.proto)
    }

    /// Compile requires/ensures contract expressions as mini-functions (0.33 Phase F).
    /// Each contract expression becomes a standalone function that takes the parent's
    /// parameters (plus `result` for ensures) and returns a bool.
    fn compile_contract_funcs(
        &mut self,
        proto: &mut FunctionProto,
        f: &FuncDef,
    ) -> Result<(), InterpError> {
        for stmt in &f.body {
            match stmt.unlocated() {
                Stmt::Requires(expr, _) => {
                    let idx = self.compile_contract_expr(expr, f, false)?;
                    proto.requires_funcs.push(idx);
                }
                Stmt::Ensures(expr, _) => {
                    let idx = self.compile_contract_expr(expr, f, true)?;
                    proto.ensures_funcs.push(idx);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Compile a single contract expression as a mini-function.
    /// Parameters: same as parent function. For ensures, adds `result` param.
    /// Returns the FuncIdx of the compiled mini-function.
    fn compile_contract_expr(
        &mut self,
        expr: &Expr,
        f: &FuncDef,
        is_ensures: bool,
    ) -> Result<FuncIdx, InterpError> {
        let param_count = f.params.len() as u16 + if is_ensures { 1 } else { 0 };
        let name = format!(
            "__contract_{}_{}",
            f.name,
            if is_ensures { "ensures" } else { "requires" }
        );
        let mut fc = FuncCompiler::new(name, param_count);

        // Bind parent parameters to registers 0..N.
        for (i, param) in f.params.iter().enumerate() {
            fc.vars[0].insert(param.name.clone(), i as Reg);
            let ty = match param.ty.unlocated() {
                Type::Name(n, _) if n == "f64" || n == "f32" => VarType::Float,
                Type::Name(n, _) if n == "i32" || n == "i64" => VarType::Int,
                Type::Name(n, _) if n == "bool" => VarType::Bool,
                Type::Name(n, _) if n == "string" => VarType::String,
                _ => VarType::Unknown,
            };
            fc.set_reg_type(&param.name, ty);
        }
        // For ensures, bind `result` at register N.
        if is_ensures {
            let result_reg = f.params.len() as Reg;
            fc.vars[0].insert("result".to_string(), result_reg);
        }
        while fc.proto.register_count < param_count {
            fc.proto.alloc_reg();
        }

        // Compile the contract expression and return it.
        let reg = self.compile_expr(&mut fc, expr)?;
        fc.emit(Op::Ret { ra: reg });

        let idx = self.functions.len() as FuncIdx;
        self.functions.push(fc.proto);
        Ok(idx)
    }

    /// Compile an impl method (has implicit `self` at register 0).
    fn compile_func_impl(&mut self, f: &FuncDef) -> Result<FunctionProto, InterpError> {
        // param_count = explicit params + 1 (self).
        let total_params = f.params.len() as u16 + 1;
        let mut fc = FuncCompiler::new(f.name.clone(), total_params);

        // Bind `self` to register 0.
        fc.vars[0].insert("self".to_string(), 0);

        // Bind explicit parameters to registers 1..param_count+1.
        for (i, param) in f.params.iter().enumerate() {
            fc.vars[0].insert(param.name.clone(), (i + 1) as Reg);
            fc.set_var_mut(&param.name, param.mut_);
            let ty = match param.ty.unlocated() {
                Type::Name(n, _) if n == "f64" || n == "f32" => VarType::Float,
                Type::Name(n, _) if n == "i32" || n == "i64" => VarType::Int,
                Type::Name(n, _) if n == "bool" => VarType::Bool,
                Type::Name(n, _) if n == "string" => VarType::String,
                Type::DynTrait(names) => VarType::Dyn(names.join(" + ")),
                Type::Name(n, _) => VarType::User(n.clone()),
                _ => VarType::Unknown,
            };
            fc.set_reg_type(&param.name, ty);
        }
        // Ensure register_count accounts for self + params.
        while fc.proto.register_count < total_params {
            fc.proto.alloc_reg();
        }

        fc.has_mut_params(f);

        // Compile body statements.
        let last_reg = self.compile_block(&mut fc, &f.body)?;

        if let Some(r) = last_reg {
            fc.emit(Op::Ret { ra: r });
        } else {
            fc.emit(Op::RetUnit);
        }

        Ok(fc.proto)
    }

    /// Compile a block of statements.
    fn compile_block(
        &mut self,
        fc: &mut FuncCompiler,
        block: &Block,
    ) -> Result<Option<Reg>, InterpError> {
        // Pre-scan for OnFailure blocks in this block to decide whether
        // to emit a fault handler wrapper. We only care about direct
        // children (Stmt::OnFailure), not nested ones in sub-blocks.
        let has_on_failure = block
            .iter()
            .any(|s| matches!(s.unlocated(), Stmt::OnFailure(_)));

        let mut set_fault_idx = None;
        if has_on_failure {
            set_fault_idx = Some(fc.emit(Op::SetFaultPc { handler_pc: 0 }));
        }

        let mut last_reg = None;
        for (i, stmt) in block.iter().enumerate() {
            // Track source line for error context (D12).
            fc.set_line_from_meta(stmt.meta());
            let is_last = i == block.len() - 1;
            match stmt.unlocated() {
                Stmt::Expr(e) => {
                    let r = self.compile_expr(fc, e)?;
                    if is_last {
                        last_reg = Some(r);
                    }
                }
                Stmt::Let {
                    pat, init, mut_, ..
                } => {
                    if let Some(init_expr) = init {
                        // Detect borrow: let x = &mut place / &place.
                        // Record alias so `*x = v` can write back to the place.
                        if let PatternKind::Variable(name) = &pat.kind {
                            if let Expr::Unary(UnOp::RefMut | UnOp::Ref, place) =
                                init_expr.unlocated()
                            {
                                fc.borrow_aliases.insert(name.clone(), *place.clone());
                            }
                        }
                        let r = self.compile_expr(fc, init_expr)?;
                        // Track variable type for int/float dispatch.
                        if let PatternKind::Variable(name) = &pat.kind {
                            let ty = self.infer_expr_type(fc, init_expr);
                            fc.set_reg_type(name, ty);
                            fc.set_var_mut(name, *mut_);
                            // Copy to a new register so subsequent mutation of the
                            // original does not affect this binding (C-comp1/C-comp2).
                            let new_r = fc.proto.alloc_reg();
                            fc.emit(Op::Mov { rd: new_r, rs: r });
                            fc.vars_mut().insert(name.clone(), new_r);
                        } else {
                            self.bind_pattern(fc, pat, r);
                        }
                    } else {
                        // let x; → Unit
                        if let PatternKind::Variable(name) = &pat.kind {
                            let r = fc.bind_var(name);
                            fc.set_var_mut(name, *mut_);
                            fc.emit(Op::LoadUnit { rd: r });
                        }
                    }
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign(fc, target, value)?;
                }
                Stmt::Return(expr) => {
                    // Execute deferred blocks in LIFO order before returning.
                    let defers: Vec<Block> =
                        fc.defer_scopes.last().map_or(Vec::new(), |s| s.clone());
                    if let Some(s) = fc.defer_scopes.last_mut() {
                        s.clear();
                    }
                    for d in defers.into_iter().rev() {
                        self.compile_block(fc, &d)?;
                    }
                    if let Some(e) = expr {
                        let r = self.compile_expr(fc, e)?;
                        fc.emit(Op::Ret { ra: r });
                    } else {
                        fc.emit(Op::RetUnit);
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    if is_last {
                        // If as last expression: produces a value.
                        let r = self.compile_if_expr(fc, cond, then_, else_)?;
                        last_reg = Some(r);
                    } else {
                        self.compile_if_stmt(fc, cond, then_, else_.as_ref())?;
                    }
                }
                Stmt::IfLet {
                    pat,
                    init,
                    then_,
                    else_,
                } => {
                    // v0.34.3: if-let is a statement-only guard (no value form).
                    self.compile_if_let_stmt(fc, pat, init, then_, else_.as_ref())?;
                }
                Stmt::While { cond, body } => {
                    let result_reg = self.compile_while(fc, cond, body)?;
                    if is_last {
                        last_reg = Some(result_reg);
                    }
                }
                Stmt::For {
                    var,
                    iterable,
                    body,
                } => {
                    let result_reg = self.compile_for(fc, var, iterable, body)?;
                    if is_last {
                        last_reg = Some(result_reg);
                    }
                }
                Stmt::Block(b) => {
                    fc.push_scope();
                    let r = self.compile_block(fc, b)?;
                    fc.pop_scope();
                    if is_last {
                        last_reg = r;
                    }
                }
                Stmt::Break(val) => {
                    if fc.break_jumps.is_empty() {
                        return Err(InterpError::new("break outside loop"));
                    }
                    // Break with value: store the value in the loop result register.
                    if let Some(expr) = val {
                        if let Some(&result_reg) = fc.loop_result_regs.last() {
                            let r_val = self.compile_expr(fc, expr)?;
                            if r_val != result_reg {
                                fc.emit(Op::Mov {
                                    rd: result_reg,
                                    rs: r_val,
                                });
                            }
                        }
                    }
                    let idx = fc.emit(Op::Jmp { offset: 0 });
                    fc.break_jumps_mut().push(idx);
                }
                Stmt::Continue => {
                    if fc.continue_jumps.is_empty() {
                        return Err(InterpError::new("continue outside loop"));
                    }
                    let idx = fc.emit(Op::Jmp { offset: 0 });
                    fc.continue_jumps_mut().push(idx);
                }
                // Skip non-executable statements.
                Stmt::Desc(..)
                | Stmt::Rule(..)
                | Stmt::Requires(..)
                | Stmt::Ensures(..)
                | Stmt::Invariant(..)
                | Stmt::Math(..)
                | Stmt::MmsBlock { .. } => {}

                // Nested function definition: compile as a closure and bind
                // the function name as a local variable holding the closure.
                // Calls to this name will resolve via CallIndirect.
                Stmt::Func(f) => {
                    let closure_reg = self.compile_lambda(fc, &f.params, &f.body)?;
                    fc.vars_mut().insert(f.name.clone(), closure_reg);
                }

                // ── Phase B: Stmt 补全 I ──────────────────────
                Stmt::Loop(body) => {
                    let result_reg = self.compile_loop(fc, body)?;
                    if is_last {
                        last_reg = Some(result_reg);
                    }
                }

                Stmt::WhileLet { pat, init, body } => {
                    let result_reg = self.compile_while_let(fc, pat, init, body)?;
                    if is_last {
                        last_reg = Some(result_reg);
                    }
                }

                Stmt::Unsafe(block) => {
                    // Interpreter doesn't enforce safety — just compile the block.
                    fc.push_scope();
                    let result = self.compile_block(fc, block)?;
                    fc.pop_scope();
                    if is_last {
                        last_reg = result;
                    }
                }

                Stmt::IeeeFloat(block) => {
                    // v0.34.10a (SD-9): suspend finiteness trap inside.
                    fc.emit(Op::IeeeEnter);
                    fc.push_scope();
                    let result = self.compile_block(fc, block)?;
                    fc.pop_scope();
                    fc.emit(Op::IeeeExit);
                    if is_last {
                        last_reg = result;
                    }
                }

                Stmt::Arena(block) => {
                    // Interpreter doesn't do region-based memory — just compile the block.
                    fc.push_scope();
                    let result = self.compile_block(fc, block)?;
                    fc.pop_scope();
                    if is_last {
                        last_reg = result;
                    }
                }

                Stmt::Drop(expr) => {
                    // Drop is a no-op in the interpreter (values are GC'd).
                    // Just compile the expression for side effects.
                    self.compile_expr(fc, expr)?;
                }

                Stmt::Alloc { body, .. } => {
                    // Allocator block — just compile the body (region memory is
                    // a tree-walker-only feature; the block value flows through).
                    fc.push_scope();
                    let result = self.compile_block(fc, body)?;
                    fc.pop_scope();
                    if is_last {
                        last_reg = result;
                    }
                }

                Stmt::Defer(block) => {
                    // Record the deferred block for scope-exit execution (LIFO).
                    fc.defer_scopes_mut().push(block.clone());
                }

                // ── Phase B: Stmt 补全 II ─────────────────────
                Stmt::SharedLet {
                    name, init, kind, ..
                } => {
                    // Shared/Weak ownership binding.
                    let r = self.compile_expr(fc, init)?;
                    let r_var = fc.bind_var(name);
                    match kind {
                        SharedKind::Shared | SharedKind::LocalShared => {
                            fc.emit(Op::SharedNew { rd: r_var, ra: r });
                        }
                        SharedKind::Weak | SharedKind::WeakLocal => {
                            fc.emit(Op::WeakNew { rd: r_var, ra: r });
                        }
                    }
                }

                Stmt::OnFailure(block) => {
                    // Register compensation block for fault-triggered execution at scope exit.
                    fc.on_failure_scopes_mut().push(block.clone());
                }

                Stmt::Do(block) => {
                    // Do block — transition implementation body.
                    fc.push_scope();
                    let result = self.compile_block(fc, block)?;
                    fc.pop_scope();
                    if is_last {
                        last_reg = result;
                    }
                }

                Stmt::Parasteps(block) => {
                    // Parallel steps — compile the block.
                    // Parallel execution semantics handled at runtime.
                    fc.push_scope();
                    self.compile_block(fc, block)?;
                    fc.pop_scope();
                }

                // v0.34.1: `delegate` removed (clause 2); Pinned is still live
                // (FFI pinning) but its timeout field is DEAD (clause 10).
                Stmt::Pinned { expr, var, body } => {
                    // H3 fix: mirror codegen (block.rs:999) — evaluate the pinned
                    // expr, bind the optional |var|, run the body. Was a no-op
                    // (`Stmt::Pinned { .. } => {}`), which silently skipped the
                    // body and broke L1 dual-backend equivalence for any pinned
                    // body with observable side effects.
                    let r = self.compile_expr(fc, expr)?;
                    fc.push_scope();
                    if let Some(v) = var {
                        let ty = self.infer_expr_type(fc, expr);
                        fc.set_reg_type(v, ty);
                        let new_r = fc.proto.alloc_reg();
                        fc.emit(Op::Mov { rd: new_r, rs: r });
                        fc.vars_mut().insert(v.clone(), new_r);
                    }
                    let result = self.compile_block(fc, body)?;
                    fc.pop_scope();
                    if is_last {
                        last_reg = result;
                    }
                }

                _ => {
                    // Remaining unsupported: Ellipsis, Located (wrapper).
                }
            }
        }

        // Emit deferred blocks in LIFO order at scope exit.
        let defers: Vec<Block> = fc.defer_scopes.last().map_or(Vec::new(), |s| s.clone());
        if let Some(s) = fc.defer_scopes.last_mut() {
            s.clear();
        }
        for d in defers.into_iter().rev() {
            self.compile_block(fc, &d)?;
        }

        // Emit OnFailure fault handler at scope exit (if any OnFailure blocks were registered).
        let on_failure_blocks: Vec<Block> = fc
            .on_failure_scopes
            .last()
            .map_or(Vec::new(), |s| s.clone());
        if let Some(s) = fc.on_failure_scopes.last_mut() {
            s.clear();
        }
        if !on_failure_blocks.is_empty() {
            if let Some(idx) = set_fault_idx {
                // Emit ClearFaultPc for the normal (success) path.
                fc.emit(Op::ClearFaultPc);
                // Jump past the fault handler on normal exit.
                let jmp_past = fc.emit(Op::Jmp { offset: 0 });

                // Patch SetFaultPc to point to the handler start.
                let handler_pc = fc.proto.code.len() as u32;
                if let Op::SetFaultPc {
                    handler_pc: ref mut pc,
                } = fc.proto.code[idx]
                {
                    *pc = handler_pc;
                }

                // Emit OnFailure blocks in LIFO order (last registered runs first).
                for b in on_failure_blocks.into_iter().rev() {
                    self.compile_block(fc, &b)?;
                }

                // After compensations, return with the original error.
                fc.emit(Op::FaultRetEarly);

                // Patch the jump to skip past the handler.
                fc.proto.patch_jump(jmp_past);
            }
        } else if let Some(idx) = set_fault_idx {
            // No OnFailure blocks but SetFaultPc was emitted — patch to Nop.
            fc.proto.code[idx] = Op::Nop;
        }

        Ok(last_reg)
    }

    /// Compile an expression, returning the register holding the result.
    /// Compile an expression directly into a pre-assigned register.
    ///
    /// Simple, side-effect-free expression shapes (literals, arithmetic,
    /// unary ops) emit their result straight into `target`, eliminating the
    /// `Op::Mov` that `compile_expr` + a copy would otherwise produce.
    /// Anything else falls back to `compile_expr` + `Op::Mov` (safe, no
    /// reordering of side effects).
    fn compile_expr_into(
        &mut self,
        fc: &mut FuncCompiler,
        expr: &Expr,
        target: Reg,
    ) -> Result<Reg, InterpError> {
        fc.set_line_from_meta(expr.meta());
        match expr.unlocated() {
            Expr::Literal(lit) => {
                if matches!(lit, Lit::FString(_)) {
                    let r = self.compile_literal(fc, lit)?;
                    return self.copy_into(fc, r, target);
                }
                return self.compile_literal_into(fc, lit, target);
            }
            Expr::Binary(op, l, r) => self.compile_binary_into(fc, *op, l, r, target),
            Expr::Unary(op, e) => self.compile_unary_into(fc, *op, e, target),
            _ => {
                let r = self.compile_expr(fc, expr)?;
                self.copy_into(fc, r, target)
            }
        }
    }

    fn copy_into(&mut self, fc: &mut FuncCompiler, rs: Reg, rd: Reg) -> Result<Reg, InterpError> {
        if rs != rd {
            fc.emit(Op::Mov { rd, rs });
        }
        Ok(rd)
    }

    fn compile_expr(&mut self, fc: &mut FuncCompiler, expr: &Expr) -> Result<Reg, InterpError> {
        // Track source line for error context (D12).
        fc.set_line_from_meta(expr.meta());
        match expr.unlocated() {
            Expr::Literal(lit) => self.compile_literal(fc, lit),
            Expr::Ident(name) => {
                // Constants: inline the value expression.
                if let Some(const_expr) = self.constants.get(name).cloned() {
                    return self.compile_expr(fc, &const_expr);
                }
                // Local variable reference (shadows nullary constructors and
                // builtins — e.g. `let None = 99; None` is the variable).
                if let Some(r) = fc.lookup_var(name) {
                    return Ok(r);
                }
                // Nullary variant constructors used as identifiers.
                if name == "None" {
                    let rd = fc.proto.alloc_reg();
                    fc.emit(Op::None { rd });
                    return Ok(rd);
                }
                // First-class function reference: emit a zero-capture closure
                // pointing to the function prototype. This enables HOF usage
                // like `map_list(xs, increment)` where `increment` is a func.
                if let Some(&fidx) = self.func_table.get(name.as_str()) {
                    let rd = fc.proto.alloc_reg();
                    fc.emit(Op::NewClosure {
                        rd,
                        proto: fidx,
                        captures_base: 0,
                        capture_count: 0,
                    });
                    return Ok(rd);
                }
                // Nullary enum variant constructors: Yes, No, Done, etc.
                if self.variant_names.contains(name.as_str()) {
                    let rd = fc.proto.alloc_reg();
                    let type_name_idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                    fc.emit(Op::NewVariant {
                        rd,
                        type_name: type_name_idx,
                        variant: 0,
                        base: 0,
                        arity: 0,
                    });
                    return Ok(rd);
                }
                // Capability names: FullAccess, Read, Write, etc.
                if self.cap_names.contains(name.as_str()) {
                    let rd = fc.proto.alloc_reg();
                    // Store components as comma-separated string for VM to parse.
                    let components = self
                        .cap_components
                        .get(name.as_str())
                        .map(|c| c.join(","))
                        .unwrap_or_else(|| name.clone());
                    let name_idx = fc.proto.add_const(ConstValue::Str(components));
                    fc.emit(Op::NewCap { rd, name: name_idx });
                    return Ok(rd);
                }
                Err(InterpError::new(format!(
                    "undefined variable '{}' in bytecode",
                    name
                )))
            }
            Expr::Binary(op, l, r) => self.compile_binary(fc, *op, l, r),
            Expr::Unary(op, e) => self.compile_unary(fc, *op, e),
            Expr::Call(callee, args) => self.compile_call(fc, callee, args),
            Expr::If { cond, then_, else_ } => self.compile_if_expr(fc, cond, then_, else_),
            Expr::Block(b) => {
                fc.push_scope();
                let result = self.compile_block(fc, b)?;
                fc.pop_scope();
                Ok(result.unwrap_or_else(|| {
                    let r = fc.proto.alloc_reg();
                    fc.emit(Op::LoadUnit { rd: r });
                    r
                }))
            }
            Expr::Index(obj, idx) => self.compile_index(fc, obj, idx),
            Expr::List(elems) => self.compile_list(fc, elems),
            Expr::Tuple(elems) => self.compile_tuple(fc, elems),
            Expr::TupleIndex(obj, idx) => {
                let r_obj = self.compile_expr(fc, obj)?;
                let rd = fc.proto.alloc_reg();
                fc.emit(Op::TupleGet {
                    rd,
                    ra: r_obj,
                    idx: *idx as u16,
                });
                Ok(rd)
            }
            Expr::Cast(inner, ty) => {
                let r = self.compile_expr(fc, inner)?;
                let rd = fc.proto.alloc_reg();
                match ty.unlocated() {
                    Type::Name(n, _) if n == "f64" || n == "f32" => {
                        fc.emit(Op::IntToFloat { rd, ra: r });
                    }
                    Type::Name(n, _) if n == "i64" || n == "i32" || n == "int" => {
                        // Cast to int: truncate Float → Int.
                        fc.emit(Op::Cast {
                            rd,
                            ra: r,
                            target: 0,
                        });
                    }
                    _ => {
                        fc.emit(Op::Mov { rd, rs: r });
                    }
                }
                Ok(rd)
            }
            Expr::Field(obj, field) => {
                let r_obj = self.compile_expr(fc, obj)?;
                let rd = fc.proto.alloc_reg();
                // Field access by name (stored as string constant).
                let field_idx = fc.proto.add_const(ConstValue::Str(field.clone()));
                fc.emit(Op::RecordGet {
                    rd,
                    ra: r_obj,
                    field: field_idx,
                });
                Ok(rd)
            }
            Expr::Record { ty, fields } => self.compile_record(fc, ty.as_deref(), fields),
            Expr::Lambda {
                params,
                ret: _,
                body,
            } => self.compile_lambda(fc, params, body),
            Expr::Match(subject, arms) => self.compile_match(fc, subject, arms),

            // ── Phase B: Expr 补全 ──────────────────────────
            Expr::Comprehension {
                expr,
                var,
                iter,
                guard,
            } => {
                // [expr for var in iter (if guard)] → loop + list push
                let r_iter = self.compile_expr(fc, iter)?;
                let rd = fc.proto.alloc_reg();
                fc.emit(Op::NewList { rd, capacity: 0 });

                // Loop index.
                let r_idx = fc.proto.alloc_reg();
                let r_len = fc.proto.alloc_reg();
                let r_one = fc.proto.alloc_reg();
                let c0 = fc.proto.add_const(ConstValue::Int(0));
                let c1 = fc.proto.add_const(ConstValue::Int(1));
                fc.emit(Op::LoadConst { rd: r_idx, idx: c0 });
                fc.emit(Op::LoadConst { rd: r_one, idx: c1 });
                fc.emit(Op::Len {
                    rd: r_len,
                    ra: r_iter,
                });

                let loop_start = fc.proto.code.len();
                let r_cmp = fc.proto.alloc_reg();
                fc.emit(Op::LtInt {
                    rd: r_cmp,
                    ra: r_idx,
                    rb: r_len,
                });
                let jmp_end = fc.emit(Op::JmpIfNot {
                    offset: 0,
                    ra: r_cmp,
                });

                // Bind loop variable.
                fc.push_scope();
                let r_var = fc.bind_var(var);
                fc.emit(Op::ListGet {
                    rd: r_var,
                    ra: r_iter,
                    rb: r_idx,
                });

                // Guard check.
                let guard_skip = if let Some(guard_expr) = guard {
                    let r_guard = self.compile_expr(fc, guard_expr)?;
                    Some(fc.emit(Op::JmpIfNot {
                        offset: 0,
                        ra: r_guard,
                    }))
                } else {
                    None
                };

                // Evaluate expression and push to result list.
                let r_elem = self.compile_expr(fc, expr)?;
                fc.emit(Op::ListPush { ra: rd, rb: r_elem });

                if let Some(skip) = guard_skip {
                    fc.proto.patch_jump(skip);
                }
                fc.pop_scope();

                // Increment and loop.
                fc.emit(Op::AddInt {
                    rd: r_idx,
                    ra: r_idx,
                    rb: r_one,
                });
                fc.emit(Op::Jmp { offset: 0 });
                let jmp_back = fc.proto.code.len() - 1;
                fc.proto.patch_jump_to(jmp_back, loop_start);
                fc.proto.patch_jump_to(jmp_end, fc.proto.code.len());

                Ok(rd)
            }

            Expr::SliceExpr { target, start, end } => {
                // target[start..end] → __slice builtin (handles List + String + negative indices).
                let r_target = self.compile_expr(fc, target)?;
                let r_start = if let Some(s) = start {
                    self.compile_expr(fc, s)?
                } else {
                    let r = fc.proto.alloc_reg();
                    let c0 = fc.proto.add_const(ConstValue::Int(0));
                    fc.emit(Op::LoadConst { rd: r, idx: c0 });
                    r
                };
                let r_end = if let Some(e) = end {
                    self.compile_expr(fc, e)?
                } else {
                    let r = fc.proto.alloc_reg();
                    fc.emit(Op::Len {
                        rd: r,
                        ra: r_target,
                    });
                    r
                };

                // Call __slice(target, start, end).
                let rd = fc.proto.alloc_reg();
                let args_base = fc.proto.alloc_reg();
                fc.proto.alloc_reg();
                fc.proto.alloc_reg();
                fc.emit(Op::Mov {
                    rd: args_base,
                    rs: r_target,
                });
                fc.emit(Op::Mov {
                    rd: args_base + 1,
                    rs: r_start,
                });
                fc.emit(Op::Mov {
                    rd: args_base + 2,
                    rs: r_end,
                });
                if let Some(&bidx) = self.builtin_table.get("__slice") {
                    fc.emit(Op::CallBuiltin {
                        rd,
                        builtin: bidx,
                        args_base,
                        argc: 3,
                    });
                } else {
                    return Err(InterpError::new("bytecode: __slice builtin not registered"));
                }
                Ok(rd)
            }

            Expr::OptionalChain(obj, field) => {
                // obj?.field → if obj is None/Err → None, else → Some(obj.field)
                let r_obj = self.compile_expr(fc, obj)?;
                let rd = fc.proto.alloc_reg();

                // Check if obj is None variant.
                let r_is_none = fc.proto.alloc_reg();
                let none_tag = fc.proto.add_const(ConstValue::Str("None".into()));
                fc.emit(Op::IsVariant {
                    rd: r_is_none,
                    ra: r_obj,
                    tag: none_tag,
                });
                let jmp_not_none = fc.emit(Op::JmpIfNot {
                    offset: 0,
                    ra: r_is_none,
                });

                // None branch: rd = None.
                fc.emit(Op::None { rd });
                let jmp_end = fc.emit(Op::Jmp { offset: 0 });

                // Not-None branch: check if obj is Err variant.
                fc.proto.patch_jump(jmp_not_none);
                let r_is_err = fc.proto.alloc_reg();
                let err_tag = fc.proto.add_const(ConstValue::Str("Err".into()));
                fc.emit(Op::IsVariant {
                    rd: r_is_err,
                    ra: r_obj,
                    tag: err_tag,
                });
                let jmp_not_err = fc.emit(Op::JmpIfNot {
                    offset: 0,
                    ra: r_is_err,
                });

                // Err branch: rd = None.
                fc.emit(Op::None { rd });
                let jmp_end2 = fc.emit(Op::Jmp { offset: 0 });

                // Some/Ok branch: rd = Some(obj.field).
                fc.proto.patch_jump(jmp_not_err);
                let field_idx = fc.proto.add_const(ConstValue::Str(field.clone()));
                let r_field = fc.proto.alloc_reg();
                fc.emit(Op::RecordGet {
                    rd: r_field,
                    ra: r_obj,
                    field: field_idx,
                });
                // Wrap the field value back in Some.
                fc.emit(Op::Some { rd, ra: r_field });
                fc.proto.patch_jump(jmp_end);
                fc.proto.patch_jump(jmp_end2);

                Ok(rd)
            }

            Expr::TypeOf(inner) => {
                let r = self.compile_expr(fc, inner)?;
                let rd = fc.proto.alloc_reg();
                fc.emit(Op::TypeOf { rd, ra: r });
                Ok(rd)
            }

            Expr::TypeInfo(ty) => {
                // Return type name as a string constant.
                let rd = fc.proto.alloc_reg();
                let type_str = format!("{:?}", ty);
                let idx = fc.proto.add_const(ConstValue::Str(type_str));
                fc.emit(Op::LoadConst { rd, idx });
                Ok(rd)
            }

            Expr::MapLiteral { entries } => {
                let rd = fc.proto.alloc_reg();
                fc.emit(Op::NewMap { rd });
                for (key_expr, val_expr) in entries {
                    let r_key = self.compile_expr(fc, key_expr)?;
                    let r_val = self.compile_expr(fc, val_expr)?;
                    fc.emit(Op::MapSet {
                        ra: rd,
                        rb: r_key,
                        rc: r_val,
                    });
                }
                Ok(rd)
            }

            Expr::SetLiteral(elems) => {
                let rd = fc.proto.alloc_reg();
                fc.emit(Op::NewSet { rd });
                for elem in elems {
                    let r = self.compile_expr(fc, elem)?;
                    fc.emit(Op::SetAdd { ra: rd, rb: r });
                }
                Ok(rd)
            }

            Expr::Turbofish(name, type_args, args) => {
                // Special case: from_json::<T>(s) → typed deserialization.
                // Pass the type string as a second argument so the builtin
                // can coerce the generic JSON value to the target type.
                if name == "from_json" && !type_args.is_empty() {
                    let resolved = self.resolve_type(&type_args[0]);
                    let type_str = crate::core::fmt_type(&resolved);
                    // Compile the JSON string argument.
                    let r_json = self.compile_expr(fc, &args[0])?;
                    // Allocate consecutive registers for args: [json_str, type_str].
                    let args_base = fc.proto.alloc_reg();
                    let r_type = fc.proto.alloc_reg();
                    // Move json arg into args_base (may already be there).
                    if r_json != args_base {
                        fc.emit(Op::Mov {
                            rd: args_base,
                            rs: r_json,
                        });
                    }
                    // Load type string into r_type (args_base + 1).
                    let type_idx = fc.proto.add_const(ConstValue::Str(type_str));
                    fc.emit(Op::LoadConst {
                        rd: r_type,
                        idx: type_idx,
                    });
                    // Call from_json_typed(json_str, type_str).
                    let rd = fc.proto.alloc_reg();
                    if let Some(&bidx) = self.builtin_table.get("from_json_typed") {
                        fc.emit(Op::CallBuiltin {
                            rd,
                            builtin: bidx,
                            args_base,
                            argc: 2,
                        });
                    } else {
                        return Err(InterpError::new("from_json_typed builtin not registered"));
                    }
                    return Ok(rd);
                }
                // Other turbofish: type arguments ignored at runtime.
                let callee = Expr::Ident(name.clone());
                self.compile_call(fc, &callee, args)
            }

            Expr::Try(inner) => {
                // ? operator: unwrap Ok/Some, or return Err/None early.
                let r_inner = self.compile_expr(fc, inner)?;
                let rd = fc.proto.alloc_reg();

                // Check if it's Ok/Some (variant tag).
                let r_is_ok = fc.proto.alloc_reg();
                let ok_tag = fc.proto.add_const(ConstValue::Str("Ok".into()));
                fc.emit(Op::IsVariant {
                    rd: r_is_ok,
                    ra: r_inner,
                    tag: ok_tag,
                });
                let jmp_err = fc.emit(Op::JmpIfNot {
                    offset: 0,
                    ra: r_is_ok,
                });

                // Ok branch: unwrap.
                fc.emit(Op::Unwrap { rd, ra: r_inner });
                let jmp_end = fc.emit(Op::Jmp { offset: 0 });

                // Err branch: return the error value (via RetEarly so
                // wrap_ok can distinguish `?` from final-expression Err).
                fc.proto.patch_jump(jmp_err);
                fc.emit(Op::RetEarly { ra: r_inner });

                fc.proto.patch_jump(jmp_end);
                Ok(rd)
            }

            Expr::Old(inner) => {
                // old(expr) — in the interpreter, just evaluate the inner expression.
                // Snapshot semantics are handled by the verifier.
                self.compile_expr(fc, inner)
            }

            Expr::Spawn(inner) => {
                // spawn(expr) — compile the inner expression as a closure and spawn.
                // For now, compile as a regular call (concurrency runtime in Phase D).
                let r = self.compile_expr(fc, inner)?;
                let rd = fc.proto.alloc_reg();
                fc.emit(Op::Mov { rd, rs: r });
                Ok(rd)
            }

            Expr::Await(inner) => {
                // await(expr) — for now, just evaluate the inner expression.
                // Full async support in Phase D.
                self.compile_expr(fc, inner)
            }

            Expr::Comptime(block) => {
                // Comptime blocks are evaluated at runtime like a plain block
                // (tree-walker eval_comptime parity): statements execute in the
                // enclosing scope and may reference enclosing locals, so they
                // cannot be folded at compile time.
                fc.push_scope();
                let result = self.compile_block(fc, block)?;
                fc.pop_scope();
                Ok(result.unwrap_or_else(|| {
                    let r = fc.proto.alloc_reg();
                    fc.emit(Op::LoadUnit { rd: r });
                    r
                }))
            }

            Expr::Quote(block) => self.compile_quote_block_expr(fc, block),
            Expr::QuoteInterpolate(inner) => {
                let r = self.compile_expr(fc, inner)?;
                fc.emit(Op::QuoteInterpPush { rs: r });
                let rd = fc.proto.alloc_reg();
                fc.emit(Op::QuoteResult { rd });
                Ok(rd)
            }

            Expr::Arena(block) => {
                // Arena block: same semantics as a regular block expression.
                fc.push_scope();
                let result = self.compile_block(fc, block)?;
                fc.pop_scope();
                Ok(result.unwrap_or_else(|| {
                    let r = fc.proto.alloc_reg();
                    fc.emit(Op::LoadUnit { rd: r });
                    r
                }))
            }

            Expr::NamedArg(_name, inner) => {
                // Named argument: the name is call-site metadata; compile the value.
                self.compile_expr(fc, inner)
            }

            _ => Err(InterpError::new(format!(
                "bytecode compiler: expression {:?} not yet supported",
                std::mem::discriminant(expr.unlocated())
            ))),
        }
    }

    /// Compile a `quote! { ... }` block expression (0.33 Phase F).
    ///
    /// Emits stack-machine ops that assemble a `QuotedAst` on
    /// `BytecodeVM::quote_stack` at runtime, then `QuoteResult` into a
    /// register. Interpolation points (`$(expr)`) are compiled as ordinary
    /// expressions and embedded via `QuoteInterpPush`. Free identifiers are
    /// captured from the current function scope via `QuoteCapture` so
    /// `ast_eval` can resolve them in a temporary tree-walker env.
    fn compile_quote_block_expr(
        &mut self,
        fc: &mut FuncCompiler,
        block: &Block,
    ) -> Result<Reg, InterpError> {
        let mut shadowed = std::collections::HashSet::new();
        self.compile_quote_block(fc, block, &mut shadowed)?;
        fc.emit(Op::QuoteBlock {
            n: block.len() as u16,
        });
        let rd = fc.proto.alloc_reg();
        fc.emit(Op::QuoteResult { rd });
        Ok(rd)
    }

    /// Compile a quote block's statements onto the quote stack.
    fn compile_quote_block(
        &mut self,
        fc: &mut FuncCompiler,
        block: &Block,
        shadowed: &mut std::collections::HashSet<String>,
    ) -> Result<(), InterpError> {
        for stmt in block {
            self.compile_quote_stmt(fc, stmt, shadowed)?;
        }
        Ok(())
    }

    /// Compile a single statement inside a quote. Unsupported statements are
    /// skipped (mirroring the tree-walker's `quote_stmt -> None`).
    fn compile_quote_stmt(
        &mut self,
        fc: &mut FuncCompiler,
        stmt: &Stmt,
        shadowed: &mut std::collections::HashSet<String>,
    ) -> Result<(), InterpError> {
        match stmt.unlocated() {
            Stmt::Let { pat, init, .. } => {
                let name = match &pat.kind {
                    PatternKind::Variable(n) => n.clone(),
                    _ => return Ok(()),
                };
                shadowed.insert(name.clone());
                if let Some(e) = init {
                    self.compile_quote_expr(fc, e, shadowed)?;
                } else {
                    let idx = fc.proto.add_const(ConstValue::Unit);
                    fc.emit(Op::QuotePushLit { const_idx: idx });
                }
                let name_idx = fc.proto.add_const(ConstValue::Str(name));
                fc.emit(Op::QuoteLet { str_idx: name_idx });
            }
            Stmt::Expr(e) => {
                self.compile_quote_expr(fc, e, shadowed)?;
                fc.emit(Op::QuoteExprStmt);
            }
            Stmt::Return(e) => {
                if let Some(inner) = e {
                    self.compile_quote_expr(fc, inner, shadowed)?;
                    fc.emit(Op::QuoteReturn { has_value: true });
                } else {
                    fc.emit(Op::QuoteReturn { has_value: false });
                }
            }
            Stmt::Block(inner) => {
                self.compile_quote_block(fc, inner, shadowed)?;
                fc.emit(Op::QuoteBlock {
                    n: inner.len() as u16,
                });
            }
            Stmt::If { cond, then_, else_ } => {
                self.compile_quote_expr(fc, cond, shadowed)?;
                self.compile_quote_block(fc, then_, shadowed)?;
                fc.emit(Op::QuoteBlock {
                    n: then_.len() as u16,
                });
                let has_else = else_.is_some();
                if let Some(e) = else_ {
                    self.compile_quote_block(fc, e, shadowed)?;
                    fc.emit(Op::QuoteBlock { n: e.len() as u16 });
                }
                fc.emit(Op::QuoteIf { has_else });
            }
            Stmt::While { cond, body } => {
                self.compile_quote_expr(fc, cond, shadowed)?;
                self.compile_quote_block(fc, body, shadowed)?;
                fc.emit(Op::QuoteBlock {
                    n: body.len() as u16,
                });
                fc.emit(Op::QuoteWhile);
            }
            Stmt::WhileLet { pat, init, body } => {
                self.compile_quote_expr(fc, init, shadowed)?;
                self.compile_quote_block(fc, body, shadowed)?;
                fc.emit(Op::QuoteBlock {
                    n: body.len() as u16,
                });
                let pat_idx = fc.proto.add_const(ConstValue::Pattern(pat.clone()));
                fc.emit(Op::QuoteWhileLet { pat_idx });
            }
            Stmt::Break(e) => {
                if let Some(inner) = e {
                    self.compile_quote_expr(fc, inner, shadowed)?;
                }
                fc.emit(Op::QuoteBreak {
                    has_value: e.is_some(),
                });
            }
            Stmt::Continue => {
                fc.emit(Op::QuoteContinue);
            }
            Stmt::For {
                var,
                iterable,
                body,
            } => {
                self.compile_quote_expr(fc, iterable, shadowed)?;
                let var_name = var
                    .single_var_name()
                    .ok_or_else(|| {
                        InterpError::new("quote for-loop must bind a single identifier")
                    })?
                    .to_string();
                shadowed.insert(var_name.clone());
                self.compile_quote_block(fc, body, shadowed)?;
                fc.emit(Op::QuoteBlock {
                    n: body.len() as u16,
                });
                shadowed.remove(&var_name);
                let var_idx = fc.proto.add_const(ConstValue::Str(var_name.clone()));
                fc.emit(Op::QuoteFor { var_idx });
            }
            Stmt::Assign { target, value } => {
                self.compile_quote_expr(fc, target, shadowed)?;
                self.compile_quote_expr(fc, value, shadowed)?;
                fc.emit(Op::QuoteAssign);
            }
            Stmt::Loop(body) => {
                self.compile_quote_block(fc, body, shadowed)?;
                fc.emit(Op::QuoteBlock {
                    n: body.len() as u16,
                });
                fc.emit(Op::QuoteLoop);
            }
            // 0.31.22 soundness: contracts in quote! must error, not silently skip.
            Stmt::Requires(_, span) => {
                return Err(InterpError::new(format!(
                    "quote! does not support `requires` contracts (soundness hole fix). \
                     Contract at line {} col {} cannot be silently filtered.",
                    span.start_line, span.start_col
                )));
            }
            Stmt::Ensures(_, span) => {
                return Err(InterpError::new(format!(
                    "quote! does not support `ensures` contracts (soundness hole fix). \
                     Contract at line {} col {} cannot be silently filtered.",
                    span.start_line, span.start_col
                )));
            }
            Stmt::Math(_) => {
                return Err(InterpError::new(
                    "quote! does not support `math` blocks (soundness hole fix).",
                ));
            }
            // Unsupported statements are skipped (tree-walker parity).
            _ => {}
        }
        Ok(())
    }

    /// Compile a quote expression onto the quote stack.
    fn compile_quote_expr(
        &mut self,
        fc: &mut FuncCompiler,
        expr: &Expr,
        shadowed: &mut std::collections::HashSet<String>,
    ) -> Result<(), InterpError> {
        match expr.unlocated() {
            Expr::Literal(l) => {
                let idx = match l {
                    Lit::Int(v) => fc.proto.add_const(ConstValue::Int(*v)),
                    Lit::Float(v) => fc.proto.add_const(ConstValue::Float(*v)),
                    Lit::Bool(v) => fc.proto.add_const(ConstValue::Bool(*v)),
                    Lit::String(v) => fc.proto.add_const(ConstValue::Str(v.clone())),
                    Lit::Unit => fc.proto.add_const(ConstValue::Unit),
                    Lit::FString(_) => {
                        return Err(InterpError::new(
                            "bytecode quote: f-strings not supported in quote context",
                        ))
                    }
                };
                fc.emit(Op::QuotePushLit { const_idx: idx });
            }
            Expr::Ident(name) => {
                if !shadowed.contains(name) {
                    if let Some(reg) = fc.lookup_var(name) {
                        let name_idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                        fc.emit(Op::QuoteCapture {
                            str_idx: name_idx,
                            reg,
                        });
                    }
                }
                let idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                fc.emit(Op::QuotePushIdent { str_idx: idx });
            }
            Expr::Binary(op, l, r) => {
                self.compile_quote_expr(fc, l, shadowed)?;
                self.compile_quote_expr(fc, r, shadowed)?;
                fc.emit(Op::QuoteBinary { op: *op });
            }
            Expr::Unary(op, e) => {
                self.compile_quote_expr(fc, e, shadowed)?;
                fc.emit(Op::QuoteUnary { op: *op });
            }
            Expr::Call(callee, args) => {
                self.compile_quote_expr(fc, callee, shadowed)?;
                for a in args {
                    self.compile_quote_expr(fc, a, shadowed)?;
                }
                fc.emit(Op::QuoteCall {
                    argc: args.len() as u16,
                });
            }
            Expr::Field(obj, field) => {
                self.compile_quote_expr(fc, obj, shadowed)?;
                let idx = fc.proto.add_const(ConstValue::Str(field.clone()));
                fc.emit(Op::QuoteField { str_idx: idx });
            }
            Expr::Index(obj, idx_expr) => {
                self.compile_quote_expr(fc, obj, shadowed)?;
                self.compile_quote_expr(fc, idx_expr, shadowed)?;
                fc.emit(Op::QuoteIndex);
            }
            Expr::Tuple(elems) => {
                for e in elems {
                    self.compile_quote_expr(fc, e, shadowed)?;
                }
                fc.emit(Op::QuoteTuple {
                    n: elems.len() as u16,
                });
            }
            Expr::List(elems) => {
                for e in elems {
                    self.compile_quote_expr(fc, e, shadowed)?;
                }
                fc.emit(Op::QuoteList {
                    n: elems.len() as u16,
                });
            }
            Expr::If { cond, then_, else_ } => {
                self.compile_quote_expr(fc, cond, shadowed)?;
                self.compile_quote_block(fc, then_, shadowed)?;
                fc.emit(Op::QuoteBlock {
                    n: then_.len() as u16,
                });
                let has_else = else_.is_some();
                if let Some(e) = else_ {
                    self.compile_quote_block(fc, e, shadowed)?;
                    fc.emit(Op::QuoteBlock { n: e.len() as u16 });
                }
                fc.emit(Op::QuoteIf { has_else });
            }
            Expr::QuoteInterpolate(inner) | Expr::Old(inner) => {
                let r = self.compile_expr(fc, inner)?;
                fc.emit(Op::QuoteInterpPush { rs: r });
            }
            Expr::Quote(inner_block) => {
                let mut inner_shadowed = std::collections::HashSet::new();
                self.compile_quote_block(fc, inner_block, &mut inner_shadowed)?;
                fc.emit(Op::QuoteBlock {
                    n: inner_block.len() as u16,
                });
                let tmp = fc.proto.alloc_reg();
                fc.emit(Op::QuoteResult { rd: tmp });
                fc.emit(Op::QuoteAstPush { rs: tmp });
            }
            Expr::Cast(inner, ty) => {
                self.compile_quote_expr(fc, inner, shadowed)?;
                let idx = fc.proto.add_const(ConstValue::Type(ty.clone()));
                fc.emit(Op::QuoteCast { type_idx: idx });
            }
            Expr::Try(e) => {
                self.compile_quote_expr(fc, e, shadowed)?;
                fc.emit(Op::QuoteTry);
            }
            Expr::Turbofish(name, _type_args, args) => {
                let idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                fc.emit(Op::QuotePushIdent { str_idx: idx });
                for a in args {
                    self.compile_quote_expr(fc, a, shadowed)?;
                }
                fc.emit(Op::QuoteCall {
                    argc: args.len() as u16,
                });
            }
            Expr::Match(_, _) => {
                return Err(InterpError::new(
                    "quoted AST node 'Match' is unsupported by ABI v1",
                ));
            }
            Expr::Lambda { params, ret, body } => {
                // Emit QuoteCapture for free variables in the lambda body
                // (tree-walker eval_lambda captures from current env).
                let free_vars = self.collect_free_vars(body, params);
                for name in &free_vars {
                    if !shadowed.contains(name) {
                        if let Some(reg) = fc.lookup_var(name) {
                            let name_idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                            fc.emit(Op::QuoteCapture {
                                str_idx: name_idx,
                                reg,
                            });
                        }
                    }
                }
                let spec_idx = fc.proto.add_const(ConstValue::LambdaSpec {
                    params: params.clone(),
                    ret: ret.clone(),
                    body: body.clone(),
                    free_vars: free_vars.iter().cloned().collect(),
                });
                fc.emit(Op::QuoteLambda { spec_idx });
            }
            Expr::Record { ty, fields } => {
                // Emit field values onto quote stack, then QuoteRecord.
                for f in fields {
                    self.compile_quote_expr(fc, &f.value, shadowed)?;
                }
                let names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
                let names_idx = fc.proto.add_const(ConstValue::StrVec(names));
                let ty_str = ty.clone().unwrap_or_default();
                let ty_idx = fc.proto.add_const(ConstValue::Str(ty_str));
                fc.emit(Op::QuoteRecord {
                    n: fields.len() as u16,
                    names_idx,
                    ty_idx,
                });
            }
            other => {
                return Err(InterpError::new(format!(
                    "bytecode quote: expression {:?} not supported in quote context",
                    std::mem::discriminant(other)
                )));
            }
        }
        Ok(())
    }

    /// Evaluate a `comptime { ... }` block at compile time.
    /// Compiles the block as a temporary zero-param function, runs it in a
    /// sub-VM against the already-compiled function table, and inlines the
    /// result as a constant.

    /// Constant folding: evaluate binary operations on literals at compile time.
    fn fold_constants(&self, op: BinOp, l: &Lit, r: &Lit) -> Option<Lit> {
        match (l, r) {
            (Lit::Int(a), Lit::Int(b)) => {
                let result = match op {
                    BinOp::Add => a.checked_add(*b)?,
                    BinOp::Sub => a.checked_sub(*b)?,
                    BinOp::Mul => a.checked_mul(*b)?,
                    BinOp::Div => {
                        if *b == 0 {
                            return None; // Don't fold division by zero
                        }
                        a.checked_div(*b)?
                    }
                    BinOp::Mod => {
                        if *b == 0 {
                            return None;
                        }
                        a.checked_rem(*b)?
                    }
                    BinOp::EqCmp => return Some(Lit::Bool(a == b)),
                    BinOp::NeCmp => return Some(Lit::Bool(a != b)),
                    BinOp::Lt => return Some(Lit::Bool(a < b)),
                    BinOp::Gt => return Some(Lit::Bool(a > b)),
                    BinOp::Le => return Some(Lit::Bool(a <= b)),
                    BinOp::Ge => return Some(Lit::Bool(a >= b)),
                    BinOp::BitAnd => a & b,
                    BinOp::BitOr => a | b,
                    BinOp::BitXor => a ^ b,
                    BinOp::Shl => a.checked_shl(*b as u32)?,
                    BinOp::Shr => a.checked_shr(*b as u32)?,
                    BinOp::Pow => {
                        let exp = u32::try_from(*b).ok()?;
                        a.checked_pow(exp)?
                    }
                    _ => return None,
                };
                Some(Lit::Int(result))
            }
            (Lit::Float(a), Lit::Float(b)) => {
                let result = match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => {
                        if *b == 0.0 {
                            return None; // Don't fold division by zero
                        }
                        a / b
                    }
                    BinOp::EqCmp => return Some(Lit::Bool(a == b)),
                    BinOp::NeCmp => return Some(Lit::Bool(a != b)),
                    BinOp::Lt => return Some(Lit::Bool(a < b)),
                    BinOp::Gt => return Some(Lit::Bool(a > b)),
                    BinOp::Le => return Some(Lit::Bool(a <= b)),
                    BinOp::Ge => return Some(Lit::Bool(a >= b)),
                    BinOp::Pow => a.powf(*b),
                    _ => return None,
                };
                // Don't fold NaN/Inf results.
                if result.is_nan() || result.is_infinite() {
                    return None;
                }
                Some(Lit::Float(result))
            }
            (Lit::Bool(a), Lit::Bool(b)) => {
                let result = match op {
                    BinOp::And => *a && *b,
                    BinOp::Or => *a || *b,
                    BinOp::EqCmp => a == b,
                    BinOp::NeCmp => a != b,
                    _ => return None,
                };
                Some(Lit::Bool(result))
            }
            _ => None,
        }
    }

    fn compile_literal(&mut self, fc: &mut FuncCompiler, lit: &Lit) -> Result<Reg, InterpError> {
        let rd = fc.proto.alloc_reg();
        match lit {
            Lit::Int(v) => {
                let idx = fc.proto.add_const(ConstValue::Int(*v));
                fc.emit(Op::LoadConst { rd, idx });
            }
            Lit::Float(v) => {
                let idx = fc.proto.add_const(ConstValue::Float(*v));
                fc.emit(Op::LoadConst { rd, idx });
            }
            Lit::Bool(true) => {
                fc.emit(Op::LoadTrue { rd });
            }
            Lit::Bool(false) => {
                fc.emit(Op::LoadFalse { rd });
            }
            Lit::String(s) => {
                let idx = fc.proto.add_const(ConstValue::Str(s.clone()));
                fc.emit(Op::LoadConst { rd, idx });
            }
            Lit::Unit => {
                fc.emit(Op::LoadUnit { rd });
            }
            Lit::FString(parts) => {
                let mut prev = None;
                for part in parts {
                    let r_part = match part {
                        FStringPart::Text(t) => {
                            let r = fc.proto.alloc_reg();
                            let idx = fc.proto.add_const(ConstValue::Str(t.clone()));
                            fc.emit(Op::LoadConst { rd: r, idx });
                            r
                        }
                        FStringPart::Interp(expr) => {
                            let r_val = self.compile_expr(fc, expr)?;
                            let r_str = fc.proto.alloc_reg();
                            fc.emit(Op::ToString {
                                rd: r_str,
                                ra: r_val,
                            });
                            r_str
                        }
                    };
                    match prev {
                        None => prev = Some(r_part),
                        Some(r_acc) => {
                            let r_new = fc.proto.alloc_reg();
                            fc.emit(Op::ConcatStr {
                                rd: r_new,
                                ra: r_acc,
                                rb: r_part,
                            });
                            prev = Some(r_new);
                        }
                    }
                }
                let rd = fc.proto.alloc_reg();
                if let Some(r) = prev {
                    fc.emit(Op::Mov { rd, rs: r });
                } else {
                    let idx = fc.proto.add_const(ConstValue::Str(String::new()));
                    fc.emit(Op::LoadConst { rd, idx });
                }
                return Ok(rd);
            }
        }
        Ok(rd)
    }

    fn compile_literal_into(
        &mut self,
        fc: &mut FuncCompiler,
        lit: &Lit,
        rd: Reg,
    ) -> Result<Reg, InterpError> {
        match lit {
            Lit::Int(v) => {
                let idx = fc.proto.add_const(ConstValue::Int(*v));
                fc.emit(Op::LoadConst { rd, idx });
            }
            Lit::Float(v) => {
                let idx = fc.proto.add_const(ConstValue::Float(*v));
                fc.emit(Op::LoadConst { rd, idx });
            }
            Lit::Bool(true) => {
                fc.emit(Op::LoadTrue { rd });
            }
            Lit::Bool(false) => {
                fc.emit(Op::LoadFalse { rd });
            }
            Lit::String(s) => {
                let idx = fc.proto.add_const(ConstValue::Str(s.clone()));
                fc.emit(Op::LoadConst { rd, idx });
            }
            Lit::Unit => {
                fc.emit(Op::LoadUnit { rd });
            }
            Lit::FString(_) => {
                return Err(InterpError::new(
                    "compile_literal_into: FString must be compiled via compile_expr",
                ));
            }
        }
        Ok(rd)
    }

    fn compile_binary(
        &mut self,
        fc: &mut FuncCompiler,
        op: BinOp,
        l: &Expr,
        r: &Expr,
    ) -> Result<Reg, InterpError> {
        self.compile_binary_hinted(fc, op, l, r, None)
    }

    fn compile_binary_into(
        &mut self,
        fc: &mut FuncCompiler,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        target: Reg,
    ) -> Result<Reg, InterpError> {
        self.compile_binary_hinted(fc, op, l, r, Some(target))
    }

    /// Shared implementation: `hint` pins the result register when provided.
    fn compile_binary_hinted(
        &mut self,
        fc: &mut FuncCompiler,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        hint: Option<Reg>,
    ) -> Result<Reg, InterpError> {
        // Short-circuit for && and ||.
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.compile_short_circuit(fc, op, l, r);
        }

        // Range operator: a..b → compile as Expr::Range (builds a list).
        if matches!(op, BinOp::Range) {
            let r_start = self.compile_expr(fc, l)?;
            let r_end = self.compile_expr(fc, r)?;
            return self.compile_range_loop(fc, r_start, r_end);
        }

        // Constant folding: if both operands are literals, compute at compile time.
        if let (Expr::Literal(l_lit), Expr::Literal(r_lit)) = (l.unlocated(), r.unlocated()) {
            if let Some(folded) = self.fold_constants(op, l_lit, r_lit) {
                let folded = self.compile_literal(fc, &folded)?;
                return self.copy_into(fc, folded, hint.unwrap_or(folded));
            }
        }

        let ra = self.compile_expr(fc, l)?;
        let rb = self.compile_expr(fc, r)?;
        let rd = match hint {
            Some(t) => t,
            None => fc.proto.alloc_reg(),
        };

        // Determine if this is an int or float operation based on the AST.
        // In the full compiler, we'd use CheckedProgram types. For now,
        // we emit a generic dispatch that checks at runtime.
        // OPTIMIZATION: with type info, emit AddInt vs AddFloat directly.
        let is_float = self.expr_is_float(fc, l) || self.expr_is_float(fc, r);

        // String concatenation: + on strings emits ConcatStr.
        if matches!(op, BinOp::Add) && (self.expr_is_string(fc, l) || self.expr_is_string(fc, r)) {
            fc.emit(Op::ConcatStr { rd, ra, rb });
            return Ok(rd);
        }

        // String equality: == / != on strings emits generic Eq/Ne.
        if matches!(op, BinOp::EqCmp | BinOp::NeCmp)
            && (self.expr_is_string(fc, l) || self.expr_is_string(fc, r))
        {
            let instr = match op {
                BinOp::EqCmp => Op::Eq { rd, ra, rb },
                BinOp::NeCmp => Op::Ne { rd, ra, rb },
                _ => unreachable!(),
            };
            fc.emit(instr);
            return Ok(rd);
        }

        if is_float {
            self.emit_float_binop(fc, op, rd, ra, rb)?;
        } else {
            self.emit_int_binop(fc, op, rd, ra, rb)?;
        }
        Ok(rd)
    }

    fn emit_int_binop(
        &mut self,
        fc: &mut FuncCompiler,
        op: BinOp,
        rd: Reg,
        ra: Reg,
        rb: Reg,
    ) -> Result<(), InterpError> {
        let instr = match op {
            BinOp::Add => Op::AddInt { rd, ra, rb },
            BinOp::Sub => Op::SubInt { rd, ra, rb },
            BinOp::Mul => Op::MulInt { rd, ra, rb },
            BinOp::Div => Op::DivInt { rd, ra, rb },
            BinOp::Mod => Op::ModInt { rd, ra, rb },
            BinOp::EqCmp => Op::EqInt { rd, ra, rb },
            BinOp::NeCmp => Op::NeInt { rd, ra, rb },
            BinOp::Lt => Op::LtInt { rd, ra, rb },
            BinOp::Gt => Op::GtInt { rd, ra, rb },
            BinOp::Le => Op::LeInt { rd, ra, rb },
            BinOp::Ge => Op::GeInt { rd, ra, rb },
            BinOp::BitAnd => Op::BitAnd { rd, ra, rb },
            BinOp::BitOr => Op::BitOr { rd, ra, rb },
            BinOp::BitXor => Op::BitXor { rd, ra, rb },
            BinOp::Shl => Op::Shl { rd, ra, rb },
            BinOp::Shr => Op::Shr { rd, ra, rb },
            BinOp::Pow => Op::PowInt { rd, ra, rb },
            _ => {
                return Err(InterpError::new(format!(
                    "bytecode: unsupported int binary op {:?}",
                    op
                )))
            }
        };
        fc.emit(instr);
        Ok(())
    }

    fn emit_float_binop(
        &mut self,
        fc: &mut FuncCompiler,
        op: BinOp,
        rd: Reg,
        ra: Reg,
        rb: Reg,
    ) -> Result<(), InterpError> {
        let instr = match op {
            BinOp::Add => Op::AddFloat { rd, ra, rb },
            BinOp::Sub => Op::SubFloat { rd, ra, rb },
            BinOp::Mul => Op::MulFloat { rd, ra, rb },
            BinOp::Div => Op::DivFloat { rd, ra, rb },
            BinOp::Lt => Op::LtFloat { rd, ra, rb },
            BinOp::Gt => Op::GtFloat { rd, ra, rb },
            BinOp::Le => Op::LeFloat { rd, ra, rb },
            BinOp::Ge => Op::GeFloat { rd, ra, rb },
            BinOp::EqCmp => Op::Eq { rd, ra, rb },
            BinOp::NeCmp => Op::Ne { rd, ra, rb },
            BinOp::Pow => Op::PowFloat { rd, ra, rb },
            _ => {
                return Err(InterpError::new(format!(
                    "bytecode: unsupported float binary op {:?}",
                    op
                )))
            }
        };
        fc.emit(instr);
        Ok(())
    }

    fn compile_short_circuit(
        &mut self,
        fc: &mut FuncCompiler,
        op: BinOp,
        l: &Expr,
        r: &Expr,
    ) -> Result<Reg, InterpError> {
        let ra = self.compile_expr(fc, l)?;
        let rd = fc.proto.alloc_reg();

        match op {
            BinOp::And => {
                // if !ra goto false_branch
                let jmp_false = fc.emit(Op::JmpIfNot { offset: 0, ra });
                let rb = self.compile_expr(fc, r)?;
                fc.emit(Op::Mov { rd, rs: rb });
                let jmp_end = fc.emit(Op::Jmp { offset: 0 });
                // false_branch: rd = false
                fc.proto.patch_jump(jmp_false);
                fc.emit(Op::LoadFalse { rd });
                fc.proto.patch_jump(jmp_end);
            }
            BinOp::Or => {
                // if ra goto true_branch
                let jmp_true = fc.emit(Op::JmpIf { offset: 0, ra });
                let rb = self.compile_expr(fc, r)?;
                fc.emit(Op::Mov { rd, rs: rb });
                let jmp_end = fc.emit(Op::Jmp { offset: 0 });
                // true_branch: rd = true
                fc.proto.patch_jump(jmp_true);
                fc.emit(Op::LoadTrue { rd });
                fc.proto.patch_jump(jmp_end);
            }
            _ => unreachable!(),
        }
        Ok(rd)
    }

    fn compile_unary(
        &mut self,
        fc: &mut FuncCompiler,
        op: UnOp,
        e: &Expr,
    ) -> Result<Reg, InterpError> {
        self.compile_unary_hinted(fc, op, e, None)
    }

    fn compile_unary_into(
        &mut self,
        fc: &mut FuncCompiler,
        op: UnOp,
        e: &Expr,
        target: Reg,
    ) -> Result<Reg, InterpError> {
        self.compile_unary_hinted(fc, op, e, Some(target))
    }

    fn compile_unary_hinted(
        &mut self,
        fc: &mut FuncCompiler,
        op: UnOp,
        e: &Expr,
        hint: Option<Reg>,
    ) -> Result<Reg, InterpError> {
        let ra = self.compile_expr(fc, e)?;
        let rd = match hint {
            Some(t) => t,
            None => fc.proto.alloc_reg(),
        };
        match op {
            UnOp::Neg => {
                // Determine int vs float.
                if self.expr_is_float(fc, e) {
                    fc.emit(Op::NegFloat { rd, ra });
                } else {
                    fc.emit(Op::NegInt { rd, ra });
                }
            }
            UnOp::Not => {
                fc.emit(Op::Not { rd, ra });
            }
            // Ownership operators: no-ops in value semantics.
            // &x, &mut x, *x all evaluate to the inner value at runtime.
            UnOp::Deref => {
                // *r reads through the borrow alias: the CURRENT value of the
                // original place (assignments via *r already write back).
                if let Expr::Ident(alias_name) = e.unlocated() {
                    if let Some(place) = fc.borrow_aliases.get(alias_name).cloned() {
                        let r_place = self.compile_expr(fc, &place)?;
                        fc.emit(Op::Mov { rd, rs: r_place });
                        return Ok(rd);
                    }
                }
                // *x on shared values unwraps the inner value; plain values
                // pass through unchanged (value semantics).
                fc.emit(Op::DerefValue { rd, ra });
            }
            UnOp::Ref | UnOp::RefMut => {
                fc.emit(Op::Mov { rd, rs: ra });
            }
        }
        Ok(rd)
    }

    fn compile_call(
        &mut self,
        fc: &mut FuncCompiler,
        callee: &Expr,
        args: &[Expr],
    ) -> Result<Reg, InterpError> {
        // ── Early-return special cases (no pre-allocation) ──
        if let Expr::Ident(name) = callee.unlocated() {
            // Special case: push(var, elem) → ListPush (in-place mutation).
            if name == "push" && args.len() == 2 {
                if let Expr::Ident(var_name) = args[0].unlocated() {
                    if let Some(var_reg) = fc.lookup_var(var_name) {
                        let elem_reg = self.compile_expr(fc, &args[1])?;
                        fc.emit(Op::ListPush {
                            ra: var_reg,
                            rb: elem_reg,
                        });
                        // Return Unit (tree-walker parity, eval/expr.rs):
                        // `push(var, val)` writes back in place and returns
                        // Unit, so push doesn't leak as a block value — which
                        // would make a `for { push(...) }` body non-Unit and
                        // terminate the loop as if it were a break-with-value.
                        let rd = fc.proto.alloc_reg();
                        fc.emit(Op::LoadUnit { rd });
                        return Ok(rd);
                    }
                }
            }

            // Variant constructors: Ok(v), Err(v), Some(v), None.
            match name.as_str() {
                "Ok" => {
                    let rd = fc.proto.alloc_reg();
                    let r_arg = self.compile_expr(fc, &args[0])?;
                    fc.emit(Op::Ok { rd, ra: r_arg });
                    return Ok(rd);
                }
                "Err" => {
                    let rd = fc.proto.alloc_reg();
                    let r_arg = self.compile_expr(fc, &args[0])?;
                    fc.emit(Op::Err { rd, ra: r_arg });
                    return Ok(rd);
                }
                "Some" => {
                    let rd = fc.proto.alloc_reg();
                    let r_arg = self.compile_expr(fc, &args[0])?;
                    fc.emit(Op::Some { rd, ra: r_arg });
                    return Ok(rd);
                }
                "None" => {
                    let rd = fc.proto.alloc_reg();
                    fc.emit(Op::None { rd });
                    return Ok(rd);
                }
                _ => {}
            }

            // Newtype constructors: UserId(42), etc.
            if self.newtype_names.contains(name.as_str()) {
                let rd = fc.proto.alloc_reg();
                let r_inner = self.compile_expr(fc, &args[0])?;
                // Newtype is represented as Variant(name, [inner]).
                let type_name_idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                fc.emit(Op::NewVariant {
                    rd,
                    type_name: type_name_idx,
                    variant: 0,
                    base: r_inner,
                    arity: 1,
                });
                return Ok(rd);
            }

            // 条款 11 escape hatch: unsafe_cast_protocol(x) is an identity —
            // the value passes through unchanged. dyn packing is implicit in
            // the bytecode value model (Value::Record carries the concrete
            // type name; dyn dispatch resolves by it at runtime).
            if name == "unsafe_cast_protocol" && args.len() == 1 {
                return self.compile_expr(fc, &args[0]);
            }
        }

        // ── Normal path: pre-allocate and compile all arguments ──
        // Pre-check: resolve target before compiling args to avoid
        // waste / half-compiled bytecode on failure.
        if let Expr::Ident(name) = callee.unlocated() {
            let is_known = self.builtin_table.contains_key(name.as_str())
                || self.func_table.contains_key(name.as_str())
                || self.variant_names.contains(name.as_str())
                || self.extern_names.contains(name.as_str())
                || fc.lookup_var(name).is_some();
            if !is_known {
                return Err(InterpError::new(format!("undefined function '{}'", name)));
            }
        }

        // ── Named argument reordering + default parameter filling ──
        // Resolve the effective argument list before compiling.
        let effective_args: Vec<Expr> = if let Expr::Ident(name) = callee.unlocated() {
            let has_named = args
                .iter()
                .any(|a| matches!(a.unlocated(), Expr::NamedArg(_, _)));
            let param_names = self.func_param_names.get(name.as_str());
            let defaults = self.func_defaults.get(name.as_str());

            if has_named
                || (defaults.is_some() && args.len() < param_names.map(|p| p.len()).unwrap_or(0))
            {
                if let Some(pnames) = param_names {
                    let mut reordered: Vec<Option<Expr>> = vec![None; pnames.len()];
                    // Place positional args first.
                    let mut pos_idx = 0;
                    for arg in args {
                        match arg.unlocated() {
                            Expr::NamedArg(arg_name, inner) => {
                                if let Some(idx) = pnames.iter().position(|p| p == arg_name) {
                                    reordered[idx] = Some(*inner.clone());
                                }
                            }
                            _ => {
                                if pos_idx < reordered.len() {
                                    reordered[pos_idx] = Some(arg.clone());
                                    pos_idx += 1;
                                }
                            }
                        }
                    }
                    // Fill missing args with defaults.
                    if let Some(defaults) = defaults {
                        for (i, slot) in reordered.iter_mut().enumerate() {
                            if slot.is_none() {
                                if let Some(default_expr) = defaults.get(i).and_then(|d| d.clone())
                                {
                                    *slot = Some(default_expr);
                                }
                            }
                        }
                    }
                    // Collect filled args (stop at first None — trailing defaults only).
                    reordered.into_iter().flatten().collect()
                } else {
                    args.to_vec()
                }
            } else {
                args.to_vec()
            }
        } else {
            args.to_vec()
        };

        let args_base = fc.proto.alloc_reg();
        for _ in 1..effective_args.len() {
            fc.proto.alloc_reg();
        }

        for (i, arg) in effective_args.iter().enumerate() {
            let target = args_base + i as Reg;
            let r = self.compile_expr_into(fc, arg, target)?;
            if r != target {
                fc.emit(Op::Mov { rd: target, rs: r });
            }
        }

        let rd = fc.proto.alloc_reg();

        if let Expr::Ident(name) = callee.unlocated() {
            // Resolution order (matches tree-walker scoping):
            // 1. User functions (top-level named functions)
            // 2. Local variables (closures bound via let)
            // 3. Builtins
            // 4. Variant constructors
            if let Some(&fidx) = self.func_table.get(name.as_str()) {
                let proto = &self.functions[fidx as usize];
                let mut targets: Vec<Reg> = Vec::new();
                let mut field_targets: Vec<(Reg, u32)> = Vec::new();
                for &pi in &proto.mut_param_indices {
                    if let Some(Expr::Ident(var_name)) =
                        effective_args.get(pi as usize).map(|a| a.unlocated())
                    {
                        if let Some(reg) = fc.lookup_var(var_name) {
                            targets.push(reg);
                        }
                    }
                    // v0.34.13 (clause 6, golden §3.3): payload member-level
                    // mutate borrow — `apply_filter(mutate self.buffer, s)`.
                    // The field is passed by value, but the callee's final
                    // parameter value must be RecordSet back into the payload
                    // slot on return (previously silently dropped).
                    // TODO(M3): only single-level Field(Ident, _) places get a
                    // writeback. A nested place (`mutate self.a.b` / `o.inner.value`
                    // → Field(Field(_))) matches neither the Ident arm above nor
                    // this arm, so its mutation is SILENTLY DROPPED (both backends
                    // agree — not an L1 break, but silent data loss). The checker
                    // does no mutate-arg place validation. Tracked by
                    // flow_features::mutate_nested_field_writeback_gap_m3 (#[ignore]).
                    if let Some(Expr::Field(obj, field)) =
                        effective_args.get(pi as usize).map(|a| a.unlocated())
                    {
                        if let Expr::Ident(obj_name) = obj.unlocated() {
                            if let Some(obj_reg) = fc.lookup_var(obj_name) {
                                let field_idx = fc.proto.add_const(ConstValue::Str(field.clone()));
                                field_targets.push((obj_reg, field_idx));
                            }
                        }
                    }
                }
                if !targets.is_empty() {
                    let base = fc.proto.alloc_reg();
                    for _ in 1..targets.len() {
                        fc.proto.alloc_reg();
                    }
                    for (i, t) in targets.iter().enumerate() {
                        let target = base + i as Reg;
                        let cidx = fc.proto.add_const(ConstValue::Int(*t as i64));
                        fc.emit(Op::LoadConst {
                            rd: target,
                            idx: cidx,
                        });
                    }
                    fc.emit(Op::MutateSetup {
                        regs_base: base,
                        count: targets.len() as u16,
                    });
                }
                if !field_targets.is_empty() {
                    let base = fc.proto.alloc_reg();
                    for _ in 1..field_targets.len() * 2 {
                        fc.proto.alloc_reg();
                    }
                    for (i, (obj_reg, field_idx)) in field_targets.iter().enumerate() {
                        let obj_slot = base + (i * 2) as Reg;
                        let field_slot = obj_slot + 1;
                        let oidx = fc.proto.add_const(ConstValue::Int(*obj_reg as i64));
                        fc.emit(Op::LoadConst {
                            rd: obj_slot,
                            idx: oidx,
                        });
                        fc.emit(Op::LoadConst {
                            rd: field_slot,
                            idx: *field_idx,
                        });
                    }
                    fc.emit(Op::MutateSetupField {
                        regs_base: base,
                        count: field_targets.len() as u16,
                    });
                }
                fc.emit(Op::Call {
                    rd,
                    func: fidx,
                    args_base,
                    argc: effective_args.len() as u16,
                });
                return Ok(rd);
            }
            // Local variable holding a closure (shadows builtins).
            if let Some(callee_reg) = fc.lookup_var(name) {
                fc.emit(Op::CallIndirect {
                    rd,
                    callee: callee_reg,
                    args_base,
                    argc: effective_args.len() as u16,
                });
                return Ok(rd);
            }
            // Builtin function.
            if let Some(&bidx) = self.builtin_table.get(name.as_str()) {
                fc.emit(Op::CallBuiltin {
                    rd,
                    builtin: bidx,
                    args_base,
                    argc: effective_args.len() as u16,
                });
                return Ok(rd);
            }
            // Extern (FFI) function — resolved at runtime through the shared
            // FfiRuntime (0.33 Phase D FFI forwarding).
            if let Some(pos) = self.extern_name_order.iter().position(|n| n == name) {
                fc.emit(Op::CallExtern {
                    rd,
                    extern_idx: pos as u16,
                    args_base,
                    argc: effective_args.len() as u16,
                });
                return Ok(rd);
            }
            // Enum variant constructors: Circle(5), Point(1, 2), etc.
            if self.variant_names.contains(name.as_str()) {
                let type_name_idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                fc.emit(Op::NewVariant {
                    rd,
                    type_name: type_name_idx,
                    variant: 0,
                    base: args_base,
                    arity: args.len() as u16,
                });
                return Ok(rd);
            }
        }

        // Method call: obj.method(args) → method(obj, args)
        if let Expr::Field(obj, method) = callee.unlocated() {
            // ── Actor spawn: ActorName.spawn() / ActorName.spawn_detached() ──
            // Checked BEFORE flow transitions (tree-walker order): a flow and an
            // actor may share a name (e.g. `flow W` + `actor W`); `W.spawn()`
            // must resolve to the actor constructor, not a flow transition.
            if let Expr::Ident(flow_name) = obj.unlocated() {
                if self.actor_names.contains(flow_name.as_str())
                    && (method == "spawn" || method == "spawn_detached")
                {
                    let actor_idx = fc.proto.add_const(ConstValue::Str(flow_name.clone()));
                    if method == "spawn_detached" {
                        fc.emit(Op::ActorSpawnDetached {
                            rd,
                            actor: actor_idx,
                        });
                    } else {
                        fc.emit(Op::ActorSpawn {
                            rd,
                            actor: actor_idx,
                        });
                    }
                    return Ok(rd);
                }
                // ── Flow transition call: FlowName::method(state, args) ──
                if self.flow_names.contains(flow_name.as_str()) {
                    // Compile all args (first arg is the from-state value).
                    let flow_idx = fc.proto.add_const(ConstValue::Str(flow_name.clone()));
                    let method_idx = fc.proto.add_const(ConstValue::Str(method.clone()));
                    fc.emit(Op::FlowTransition {
                        rd,
                        flow: flow_idx,
                        method: method_idx,
                        args_base,
                        argc: args.len() as u16,
                    });
                    return Ok(rd);
                }
            }
            // ── Module-qualified function call: Module::func(args) ──
            // `M::f` (and `Outer::Inner::f`) parses as a Field chain; if it
            // names a registered module function, call it like a top-level
            // function (tree-walker `build_qualified_path` parity).
            if let Some(qualified) = Self::build_qualified_path(obj, method) {
                if let Some(&fidx) = self.func_table.get(&qualified) {
                    let proto = &self.functions[fidx as usize];
                    let mut targets: Vec<Reg> = Vec::new();
                    for &pi in &proto.mut_param_indices {
                        if let Some(Expr::Ident(var_name)) =
                            args.get(pi as usize).map(|a| a.unlocated())
                        {
                            if let Some(reg) = fc.lookup_var(var_name) {
                                targets.push(reg);
                            }
                        }
                    }
                    if !targets.is_empty() {
                        let base = fc.proto.alloc_reg();
                        for _ in 1..targets.len() {
                            fc.proto.alloc_reg();
                        }
                        for (i, t) in targets.iter().enumerate() {
                            let target = base + i as Reg;
                            let cidx = fc.proto.add_const(ConstValue::Int(*t as i64));
                            fc.emit(Op::LoadConst {
                                rd: target,
                                idx: cidx,
                            });
                        }
                        fc.emit(Op::MutateSetup {
                            regs_base: base,
                            count: targets.len() as u16,
                        });
                    }
                    fc.emit(Op::Call {
                        rd,
                        func: fidx,
                        args_base,
                        argc: args.len() as u16,
                    });
                    return Ok(rd);
                }
            }

            // Allocate shifted arg block BEFORE compiling receiver,
            // so receiver compilation cannot accidentally alias the block.
            let new_args_base = fc.proto.alloc_reg();
            for _ in 0..args.len() {
                fc.proto.alloc_reg();
            }
            // Compile receiver and move it into place.
            let recv_reg = self.compile_expr(fc, obj)?;
            fc.emit(Op::Mov {
                rd: new_args_base,
                rs: recv_reg,
            });
            // Move existing args to new_args_base + 1..
            for i in 0..args.len() {
                let src = args_base + i as Reg;
                let dst = new_args_base + 1 + i as Reg;
                fc.emit(Op::Mov { rd: dst, rs: src });
            }

            let total_args = args.len() + 1;

            // Actor methods shadow builtin opcode shortcuts (e.g. an actor's
            // own `unwrap` must not be compiled to Op::Unwrap).
            let is_actor_method = self
                .actor_defs
                .values()
                .any(|a| a.methods.iter().any(|m| m.name == *method));
            if !is_actor_method {
                // Option/Result built-in methods (handled via opcodes, not function calls).
                match method.as_str() {
                    "is_some" | "is_ok" => {
                        fc.emit(Op::IsSome { rd, ra: recv_reg });
                        return Ok(rd);
                    }
                    "is_none" | "is_err" => {
                        let r_tmp = fc.proto.alloc_reg();
                        fc.emit(Op::IsSome {
                            rd: r_tmp,
                            ra: recv_reg,
                        });
                        fc.emit(Op::Not { rd, ra: r_tmp });
                        return Ok(rd);
                    }
                    "unwrap" | "expect" => {
                        fc.emit(Op::Unwrap { rd, ra: recv_reg });
                        return Ok(rd);
                    }
                    "unwrap_or" => {
                        // if is_some(recv) { unwrap(recv) } else { default }
                        let r_test = fc.proto.alloc_reg();
                        fc.emit(Op::IsSome {
                            rd: r_test,
                            ra: recv_reg,
                        });
                        let jmp_else = fc.emit(Op::JmpIfNot {
                            offset: 0,
                            ra: r_test,
                        });
                        fc.emit(Op::Unwrap { rd, ra: recv_reg });
                        let jmp_end = fc.emit(Op::Jmp { offset: 0 });
                        fc.proto.patch_jump(jmp_else);
                        let r_default = self.compile_expr(fc, &args[0])?;
                        fc.emit(Op::Mov { rd, rs: r_default });
                        fc.proto.patch_jump(jmp_end);
                        return Ok(rd);
                    }
                    _ => {}
                }
            }

            // If the method matches a known actor method, use DynMethodCall
            // (actor methods shadow builtins with the same name, e.g. `format`).
            if !is_actor_method {
                // Try builtin methods (only if not an actor method).
                if let Some(&bidx) = self.builtin_table.get(method.as_str()) {
                    fc.emit(Op::CallBuiltin {
                        rd,
                        builtin: bidx,
                        args_base: new_args_base,
                        argc: total_args as u16,
                    });
                    return Ok(rd);
                }
            }

            // Try CheckedProgram method table (G1: precise dispatch).
            // The method_table maps (type_name, method_name) → mangled function name.
            // We try all known impl types as potential receivers.
            // dyn receivers: skip static resolution entirely — the concrete
            // type is only known at runtime (tree-walker DynTrait semantics).
            let recv_is_dyn = matches!(
                obj.unlocated(),
                Expr::Ident(name) if fc.var_is_dyn(name)
            );
            if !recv_is_dyn {
                for type_name in &self.impl_type_names.clone() {
                    if let Some(mangled) =
                        self.method_table.get(&(type_name.clone(), method.clone()))
                    {
                        if let Some(&fidx) = self.func_table.get(mangled) {
                            fc.emit(Op::Call {
                                rd,
                                func: fidx,
                                args_base: new_args_base,
                                argc: total_args as u16,
                            });
                            return Ok(rd);
                        }
                    }
                }

                // Fallback: mangled names via string prefix matching.
                let mut prefixes: Vec<&str> =
                    self.impl_type_names.iter().map(|s| s.as_str()).collect();
                for p in [
                    "List", "list", "String", "string", "Map", "map", "Set", "set",
                ] {
                    if !prefixes.contains(&p) {
                        prefixes.push(p);
                    }
                }
                for prefix in &prefixes {
                    let mangled = format!("{}_{}", prefix, method);
                    if let Some(&fidx) = self.func_table.get(&mangled) {
                        fc.emit(Op::Call {
                            rd,
                            func: fidx,
                            args_base: new_args_base,
                            argc: total_args as u16,
                        });
                        return Ok(rd);
                    }
                }
            }

            // Fallback: dynamic method call (runtime dispatch for actors, records, etc.).
            // This MUST come before bare user function lookup to prevent stdlib
            // functions from shadowing actor methods (e.g., `c.increment()` must
            // dispatch to the actor, not to a stdlib `increment` function).
            let method_idx = fc.proto.add_const(ConstValue::Str(method.clone()));
            fc.emit(Op::DynMethodCall {
                rd,
                method: method_idx,
                args_base: new_args_base,
                argc: total_args as u16,
            });
            return Ok(rd);
        }

        // Fallback: arbitrary callee expression (e.g., fns[0](args), get_func()(args)).
        // Compile the callee to a register and use CallIndirect.
        let r_callee = self.compile_expr(fc, callee)?;
        fc.emit(Op::CallIndirect {
            rd,
            callee: r_callee,
            args_base,
            argc: args.len() as u16,
        });
        Ok(rd)
    }

    fn compile_if_expr(
        &mut self,
        fc: &mut FuncCompiler,
        cond: &Expr,
        then_: &Block,
        else_: &Option<Block>,
    ) -> Result<Reg, InterpError> {
        let r_cond = self.compile_expr(fc, cond)?;
        let rd = fc.proto.alloc_reg();

        let jmp_else = fc.emit(Op::JmpIfNot {
            offset: 0,
            ra: r_cond,
        });
        fc.push_scope();
        let r_then = self.compile_block(fc, then_)?.unwrap_or_else(|| {
            let r = fc.proto.alloc_reg();
            fc.emit(Op::LoadUnit { rd: r });
            r
        });
        fc.pop_scope();
        fc.emit(Op::Mov { rd, rs: r_then });
        let jmp_end = fc.emit(Op::Jmp { offset: 0 });

        fc.proto.patch_jump(jmp_else);
        if let Some(else_block) = else_ {
            fc.push_scope();
            let r_else = self.compile_block(fc, else_block)?.unwrap_or_else(|| {
                let r = fc.proto.alloc_reg();
                fc.emit(Op::LoadUnit { rd: r });
                r
            });
            fc.pop_scope();
            fc.emit(Op::Mov { rd, rs: r_else });
        } else {
            fc.emit(Op::LoadUnit { rd });
        }
        fc.proto.patch_jump(jmp_end);

        Ok(rd)
    }

    /// Compile a match expression.
    ///
    /// Strategy: for each arm, emit a pattern test. If the test passes,
    /// bind variables and evaluate the body. Otherwise, fall through to
    /// the next arm.
    fn compile_match(
        &mut self,
        fc: &mut FuncCompiler,
        subject: &Expr,
        arms: &[MatchArm],
    ) -> Result<Reg, InterpError> {
        let r_subject = self.compile_expr(fc, subject)?;
        let rd = fc.proto.alloc_reg();

        let mut end_jumps = Vec::new();

        for arm in arms {
            // Compile pattern test. Returns (test_reg, bindings).
            // test_reg is None for patterns that always match (Wildcard, Variable).
            let (test_reg, bindings) = self.compile_pattern_test(fc, &arm.pat, r_subject)?;

            // If there's a test, emit JmpIfNot to skip this arm.
            let skip_jump = test_reg.map(|r_test| {
                fc.emit(Op::JmpIfNot {
                    offset: 0,
                    ra: r_test,
                })
            });

            // Check guard if present.
            let guard_jump = if let Some(guard) = &arm.guard {
                fc.push_scope();
                for (name, r) in &bindings {
                    fc.vars_mut().insert(name.clone(), *r);
                }
                let r_guard = self.compile_expr(fc, guard)?;
                fc.pop_scope();
                Some(fc.emit(Op::JmpIfNot {
                    offset: 0,
                    ra: r_guard,
                }))
            } else {
                None
            };

            // Bind pattern variables and compile body.
            fc.push_scope();
            for (name, r) in &bindings {
                fc.vars_mut().insert(name.clone(), *r);
            }
            let r_body = self.compile_expr(fc, &arm.body)?;
            fc.pop_scope();

            fc.emit(Op::Mov { rd, rs: r_body });
            end_jumps.push(fc.emit(Op::Jmp { offset: 0 }));

            // Patch skip jumps to here.
            if let Some(j) = skip_jump {
                fc.proto.patch_jump(j);
            }
            if let Some(j) = guard_jump {
                fc.proto.patch_jump(j);
            }
        }

        // Non-exhaustive match: return Unit (or could emit an error).
        fc.emit(Op::LoadUnit { rd });

        // Patch all end jumps.
        for j in end_jumps {
            fc.proto.patch_jump(j);
        }

        Ok(rd)
    }

    /// Compile a pattern test.
    ///
    /// Returns (test_reg, bindings):
    /// - test_reg: Some(reg) if the pattern needs a runtime test, None if it always matches
    /// - bindings: (name, reg) pairs for variables bound by the pattern
    fn compile_pattern_test(
        &mut self,
        fc: &mut FuncCompiler,
        pat: &Pattern,
        r_subject: Reg,
    ) -> Result<(Option<Reg>, Vec<(String, Reg)>), InterpError> {
        match &pat.kind {
            PatternKind::Wildcard => Ok((None, Vec::new())),

            PatternKind::Variable(name) => {
                // If the name is a known enum variant, treat as nullary constructor
                // pattern (check variant tag) rather than a catch-all variable binding.
                if self.variant_names.contains(name.as_str()) {
                    let r_test = fc.proto.alloc_reg();
                    let tag_idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                    fc.emit(Op::IsVariant {
                        rd: r_test,
                        ra: r_subject,
                        tag: tag_idx,
                    });
                    return Ok((Some(r_test), Vec::new()));
                }
                // Always matches; bind the subject to the variable.
                Ok((None, vec![(name.clone(), r_subject)]))
            }

            PatternKind::Literal(lit) => {
                // Compare subject with the literal using type-specialized op.
                let r_lit = self.compile_literal(fc, lit)?;
                let r_test = fc.proto.alloc_reg();
                let eq_op = match lit {
                    Lit::Int(_) => Op::EqInt {
                        rd: r_test,
                        ra: r_subject,
                        rb: r_lit,
                    },
                    Lit::Float(_) => Op::EqFloat {
                        rd: r_test,
                        ra: r_subject,
                        rb: r_lit,
                    },
                    _ => Op::Eq {
                        rd: r_test,
                        ra: r_subject,
                        rb: r_lit,
                    },
                };
                fc.emit(eq_op);
                Ok((Some(r_test), Vec::new()))
            }

            PatternKind::Constructor(name, pats) => {
                // Check variant tag.
                let r_test = fc.proto.alloc_reg();
                let tag_idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                fc.emit(Op::IsVariant {
                    rd: r_test,
                    ra: r_subject,
                    tag: tag_idx,
                });

                // Emit JmpIfNot to skip field extraction if variant doesn't match.
                // This prevents VariantGet from crashing on non-matching variants
                // (e.g., extracting field 0 from a zero-arity variant like None).
                let skip_extraction = fc.emit(Op::JmpIfNot {
                    offset: 0,
                    ra: r_test,
                });

                let mut binding_map = std::collections::HashMap::new();
                for (field_name, sub_pat) in pats.iter() {
                    // v0.34.15: extract by NAME (PatternField) — flow states
                    // are Record(Some(name), HashMap) so index-based VariantGet
                    // cannot address their fields; Variant payloads keep the
                    // positional _0.._N mapping inside the VM.
                    let r_field = fc.proto.alloc_reg();
                    let field_idx = fc.proto.add_const(ConstValue::Str(field_name.clone())) as u16;
                    fc.emit(Op::PatternField {
                        rd: r_field,
                        ra: r_subject,
                        field: field_idx,
                    });
                    // Recursively match the sub-pattern.
                    let (sub_test, sub_bindings) =
                        self.compile_pattern_test(fc, sub_pat, r_field)?;
                    // If sub-pattern has a test, AND it with the main test.
                    if let Some(r_sub) = sub_test {
                        fc.emit(Op::And {
                            rd: r_test,
                            ra: r_test,
                            rb: r_sub,
                        });
                    }
                    for (n, r) in sub_bindings {
                        binding_map.insert(n, r);
                    }
                    // If field_name is not a placeholder, bind it.
                    if !field_name.starts_with('_') {
                        binding_map.insert(field_name.clone(), r_field);
                    }
                }
                let bindings: Vec<_> = binding_map.into_iter().collect();

                // Patch skip_extraction to jump past field extraction.
                fc.proto.patch_jump(skip_extraction);

                Ok((Some(r_test), bindings))
            }

            PatternKind::Tuple(pats) => {
                // Type guard: check subject is a tuple before element access.
                let r_type = fc.proto.alloc_reg();
                fc.emit(Op::TypeOf {
                    rd: r_type,
                    ra: r_subject,
                });
                let r_tuple_str = fc.proto.alloc_reg();
                let tuple_str_idx = fc.proto.add_const(ConstValue::Str("tuple".into()));
                fc.emit(Op::LoadConst {
                    rd: r_tuple_str,
                    idx: tuple_str_idx,
                });
                let r_is_tuple = fc.proto.alloc_reg();
                fc.emit(Op::Eq {
                    rd: r_is_tuple,
                    ra: r_type,
                    rb: r_tuple_str,
                });

                let mut bindings = Vec::new();
                let mut test_reg = Some(r_is_tuple);

                for (i, sub_pat) in pats.iter().enumerate() {
                    let r_elem = fc.proto.alloc_reg();
                    fc.emit(Op::TupleGet {
                        rd: r_elem,
                        ra: r_subject,
                        idx: i as u16,
                    });
                    let (sub_test, sub_bindings) =
                        self.compile_pattern_test(fc, sub_pat, r_elem)?;
                    if let Some(r_sub) = sub_test {
                        if let Some(r_main) = test_reg {
                            fc.emit(Op::And {
                                rd: r_main,
                                ra: r_main,
                                rb: r_sub,
                            });
                        } else {
                            test_reg = Some(r_sub);
                        }
                    }
                    bindings.extend(sub_bindings);
                }

                Ok((test_reg, bindings))
            }

            PatternKind::Array(pats) => {
                // Exact-length array pattern: [p1, p2, p3]
                let r_type = fc.proto.alloc_reg();
                fc.emit(Op::TypeOf {
                    rd: r_type,
                    ra: r_subject,
                });
                let r_list_str = fc.proto.alloc_reg();
                let list_str_idx = fc.proto.add_const(ConstValue::Str("list".into()));
                fc.emit(Op::LoadConst {
                    rd: r_list_str,
                    idx: list_str_idx,
                });
                let r_is_list = fc.proto.alloc_reg();
                fc.emit(Op::Eq {
                    rd: r_is_list,
                    ra: r_type,
                    rb: r_list_str,
                });

                let mut bindings = Vec::new();
                let mut test_reg: Option<Reg> = Some(r_is_list);

                // Check exact length.
                let r_len = fc.proto.alloc_reg();
                fc.emit(Op::Len {
                    rd: r_len,
                    ra: r_subject,
                });
                let r_expected = fc.proto.alloc_reg();
                let len_idx = fc.proto.add_const(ConstValue::Int(pats.len() as i64));
                fc.emit(Op::LoadConst {
                    rd: r_expected,
                    idx: len_idx,
                });
                let r_len_test = fc.proto.alloc_reg();
                fc.emit(Op::EqInt {
                    rd: r_len_test,
                    ra: r_len,
                    rb: r_expected,
                });
                if let Some(r_main) = test_reg {
                    fc.emit(Op::And {
                        rd: r_main,
                        ra: r_main,
                        rb: r_len_test,
                    });
                } else {
                    test_reg = Some(r_len_test);
                }

                // Match each element (guarded by length check above).
                // If length check already failed, element access would OOB.
                // Use a conditional skip: if test_reg is false, skip element access.
                let skip_elements = test_reg.map(|r_test| {
                    fc.emit(Op::JmpIfNot {
                        offset: 0,
                        ra: r_test,
                    })
                });

                for (i, sub_pat) in pats.iter().enumerate() {
                    let r_elem = fc.proto.alloc_reg();
                    let r_idx = fc.proto.alloc_reg();
                    let idx_const = fc.proto.add_const(ConstValue::Int(i as i64));
                    fc.emit(Op::LoadConst {
                        rd: r_idx,
                        idx: idx_const,
                    });
                    fc.emit(Op::ListGet {
                        rd: r_elem,
                        ra: r_subject,
                        rb: r_idx,
                    });
                    let (sub_test, sub_bindings) =
                        self.compile_pattern_test(fc, sub_pat, r_elem)?;
                    if let Some(r_sub) = sub_test {
                        if let Some(r_main) = test_reg {
                            fc.emit(Op::And {
                                rd: r_main,
                                ra: r_main,
                                rb: r_sub,
                            });
                        } else {
                            test_reg = Some(r_sub);
                        }
                    }
                    bindings.extend(sub_bindings);
                }

                // Patch the skip jump to land here (after element access).
                if let Some(skip_idx) = skip_elements {
                    fc.proto.patch_jump(skip_idx);
                }

                Ok((test_reg, bindings))
            }

            PatternKind::Slice(pats, rest) => {
                // Slice pattern: [p1, p2, ..rest] — length >= pats.len().
                let r_type = fc.proto.alloc_reg();
                fc.emit(Op::TypeOf {
                    rd: r_type,
                    ra: r_subject,
                });
                let r_list_str = fc.proto.alloc_reg();
                let list_str_idx = fc.proto.add_const(ConstValue::Str("list".into()));
                fc.emit(Op::LoadConst {
                    rd: r_list_str,
                    idx: list_str_idx,
                });
                let r_is_list = fc.proto.alloc_reg();
                fc.emit(Op::Eq {
                    rd: r_is_list,
                    ra: r_type,
                    rb: r_list_str,
                });

                let mut bindings = Vec::new();
                let mut test_reg: Option<Reg> = Some(r_is_list);

                // Check length >= pats.len() (minimum required elements).
                let r_len = fc.proto.alloc_reg();
                fc.emit(Op::Len {
                    rd: r_len,
                    ra: r_subject,
                });
                let r_min = fc.proto.alloc_reg();
                let min_idx = fc.proto.add_const(ConstValue::Int(pats.len() as i64));
                fc.emit(Op::LoadConst {
                    rd: r_min,
                    idx: min_idx,
                });
                let r_len_test = fc.proto.alloc_reg();
                fc.emit(Op::GeInt {
                    rd: r_len_test,
                    ra: r_len,
                    rb: r_min,
                });
                if let Some(r_main) = test_reg {
                    fc.emit(Op::And {
                        rd: r_main,
                        ra: r_main,
                        rb: r_len_test,
                    });
                } else {
                    test_reg = Some(r_len_test);
                }

                // Match each fixed element (guarded by length check).
                // skip_elems jumps past element binds AND the rest binding:
                // it is patched to the end after the rest binding is emitted,
                // so a failed length test (e.g. `[a, ..rest]` on an empty
                // list) must not execute the rest slice (start = pats.len()
                // would exceed len and __slice rejects it).
                let skip_elems = test_reg.map(|r_test| {
                    fc.emit(Op::JmpIfNot {
                        offset: 0,
                        ra: r_test,
                    })
                });

                for (i, sub_pat) in pats.iter().enumerate() {
                    let r_elem = fc.proto.alloc_reg();
                    let r_idx = fc.proto.alloc_reg();
                    let idx_const = fc.proto.add_const(ConstValue::Int(i as i64));
                    fc.emit(Op::LoadConst {
                        rd: r_idx,
                        idx: idx_const,
                    });
                    fc.emit(Op::ListGet {
                        rd: r_elem,
                        ra: r_subject,
                        rb: r_idx,
                    });
                    let (sub_test, sub_bindings) =
                        self.compile_pattern_test(fc, sub_pat, r_elem)?;
                    if let Some(r_sub) = sub_test {
                        if let Some(r_main) = test_reg {
                            fc.emit(Op::And {
                                rd: r_main,
                                ra: r_main,
                                rb: r_sub,
                            });
                        } else {
                            test_reg = Some(r_sub);
                        }
                    }
                    bindings.extend(sub_bindings);
                }

                // Bind rest pattern: rest = subject[pats.len()..]
                if let Some(rest_pat) = rest {
                    if let PatternKind::Variable(rest_name) = &rest_pat.kind {
                        let r_rest = fc.proto.alloc_reg();
                        let r_start = fc.proto.alloc_reg();
                        let start_idx = fc.proto.add_const(ConstValue::Int(pats.len() as i64));
                        fc.emit(Op::LoadConst {
                            rd: r_start,
                            idx: start_idx,
                        });
                        // __slice(subject, pats.len(), len(subject))
                        let args_base = fc.proto.alloc_reg();
                        fc.proto.alloc_reg();
                        fc.proto.alloc_reg();
                        fc.emit(Op::Mov {
                            rd: args_base,
                            rs: r_subject,
                        });
                        fc.emit(Op::Mov {
                            rd: args_base + 1,
                            rs: r_start,
                        });
                        fc.emit(Op::Mov {
                            rd: args_base + 2,
                            rs: r_len,
                        });
                        if let Some(&bidx) = self.builtin_table.get("__slice") {
                            fc.emit(Op::CallBuiltin {
                                rd: r_rest,
                                builtin: bidx,
                                args_base,
                                argc: 3,
                            });
                        }
                        bindings.push((rest_name.clone(), r_rest));
                    }
                }

                // Patch the length-test jump to skip element binds AND the
                // rest binding (emitted above). On a failed length test the
                // pattern does not match; no binding registers are valid.
                if let Some(skip_idx) = skip_elems {
                    fc.proto.patch_jump(skip_idx);
                }

                Ok((test_reg, bindings))
            }
        }
    }

    fn compile_if_stmt(
        &mut self,
        fc: &mut FuncCompiler,
        cond: &Expr,
        then_: &Block,
        else_: Option<&Block>,
    ) -> Result<(), InterpError> {
        let r_cond = self.compile_expr(fc, cond)?;
        let jmp_else = fc.emit(Op::JmpIfNot {
            offset: 0,
            ra: r_cond,
        });

        fc.push_scope();
        self.compile_block(fc, then_)?;
        fc.pop_scope();

        let jmp_end = fc.emit(Op::Jmp { offset: 0 });
        fc.proto.patch_jump(jmp_else);

        if let Some(else_block) = else_ {
            fc.push_scope();
            self.compile_block(fc, else_block)?;
            fc.pop_scope();
        }
        fc.proto.patch_jump(jmp_end);

        Ok(())
    }

    /// Compile `if let pat = init { then_ } else { else_ }`.
    /// v0.34.3: pattern-match guard — test init against pat, bind pattern
    /// variables in the then-branch scope.
    fn compile_if_let_stmt(
        &mut self,
        fc: &mut FuncCompiler,
        pat: &Pattern,
        init: &Expr,
        then_: &Block,
        else_: Option<&Block>,
    ) -> Result<(), InterpError> {
        let r_init = self.compile_expr(fc, init)?;
        let (test_reg, bindings) = self.compile_pattern_test(fc, pat, r_init)?;

        let jmp_else = test_reg.map(|r_test| {
            fc.emit(Op::JmpIfNot {
                offset: 0,
                ra: r_test,
            })
        });

        // Then-branch: bind pattern variables, compile body.
        fc.push_scope();
        for (name, r) in &bindings {
            fc.vars_mut().insert(name.clone(), *r);
        }
        self.compile_block(fc, then_)?;
        fc.pop_scope();

        let jmp_end = fc.emit(Op::Jmp { offset: 0 });
        if let Some(jmp) = jmp_else {
            fc.proto.patch_jump(jmp);
        }

        if let Some(else_block) = else_ {
            fc.push_scope();
            self.compile_block(fc, else_block)?;
            fc.pop_scope();
        }
        fc.proto.patch_jump(jmp_end);

        Ok(())
    }

    /// Compile `while cond { body }`.
    /// Mimi semantic: non-Unit body value terminates the loop (loop-as-expression).
    fn compile_while(
        &mut self,
        fc: &mut FuncCompiler,
        cond: &Expr,
        body: &Block,
    ) -> Result<Reg, InterpError> {
        let result_reg = fc.proto.alloc_reg();
        fc.emit(Op::LoadUnit { rd: result_reg });
        fc.loop_result_regs.push(result_reg);

        fc.break_jumps.push(Vec::new());
        fc.continue_jumps.push(Vec::new());

        let loop_start = fc.proto.code.len();
        let r_cond = self.compile_expr(fc, cond)?;
        let jmp_end = fc.emit(Op::JmpIfNot {
            offset: 0,
            ra: r_cond,
        });

        fc.push_scope();
        let body_result = self.compile_block(fc, body)?;
        fc.pop_scope();

        // Loop-as-expression: non-Unit body value → terminate.
        if let Some(r_body) = body_result {
            let r_is_unit = fc.proto.alloc_reg();
            let r_unit = fc.proto.alloc_reg();
            fc.emit(Op::LoadUnit { rd: r_unit });
            fc.emit(Op::Eq {
                rd: r_is_unit,
                ra: r_body,
                rb: r_unit,
            });
            let jmp_continue = fc.emit(Op::JmpIf {
                offset: 0,
                ra: r_is_unit,
            });
            fc.emit(Op::Mov {
                rd: result_reg,
                rs: r_body,
            });
            let jmp_break = fc.emit(Op::Jmp { offset: 0 });
            fc.break_jumps_mut().push(jmp_break);
            fc.proto.patch_jump(jmp_continue);
        }

        // Jump back to loop start.
        fc.emit(Op::Jmp { offset: 0 });
        let jmp_back = fc.proto.code.len() - 1;
        fc.proto.patch_jump_to(jmp_back, loop_start);

        // Patch exit jump.
        let end = fc.proto.code.len();
        fc.proto.patch_jump_to(jmp_end, end);

        // Patch break jumps.
        if let Some(breaks) = fc.break_jumps.pop() {
            for b in breaks {
                fc.proto.patch_jump_to(b, end);
            }
        }
        // Continue jumps back to condition check.
        if let Some(continues) = fc.continue_jumps.pop() {
            for c in continues {
                fc.proto.patch_jump_to(c, loop_start);
            }
        }

        fc.loop_result_regs.pop();
        Ok(result_reg)
    }

    /// Compile `loop { body }` — infinite loop with break.
    /// Compile an infinite loop. Returns the loop result register
    /// (holds the value from `break expr`, or Unit if no break value).
    fn compile_loop(&mut self, fc: &mut FuncCompiler, body: &Block) -> Result<Reg, InterpError> {
        // Allocate loop result register (initialized to Unit).
        let result_reg = fc.proto.alloc_reg();
        fc.emit(Op::LoadUnit { rd: result_reg });
        fc.loop_result_regs.push(result_reg);

        fc.break_jumps.push(Vec::new());
        fc.continue_jumps.push(Vec::new());

        let loop_start = fc.proto.code.len();

        fc.push_scope();
        self.compile_block(fc, body)?;
        fc.pop_scope();

        // Jump back to loop start (infinite).
        fc.emit(Op::Jmp { offset: 0 });
        let jmp_back = fc.proto.code.len() - 1;
        fc.proto.patch_jump_to(jmp_back, loop_start);

        let end = fc.proto.code.len();

        // Patch break jumps.
        if let Some(breaks) = fc.break_jumps.pop() {
            for b in breaks {
                fc.proto.patch_jump_to(b, end);
            }
        }
        // Continue jumps back to loop start.
        if let Some(continues) = fc.continue_jumps.pop() {
            for c in continues {
                fc.proto.patch_jump_to(c, loop_start);
            }
        }

        fc.loop_result_regs.pop();
        Ok(result_reg)
    }

    /// Compile `while let pat = init { body }`.
    /// Mimi semantic: if the body produces a non-Unit value, the loop
    /// terminates and returns that value (loop-as-expression).
    /// Returns the loop result register.
    fn compile_while_let(
        &mut self,
        fc: &mut FuncCompiler,
        pat: &Pattern,
        init: &Expr,
        body: &Block,
    ) -> Result<Reg, InterpError> {
        // Loop result register (for body-value termination + break-with-value).
        let result_reg = fc.proto.alloc_reg();
        fc.emit(Op::LoadUnit { rd: result_reg });
        fc.loop_result_regs.push(result_reg);

        fc.break_jumps.push(Vec::new());
        fc.continue_jumps.push(Vec::new());

        let loop_start = fc.proto.code.len();

        // Evaluate the init expression.
        let r_init = self.compile_expr(fc, init)?;

        // Try to match the pattern. If it fails, exit the loop.
        let (test_reg, bindings) = self.compile_pattern_test(fc, pat, r_init)?;

        // Pattern always matches (e.g., variable pattern) => no test emitted.
        let jmp_end = test_reg.map(|r_test| {
            fc.emit(Op::JmpIfNot {
                offset: 0,
                ra: r_test,
            })
        });

        // Bind pattern variables and compile body.
        fc.push_scope();
        for (name, r) in &bindings {
            fc.vars_mut().insert(name.clone(), *r);
        }
        let body_result = self.compile_block(fc, body)?;
        fc.pop_scope();

        // Mimi loop-as-expression: if body produced a non-Unit value,
        // store it in result_reg and terminate the loop.
        if let Some(r_body) = body_result {
            // Check if body value != Unit → terminate.
            let r_is_unit = fc.proto.alloc_reg();
            let r_unit = fc.proto.alloc_reg();
            fc.emit(Op::LoadUnit { rd: r_unit });
            fc.emit(Op::Eq {
                rd: r_is_unit,
                ra: r_body,
                rb: r_unit,
            });
            let jmp_continue = fc.emit(Op::JmpIf {
                offset: 0,
                ra: r_is_unit,
            });
            // Non-Unit: store and break.
            fc.emit(Op::Mov {
                rd: result_reg,
                rs: r_body,
            });
            let jmp_break = fc.emit(Op::Jmp { offset: 0 });
            fc.break_jumps_mut().push(jmp_break);
            // Unit: continue looping.
            fc.proto.patch_jump(jmp_continue);
        }

        // Jump back to loop start.
        fc.emit(Op::Jmp { offset: 0 });
        let jmp_back = fc.proto.code.len() - 1;
        fc.proto.patch_jump_to(jmp_back, loop_start);

        let end = fc.proto.code.len();

        if let Some(j) = jmp_end {
            fc.proto.patch_jump_to(j, end);
        }

        // Patch break jumps.
        if let Some(breaks) = fc.break_jumps.pop() {
            for b in breaks {
                fc.proto.patch_jump_to(b, end);
            }
        }
        // Continue jumps back to loop start (re-evaluate init).
        if let Some(continues) = fc.continue_jumps.pop() {
            for c in continues {
                fc.proto.patch_jump_to(c, loop_start);
            }
        }

        fc.loop_result_regs.pop();
        Ok(result_reg)
    }

    /// Compile `for var in iter { body }`.
    /// Mimi semantic: non-Unit body value terminates the loop (loop-as-expression).
    /// Optimization: `for i in range(a, b)` compiles as a counter loop (no list allocation).
    fn compile_for(
        &mut self,
        fc: &mut FuncCompiler,
        var: &Pattern,
        iter: &Expr,
        body: &Block,
    ) -> Result<Reg, InterpError> {
        // v0.34.3: for-loop patterns — single identifier binds the element;
        // tuple/constructor patterns destructure it via bind_pattern.
        // Detect `range(start, end)` pattern → compile as counter loop.
        if let Expr::Call(callee, args) = iter {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "range" && args.len() == 2 {
                    return self.compile_for_range(fc, var, &args[0], &args[1], body);
                }
            }
        }

        let result_reg = fc.proto.alloc_reg();
        fc.emit(Op::LoadUnit { rd: result_reg });
        fc.loop_result_regs.push(result_reg);

        // Compile iterable.
        let r_iter = self.compile_expr(fc, iter)?;
        // Allocate index counter and length.
        let r_idx = fc.proto.alloc_reg();
        let r_len = fc.proto.alloc_reg();
        let r_one = fc.proto.alloc_reg();

        let c0 = fc.proto.add_const(ConstValue::Int(0));
        let c1 = fc.proto.add_const(ConstValue::Int(1));

        fc.emit(Op::LoadConst { rd: r_idx, idx: c0 });
        fc.emit(Op::LoadConst { rd: r_one, idx: c1 });
        fc.emit(Op::Len {
            rd: r_len,
            ra: r_iter,
        });

        fc.break_jumps.push(Vec::new());
        fc.continue_jumps.push(Vec::new());

        let loop_start = fc.proto.code.len();
        // r_cmp = (idx < len)
        let r_cmp = fc.proto.alloc_reg();
        fc.emit(Op::LtInt {
            rd: r_cmp,
            ra: r_idx,
            rb: r_len,
        });
        let jmp_end = fc.emit(Op::JmpIfNot {
            offset: 0,
            ra: r_cmp,
        });

        // Push scope for loop variable (prevents leak to outer scope).
        fc.push_scope();
        // element = iter[idx]; bind via pattern (single ident or destructure).
        let r_elem = fc.proto.alloc_reg();
        fc.emit(Op::ListGet {
            rd: r_elem,
            ra: r_iter,
            rb: r_idx,
        });
        self.bind_pattern(fc, var, r_elem);

        let body_result = self.compile_block(fc, body)?;
        fc.pop_scope();

        // Loop-as-expression: non-Unit body value → terminate.
        if let Some(r_body) = body_result {
            let r_is_unit = fc.proto.alloc_reg();
            let r_unit = fc.proto.alloc_reg();
            fc.emit(Op::LoadUnit { rd: r_unit });
            fc.emit(Op::Eq {
                rd: r_is_unit,
                ra: r_body,
                rb: r_unit,
            });
            let jmp_continue = fc.emit(Op::JmpIf {
                offset: 0,
                ra: r_is_unit,
            });
            fc.emit(Op::Mov {
                rd: result_reg,
                rs: r_body,
            });
            let jmp_break = fc.emit(Op::Jmp { offset: 0 });
            fc.break_jumps_mut().push(jmp_break);
            fc.proto.patch_jump(jmp_continue);
        }

        // Increment step (continue jumps here).
        let increment_pos = fc.proto.code.len();
        fc.emit(Op::AddInt {
            rd: r_idx,
            ra: r_idx,
            rb: r_one,
        });
        fc.emit(Op::Jmp { offset: 0 });
        let jmp_back = fc.proto.code.len() - 1;
        fc.proto.patch_jump_to(jmp_back, loop_start);

        let end = fc.proto.code.len();
        fc.proto.patch_jump_to(jmp_end, end);
        if let Some(breaks) = fc.break_jumps.pop() {
            for b in breaks {
                fc.proto.patch_jump_to(b, end);
            }
        }
        // Continue jumps to increment step (skip body, go to idx++).
        if let Some(continues) = fc.continue_jumps.pop() {
            for c in continues {
                fc.proto.patch_jump_to(c, increment_pos);
            }
        }

        fc.loop_result_regs.pop();
        Ok(result_reg)
    }

    /// Optimized `for var in range(start, end) { body }` — counter loop, no list allocation.
    fn compile_for_range(
        &mut self,
        fc: &mut FuncCompiler,
        var: &Pattern,
        start_expr: &Expr,
        end_expr: &Expr,
        body: &Block,
    ) -> Result<Reg, InterpError> {
        let result_reg = fc.proto.alloc_reg();
        fc.emit(Op::LoadUnit { rd: result_reg });
        fc.loop_result_regs.push(result_reg);

        // Compile start and end bounds.
        // Allocate fresh registers to avoid aliasing the original variables
        // (the loop increments r_idx in place).
        let r_start = self.compile_expr(fc, start_expr)?;
        let r_idx = fc.proto.alloc_reg();
        fc.emit(Op::Mov {
            rd: r_idx,
            rs: r_start,
        });
        let r_end_src = self.compile_expr(fc, end_expr)?;
        let r_end = fc.proto.alloc_reg();
        fc.emit(Op::Mov {
            rd: r_end,
            rs: r_end_src,
        });
        let r_one = fc.proto.alloc_reg();
        let c1 = fc.proto.add_const(ConstValue::Int(1));
        fc.emit(Op::LoadConst { rd: r_one, idx: c1 });

        fc.break_jumps.push(Vec::new());
        fc.continue_jumps.push(Vec::new());

        let loop_start = fc.proto.code.len();
        // r_cmp = (idx < end)
        let r_cmp = fc.proto.alloc_reg();
        fc.emit(Op::LtInt {
            rd: r_cmp,
            ra: r_idx,
            rb: r_end,
        });
        let jmp_end = fc.emit(Op::JmpIfNot {
            offset: 0,
            ra: r_cmp,
        });

        // Bind loop variable to the counter (range element is an i32 value).
        fc.push_scope();
        let r_elem = fc.proto.alloc_reg();
        fc.emit(Op::Mov {
            rd: r_elem,
            rs: r_idx,
        });
        self.bind_pattern(fc, var, r_elem);

        let body_result = self.compile_block(fc, body)?;
        fc.pop_scope();

        // Loop-as-expression: non-Unit body value → terminate.
        if let Some(r_body) = body_result {
            let r_is_unit = fc.proto.alloc_reg();
            let r_unit = fc.proto.alloc_reg();
            fc.emit(Op::LoadUnit { rd: r_unit });
            fc.emit(Op::Eq {
                rd: r_is_unit,
                ra: r_body,
                rb: r_unit,
            });
            let jmp_continue = fc.emit(Op::JmpIf {
                offset: 0,
                ra: r_is_unit,
            });
            fc.emit(Op::Mov {
                rd: result_reg,
                rs: r_body,
            });
            let jmp_break = fc.emit(Op::Jmp { offset: 0 });
            fc.break_jumps_mut().push(jmp_break);
            fc.proto.patch_jump(jmp_continue);
        }

        // Increment counter (continue jumps here).
        let increment_pos = fc.proto.code.len();
        fc.emit(Op::AddInt {
            rd: r_idx,
            ra: r_idx,
            rb: r_one,
        });
        fc.emit(Op::Jmp { offset: 0 });
        let jmp_back = fc.proto.code.len() - 1;
        fc.proto.patch_jump_to(jmp_back, loop_start);

        let end = fc.proto.code.len();
        fc.proto.patch_jump_to(jmp_end, end);
        if let Some(breaks) = fc.break_jumps.pop() {
            for b in breaks {
                fc.proto.patch_jump_to(b, end);
            }
        }
        if let Some(continues) = fc.continue_jumps.pop() {
            for c in continues {
                fc.proto.patch_jump_to(c, increment_pos);
            }
        }

        fc.loop_result_regs.pop();
        Ok(result_reg)
    }

    fn compile_assign(
        &mut self,
        fc: &mut FuncCompiler,
        target: &Expr,
        value: &Expr,
    ) -> Result<(), InterpError> {
        match target.unlocated() {
            Expr::Ident(name) => {
                // Immutable assignment guard (tree-walker parity, scope_env::assign):
                // `x = v` on a non-mut binding is a compile-time error here.
                // Borrow aliases are write-through targets, not plain variables —
                // check them first so `*alias = v` (compiled recursively to the
                // original place) is unaffected.
                if !fc.borrow_aliases.contains_key(name) && !fc.var_is_mut(name) {
                    return Err(InterpError::new(format!(
                        "cannot assign to immutable variable '{}' — use `let mut`",
                        name
                    )));
                }
                // Optimization: detect `s = s + expr` → StrAppend (in-place, avoids O(n²)).
                // SAFETY: only when variable is known String type (prevents Int += Int miscompile).
                if fc.reg_is_string(name) {
                    if let Expr::Binary(BinOp::Add, lhs, rhs) = value.unlocated() {
                        if let Expr::Ident(lhs_name) = lhs.unlocated() {
                            if lhs_name == name {
                                let r_var = fc.get_or_bind(name);
                                let r_rhs = self.compile_expr(fc, rhs)?;
                                fc.emit(Op::StrAppend {
                                    ra: r_var,
                                    rb: r_rhs,
                                });
                                return Ok(());
                            }
                        }
                    }
                }
                let r_val = self.compile_expr(fc, value)?;
                let r_var = fc.get_or_bind(name);
                // Track type for int/float dispatch.
                let ty = self.infer_expr_type(fc, value);
                fc.set_reg_type(name, ty);
                if r_val != r_var {
                    fc.emit(Op::Mov {
                        rd: r_var,
                        rs: r_val,
                    });
                }
                Ok(())
            }
            Expr::Index(obj, idx) => {
                let r_obj = self.compile_expr(fc, obj)?;
                let r_idx = self.compile_expr(fc, idx)?;
                let r_val = self.compile_expr(fc, value)?;
                fc.emit(Op::ListSet {
                    ra: r_obj,
                    rb: r_idx,
                    rc: r_val,
                });
                Ok(())
            }
            Expr::TupleIndex(obj, idx) => {
                // pair.1 = v → TupleSet (write-through works on tuples
                // held by value in the variable register).
                let r_obj = self.compile_expr(fc, obj)?;
                let r_val = self.compile_expr(fc, value)?;
                let field_idx = fc.proto.add_const(ConstValue::Str(idx.to_string()));
                fc.emit(Op::TupleSet {
                    ra: r_obj,
                    idx: field_idx,
                    rb: r_val,
                });
                Ok(())
            }
            Expr::Field(obj, field) => {
                // obj.field = value (record or actor field set).
                // For nested fields (e.g. outer.inner.value = x), we need
                // write-back: modify the sub-record, then store it back into
                // the parent. This is recursive.
                let r_val = self.compile_expr(fc, value)?;
                self.compile_field_assign(fc, obj, field, r_val)
            }
            Expr::Unary(UnOp::Deref, inner) => {
                // *ref = value → write through borrow alias to the original place.
                if let Expr::Ident(alias_name) = inner.unlocated() {
                    if let Some(place) = fc.borrow_aliases.get(alias_name).cloned() {
                        return self.compile_assign(fc, &place, value);
                    }
                }
                // Compile the value first.
                let r_val = self.compile_expr(fc, value)?;
                // Compile the deref target.
                let r_target = self.compile_expr(fc, inner)?;
                // Emit SharedSet to write through the reference.
                fc.emit(Op::SharedSet {
                    ra: r_target,
                    rb: r_val,
                });
                Ok(())
            }
            _ => Err(InterpError::new("bytecode: unsupported assignment target")),
        }
    }

    /// Assign to a field with write-back for nested access.
    /// For `outer.inner.value = x`:
    ///   1. Get outer's register (the root variable)
    ///   2. RecordGet outer.inner → temp_inner
    ///   3. RecordSet temp_inner.value = x
    ///   4. RecordSet outer.inner = temp_inner (write-back)
    fn compile_field_assign(
        &mut self,
        fc: &mut FuncCompiler,
        obj: &Expr,
        field: &str,
        r_val: Reg,
    ) -> Result<(), InterpError> {
        match obj.unlocated() {
            Expr::Ident(name) => {
                // Simple case: var.field = value. No write-back needed.
                let r_obj = fc.lookup_var(name).ok_or_else(|| {
                    InterpError::new(format!("undefined variable '{}' in field assign", name))
                })?;
                let field_idx = fc.proto.add_const(ConstValue::Str(field.to_string()));
                // Numeric field (tuple element access): pair.1 = v → TupleSet.
                if field.parse::<usize>().is_ok() {
                    fc.emit(Op::TupleSet {
                        ra: r_obj,
                        idx: field_idx,
                        rb: r_val,
                    });
                } else {
                    fc.emit(Op::RecordSet {
                        ra: r_obj,
                        field: field_idx,
                        rb: r_val,
                    });
                }
                Ok(())
            }
            Expr::Field(parent, parent_field) => {
                // Nested: parent.parent_field.field = value.
                // 1. Compile parent to get its register.
                let r_parent = self.compile_expr(fc, parent)?;
                // 2. Get the sub-record.
                let pf_idx = fc.proto.add_const(ConstValue::Str(parent_field.clone()));
                let r_sub = fc.proto.alloc_reg();
                fc.emit(Op::RecordGet {
                    rd: r_sub,
                    ra: r_parent,
                    field: pf_idx,
                });
                // 3. Set the target field on the sub-record.
                let f_idx = fc.proto.add_const(ConstValue::Str(field.to_string()));
                fc.emit(Op::RecordSet {
                    ra: r_sub,
                    field: f_idx,
                    rb: r_val,
                });
                // 4. Write back the modified sub-record into the parent.
                fc.emit(Op::RecordSet {
                    ra: r_parent,
                    field: pf_idx,
                    rb: r_sub,
                });
                // 5. If parent is itself nested, propagate write-back upward.
                //    (Handled recursively: compile_expr for Field chains resolves
                //    to the root variable's register via RecordGet chains, and
                //    the write-back at step 4 updates the immediate parent. For
                //    deeper nesting, the parent's RecordGet reads from the root.)
                Ok(())
            }
            Expr::Index(list_expr, idx_expr) => {
                // list[idx].field = value.
                let r_list = self.compile_expr(fc, list_expr)?;
                let r_idx = self.compile_expr(fc, idx_expr)?;
                // Get element.
                let r_elem = fc.proto.alloc_reg();
                fc.emit(Op::ListGet {
                    rd: r_elem,
                    ra: r_list,
                    rb: r_idx,
                });
                // Set field on element.
                let f_idx = fc.proto.add_const(ConstValue::Str(field.to_string()));
                fc.emit(Op::RecordSet {
                    ra: r_elem,
                    field: f_idx,
                    rb: r_val,
                });
                // Write back element to list.
                fc.emit(Op::ListSet {
                    ra: r_list,
                    rb: r_idx,
                    rc: r_elem,
                });
                Ok(())
            }
            _ => {
                // Fallback: compile obj normally and set field.
                let r_obj = self.compile_expr(fc, obj)?;
                let field_idx = fc.proto.add_const(ConstValue::Str(field.to_string()));
                fc.emit(Op::RecordSet {
                    ra: r_obj,
                    field: field_idx,
                    rb: r_val,
                });
                Ok(())
            }
        }
    }

    /// Compile a range expression (start..end) into a list via the range builtin.
    fn compile_range_loop(
        &mut self,
        fc: &mut FuncCompiler,
        r_start: Reg,
        r_end: Reg,
    ) -> Result<Reg, InterpError> {
        let rd = fc.proto.alloc_reg();
        let args_base = fc.proto.alloc_reg();
        fc.proto.alloc_reg(); // second arg slot
        fc.emit(Op::Mov {
            rd: args_base,
            rs: r_start,
        });
        fc.emit(Op::Mov {
            rd: args_base + 1,
            rs: r_end,
        });
        if let Some(&bidx) = self.builtin_table.get("range") {
            fc.emit(Op::CallBuiltin {
                rd,
                builtin: bidx,
                args_base,
                argc: 2,
            });
        } else {
            return Err(InterpError::new("bytecode: range builtin not registered"));
        }
        Ok(rd)
    }

    fn compile_index(
        &mut self,
        fc: &mut FuncCompiler,
        obj: &Expr,
        idx: &Expr,
    ) -> Result<Reg, InterpError> {
        let r_obj = self.compile_expr(fc, obj)?;
        let r_idx = self.compile_expr(fc, idx)?;
        let rd = fc.proto.alloc_reg();
        fc.emit(Op::ListGet {
            rd,
            ra: r_obj,
            rb: r_idx,
        });
        Ok(rd)
    }

    fn compile_list(&mut self, fc: &mut FuncCompiler, elems: &[Expr]) -> Result<Reg, InterpError> {
        let rd = fc.proto.alloc_reg();
        fc.emit(Op::NewList {
            rd,
            capacity: elems.len() as u32,
        });
        for elem in elems {
            let r = self.compile_expr(fc, elem)?;
            fc.emit(Op::ListPush { ra: rd, rb: r });
        }
        Ok(rd)
    }

    fn compile_tuple(&mut self, fc: &mut FuncCompiler, elems: &[Expr]) -> Result<Reg, InterpError> {
        let base = fc.proto.alloc_reg();
        for _ in 1..elems.len() {
            fc.proto.alloc_reg();
        }
        for (i, elem) in elems.iter().enumerate() {
            let r = self.compile_expr(fc, elem)?;
            let target = base + i as Reg;
            if r != target {
                fc.emit(Op::Mov { rd: target, rs: r });
            }
        }
        let rd = fc.proto.alloc_reg();
        fc.emit(Op::NewTuple {
            rd,
            base,
            arity: elems.len() as u16,
        });
        Ok(rd)
    }

    fn compile_record(
        &mut self,
        fc: &mut FuncCompiler,
        ty: Option<&str>,
        fields: &[RecordFieldExpr],
    ) -> Result<Reg, InterpError> {
        // IMPORTANT: Add type name + field names to constant pool FIRST,
        // before compiling field values. Use add_const_raw (no dedup) to
        // ensure contiguous indices — the VM's NewRecord handler reads
        // field names from constants[type_name+1..type_name+1+count].
        let type_name_idx = fc.proto.add_const_raw(ConstValue::Str(
            ty.map(|s| s.to_string()).unwrap_or_default(),
        ));
        for field in fields {
            fc.proto.add_const_raw(ConstValue::Str(field.name.clone()));
        }

        // Allocate registers for field values.
        let base = fc.proto.alloc_reg();
        for _ in 1..fields.len() {
            fc.proto.alloc_reg();
        }

        // Compile each field value.
        for (i, field) in fields.iter().enumerate() {
            let r = self.compile_expr(fc, &field.value)?;
            let target = base + i as Reg;
            if r != target {
                fc.emit(Op::Mov { rd: target, rs: r });
            }
        }

        let rd = fc.proto.alloc_reg();
        fc.emit(Op::NewRecord {
            rd,
            type_name: type_name_idx,
            base,
            count: fields.len() as u16,
        });
        Ok(rd)
    }

    /// Compile a lambda expression into a closure.
    ///
    /// Strategy:
    /// 1. Collect free variables (capture analysis)
    /// 2. Create a new FunctionProto for the lambda body
    /// 3. Compile the body with parameters + captured variables bound
    /// 4. Emit NewClosure with the proto index and captured variables
    fn compile_lambda(
        &mut self,
        fc: &mut FuncCompiler,
        params: &[Param],
        body: &Block,
    ) -> Result<Reg, InterpError> {
        // Step 1: Collect free variables that need to be captured.
        let free_vars = self.collect_free_vars(body, params);

        // Filter to only variables that exist in the outer scope.
        let captures: Vec<(String, Reg)> = free_vars
            .iter()
            .filter_map(|name| fc.lookup_var(name).map(|reg| (name.clone(), reg)))
            .collect();

        // Create a new function proto for the lambda.
        let lambda_name = format!("__lambda_{}", self.functions.len());
        let mut lambda_fc = FuncCompiler::new(lambda_name.clone(), params.len() as u16);

        // Bind parameters to registers 0..param_count.
        for (i, param) in params.iter().enumerate() {
            lambda_fc.vars[0].insert(param.name.clone(), i as Reg);
            if let Type::Name(n, _) = param.ty.unlocated() {
                if n == "f64" {
                    lambda_fc
                        .var_types
                        .insert(param.name.clone(), VarType::Float);
                }
            }
        }
        // Ensure register_count accounts for params.
        while lambda_fc.proto.register_count < params.len() as u16 {
            lambda_fc.proto.alloc_reg();
        }

        // Bind captured variables to registers param_count..param_count+capture_count.
        // The VM will load these from the closure's captured map when calling.
        for (i, (name, _outer_reg)) in captures.iter().enumerate() {
            let capture_reg = params.len() as Reg + i as Reg;
            lambda_fc.vars[0].insert(name.clone(), capture_reg);
        }
        // Ensure register_count accounts for captures.
        while lambda_fc.proto.register_count < (params.len() + captures.len()) as u16 {
            lambda_fc.proto.alloc_reg();
        }

        // Compile the body.
        lambda_fc.push_scope();
        let result_reg = self.compile_block(&mut lambda_fc, body)?;
        if let Some(r) = result_reg {
            lambda_fc.emit(Op::Ret { ra: r });
        } else {
            let r = lambda_fc.proto.alloc_reg();
            lambda_fc.emit(Op::LoadUnit { rd: r });
            lambda_fc.emit(Op::Ret { ra: r });
        }
        lambda_fc.pop_scope();

        // Add the lambda proto to the program.
        let lambda_idx = self.functions.len() as FuncIdx;
        // Set capture names in the proto.
        lambda_fc.proto.capture_names = captures.iter().map(|(name, _)| name.clone()).collect();
        self.functions.push(lambda_fc.proto);

        // Emit code to capture the variables.
        // Captures are stored as (name, value) pairs in consecutive registers.
        let captures_base = fc.proto.alloc_reg();
        for _ in 1..captures.len() {
            fc.proto.alloc_reg();
        }
        for (i, (_name, outer_reg)) in captures.iter().enumerate() {
            let target = captures_base + i as Reg;
            if *outer_reg != target {
                fc.emit(Op::Mov {
                    rd: target,
                    rs: *outer_reg,
                });
            }
        }

        let rd = fc.proto.alloc_reg();
        fc.emit(Op::NewClosure {
            rd,
            proto: lambda_idx,
            captures_base,
            capture_count: captures.len() as u16,
        });
        Ok(rd)
    }

    fn bind_pattern(&self, fc: &mut FuncCompiler, pat: &Pattern, reg: Reg) {
        match &pat.kind {
            PatternKind::Variable(name) => {
                fc.vars_mut().insert(name.clone(), reg);
            }
            PatternKind::Tuple(pats) => {
                for (i, p) in pats.iter().enumerate() {
                    let r = fc.proto.alloc_reg();
                    fc.emit(Op::TupleGet {
                        rd: r,
                        ra: reg,
                        idx: i as u16,
                    });
                    self.bind_pattern(fc, p, r);
                }
            }
            PatternKind::Constructor(_name, pats) => {
                // Newtype pattern: UserId(v) unwraps the inner value.
                // TupleGet already unwraps Value::Newtype at idx 0.
                if pats.len() == 1 {
                    let r = fc.proto.alloc_reg();
                    fc.emit(Op::TupleGet {
                        rd: r,
                        ra: reg,
                        idx: 0,
                    });
                    self.bind_pattern(fc, &pats[0].1, r);
                }
            }
            PatternKind::Wildcard => {}
            _ => {}
        }
    }

    /// Determine if an expression produces a float value.
    /// Uses literal detection + variable type tracking (until CheckedProgram integration).
    fn expr_is_float(&self, fc: &FuncCompiler, expr: &Expr) -> bool {
        match expr.unlocated() {
            Expr::Literal(Lit::Float(_)) => true,
            Expr::Cast(_, ty) => matches!(ty.unlocated(), Type::Name(n, _) if n == "f64"),
            Expr::Ident(name) => fc.reg_is_float(name),
            Expr::Binary(_, l, r) => self.expr_is_float(fc, l) || self.expr_is_float(fc, r),
            Expr::Unary(_, e) => self.expr_is_float(fc, e),
            Expr::If { then_, else_, .. } => {
                // Check if the then block's last expr is float.
                then_.last().map_or(false, |s| {
                    if let Stmt::Expr(e) = s.unlocated() {
                        self.expr_is_float(fc, e)
                    } else {
                        false
                    }
                }) || else_.as_ref().map_or(false, |b| {
                    b.last().map_or(false, |s| {
                        if let Stmt::Expr(e) = s.unlocated() {
                            self.expr_is_float(fc, e)
                        } else {
                            false
                        }
                    })
                })
            }
            _ => false,
        }
    }

    /// Determine if an expression produces a string value.
    fn expr_is_string(&self, fc: &FuncCompiler, expr: &Expr) -> bool {
        match expr.unlocated() {
            Expr::Literal(Lit::String(_)) => true,
            Expr::Ident(name) => fc.reg_is_string(name),
            Expr::Binary(BinOp::Add, l, r) => {
                self.expr_is_string(fc, l) || self.expr_is_string(fc, r)
            }
            _ => false,
        }
    }

    fn field_index(&self, _field: &str) -> u16 {
        // TODO: resolve from CheckedProgram type definitions.
        0
    }

    /// Collect free variables from a block (variables used but not defined locally).
    /// Returns a set of variable names that need to be captured.
    fn collect_free_vars(
        &self,
        block: &Block,
        params: &[Param],
    ) -> std::collections::HashSet<String> {
        let mut free_vars = std::collections::HashSet::new();
        let mut local_vars: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        self.collect_free_vars_block(block, &mut local_vars, &mut free_vars);
        free_vars
    }

    fn collect_free_vars_block(
        &self,
        block: &Block,
        local_vars: &mut std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashSet<String>,
    ) {
        for stmt in block {
            self.collect_free_vars_stmt(stmt, local_vars, free_vars);
        }
    }

    fn collect_free_vars_stmt(
        &self,
        stmt: &Stmt,
        local_vars: &mut std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashSet<String>,
    ) {
        match stmt.unlocated() {
            Stmt::Let { pat, init, .. } => {
                // First collect from init (before binding the pattern).
                if let Some(init_expr) = init {
                    self.collect_free_vars_expr(init_expr, local_vars, free_vars);
                }
                // Then bind the pattern variables.
                self.collect_pattern_vars(pat, local_vars);
            }
            Stmt::Expr(e) => {
                self.collect_free_vars_expr(e, local_vars, free_vars);
            }
            Stmt::If { cond, then_, else_ } => {
                self.collect_free_vars_expr(cond, local_vars, free_vars);
                self.collect_free_vars_block(then_, local_vars, free_vars);
                if let Some(else_block) = else_ {
                    self.collect_free_vars_block(else_block, local_vars, free_vars);
                }
            }
            Stmt::While { cond, body } => {
                self.collect_free_vars_expr(cond, local_vars, free_vars);
                self.collect_free_vars_block(body, local_vars, free_vars);
            }
            Stmt::For {
                var,
                iterable,
                body,
            } => {
                self.collect_free_vars_expr(iterable, local_vars, free_vars);
                if let Some(var_name) = var.single_var_name() {
                    local_vars.insert(var_name.to_string());
                }
                self.collect_free_vars_block(body, local_vars, free_vars);
            }
            Stmt::Return(e) => {
                if let Some(ret_expr) = e {
                    self.collect_free_vars_expr(ret_expr, local_vars, free_vars);
                }
            }
            Stmt::Assign { target, value } => {
                self.collect_free_vars_expr(target, local_vars, free_vars);
                self.collect_free_vars_expr(value, local_vars, free_vars);
            }
            Stmt::Drop(e) => {
                self.collect_free_vars_expr(e, local_vars, free_vars);
            }
            Stmt::Defer(block)
            | Stmt::Unsafe(block)
            | Stmt::Arena(block)
            | Stmt::IeeeFloat(block) => {
                self.collect_free_vars_block(block, local_vars, free_vars);
            }
            _ => {}
        }
    }

    fn collect_free_vars_expr(
        &self,
        expr: &Expr,
        local_vars: &mut std::collections::HashSet<String>,
        free_vars: &mut std::collections::HashSet<String>,
    ) {
        match expr.unlocated() {
            Expr::Ident(name) => {
                if !local_vars.contains(name) {
                    free_vars.insert(name.clone());
                }
            }
            Expr::Binary(_, l, r) => {
                self.collect_free_vars_expr(l, local_vars, free_vars);
                self.collect_free_vars_expr(r, local_vars, free_vars);
            }
            Expr::Unary(_, e) => {
                self.collect_free_vars_expr(e, local_vars, free_vars);
            }
            Expr::Call(callee, args) => {
                self.collect_free_vars_expr(callee, local_vars, free_vars);
                for arg in args {
                    self.collect_free_vars_expr(arg, local_vars, free_vars);
                }
            }
            Expr::Turbofish(_, _, args) => {
                for arg in args {
                    self.collect_free_vars_expr(arg, local_vars, free_vars);
                }
            }
            Expr::If { cond, then_, else_ } => {
                self.collect_free_vars_expr(cond, local_vars, free_vars);
                self.collect_free_vars_block(then_, local_vars, free_vars);
                if let Some(else_block) = else_ {
                    self.collect_free_vars_block(else_block, local_vars, free_vars);
                }
            }
            Expr::Block(b) => {
                self.collect_free_vars_block(b, local_vars, free_vars);
            }
            Expr::Index(obj, idx) => {
                self.collect_free_vars_expr(obj, local_vars, free_vars);
                self.collect_free_vars_expr(idx, local_vars, free_vars);
            }
            Expr::List(elems) => {
                for elem in elems {
                    self.collect_free_vars_expr(elem, local_vars, free_vars);
                }
            }
            Expr::Tuple(elems) => {
                for elem in elems {
                    self.collect_free_vars_expr(elem, local_vars, free_vars);
                }
            }
            Expr::Field(obj, _) => {
                self.collect_free_vars_expr(obj, local_vars, free_vars);
            }
            Expr::Match(subject, arms) => {
                self.collect_free_vars_expr(subject, local_vars, free_vars);
                for arm in arms {
                    // Pattern variables are local to the arm.
                    let mut arm_locals = local_vars.clone();
                    self.collect_pattern_vars(&arm.pat, &mut arm_locals);
                    if let Some(guard) = &arm.guard {
                        self.collect_free_vars_expr(guard, &mut arm_locals, free_vars);
                    }
                    self.collect_free_vars_expr(&arm.body, &mut arm_locals, free_vars);
                }
            }
            Expr::Lambda { params, body, .. } => {
                // Nested lambda: params are local, body may capture from outer.
                let mut nested_locals: std::collections::HashSet<String> =
                    params.iter().map(|p| p.name.clone()).collect();
                self.collect_free_vars_block(body, &mut nested_locals, free_vars);
            }
            Expr::Cast(inner, _) => {
                self.collect_free_vars_expr(inner, local_vars, free_vars);
            }
            // ── Previously missing variants (audit fix) ──
            Expr::Comprehension {
                expr,
                var,
                iter,
                guard,
            } => {
                self.collect_free_vars_expr(iter, local_vars, free_vars);
                if let Some(g) = guard {
                    self.collect_free_vars_expr(g, local_vars, free_vars);
                }
                let mut comp_locals = local_vars.clone();
                comp_locals.insert(var.clone());
                self.collect_free_vars_expr(expr, &mut comp_locals, free_vars);
            }
            Expr::Record { fields, .. } => {
                for f in fields {
                    self.collect_free_vars_expr(&f.value, local_vars, free_vars);
                }
            }
            Expr::Try(e)
            | Expr::OptionalChain(e, _)
            | Expr::Spawn(e)
            | Expr::Await(e)
            | Expr::QuoteInterpolate(e)
            | Expr::Old(e)
            | Expr::TypeOf(e)
            | Expr::TupleIndex(e, _)
            | Expr::NamedArg(_, e) => {
                self.collect_free_vars_expr(e, local_vars, free_vars);
            }
            Expr::SliceExpr { target, start, end } => {
                self.collect_free_vars_expr(target, local_vars, free_vars);
                if let Some(s) = start {
                    self.collect_free_vars_expr(s, local_vars, free_vars);
                }
                if let Some(e) = end {
                    self.collect_free_vars_expr(e, local_vars, free_vars);
                }
            }
            Expr::MapLiteral { entries } => {
                for (k, v) in entries {
                    self.collect_free_vars_expr(k, local_vars, free_vars);
                    self.collect_free_vars_expr(v, local_vars, free_vars);
                }
            }
            Expr::SetLiteral(elems) => {
                for e in elems {
                    self.collect_free_vars_expr(e, local_vars, free_vars);
                }
            }
            Expr::Comptime(block) | Expr::Arena(block) => {
                self.collect_free_vars_block(block, local_vars, free_vars);
            }
            _ => {}
        }
    }

    fn collect_pattern_vars(
        &self,
        pat: &Pattern,
        local_vars: &mut std::collections::HashSet<String>,
    ) {
        match &pat.kind {
            PatternKind::Variable(name) => {
                local_vars.insert(name.clone());
            }
            PatternKind::Tuple(pats) => {
                for p in pats {
                    self.collect_pattern_vars(p, local_vars);
                }
            }
            PatternKind::Constructor(_, pats) => {
                for (_, p) in pats {
                    self.collect_pattern_vars(p, local_vars);
                }
            }
            PatternKind::Array(pats) | PatternKind::Slice(pats, _) => {
                for p in pats {
                    self.collect_pattern_vars(p, local_vars);
                }
            }
            _ => {}
        }
    }

    /// Infer the VarType of an expression (lightweight, for int/float dispatch).
    fn infer_expr_type(&self, fc: &FuncCompiler, expr: &Expr) -> VarType {
        match expr.unlocated() {
            Expr::Literal(Lit::Int(_)) => VarType::Int,
            Expr::Literal(Lit::Float(_)) => VarType::Float,
            Expr::Literal(Lit::Bool(_)) => VarType::Bool,
            Expr::Literal(Lit::String(_)) => VarType::String,
            Expr::Cast(_, ty) => match ty.unlocated() {
                Type::Name(n, _) if n == "f64" => VarType::Float,
                Type::Name(n, _) if n == "i32" || n == "i64" => VarType::Int,
                Type::Name(n, _) => VarType::User(n.clone()),
                _ => VarType::Unknown,
            },
            Expr::Ident(name) => fc.var_types.get(name).cloned().unwrap_or(VarType::Unknown),
            Expr::Binary(op, l, r) => {
                // Comparison operators produce Bool.
                if matches!(
                    op,
                    BinOp::EqCmp | BinOp::NeCmp | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge
                ) {
                    VarType::Bool
                } else {
                    let lt = self.infer_expr_type(fc, l);
                    let rt = self.infer_expr_type(fc, r);
                    if lt == VarType::Float || rt == VarType::Float {
                        VarType::Float
                    } else if lt == VarType::Int && rt == VarType::Int {
                        VarType::Int
                    } else {
                        VarType::Unknown
                    }
                }
            }
            Expr::Unary(_, e) => self.infer_expr_type(fc, e),
            _ => VarType::Unknown,
        }
    }
}

impl Default for BytecodeCompiler {
    fn default() -> Self {
        Self::new()
    }
}

impl FuncCompiler {
    fn has_mut_params(&mut self, f: &FuncDef) {
        self.proto.has_mut_params = f.params.iter().any(|p| p.mut_);
        self.proto.mut_param_indices = f
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| p.mut_)
            .map(|(i, _)| i as u16)
            .collect();
    }
}

/// Convert a surface AST Type to a lightweight VarType tag (G1 helper).
fn surface_type_to_var_type(ty: &Type) -> VarType {
    match ty.unlocated() {
        Type::Name(n, _) => match n.as_str() {
            "i32" | "i64" | "i8" | "i16" | "u8" | "u16" | "u32" | "u64" => VarType::Int,
            "f32" | "f64" => VarType::Float,
            "bool" => VarType::Bool,
            "string" => VarType::String,
            other => VarType::User(other.to_string()),
        },
        Type::Ref(_, inner) | Type::RefMut(_, inner) => surface_type_to_var_type(inner),
        _ => VarType::Unknown,
    }
}

/// Evaluate a comptime block using the bytecode VM (0.33 Phase F: codegen comptime migration).
///
/// Wraps the block in a synthetic `func __comptime_eval() -> T { <block> }`,
/// compiles the file, and runs the function. Pre-computed comptime values are
/// injected as top-level constants so calls inside the block resolve correctly.
///
/// Used by codegen to replace the tree-walker dependency for inline comptime evaluation.
pub fn eval_comptime_block_bytecode(
    file: &File,
    block: &Block,
    comptime_values: &HashMap<String, crate::interp::Value>,
) -> Result<crate::interp::Value, String> {
    use crate::ast::{AstNodeMeta, AstOrigin};
    use crate::span::Span;

    // Pre-process: resolve QuoteInterpolate nodes by unwrapping them.
    // In a compile-time evaluation context, $(expr) means "evaluate expr".
    let resolved_block = resolve_quote_interpolations(block);

    // Build a synthetic file: original items + a wrapper function.
    let mut synth = file.clone();
    // Unique wrapper name: nested evaluations (ast_eval inside a comptime
    // fold) reuse the caller's file, which may already contain a
    // `__comptime_eval` wrapper. A duplicate name would make
    // `function_index` resolve to the first (stale/empty) proto.
    static COMPTIME_EVAL_COUNTER: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    let wrapper_name = format!(
        "__comptime_eval_{}",
        COMPTIME_EVAL_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let wrapper = FuncDef {
        meta: AstNodeMeta::inherited(
            Span::UNKNOWN,
            AstOrigin::Desugared("bytecode.comptime_eval"),
        ),
        name: wrapper_name.clone(),
        pub_: false,
        params: vec![],
        ret: None,
        body: resolved_block,
        where_clause: vec![],
        generics: vec![],
        effects: vec![],
        is_comptime: false,
        is_async: false,
        extern_abi: None,
        has_requires: false,
        has_ensures: false,
        has_mutate_params: false,
    };
    synth.items.push(Item::Func(wrapper.clone()));

    // Inject pre-computed comptime values as constants.
    for (name, value) in comptime_values {
        let const_expr = value_to_const_expr(value);
        synth.items.push(Item::Const {
            meta: AstNodeMeta::inherited(
                Span::UNKNOWN,
                AstOrigin::Desugared("bytecode.comptime_const"),
            ),
            name: name.clone(),
            ty: None,
            value: const_expr,
            pub_: false,
        });
    }

    let mut compiler = BytecodeCompiler::new();
    let prog = match compiler.compile_file(&synth) {
        Ok(p) => p,
        Err(_first_err) => {
            // Retry with filtered file: only comptime funcs + types + consts.
            // Non-comptime functions may have unsupported constructs (ast_eval, quote!).
            let mut filtered = File {
                sources: file.sources.clone(),
                imports: Vec::new(),
                items: Vec::new(),
                implicit_single: file.implicit_single,
            };
            for item in &file.items {
                match item {
                    Item::Func(f) if f.is_comptime => filtered.items.push(item.clone()),
                    Item::Type(_)
                    | Item::Const { .. }
                    | Item::Trait(_)
                    | Item::Impl(_)
                    | Item::Cap(_) => filtered.items.push(item.clone()),
                    _ => {}
                }
            }
            filtered.items.push(Item::Func(wrapper));
            for (name, value) in comptime_values {
                let const_expr = value_to_const_expr(value);
                filtered.items.push(Item::Const {
                    meta: AstNodeMeta::inherited(
                        Span::UNKNOWN,
                        AstOrigin::Desugared("bytecode.comptime_const"),
                    ),
                    name: name.clone(),
                    ty: None,
                    value: const_expr,
                    pub_: false,
                });
            }
            let mut compiler2 = BytecodeCompiler::new();
            compiler2
                .compile_file(&filtered)
                .map_err(|e| format!("comptime compile (filtered): {}", e))?
        }
    };
    let fidx = prog
        .function_index(&wrapper_name)
        .ok_or_else(|| format!("comptime wrapper function '{wrapper_name}' not found"))?;
    let mut vm = super::vm::BytecodeVM::new(&prog);
    vm.verify_contracts = false;
    vm.call_function(fidx, &[])
        .map_err(|e| format!("comptime eval: {}", e.message()))
}

/// Evaluate a single expression using the bytecode VM (for $() interpolation).
pub fn eval_expr_bytecode(
    file: &File,
    expr: &Expr,
    comptime_values: &HashMap<String, crate::interp::Value>,
) -> Result<crate::interp::Value, String> {
    let block: Block = vec![Stmt::Expr(expr.clone()).into()];
    eval_comptime_block_bytecode(file, &block, comptime_values)
}

/// Convert a runtime Value back to a const Expr for injection as a top-level constant.
///
/// Supports scalar literals plus the composite shapes the bytecode VM can
/// rebuild: variants (constructor call), tuples, lists, sets, records,
/// and newtypes. Anything else falls back to a string literal encoding
/// (best-effort for comptime seeding).
fn value_to_const_expr(value: &crate::interp::Value) -> Expr {
    use crate::interp::Value;
    let origin = AstOrigin::Desugared("bytecode.comptime_const");
    let synth = |e: Expr| e.synthetic_with_origin(origin);
    match value {
        Value::Int(n) => Expr::Literal(Lit::Int(*n)),
        Value::Float(f) => Expr::Literal(Lit::Float(*f)),
        Value::Bool(b) => Expr::Literal(Lit::Bool(*b)),
        Value::String(s) => Expr::Literal(Lit::String(s.clone())),
        Value::Unit => Expr::Literal(Lit::Unit),
        // Enum/newtype variant: reconstruct via constructor call, e.g.
        // `Some(42)` → Some(42). Zero-arity variants fall back to a bare
        // identifier (the compiler resolves nullary constructors as idents).
        Value::Variant(tag, args) => {
            if args.is_empty() {
                synth(Expr::Ident(tag.clone()))
            } else {
                synth(Expr::Call(
                    Box::new(synth(Expr::Ident(tag.clone()))),
                    args.iter().map(value_to_const_expr).collect(),
                ))
            }
        }
        Value::Tuple(elems) => synth(Expr::Tuple(elems.iter().map(value_to_const_expr).collect())),
        Value::List(elems) => synth(Expr::List(elems.iter().map(value_to_const_expr).collect())),
        Value::Set(elems) => synth(Expr::SetLiteral(
            elems.iter().map(value_to_const_expr).collect(),
        )),
        Value::Record(ty, fields) => {
            let ty = ty.clone();
            let fields = fields
                .iter()
                .map(|(name, v)| RecordFieldExpr {
                    meta: AstNodeMeta::inherited(crate::span::Span::UNKNOWN, origin),
                    name: name.clone(),
                    value: value_to_const_expr(v),
                })
                .collect();
            Expr::Record { ty, fields }
        }
        Value::Newtype(name, inner) => synth(Expr::Call(
            Box::new(synth(Expr::Ident(name.clone()))),
            vec![value_to_const_expr(inner)],
        )),
        // Complex values: encode as string literal (best-effort for comptime seeding).
        other => Expr::Literal(Lit::String(format!("{}", other))),
    }
}

/// Resolve QuoteInterpolate nodes in a block by unwrapping them.
/// In a compile-time evaluation context, `$(expr)` means "evaluate expr directly".
/// This transforms `[$(seven() + 1)]` into `[seven() + 1]` so the bytecode
/// compiler can evaluate it as a regular expression.
fn resolve_quote_interpolations(block: &Block) -> Block {
    block.iter().map(resolve_stmt_interpolations).collect()
}

fn resolve_stmt_interpolations(stmt: &Stmt) -> Stmt {
    match stmt.unlocated() {
        Stmt::Expr(expr) => Stmt::Expr(resolve_expr_interpolations(expr)),
        Stmt::Let {
            pat,
            ty,
            init,
            mut_,
            ref_,
        } => Stmt::Let {
            pat: pat.clone(),
            ty: ty.clone(),
            init: init.as_ref().map(resolve_expr_interpolations),
            mut_: *mut_,
            ref_: *ref_,
        },
        Stmt::If { cond, then_, else_ } => Stmt::If {
            cond: resolve_expr_interpolations(cond),
            then_: resolve_quote_interpolations(then_),
            else_: else_.as_ref().map(resolve_quote_interpolations),
        },
        Stmt::While { cond, body } => Stmt::While {
            cond: resolve_expr_interpolations(cond),
            body: resolve_quote_interpolations(body),
        },
        Stmt::For {
            var,
            iterable,
            body,
        } => Stmt::For {
            var: var.clone(),
            iterable: resolve_expr_interpolations(iterable),
            body: resolve_quote_interpolations(body),
        },
        Stmt::Block(block) => Stmt::Block(resolve_quote_interpolations(block)),
        Stmt::Assign { target, value } => Stmt::Assign {
            target: resolve_expr_interpolations(target),
            value: resolve_expr_interpolations(value),
        },
        Stmt::Return(expr) => Stmt::Return(expr.as_ref().map(resolve_expr_interpolations)),
        // Other statements pass through unchanged.
        _ => stmt.clone(),
    }
}

fn resolve_expr_interpolations(expr: &Expr) -> Expr {
    match expr.unlocated() {
        // Unwrap $(expr) → expr
        Expr::QuoteInterpolate(inner) => resolve_expr_interpolations(inner),
        // Recurse into binary ops
        Expr::Binary(op, l, r) => Expr::Binary(
            *op,
            Box::new(resolve_expr_interpolations(l)),
            Box::new(resolve_expr_interpolations(r)),
        ),
        // Recurse into unary ops
        Expr::Unary(op, inner) => Expr::Unary(*op, Box::new(resolve_expr_interpolations(inner))),
        // Recurse into function calls (callee is an Expr)
        Expr::Call(callee, args) => Expr::Call(
            Box::new(resolve_expr_interpolations(callee)),
            args.iter().map(resolve_expr_interpolations).collect(),
        ),
        // Recurse into field access
        Expr::Field(obj, field) => {
            Expr::Field(Box::new(resolve_expr_interpolations(obj)), field.clone())
        }
        // Recurse into index
        Expr::Index(obj, idx) => Expr::Index(
            Box::new(resolve_expr_interpolations(obj)),
            Box::new(resolve_expr_interpolations(idx)),
        ),
        // Recurse into cast
        Expr::Cast(inner, ty) => {
            Expr::Cast(Box::new(resolve_expr_interpolations(inner)), ty.clone())
        }
        // Recurse into if-expr
        Expr::If { cond, then_, else_ } => Expr::If {
            cond: Box::new(resolve_expr_interpolations(cond)),
            then_: resolve_quote_interpolations(then_),
            else_: else_.as_ref().map(resolve_quote_interpolations),
        },
        // Recurse into block expr
        Expr::Block(block) => Expr::Block(resolve_quote_interpolations(block)),
        // Recurse into tuple
        Expr::Tuple(elems) => Expr::Tuple(elems.iter().map(resolve_expr_interpolations).collect()),
        // Recurse into list literal
        Expr::List(elems) => Expr::List(elems.iter().map(resolve_expr_interpolations).collect()),
        // Leaf nodes and everything else: pass through.
        _ => expr.clone(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 0.33 Phase F: QuotedAst → Block converter (tree-walker removal)
//
// Converts a QuotedAst (produced by quote! at runtime) back into a standard
// AST Block so it can be evaluated via eval_comptime_block_bytecode.
// Interpolated values (QuotedAst::Interpolate) are collected into a side map
// and injected as comptime constants by the caller.
// ═══════════════════════════════════════════════════════════════════════════

/// Convert a QuotedAst node into a Block (Vec<Stmt>).
///
/// `interp_counter` is a mutable counter for generating unique names for
/// interpolated values. `interp_values` accumulates the interpolated runtime
/// values keyed by their generated identifier names.
pub fn quoted_ast_to_block(
    qa: &crate::interp::value::QuotedAst,
    interp_counter: &mut usize,
    interp_values: &mut HashMap<String, crate::interp::Value>,
) -> Block {
    use crate::interp::value::QuotedAst;
    match qa {
        QuotedAst::Block(stmts) => stmts
            .iter()
            .map(|s| quoted_ast_to_stmt(s, interp_counter, interp_values))
            .collect(),
        // A single expression node becomes a one-statement block.
        other => vec![Stmt::Expr(quoted_ast_to_expr(
            other,
            interp_counter,
            interp_values,
        ))],
    }
}

fn quoted_ast_to_stmt(
    qa: &crate::interp::value::QuotedAst,
    interp_counter: &mut usize,
    interp_values: &mut HashMap<String, crate::interp::Value>,
) -> Stmt {
    use crate::interp::value::QuotedAst;
    match qa {
        QuotedAst::Let { name, value } => Stmt::Let {
            pat: Pattern::synthetic(
                PatternKind::Variable(name.clone()),
                AstOrigin::Desugared("quoted_ast"),
            ),
            ty: None,
            init: Some(quoted_ast_to_expr(value, interp_counter, interp_values)),
            mut_: false,
            ref_: false,
        },
        QuotedAst::ExprStmt(e) => Stmt::Expr(quoted_ast_to_expr(e, interp_counter, interp_values)),
        QuotedAst::Return(e) => Stmt::Return(
            e.as_ref()
                .map(|inner| quoted_ast_to_expr(inner, interp_counter, interp_values)),
        ),
        QuotedAst::Break(e) => Stmt::Break(
            e.as_ref()
                .map(|inner| quoted_ast_to_expr(inner, interp_counter, interp_values)),
        ),
        QuotedAst::Continue => Stmt::Continue,
        QuotedAst::While(cond, body) => Stmt::While {
            cond: quoted_ast_to_expr(cond, interp_counter, interp_values),
            body: quoted_ast_to_block(body, interp_counter, interp_values),
        },
        QuotedAst::WhileLet { pat, init, body } => Stmt::WhileLet {
            pat: pat.clone(),
            init: quoted_ast_to_expr(init, interp_counter, interp_values),
            body: quoted_ast_to_block(body, interp_counter, interp_values),
        },
        QuotedAst::Loop(body) => {
            Stmt::Loop(quoted_ast_to_block(body, interp_counter, interp_values))
        }
        QuotedAst::For(var, iter, body) => Stmt::For {
            var: Pattern::synthetic(
                PatternKind::Variable(var.clone()),
                crate::ast::AstOrigin::User,
            ),
            iterable: quoted_ast_to_expr(iter, interp_counter, interp_values),
            body: quoted_ast_to_block(body, interp_counter, interp_values),
        },
        QuotedAst::Assign(target, value) => Stmt::Assign {
            target: quoted_ast_to_expr(target, interp_counter, interp_values),
            value: quoted_ast_to_expr(value, interp_counter, interp_values),
        },
        QuotedAst::Arena(body) => {
            Stmt::Arena(quoted_ast_to_block(body, interp_counter, interp_values))
        }
        QuotedAst::Unsafe(body) => {
            Stmt::Unsafe(quoted_ast_to_block(body, interp_counter, interp_values))
        }
        QuotedAst::IeeeFloat(body) => {
            Stmt::IeeeFloat(quoted_ast_to_block(body, interp_counter, interp_values))
        }
        QuotedAst::Drop(e) => Stmt::Drop(quoted_ast_to_expr(e, interp_counter, interp_values)),
        QuotedAst::Defer(body) => {
            Stmt::Defer(quoted_ast_to_block(body, interp_counter, interp_values))
        }
        QuotedAst::SharedLet { kind, name, init } => Stmt::SharedLet {
            kind: *kind,
            name: name.clone(),
            ty: None,
            init: quoted_ast_to_expr(init, interp_counter, interp_values),
        },
        QuotedAst::OnFailure(body) => {
            Stmt::OnFailure(quoted_ast_to_block(body, interp_counter, interp_values))
        }
        QuotedAst::Parasteps(body) => {
            Stmt::Parasteps(quoted_ast_to_block(body, interp_counter, interp_values))
        }
        QuotedAst::Alloc { kind, body } => Stmt::Alloc {
            kind: *kind,
            body: quoted_ast_to_block(body, interp_counter, interp_values),
        },
        QuotedAst::Block(stmts) => Stmt::Block(
            stmts
                .iter()
                .map(|s| quoted_ast_to_stmt(s, interp_counter, interp_values))
                .collect(),
        ),
        // Everything else is an expression statement.
        other => Stmt::Expr(quoted_ast_to_expr(other, interp_counter, interp_values)),
    }
}

fn quoted_ast_to_expr(
    qa: &crate::interp::value::QuotedAst,
    interp_counter: &mut usize,
    interp_values: &mut HashMap<String, crate::interp::Value>,
) -> Expr {
    use crate::interp::value::QuotedAst;
    match qa {
        QuotedAst::Literal(l) => Expr::Literal(l.clone()),
        QuotedAst::Ident(name) => Expr::Ident(name.clone()),
        QuotedAst::Binary(op, l, r) => Expr::Binary(
            *op,
            Box::new(quoted_ast_to_expr(l, interp_counter, interp_values)),
            Box::new(quoted_ast_to_expr(r, interp_counter, interp_values)),
        ),
        QuotedAst::Unary(op, e) => Expr::Unary(
            *op,
            Box::new(quoted_ast_to_expr(e, interp_counter, interp_values)),
        ),
        QuotedAst::Call(callee, args) => Expr::Call(
            Box::new(quoted_ast_to_expr(callee, interp_counter, interp_values)),
            args.iter()
                .map(|a| quoted_ast_to_expr(a, interp_counter, interp_values))
                .collect(),
        ),
        QuotedAst::Field(obj, name) => Expr::Field(
            Box::new(quoted_ast_to_expr(obj, interp_counter, interp_values)),
            name.clone(),
        ),
        QuotedAst::Index(obj, idx) => Expr::Index(
            Box::new(quoted_ast_to_expr(obj, interp_counter, interp_values)),
            Box::new(quoted_ast_to_expr(idx, interp_counter, interp_values)),
        ),
        QuotedAst::Tuple(elems) => Expr::Tuple(
            elems
                .iter()
                .map(|e| quoted_ast_to_expr(e, interp_counter, interp_values))
                .collect(),
        ),
        QuotedAst::List(elems) => Expr::List(
            elems
                .iter()
                .map(|e| quoted_ast_to_expr(e, interp_counter, interp_values))
                .collect(),
        ),
        QuotedAst::Match(subject, arms) => {
            let converted_arms: Vec<MatchArm> = arms
                .iter()
                .map(|arm| MatchArm {
                    meta: AstNodeMeta::synthetic(AstOrigin::Desugared("quoted_ast")),
                    pat: arm.pat.clone(),
                    guard: arm
                        .guard
                        .as_ref()
                        .map(|g| quoted_ast_to_expr(g, interp_counter, interp_values)),
                    body: quoted_ast_to_expr(&arm.body, interp_counter, interp_values),
                })
                .collect();
            Expr::Match(
                Box::new(quoted_ast_to_expr(subject, interp_counter, interp_values)),
                converted_arms,
            )
        }
        QuotedAst::If(cond, then_, else_) => Expr::If {
            cond: Box::new(quoted_ast_to_expr(cond, interp_counter, interp_values)),
            then_: quoted_ast_to_block(then_, interp_counter, interp_values),
            else_: else_
                .as_ref()
                .map(|e| quoted_ast_to_block(e, interp_counter, interp_values)),
        },
        QuotedAst::Record { ty, fields } => {
            let converted_fields: Vec<RecordFieldExpr> = fields
                .iter()
                .map(|f| RecordFieldExpr {
                    meta: AstNodeMeta::synthetic(AstOrigin::Desugared("quoted_ast")),
                    name: f.name.clone(),
                    value: quoted_ast_to_expr(&f.value, interp_counter, interp_values),
                })
                .collect();
            Expr::Record {
                ty: ty.clone(),
                fields: converted_fields,
            }
        }
        QuotedAst::Try(e) => Expr::Try(Box::new(quoted_ast_to_expr(
            e,
            interp_counter,
            interp_values,
        ))),
        QuotedAst::OptionalChain(obj, field) => Expr::OptionalChain(
            Box::new(quoted_ast_to_expr(obj, interp_counter, interp_values)),
            field.clone(),
        ),
        QuotedAst::Spawn(e) => Expr::Spawn(Box::new(quoted_ast_to_expr(
            e,
            interp_counter,
            interp_values,
        ))),
        QuotedAst::Await(e) => Expr::Await(Box::new(quoted_ast_to_expr(
            e,
            interp_counter,
            interp_values,
        ))),
        QuotedAst::Interpolate(value) => {
            // Inject the runtime value as a comptime constant with a unique name.
            let name = format!("__interp_{}", *interp_counter);
            *interp_counter += 1;
            interp_values.insert(name.clone(), *value.clone());
            Expr::Ident(name)
        }
        QuotedAst::Block(stmts) => Expr::Block(
            stmts
                .iter()
                .map(|s| quoted_ast_to_stmt(s, interp_counter, interp_values))
                .collect(),
        ),
        QuotedAst::Lambda {
            params,
            ret,
            body,
            captured: _,
        } => Expr::Lambda {
            params: params.clone(),
            ret: ret.clone(),
            body: body.clone(),
        },
        QuotedAst::Cast(e, ty) => Expr::Cast(
            Box::new(quoted_ast_to_expr(e, interp_counter, interp_values)),
            ty.clone(),
        ),
        QuotedAst::NamedArg(name, e) => Expr::NamedArg(
            name.clone(),
            Box::new(quoted_ast_to_expr(e, interp_counter, interp_values)),
        ),
        QuotedAst::MapLiteral(entries) => Expr::MapLiteral {
            entries: entries
                .iter()
                .map(|(k, v)| {
                    (
                        quoted_ast_to_expr(k, interp_counter, interp_values),
                        quoted_ast_to_expr(v, interp_counter, interp_values),
                    )
                })
                .collect(),
        },
        QuotedAst::SetLiteral(elems) => Expr::SetLiteral(
            elems
                .iter()
                .map(|e| quoted_ast_to_expr(e, interp_counter, interp_values))
                .collect(),
        ),
        // Statement-level nodes that appear in expression position: wrap as block expr.
        QuotedAst::Let { .. }
        | QuotedAst::ExprStmt(_)
        | QuotedAst::Return(_)
        | QuotedAst::Break(_)
        | QuotedAst::Continue
        | QuotedAst::While(_, _)
        | QuotedAst::WhileLet { .. }
        | QuotedAst::Loop(_)
        | QuotedAst::For(_, _, _)
        | QuotedAst::Assign(_, _)
        | QuotedAst::Arena(_)
        | QuotedAst::Unsafe(_)
        | QuotedAst::IeeeFloat(_)
        | QuotedAst::Drop(_)
        | QuotedAst::Defer(_)
        | QuotedAst::SharedLet { .. }
        | QuotedAst::OnFailure(_)
        | QuotedAst::Parasteps(_)
        | QuotedAst::Alloc { .. } => {
            let block = quoted_ast_to_block(qa, interp_counter, interp_values);
            Expr::Block(block)
        }
    }
}

/// Evaluate a QuotedAst using the bytecode VM.
///
/// Converts the QuotedAst to a standard Block, collects interpolated values,
/// and evaluates via eval_comptime_block_bytecode.
pub fn eval_quoted_ast_bytecode(
    file: &File,
    qa: &crate::interp::value::QuotedAst,
    captures: &HashMap<String, crate::interp::Value>,
) -> Result<crate::interp::Value, String> {
    use crate::interp::value::QuotedAst;
    // A quoted lambda becomes a first-class Closure value directly.
    // Compiling it into a BytecodeClosure would embed a proto index that is
    // valid only in the synthetic eval program, not in the caller's program
    // (tree-walker `eval_quoted_ast_body` parity). The VM's CallIndirect
    // fallback evaluates such Closures via eval_comptime_block_bytecode.
    // `quote! { fn(x) ... }` yields Block([ExprStmt(Lambda)]), so strip the
    // statement/block wrappers before matching the Lambda node.
    let mut stripped = qa;
    loop {
        match stripped {
            QuotedAst::Block(stmts) if stmts.len() == 1 => match &stmts[0] {
                QuotedAst::ExprStmt(inner) => {
                    stripped = inner;
                    continue;
                }
                _ => break,
            },
            _ => break,
        }
    }
    if let QuotedAst::Lambda {
        params,
        ret,
        body,
        captured,
    } = stripped
    {
        return Ok(crate::interp::Value::Closure {
            params: params.clone(),
            ret: ret.clone(),
            body: body.clone(),
            captured: captured.clone(),
        });
    }
    let mut interp_counter: usize = 0;
    let mut interp_values: HashMap<String, crate::interp::Value> = HashMap::new();
    let block = quoted_ast_to_block(qa, &mut interp_counter, &mut interp_values);
    // Merge captures (free identifiers from quote!) with interpolated values.
    let mut comptime_values = captures.clone();
    comptime_values.extend(interp_values);
    eval_comptime_block_bytecode(file, &block, &comptime_values)
}
