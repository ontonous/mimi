//! AST → Bytecode compiler.
//!
//! Compiles Mimi AST functions into `FunctionProto` bytecode.
//! Register allocation: variables are assigned registers at first use.
//! Scope management: nested scopes share the register file (no reuse yet).

use super::instr::*;
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
    /// Break jump sites for the current loop (patched on loop exit).
    break_jumps: Vec<Vec<usize>>,
}

impl FuncCompiler {
    fn new(name: String, param_count: u16) -> Self {
        FuncCompiler {
            proto: FunctionProto::new(name, param_count),
            vars: vec![HashMap::new()],
            break_jumps: Vec::new(),
        }
    }

    fn push_scope(&mut self) {
        self.vars.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.vars.pop();
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

    /// Bind a variable name to a new register.
    fn bind_var(&mut self, name: &str) -> Reg {
        let r = self.proto.alloc_reg();
        self.vars.last_mut().unwrap().insert(name.to_string(), r);
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
        self.proto.emit(op)
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

        // Register builtins.
        self.register_builtin("println");
        self.register_builtin("print");
        self.register_builtin("len");
        self.register_builtin("push");
        self.register_builtin("pop");
        self.register_builtin("to_string");
        self.register_builtin("abs");
        self.register_builtin("str");
        self.register_builtin("int");
        self.register_builtin("float");

        // Pass 2: compile each function body.
        for item in &file.items {
            if let Item::Func(f) = item {
                let idx = self.func_table[&f.name];
                let proto = self.compile_func(f)?;
                self.functions[idx as usize] = proto;
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
    fn compile_func(&self, f: &FuncDef) -> Result<FunctionProto, InterpError> {
        let mut fc = FuncCompiler::new(f.name.clone(), f.params.len() as u16);

        // Bind parameters to registers 0..param_count.
        for (i, param) in f.params.iter().enumerate() {
            fc.vars[0].insert(param.name.clone(), i as Reg);
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
        &self,
        fc: &mut FuncCompiler,
        block: &Block,
    ) -> Result<Option<Reg>, InterpError> {
        let mut last_reg = None;
        for (i, stmt) in block.iter().enumerate() {
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
                        // Bind pattern variables to the result register.
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
                    // Handled by loop compilation (jump to loop start).
                    // For now, emit a nop; the loop compiler patches this.
                    fc.emit(Op::Nop);
                }
                // Skip non-executable statements.
                Stmt::Desc(..) | Stmt::Rule(..) | Stmt::Requires(..)
                | Stmt::Ensures(..) | Stmt::Invariant(..) | Stmt::Math(..) => {}
                _ => {
                    // Unsupported statement — will be filled in later phases.
                }
            }
        }
        Ok(last_reg)
    }

    /// Compile an expression, returning the register holding the result.
    fn compile_expr(
        &self,
        fc: &mut FuncCompiler,
        expr: &Expr,
    ) -> Result<Reg, InterpError> {
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
                // Field access needs type info — for now, use field name hash.
                // This will be improved with CheckedProgram integration.
                let field_idx = self.field_index(field);
                fc.emit(Op::RecordGet { rd, ra: r_obj, field: field_idx });
                Ok(rd)
            }
            _ => Err(InterpError::new(format!(
                "bytecode compiler: expression {:?} not yet supported",
                std::mem::discriminant(expr.unlocated())
            ))),
        }
    }

    fn compile_literal(
        &self,
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
        &self,
        fc: &mut FuncCompiler,
        op: BinOp,
        l: &Expr,
        r: &Expr,
    ) -> Result<Reg, InterpError> {
        // Short-circuit for && and ||.
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.compile_short_circuit(fc, op, l, r);
        }

        let ra = self.compile_expr(fc, l)?;
        let rb = self.compile_expr(fc, r)?;
        let rd = fc.proto.alloc_reg();

        // Determine if this is an int or float operation based on the AST.
        // In the full compiler, we'd use CheckedProgram types. For now,
        // we emit a generic dispatch that checks at runtime.
        // OPTIMIZATION: with type info, emit AddInt vs AddFloat directly.
        let is_float = self.expr_might_be_float(l) || self.expr_might_be_float(r);

        if is_float {
            self.emit_float_binop(fc, op, rd, ra, rb)?;
        } else {
            self.emit_int_binop(fc, op, rd, ra, rb)?;
        }
        Ok(rd)
    }

    fn emit_int_binop(
        &self,
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
        &self,
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
        &self,
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
        &self,
        fc: &mut FuncCompiler,
        op: UnOp,
        e: &Expr,
    ) -> Result<Reg, InterpError> {
        let ra = self.compile_expr(fc, e)?;
        let rd = fc.proto.alloc_reg();
        match op {
            UnOp::Neg => {
                // Determine int vs float.
                if self.expr_might_be_float(e) {
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
        &self,
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

        Err(InterpError::new(format!(
            "bytecode: cannot resolve call target {:?}",
            callee
        )))
    }

    fn compile_if_expr(
        &self,
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

    fn compile_if_stmt(
        &self,
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
        &self,
        fc: &mut FuncCompiler,
        cond: &Expr,
        body: &Block,
    ) -> Result<(), InterpError> {
        fc.break_jumps.push(Vec::new());

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

        Ok(())
    }

    fn compile_for(
        &self,
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

        let loop_start = fc.proto.code.len();
        // r_cmp = (idx < len)
        let r_cmp = fc.proto.alloc_reg();
        fc.emit(Op::LtInt { rd: r_cmp, ra: r_idx, rb: r_len });
        let jmp_end = fc.emit(Op::JmpIfNot { offset: 0, ra: r_cmp });

        // var = iter[idx]
        let r_var = fc.bind_var(var);
        fc.emit(Op::ListGet { rd: r_var, ra: r_iter, rb: r_idx });

        fc.push_scope();
        self.compile_block(fc, body)?;
        fc.pop_scope();

        // idx += 1
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

        Ok(())
    }

    fn compile_assign(
        &self,
        fc: &mut FuncCompiler,
        target: &Expr,
        value: &Expr,
    ) -> Result<(), InterpError> {
        match target.unlocated() {
            Expr::Ident(name) => {
                let r_val = self.compile_expr(fc, value)?;
                let r_var = fc.get_or_bind(name);
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
        &self,
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
        &self,
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
        &self,
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

    /// Heuristic: does this expression likely produce a float?
    /// Used until CheckedProgram type info is integrated.
    fn expr_might_be_float(&self, expr: &Expr) -> bool {
        match expr.unlocated() {
            Expr::Literal(Lit::Float(_)) => true,
            Expr::Cast(_, ty) => matches!(ty.unlocated(), Type::Name(n, _) if n == "f64"),
            Expr::Binary(_, l, r) => {
                self.expr_might_be_float(l) || self.expr_might_be_float(r)
            }
            Expr::Unary(_, e) => self.expr_might_be_float(e),
            _ => false,
        }
    }

    fn field_index(&self, _field: &str) -> u16 {
        // TODO: resolve from CheckedProgram type definitions.
        0
    }
}

impl FuncCompiler {
    fn has_mut_params(&mut self, f: &FuncDef) {
        self.proto.has_mut_params = f.params.iter().any(|p| p.mut_);
    }
}
