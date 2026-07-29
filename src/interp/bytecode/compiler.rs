//! AST → Bytecode compiler.
//!
//! Compiles Mimi AST functions into `FunctionProto` bytecode.
//! Register allocation: variables are assigned registers at first use.
//! Scope management: nested scopes share the register file (no reuse yet).

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
}

/// Per-function compilation state.
struct FuncCompiler {
    /// The prototype being built.
    proto: FunctionProto,
    /// Variable name → register mapping (current scope chain).
    vars: Vec<HashMap<String, Reg>>,
    /// Variable name → known type tag (for int/float dispatch without CheckedProgram).
    var_types: HashMap<String, VarType>,
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
}

/// Lightweight type tag for register dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarType {
    Int,
    Float,
    Bool,
    String,
    Unknown,
}

impl FuncCompiler {
    fn new(name: String, param_count: u16) -> Self {
        FuncCompiler {
            proto: FunctionProto::new(name, param_count),
            vars: vec![HashMap::new()],
            var_types: HashMap::new(),
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            current_line: 0,
            free_regs: Vec::new(),
            scope_regs: vec![Vec::new()],
        }
    }

    fn push_scope(&mut self) {
        self.vars.push(HashMap::new());
        self.scope_regs.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        self.vars.pop();
        // Reclaim registers allocated in this scope.
        if let Some(regs) = self.scope_regs.pop() {
            self.free_regs.extend(regs);
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
        self.vars.last_mut().unwrap().insert(name.to_string(), r);
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
}

impl BytecodeCompiler {
    pub fn new() -> Self {
        BytecodeCompiler {
            func_table: HashMap::new(),
            builtin_table: HashMap::new(),
            functions: Vec::new(),
            builtin_names: Vec::new(),
        }
    }

    /// Compile a full AST file into a BytecodeProgram.
    pub fn compile_file(&mut self, file: &File) -> Result<BytecodeProgram, InterpError> {
        // Pass 1: register all function names.
        for item in &file.items {
            if let Item::Func(f) = item {
                let idx = self.functions.len() as FuncIdx;
                self.func_table.insert(f.name.clone(), idx);
                // Push placeholder.
                self.functions.push(FunctionProto::new(f.name.clone(), f.params.len() as u16));
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
        }

        // Pass 3: compile impl methods as mangled functions.
        // Method `foo` on type `Bar` becomes `Bar_foo`.
        for item in &file.items {
            if let Item::Impl(impl_def) = item {
                for method in &impl_def.methods {
                    let mangled_name = format!("{}_{}", impl_def.type_name, method.name);
                    // Register the mangled function name.
                    let idx = self.functions.len() as FuncIdx;
                    self.func_table.insert(mangled_name.clone(), idx);
                    self.functions.push(FunctionProto::new(mangled_name, method.params.len() as u16));
                }
            }
        }

        // Pass 4: compile impl method bodies.
        for item in &file.items {
            if let Item::Impl(impl_def) = item {
                for method in &impl_def.methods {
                    let mangled_name = format!("{}_{}", impl_def.type_name, method.name);
                    if let Some(&idx) = self.func_table.get(&mangled_name) {
                        let proto = self.compile_func(method)?;
                        self.functions[idx as usize] = proto;
                    }
                }
            }
        }

        let entry = self.func_table.get("main").copied().ok_or_else(|| {
            InterpError::new("no main function found")
        })?;

        Ok(BytecodeProgram {
            functions: std::mem::take(&mut self.functions),
            entry,
            builtin_names: std::mem::take(&mut self.builtin_names),
        })
    }

    fn register_builtin(&mut self, name: &str) {
        let idx = self.builtin_names.len() as BuiltinIdx;
        self.builtin_table.insert(name.to_string(), idx);
        self.builtin_names.push(name.to_string());
    }

    /// Compile a single function definition.
    fn compile_func(&mut self, f: &FuncDef) -> Result<FunctionProto, InterpError> {
        let mut fc = FuncCompiler::new(f.name.clone(), f.params.len() as u16);

        // Bind parameters to registers 0..param_count.
        for (i, param) in f.params.iter().enumerate() {
            fc.vars[0].insert(param.name.clone(), i as Reg);
            // Track parameter types for int/float dispatch.
            let ty = match param.ty.unlocated() {
                Type::Name(n, _) if n == "f64" || n == "f32" => VarType::Float,
                Type::Name(n, _) if n == "i32" || n == "i64" => VarType::Int,
                Type::Name(n, _) if n == "bool" => VarType::Bool,
                Type::Name(n, _) if n == "string" => VarType::String,
                _ => VarType::Unknown,
            };
            fc.set_reg_type(&param.name, ty);
        }
        // Ensure register_count accounts for params.
        while fc.proto.register_count < f.params.len() as u16 {
            fc.proto.alloc_reg();
        }

        fc.has_mut_params(f);

        // Compile body statements.
        let last_reg = self.compile_block(&mut fc, &f.body)?;

        // Return the last expression's value, or Unit if none.
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
                Stmt::Let { pat, init, .. } => {
                    if let Some(init_expr) = init {
                        let r = self.compile_expr(fc, init_expr)?;
                        // Track variable type for int/float dispatch.
                        if let PatternKind::Variable(name) = &pat.kind {
                            let ty = self.infer_expr_type(fc, init_expr);
                            fc.set_reg_type(name, ty);
                        }
                        self.bind_pattern(fc, pat, r);
                    } else {
                        // let x; → Unit
                        if let PatternKind::Variable(name) = &pat.kind {
                            let r = fc.bind_var(name);
                            fc.emit(Op::LoadUnit { rd: r });
                        }
                    }
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign(fc, target, value)?;
                }
                Stmt::Return(expr) => {
                    if let Some(e) = expr {
                        let r = self.compile_expr(fc, e)?;
                        fc.emit(Op::Ret { ra: r });
                    } else {
                        fc.emit(Op::RetUnit);
                    }
                }
                Stmt::If {
                    cond,
                    then_,
                    else_,
                } => {
                    if is_last {
                        // If as last expression: produces a value.
                        let r = self.compile_if_expr(fc, cond, then_, else_)?;
                        last_reg = Some(r);
                    } else {
                        self.compile_if_stmt(fc, cond, then_, else_.as_ref())?;
                    }
                }
                Stmt::While { cond, body } => {
                    self.compile_while(fc, cond, body)?;
                }
                Stmt::For { var, iterable, body } => {
                    self.compile_for(fc, var, iterable, body)?;
                }
                Stmt::Block(b) => {
                    fc.push_scope();
                    self.compile_block(fc, b)?;
                    fc.pop_scope();
                }
                Stmt::Break(_) => {
                    // Emit a forward jump; patched when the loop exits.
                    let idx = fc.emit(Op::Jmp { offset: 0 });
                    if let Some(jumps) = fc.break_jumps.last_mut() {
                        jumps.push(idx);
                    }
                }
                Stmt::Continue => {
                    // Emit a forward jump; patched to loop head by the enclosing loop.
                    let idx = fc.emit(Op::Jmp { offset: 0 });
                    if let Some(jumps) = fc.continue_jumps.last_mut() {
                        jumps.push(idx);
                    }
                }
                // Skip non-executable statements.
                Stmt::Desc(..) | Stmt::Rule(..) | Stmt::Requires(..)
                | Stmt::Ensures(..) | Stmt::Invariant(..) | Stmt::Math(..)
                | Stmt::MmsBlock { .. } => {}

                // ── Phase B: Stmt 补全 I ──────────────────────

                Stmt::Loop(body) => {
                    self.compile_loop(fc, body)?;
                }

                Stmt::WhileLet { pat, init, body } => {
                    self.compile_while_let(fc, pat, init, body)?;
                }

                Stmt::Unsafe(block) => {
                    // Interpreter doesn't enforce safety — just compile the block.
                    fc.push_scope();
                    self.compile_block(fc, block)?;
                    fc.pop_scope();
                }

                Stmt::Arena(block) => {
                    // Interpreter doesn't do region-based memory — just compile the block.
                    fc.push_scope();
                    self.compile_block(fc, block)?;
                    fc.pop_scope();
                }

                Stmt::Drop(expr) => {
                    // Drop is a no-op in the interpreter (values are GC'd).
                    // Just compile the expression for side effects.
                    self.compile_expr(fc, expr)?;
                }

                Stmt::Alloc { body, .. } => {
                    // Allocator block — just compile the body.
                    fc.push_scope();
                    self.compile_block(fc, body)?;
                    fc.pop_scope();
                }

                Stmt::Defer(block) => {
                    // TODO: true defer semantics (LIFO execution at scope exit).
                    // For now, compile the block immediately.
                    fc.push_scope();
                    self.compile_block(fc, block)?;
                    fc.pop_scope();
                }

                // ── Phase B: Stmt 补全 II ─────────────────────

                Stmt::SharedLet { name, init, .. } => {
                    // Shared ownership binding — compile as regular let.
                    let r = self.compile_expr(fc, init)?;
                    let ty = self.infer_expr_type(fc, init);
                    fc.set_reg_type(name, ty);
                    let r_var = fc.bind_var(name);
                    if r != r_var {
                        fc.emit(Op::Mov { rd: r_var, rs: r });
                    }
                }

                Stmt::OnFailure(block) => {
                    // On failure compensation — compile the block.
                    // Error handling semantics handled at runtime.
                    fc.push_scope();
                    self.compile_block(fc, block)?;
                    fc.pop_scope();
                }

                Stmt::Do(block) => {
                    // Do block — transition implementation body.
                    fc.push_scope();
                    self.compile_block(fc, block)?;
                    fc.pop_scope();
                }

                Stmt::Become(expr) => {
                    // become TargetState { ... } — Flow transition terminal.
                    // Compile the expression and return it.
                    let r = self.compile_expr(fc, expr)?;
                    fc.emit(Op::Ret { ra: r });
                }

                Stmt::Stay => {
                    // stay — self-loop terminal (return Unit).
                    fc.emit(Op::RetUnit);
                }

                Stmt::Parasteps(block) => {
                    // Parallel steps — compile the block.
                    // Parallel execution semantics handled at runtime.
                    fc.push_scope();
                    self.compile_block(fc, block)?;
                    fc.pop_scope();
                }

                // DEAD: 架构修正案废止 delegate/pinned。
                Stmt::Delegate { .. } | Stmt::Pinned { .. } => {}

                _ => {
                    // Remaining unsupported: Ellipsis, Located (wrapper).
                }
            }
        }
        Ok(last_reg)
    }

    /// Compile an expression, returning the register holding the result.
    fn compile_expr(
        &mut self,
        fc: &mut FuncCompiler,
        expr: &Expr,
    ) -> Result<Reg, InterpError> {
        // Track source line for error context (D12).
        fc.set_line_from_meta(expr.meta());
        match expr.unlocated() {
            Expr::Literal(lit) => self.compile_literal(fc, lit),
            Expr::Ident(name) => {
                fc.lookup_var(name).ok_or_else(|| {
                    InterpError::new(format!("undefined variable '{}' in bytecode", name))
                })
            }
            Expr::Binary(op, l, r) => self.compile_binary(fc, *op, l, r),
            Expr::Unary(op, e) => self.compile_unary(fc, *op, e),
            Expr::Call(callee, args) => self.compile_call(fc, callee, args),
            Expr::If { cond, then_, else_ } => {
                self.compile_if_expr(fc, cond, then_, else_)
            }
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
                fc.emit(Op::TupleGet { rd, ra: r_obj, idx: *idx as u16 });
                Ok(rd)
            }
            Expr::Cast(inner, ty) => {
                let r = self.compile_expr(fc, inner)?;
                // For now, handle `as f64` and `as i32`.
                let rd = fc.proto.alloc_reg();
                match ty.unlocated() {
                    Type::Name(n, _) if n == "f64" => {
                        fc.emit(Op::IntToFloat { rd, ra: r });
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
                fc.emit(Op::RecordGet { rd, ra: r_obj, field: field_idx });
                Ok(rd)
            }
            Expr::Record { ty, fields } => self.compile_record(fc, ty.as_deref(), fields),
            Expr::Lambda { params, ret: _, body } => self.compile_lambda(fc, params, body),
            Expr::Match(subject, arms) => self.compile_match(fc, subject, arms),

            // ── Phase B: Expr 补全 ──────────────────────────

            Expr::Range { start, end } => {
                // start..end → list [start, start+1, ..., end-1]
                let r_start = self.compile_expr(fc, start)?;
                let r_end = self.compile_expr(fc, end)?;
                let rd = fc.proto.alloc_reg();
                // Use range builtin.
                let args_base = fc.proto.alloc_reg();
                fc.proto.alloc_reg(); // second arg slot
                fc.emit(Op::Mov { rd: args_base, rs: r_start });
                fc.emit(Op::Mov { rd: args_base + 1, rs: r_end });
                if let Some(&bidx) = self.builtin_table.get("range") {
                    fc.emit(Op::CallBuiltin { rd, builtin: bidx, args_base, argc: 2 });
                } else {
                    return Err(InterpError::new("bytecode: range builtin not registered"));
                }
                Ok(rd)
            }

            Expr::Comprehension { expr, var, iter, guard } => {
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
                fc.emit(Op::Len { rd: r_len, ra: r_iter });

                let loop_start = fc.proto.code.len();
                let r_cmp = fc.proto.alloc_reg();
                fc.emit(Op::LtInt { rd: r_cmp, ra: r_idx, rb: r_len });
                let jmp_end = fc.emit(Op::JmpIfNot { offset: 0, ra: r_cmp });

                // Bind loop variable.
                fc.push_scope();
                let r_var = fc.bind_var(var);
                fc.emit(Op::ListGet { rd: r_var, ra: r_iter, rb: r_idx });

                // Guard check.
                let guard_skip = if let Some(guard_expr) = guard {
                    let r_guard = self.compile_expr(fc, guard_expr)?;
                    Some(fc.emit(Op::JmpIfNot { offset: 0, ra: r_guard }))
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
                fc.emit(Op::AddInt { rd: r_idx, ra: r_idx, rb: r_one });
                fc.emit(Op::Jmp { offset: 0 });
                let jmp_back = fc.proto.code.len() - 1;
                fc.proto.patch_jump_to(jmp_back, loop_start);
                fc.proto.patch_jump_to(jmp_end, fc.proto.code.len());

                Ok(rd)
            }

            Expr::SliceExpr { target, start, end } => {
                // target[start..end] → sublist
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
                    fc.emit(Op::Len { rd: r, ra: r_target });
                    r
                };

                // Build sublist via loop.
                let rd = fc.proto.alloc_reg();
                fc.emit(Op::NewList { rd, capacity: 0 });
                let r_idx = fc.proto.alloc_reg();
                let r_one = fc.proto.alloc_reg();
                let c0 = fc.proto.add_const(ConstValue::Int(0));
                let c1 = fc.proto.add_const(ConstValue::Int(1));
                fc.emit(Op::Mov { rd: r_idx, rs: r_start });
                fc.emit(Op::LoadConst { rd: r_one, idx: c1 });

                let loop_start = fc.proto.code.len();
                let r_cmp = fc.proto.alloc_reg();
                fc.emit(Op::LtInt { rd: r_cmp, ra: r_idx, rb: r_end });
                let jmp_end = fc.emit(Op::JmpIfNot { offset: 0, ra: r_cmp });

                let r_elem = fc.proto.alloc_reg();
                fc.emit(Op::ListGet { rd: r_elem, ra: r_target, rb: r_idx });
                fc.emit(Op::ListPush { ra: rd, rb: r_elem });

                fc.emit(Op::AddInt { rd: r_idx, ra: r_idx, rb: r_one });
                fc.emit(Op::Jmp { offset: 0 });
                let jmp_back = fc.proto.code.len() - 1;
                fc.proto.patch_jump_to(jmp_back, loop_start);
                fc.proto.patch_jump_to(jmp_end, fc.proto.code.len());

                Ok(rd)
            }

            Expr::OptionalChain(obj, field) => {
                // obj?.field → if obj is None → None, else → obj.field
                let r_obj = self.compile_expr(fc, obj)?;
                let rd = fc.proto.alloc_reg();

                // Check if obj is None variant.
                let r_is_none = fc.proto.alloc_reg();
                let none_tag = fc.proto.add_const(ConstValue::Str("None".into()));
                fc.emit(Op::IsVariant { rd: r_is_none, ra: r_obj, tag: none_tag });
                let jmp_else = fc.emit(Op::JmpIfNot { offset: 0, ra: r_is_none });

                // None branch: rd = None.
                fc.emit(Op::None { rd });
                let jmp_end = fc.emit(Op::Jmp { offset: 0 });

                // Some branch: rd = obj.field.
                fc.proto.patch_jump(jmp_else);
                let field_idx = fc.proto.add_const(ConstValue::Str(field.clone()));
                fc.emit(Op::RecordGet { rd, ra: r_obj, field: field_idx });
                fc.proto.patch_jump(jmp_end);

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
                    fc.emit(Op::MapSet { ra: rd, rb: r_key, rc: r_val });
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

            Expr::Turbofish(name, _type_args, args) => {
                // Type arguments ignored at runtime — compile as regular call.
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
                fc.emit(Op::IsVariant { rd: r_is_ok, ra: r_inner, tag: ok_tag });
                let jmp_err = fc.emit(Op::JmpIfNot { offset: 0, ra: r_is_ok });

                // Ok branch: unwrap.
                fc.emit(Op::Unwrap { rd, ra: r_inner });
                let jmp_end = fc.emit(Op::Jmp { offset: 0 });

                // Err branch: return the error value.
                fc.proto.patch_jump(jmp_err);
                fc.emit(Op::Ret { ra: r_inner });

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

            _ => Err(InterpError::new(format!(
                "bytecode compiler: expression {:?} not yet supported",
                std::mem::discriminant(expr.unlocated())
            ))),
        }
    }

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

    fn compile_literal(
        &mut self,
        fc: &mut FuncCompiler,
        lit: &Lit,
    ) -> Result<Reg, InterpError> {
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
            Lit::Bool(true) => { fc.emit(Op::LoadTrue { rd }); }
            Lit::Bool(false) => { fc.emit(Op::LoadFalse { rd }); }
            Lit::String(s) => {
                let idx = fc.proto.add_const(ConstValue::Str(s.clone()));
                fc.emit(Op::LoadConst { rd, idx });
            }
            Lit::Unit => { fc.emit(Op::LoadUnit { rd }); }
            Lit::FString(_) => {
                return Err(InterpError::new(
                    "bytecode: f-strings not yet supported",
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
        // Short-circuit for && and ||.
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.compile_short_circuit(fc, op, l, r);
        }

        // Constant folding: if both operands are literals, compute at compile time.
        if let (Expr::Literal(l_lit), Expr::Literal(r_lit)) = (l.unlocated(), r.unlocated()) {
            if let Some(folded) = self.fold_constants(op, l_lit, r_lit) {
                return self.compile_literal(fc, &folded);
            }
        }

        let ra = self.compile_expr(fc, l)?;
        let rb = self.compile_expr(fc, r)?;
        let rd = fc.proto.alloc_reg();

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
        let ra = self.compile_expr(fc, e)?;
        let rd = fc.proto.alloc_reg();
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
            _ => {
                return Err(InterpError::new(format!(
                    "bytecode: unsupported unary op {:?}",
                    op
                )))
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
        // Compile arguments into consecutive registers.
        let args_base = fc.proto.alloc_reg();
        // Reserve registers for all args.
        for _ in 1..args.len() {
            fc.proto.alloc_reg();
        }

        for (i, arg) in args.iter().enumerate() {
            let r = self.compile_expr(fc, arg)?;
            let target = args_base + i as Reg;
            if r != target {
                fc.emit(Op::Mov { rd: target, rs: r });
            }
        }

        let rd = fc.proto.alloc_reg();

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
                        // push returns Unit (matches tree-walker semantics).
                        let rd = fc.proto.alloc_reg();
                        let unit_idx = fc.proto.add_const(ConstValue::Unit);
                        fc.emit(Op::LoadConst { rd, idx: unit_idx });
                        return Ok(rd);
                    }
                }
            }

            // Check builtins first.
            if let Some(&bidx) = self.builtin_table.get(name.as_str()) {
                fc.emit(Op::CallBuiltin {
                    rd,
                    builtin: bidx,
                    args_base,
                    argc: args.len() as u16,
                });
                return Ok(rd);
            }
            // User function.
            if let Some(&fidx) = self.func_table.get(name.as_str()) {
                fc.emit(Op::Call {
                    rd,
                    func: fidx,
                    args_base,
                    argc: args.len() as u16,
                });
                return Ok(rd);
            }
            // Might be a closure variable.
            if let Some(callee_reg) = fc.lookup_var(name) {
                fc.emit(Op::CallIndirect {
                    rd,
                    callee: callee_reg,
                    args_base,
                    argc: args.len() as u16,
                });
                return Ok(rd);
            }
        }

        // Method call: obj.method(args) → method(obj, args)
        if let Expr::Field(obj, method) = callee.unlocated() {
            // Compile the receiver as the first argument.
            let recv_reg = self.compile_expr(fc, obj)?;

            // Shift existing args to make room for receiver.
            let new_args_base = fc.proto.alloc_reg();
            for _ in 0..args.len() {
                fc.proto.alloc_reg();
            }
            // Move receiver to new_args_base.
            fc.emit(Op::Mov { rd: new_args_base, rs: recv_reg });
            // Move existing args to new_args_base + 1..
            for i in 0..args.len() {
                let src = args_base + i as Reg;
                let dst = new_args_base + 1 + i as Reg;
                fc.emit(Op::Mov { rd: dst, rs: src });
            }

            let total_args = args.len() + 1;

            // Try to find the method as a function.
            // First try the bare method name (for builtins like len).
            if let Some(&fidx) = self.func_table.get(method.as_str()) {
                fc.emit(Op::Call {
                    rd,
                    func: fidx,
                    args_base: new_args_base,
                    argc: total_args as u16,
                });
                return Ok(rd);
            }

            // Try mangled names for common types.
            // Method `foo` on type `Bar` becomes `Bar_foo`.
            let type_prefixes = ["List", "list", "String", "string", "Map", "map", "Set", "set"];
            for prefix in &type_prefixes {
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

            // Try builtin methods.
            if let Some(&bidx) = self.builtin_table.get(method.as_str()) {
                fc.emit(Op::CallBuiltin {
                    rd,
                    builtin: bidx,
                    args_base: new_args_base,
                    argc: total_args as u16,
                });
                return Ok(rd);
            }

            return Err(InterpError::new(format!(
                "bytecode: cannot resolve method '{}'",
                method
            )));
        }

        Err(InterpError::new(format!(
            "bytecode: cannot resolve call target {:?}",
            callee
        )))
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

        let jmp_else = fc.emit(Op::JmpIfNot { offset: 0, ra: r_cond });
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
            let skip_jump = if let Some(r_test) = test_reg {
                Some(fc.emit(Op::JmpIfNot { offset: 0, ra: r_test }))
            } else {
                None
            };

            // Check guard if present.
            let guard_jump = if let Some(guard) = &arm.guard {
                fc.push_scope();
                for (name, r) in &bindings {
                    fc.vars.last_mut().unwrap().insert(name.clone(), *r);
                }
                let r_guard = self.compile_expr(fc, guard)?;
                fc.pop_scope();
                Some(fc.emit(Op::JmpIfNot { offset: 0, ra: r_guard }))
            } else {
                None
            };

            // Bind pattern variables and compile body.
            fc.push_scope();
            for (name, r) in &bindings {
                fc.vars.last_mut().unwrap().insert(name.clone(), *r);
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
                // Always matches; bind the subject to the variable.
                Ok((None, vec![(name.clone(), r_subject)]))
            }

            PatternKind::Literal(lit) => {
                // Compare subject with the literal.
                let r_lit = self.compile_literal(fc, lit)?;
                let r_test = fc.proto.alloc_reg();
                fc.emit(Op::Eq { rd: r_test, ra: r_subject, rb: r_lit });
                Ok((Some(r_test), Vec::new()))
            }

            PatternKind::Constructor(name, pats) => {
                // Check variant tag, then match fields.
                let r_test = fc.proto.alloc_reg();
                let tag_idx = fc.proto.add_const(ConstValue::Str(name.clone()));
                fc.emit(Op::IsVariant { rd: r_test, ra: r_subject, tag: tag_idx });

                let mut bindings = Vec::new();
                for (i, (field_name, sub_pat)) in pats.iter().enumerate() {
                    // Extract field i from the variant.
                    let r_field = fc.proto.alloc_reg();
                    fc.emit(Op::VariantGet {
                        rd: r_field,
                        ra: r_subject,
                        idx: i as u16,
                    });
                    // Recursively match the sub-pattern.
                    let (sub_test, sub_bindings) =
                        self.compile_pattern_test(fc, sub_pat, r_field)?;
                    // If sub-pattern has a test, AND it with the main test.
                    if let Some(r_sub) = sub_test {
                        fc.emit(Op::And { rd: r_test, ra: r_test, rb: r_sub });
                    }
                    bindings.extend(sub_bindings);
                    // If field_name is not a placeholder, bind it.
                    if !field_name.starts_with('_') {
                        bindings.push((field_name.clone(), r_field));
                    }
                }

                Ok((Some(r_test), bindings))
            }

            PatternKind::Tuple(pats) => {
                // Match each element.
                let mut bindings = Vec::new();
                let mut test_reg = None;

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
                        match test_reg {
                            None => test_reg = Some(r_sub),
                            Some(r_main) => {
                                fc.emit(Op::And { rd: r_main, ra: r_main, rb: r_sub });
                            }
                        }
                    }
                    bindings.extend(sub_bindings);
                }

                Ok((test_reg, bindings))
            }

            PatternKind::Array(pats) | PatternKind::Slice(pats, _) => {
                // For now, just match the length and elements.
                // This is a simplified implementation.
                let mut bindings = Vec::new();
                let mut test_reg = None;

                // Check length.
                let r_len = fc.proto.alloc_reg();
                fc.emit(Op::Len { rd: r_len, ra: r_subject });
                let r_expected = fc.proto.alloc_reg();
                let len_idx = fc.proto.add_const(ConstValue::Int(pats.len() as i64));
                fc.emit(Op::LoadConst { rd: r_expected, idx: len_idx });
                let r_len_test = fc.proto.alloc_reg();
                fc.emit(Op::EqInt {
                    rd: r_len_test,
                    ra: r_len,
                    rb: r_expected,
                });
                test_reg = Some(r_len_test);

                // Match each element.
                for (i, sub_pat) in pats.iter().enumerate() {
                    let r_elem = fc.proto.alloc_reg();
                    let r_idx = fc.proto.alloc_reg();
                    let idx_const = fc.proto.add_const(ConstValue::Int(i as i64));
                    fc.emit(Op::LoadConst { rd: r_idx, idx: idx_const });
                    fc.emit(Op::ListGet {
                        rd: r_elem,
                        ra: r_subject,
                        rb: r_idx,
                    });
                    let (sub_test, sub_bindings) =
                        self.compile_pattern_test(fc, sub_pat, r_elem)?;
                    if let Some(r_sub) = sub_test {
                        if let Some(r_main) = test_reg {
                            fc.emit(Op::And { rd: r_main, ra: r_main, rb: r_sub });
                        } else {
                            test_reg = Some(r_sub);
                        }
                    }
                    bindings.extend(sub_bindings);
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
        let jmp_else = fc.emit(Op::JmpIfNot { offset: 0, ra: r_cond });

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

    fn compile_while(
        &mut self,
        fc: &mut FuncCompiler,
        cond: &Expr,
        body: &Block,
    ) -> Result<(), InterpError> {
        fc.break_jumps.push(Vec::new());
        fc.continue_jumps.push(Vec::new());

        let loop_start = fc.proto.code.len();
        let r_cond = self.compile_expr(fc, cond)?;
        let jmp_end = fc.emit(Op::JmpIfNot { offset: 0, ra: r_cond });

        fc.push_scope();
        self.compile_block(fc, body)?;
        fc.pop_scope();

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

        Ok(())
    }

    /// Compile `loop { body }` — infinite loop with break.
    fn compile_loop(
        &mut self,
        fc: &mut FuncCompiler,
        body: &Block,
    ) -> Result<(), InterpError> {
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

        Ok(())
    }

    /// Compile `while let pat = init { body }`.
    fn compile_while_let(
        &mut self,
        fc: &mut FuncCompiler,
        pat: &Pattern,
        init: &Expr,
        body: &Block,
    ) -> Result<(), InterpError> {
        fc.break_jumps.push(Vec::new());
        fc.continue_jumps.push(Vec::new());

        let loop_start = fc.proto.code.len();

        // Evaluate the init expression.
        let r_init = self.compile_expr(fc, init)?;

        // Try to match the pattern. If it fails, exit the loop.
        let (test_reg, bindings) = self.compile_pattern_test(fc, pat, r_init)?;

        let jmp_end = if let Some(r_test) = test_reg {
            Some(fc.emit(Op::JmpIfNot { offset: 0, ra: r_test }))
        } else {
            None // Pattern always matches (e.g., variable pattern).
        };

        // Bind pattern variables and compile body.
        fc.push_scope();
        for (name, r) in &bindings {
            fc.vars.last_mut().unwrap().insert(name.clone(), *r);
        }
        self.compile_block(fc, body)?;
        fc.pop_scope();

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

        Ok(())
    }

    fn compile_for(
        &mut self,
        fc: &mut FuncCompiler,
        var: &str,
        iter: &Expr,
        body: &Block,
    ) -> Result<(), InterpError> {
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
        fc.emit(Op::Len { rd: r_len, ra: r_iter });

        fc.break_jumps.push(Vec::new());
        fc.continue_jumps.push(Vec::new());

        let loop_start = fc.proto.code.len();
        // r_cmp = (idx < len)
        let r_cmp = fc.proto.alloc_reg();
        fc.emit(Op::LtInt { rd: r_cmp, ra: r_idx, rb: r_len });
        let jmp_end = fc.emit(Op::JmpIfNot { offset: 0, ra: r_cmp });

        // Push scope for loop variable (prevents leak to outer scope).
        fc.push_scope();
        // var = iter[idx]
        let r_var = fc.bind_var(var);
        fc.emit(Op::ListGet { rd: r_var, ra: r_iter, rb: r_idx });

        self.compile_block(fc, body)?;
        fc.pop_scope();

        // Increment step (continue jumps here).
        let increment_pos = fc.proto.code.len();
        fc.emit(Op::AddInt { rd: r_idx, ra: r_idx, rb: r_one });
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

        Ok(())
    }

    fn compile_assign(
        &mut self,
        fc: &mut FuncCompiler,
        target: &Expr,
        value: &Expr,
    ) -> Result<(), InterpError> {
        match target.unlocated() {
            Expr::Ident(name) => {
                let r_val = self.compile_expr(fc, value)?;
                let r_var = fc.get_or_bind(name);
                // Track type for int/float dispatch.
                let ty = self.infer_expr_type(fc, value);
                fc.set_reg_type(name, ty);
                if r_val != r_var {
                    fc.emit(Op::Mov { rd: r_var, rs: r_val });
                }
                Ok(())
            }
            Expr::Index(obj, idx) => {
                let r_obj = self.compile_expr(fc, obj)?;
                let r_idx = self.compile_expr(fc, idx)?;
                let r_val = self.compile_expr(fc, value)?;
                fc.emit(Op::ListSet { ra: r_obj, rb: r_idx, rc: r_val });
                Ok(())
            }
            _ => Err(InterpError::new(
                "bytecode: unsupported assignment target",
            )),
        }
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
        fc.emit(Op::ListGet { rd, ra: r_obj, rb: r_idx });
        Ok(rd)
    }

    fn compile_list(
        &mut self,
        fc: &mut FuncCompiler,
        elems: &[Expr],
    ) -> Result<Reg, InterpError> {
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

    fn compile_tuple(
        &mut self,
        fc: &mut FuncCompiler,
        elems: &[Expr],
    ) -> Result<Reg, InterpError> {
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

        // Store field names as constants.
        let type_name_idx = fc.proto.add_const(ConstValue::Str(
            ty.map(|s| s.to_string()).unwrap_or_default(),
        ));
        for field in fields {
            fc.proto.add_const(ConstValue::Str(field.name.clone()));
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
            .filter_map(|name| {
                fc.lookup_var(name).map(|reg| (name.clone(), reg))
            })
            .collect();

        // Create a new function proto for the lambda.
        let lambda_name = format!("__lambda_{}", self.functions.len());
        let mut lambda_fc = FuncCompiler::new(lambda_name.clone(), params.len() as u16);

        // Bind parameters to registers 0..param_count.
        for (i, param) in params.iter().enumerate() {
            lambda_fc.vars[0].insert(param.name.clone(), i as Reg);
            if let Type::Name(n, _) = param.ty.unlocated() {
                if n == "f64" {
                    lambda_fc.var_types.insert(param.name.clone(), VarType::Float);
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
                fc.emit(Op::Mov { rd: target, rs: *outer_reg });
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
                fc.vars.last_mut().unwrap().insert(name.clone(), reg);
            }
            PatternKind::Tuple(pats) => {
                for (i, p) in pats.iter().enumerate() {
                    let r = fc.proto.alloc_reg();
                    fc.emit(Op::TupleGet { rd: r, ra: reg, idx: i as u16 });
                    self.bind_pattern(fc, p, r);
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
            Expr::Binary(_, l, r) => {
                self.expr_is_float(fc, l) || self.expr_is_float(fc, r)
            }
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
    fn collect_free_vars(&self, block: &Block, params: &[Param]) -> std::collections::HashSet<String> {
        let mut free_vars = std::collections::HashSet::new();
        let mut local_vars: std::collections::HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
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
            Stmt::For { var, iterable, body } => {
                self.collect_free_vars_expr(iterable, local_vars, free_vars);
                local_vars.insert(var.clone());
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
                let mut nested_locals: std::collections::HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
                self.collect_free_vars_block(body, &mut nested_locals, free_vars);
            }
            Expr::Cast(inner, _) => {
                self.collect_free_vars_expr(inner, local_vars, free_vars);
            }
            _ => {}
        }
    }

    fn collect_pattern_vars(&self, pat: &Pattern, local_vars: &mut std::collections::HashSet<String>) {
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
                _ => VarType::Unknown,
            },
            Expr::Ident(name) => fc.var_types.get(name).copied().unwrap_or(VarType::Unknown),
            Expr::Binary(op, l, r) => {
                // Comparison operators produce Bool.
                if matches!(op, BinOp::EqCmp | BinOp::NeCmp | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) {
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

impl FuncCompiler {
    fn has_mut_params(&mut self, f: &FuncDef) {
        self.proto.has_mut_params = f.params.iter().any(|p| p.mut_);
    }
}
