//! Register-based bytecode virtual machine for Mimi.
//!
//! Execution model:
//! - Each function call creates a Frame with a register file (Vec<Value>)
//! - Instructions operate on registers by index
//! - The VM dispatch loop is a single `match` on Op — no AST walking
//! - Arithmetic is typed (AddInt vs AddFloat) — zero runtime type dispatch

use super::instr::*;
use crate::interp::error::InterpError;
use crate::interp::value::Value;

/// A single function activation frame.
struct Frame {
    /// Register file: indexed by Reg (u16).
    regs: Vec<Value>,
    /// Instruction pointer (index into FunctionProto.code).
    pc: usize,
    /// Prototype index.
    proto_idx: FuncIdx,
    /// Register in the CALLER's frame where the return value goes.
    /// None for the entry frame (top-level).
    return_reg: Option<Reg>,
}

/// The bytecode VM.
pub struct BytecodeVM<'a> {
    /// The compiled program.
    program: &'a BytecodeProgram,
    /// Call stack of frames.
    stack: Vec<Frame>,
    /// Captured stdout output (for testing).
    stdout: String,
    /// Recursion depth guard.
    depth: usize,
}

const MAX_DEPTH: usize = 768;

impl<'a> BytecodeVM<'a> {
    pub fn new(program: &'a BytecodeProgram) -> Self {
        BytecodeVM {
            program,
            stack: Vec::with_capacity(64),
            stdout: String::new(),
            depth: 0,
        }
    }

    /// Run the program from the entry point. Returns the exit code.
    pub fn run(&mut self) -> Result<i64, InterpError> {
        let entry = self.program.entry;
        self.push_frame(entry, &[], None)?;
        let result = self.exec_loop()?;
        match result {
            Value::Int(code) => Ok(code),
            Value::Unit => Ok(0),
            other => Err(InterpError::new(format!(
                "main returned non-integer: {}",
                other
            ))),
        }
    }

    /// Push a new function frame onto the call stack.
    fn push_frame(
        &mut self,
        func_idx: FuncIdx,
        args: &[Value],
        return_reg: Option<Reg>,
    ) -> Result<(), InterpError> {
        if self.depth >= MAX_DEPTH {
            return Err(InterpError::new(
                "recursion limit exceeded (possible infinite recursion)",
            ));
        }
        self.depth += 1;

        let proto = &self.program.functions[func_idx as usize];
        let reg_count = proto.register_count as usize;

        let mut regs = Vec::with_capacity(reg_count);
        for (i, arg) in args.iter().enumerate() {
            if i < proto.param_count as usize {
                regs.push(arg.clone());
            }
        }
        regs.resize(reg_count, Value::Unit);

        self.stack.push(Frame {
            regs,
            pc: 0,
            proto_idx: func_idx,
            return_reg,
        });
        Ok(())
    }

    /// Iterative dispatch loop: runs until the entry frame returns.
    /// Call/Ret are handled by pushing/popping frames — no Rust recursion.
    fn exec_loop(&mut self) -> Result<Value, InterpError> {
        loop {
            let frame = self.stack.last().unwrap();
            let proto = &self.program.functions[frame.proto_idx as usize];

            if frame.pc >= proto.code.len() {
                // Fell off the end — implicit return Unit.
                let return_reg = frame.return_reg;
                self.stack.pop();
                self.depth -= 1;
                if self.stack.is_empty() {
                    return Ok(Value::Unit);
                }
                if let Some(rd) = return_reg {
                    self.set_reg(rd, Value::Unit);
                }
                continue;
            }

            let op = proto.code[frame.pc];
            self.stack.last_mut().unwrap().pc += 1;

            match op {
                // ── Constants & moves ──────────────────────────
                Op::LoadConst { rd, idx } => {
                    let val = self.load_const(proto, idx);
                    self.set_reg(rd, val);
                }
                Op::LoadUnit { rd } => self.set_reg(rd, Value::Unit),
                Op::LoadTrue { rd } => self.set_reg(rd, Value::Bool(true)),
                Op::LoadFalse { rd } => self.set_reg(rd, Value::Bool(false)),
                Op::Mov { rd, rs } => {
                    let v = self.get_reg(rs).clone();
                    self.set_reg(rd, v);
                }

                // ── Integer arithmetic ─────────────────────────
                Op::AddInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    let r = a.checked_add(b).ok_or_else(|| {
                        InterpError::integer_overflow("integer addition overflow")
                    })?;
                    self.set_reg(rd, Value::Int(r));
                }
                Op::SubInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    let r = a.checked_sub(b).ok_or_else(|| {
                        InterpError::integer_overflow("integer subtraction overflow")
                    })?;
                    self.set_reg(rd, Value::Int(r));
                }
                Op::MulInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    let r = a.checked_mul(b).ok_or_else(|| {
                        InterpError::integer_overflow("integer multiplication overflow")
                    })?;
                    self.set_reg(rd, Value::Int(r));
                }
                Op::DivInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    if b == 0 {
                        return Err(InterpError::div_by_zero());
                    }
                    let r = a.checked_div(b).ok_or_else(|| {
                        InterpError::integer_overflow("integer division overflow")
                    })?;
                    self.set_reg(rd, Value::Int(r));
                }
                Op::ModInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    if b == 0 {
                        return Err(InterpError::div_by_zero());
                    }
                    let r = a.checked_rem(b).ok_or_else(|| {
                        InterpError::integer_overflow("integer remainder overflow")
                    })?;
                    self.set_reg(rd, Value::Int(r));
                }
                Op::NegInt { rd, ra } => {
                    let a = self.get_int(ra)?;
                    let r = a.checked_neg().ok_or_else(|| {
                        InterpError::integer_overflow("integer negation overflow")
                    })?;
                    self.set_reg(rd, Value::Int(r));
                }

                // ── Float arithmetic ───────────────────────────
                Op::AddFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    self.check_float(a + b, "+")?;
                    self.set_reg(rd, Value::Float(a + b));
                }
                Op::SubFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    self.check_float(a - b, "-")?;
                    self.set_reg(rd, Value::Float(a - b));
                }
                Op::MulFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    self.check_float(a * b, "*")?;
                    self.set_reg(rd, Value::Float(a * b));
                }
                Op::DivFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    if b == 0.0 {
                        return Err(InterpError::div_by_zero());
                    }
                    let r = a / b;
                    self.check_float(r, "/")?;
                    self.set_reg(rd, Value::Float(r));
                }
                Op::NegFloat { rd, ra } => {
                    let a = self.get_float(ra)?;
                    self.set_reg(rd, Value::Float(-a));
                }
                Op::IntToFloat { rd, ra } => {
                    let a = self.get_int(ra)?;
                    self.set_reg(rd, Value::Float(a as f64));
                }

                // ── Comparison ─────────────────────────────────
                Op::EqInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a == b));
                }
                Op::NeInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a != b));
                }
                Op::LtInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a < b));
                }
                Op::GtInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a > b));
                }
                Op::LeInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a <= b));
                }
                Op::GeInt { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a >= b));
                }
                Op::EqFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a == b));
                }
                Op::LtFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a < b));
                }
                Op::GtFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a > b));
                }
                Op::LeFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a <= b));
                }
                Op::GeFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    self.set_reg(rd, Value::Bool(a >= b));
                }
                Op::Eq { rd, ra, rb } => {
                    let a = self.get_reg(ra).clone();
                    let b = self.get_reg(rb).clone();
                    self.set_reg(rd, Value::Bool(crate::interp::values_equal(&a, &b)));
                }
                Op::Ne { rd, ra, rb } => {
                    let a = self.get_reg(ra).clone();
                    let b = self.get_reg(rb).clone();
                    self.set_reg(rd, Value::Bool(!crate::interp::values_equal(&a, &b)));
                }

                // ── Bitwise ────────────────────────────────────
                Op::BitAnd { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    self.set_reg(rd, Value::Int(a & b));
                }
                Op::BitOr { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    self.set_reg(rd, Value::Int(a | b));
                }
                Op::BitXor { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    self.set_reg(rd, Value::Int(a ^ b));
                }
                Op::Shl { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    let r = u32::try_from(b)
                        .ok()
                        .and_then(|s| a.checked_shl(s))
                        .ok_or_else(|| {
                            InterpError::integer_overflow("shift overflow in <<")
                        })?;
                    self.set_reg(rd, Value::Int(r));
                }
                Op::Shr { rd, ra, rb } => {
                    let (a, b) = self.get_int2(ra, rb)?;
                    let r = u32::try_from(b)
                        .ok()
                        .and_then(|s| a.checked_shr(s))
                        .ok_or_else(|| {
                            InterpError::integer_overflow("shift overflow in >>")
                        })?;
                    self.set_reg(rd, Value::Int(r));
                }
                Op::BitNot { rd, ra } => {
                    let v = self.get_reg(ra);
                    self.set_reg(rd, Value::Bool(!crate::interp::is_truthy(v)));
                }
                Op::Not { rd, ra } => {
                    let v = self.get_reg(ra);
                    self.set_reg(rd, Value::Bool(!crate::interp::is_truthy(v)));
                }

                // ── String ─────────────────────────────────────
                Op::ConcatStr { rd, ra, rb } => {
                    let a = self.get_reg(ra).clone();
                    let b = self.get_reg(rb).clone();
                    let result = format!("{}{}", a, b);
                    self.set_reg(rd, Value::String(result));
                }

                // ── Control flow ───────────────────────────────
                Op::Jmp { offset } => {
                    let frame = self.stack.last_mut().unwrap();
                    frame.pc = (frame.pc as i32 + offset) as usize;
                }
                Op::JmpIf { offset, ra } => {
                    let v = self.get_reg(ra).clone();
                    if crate::interp::is_truthy(&v) {
                        let frame = self.stack.last_mut().unwrap();
                        frame.pc = (frame.pc as i32 + offset) as usize;
                    }
                }
                Op::JmpIfNot { offset, ra } => {
                    let v = self.get_reg(ra).clone();
                    if !crate::interp::is_truthy(&v) {
                        let frame = self.stack.last_mut().unwrap();
                        frame.pc = (frame.pc as i32 + offset) as usize;
                    }
                }

                // ── Function calls ─────────────────────────────
                Op::Call {
                    rd,
                    func,
                    args_base,
                    argc,
                } => {
                    let args: Vec<Value> = (0..argc)
                        .map(|i| self.get_reg(args_base + i).clone())
                        .collect();
                    self.push_frame(func, &args, Some(rd))?;
                    // Continue loop — new frame is now active.
                }
                Op::CallBuiltin {
                    rd,
                    builtin,
                    args_base,
                    argc,
                } => {
                    let args: Vec<Value> = (0..argc)
                        .map(|i| self.get_reg(args_base + i).clone())
                        .collect();
                    let result = self.call_builtin(builtin, &args)?;
                    self.set_reg(rd, result);
                }
                Op::CallIndirect { .. } => {
                    return Err(InterpError::new(
                        "bytecode VM: CallIndirect not yet implemented",
                    ));
                }
                Op::Ret { ra } => {
                    let v = self.get_reg(ra).clone();
                    let return_reg = self.stack.last().unwrap().return_reg;
                    self.stack.pop();
                    self.depth -= 1;
                    if self.stack.is_empty() {
                        return Ok(v);
                    }
                    if let Some(rd) = return_reg {
                        self.set_reg(rd, v);
                    }
                }
                Op::RetUnit => {
                    let return_reg = self.stack.last().unwrap().return_reg;
                    self.stack.pop();
                    self.depth -= 1;
                    if self.stack.is_empty() {
                        return Ok(Value::Unit);
                    }
                    if let Some(rd) = return_reg {
                        self.set_reg(rd, Value::Unit);
                    }
                }

                // ── Data structures ────────────────────────────
                Op::NewList { rd, capacity } => {
                    let list = Vec::with_capacity(capacity as usize);
                    self.set_reg(rd, Value::List(list));
                }
                Op::ListPush { ra, rb } => {
                    let val = self.get_reg(rb).clone();
                    let list = self.get_reg_mut(ra);
                    match list {
                        Value::List(l) => l.push(val),
                        other => {
                            return Err(InterpError::new(format!(
                                "push: expected List, got {}",
                                other
                            )))
                        }
                    }
                }
                Op::ListGet { rd, ra, rb } => {
                    let idx = self.get_int(rb)? as usize;
                    let list = self.get_reg(ra).clone();
                    match list {
                        Value::List(l) => {
                            if idx >= l.len() {
                                return Err(InterpError::new(format!(
                                    "index {} out of bounds (len {})",
                                    idx,
                                    l.len()
                                )));
                            }
                            let v = l[idx].clone();
                            self.set_reg(rd, v);
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "index: expected List, got {}",
                                other
                            )))
                        }
                    }
                }
                Op::ListSet { ra, rb, rc } => {
                    let idx = self.get_int(rb)? as usize;
                    let val = self.get_reg(rc).clone();
                    let list = self.get_reg_mut(ra);
                    match list {
                        Value::List(l) => {
                            if idx >= l.len() {
                                return Err(InterpError::new(format!(
                                    "index {} out of bounds (len {})",
                                    idx,
                                    l.len()
                                )));
                            }
                            l[idx] = val;
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "index set: expected List, got {}",
                                other
                            )))
                        }
                    }
                }
                Op::Len { rd, ra } => {
                    let v = self.get_reg(ra);
                    let len = match v {
                        Value::List(l) => l.len(),
                        Value::String(s) => s.len(),
                        Value::Tuple(t) => t.len(),
                        other => {
                            return Err(InterpError::new(format!(
                                "len: unsupported type {}",
                                other
                            )))
                        }
                    };
                    self.set_reg(rd, Value::Int(len as i64));
                }
                Op::NewTuple { rd, base, arity } => {
                    let elems: Vec<Value> = (0..arity)
                        .map(|i| self.get_reg(base + i).clone())
                        .collect();
                    self.set_reg(rd, Value::Tuple(elems));
                }
                Op::TupleGet { rd, ra, idx } => {
                    let v = self.get_reg(ra).clone();
                    match v {
                        Value::Tuple(t) => {
                            if (idx as usize) >= t.len() {
                                return Err(InterpError::new(format!(
                                    "tuple index {} out of bounds (arity {})",
                                    idx,
                                    t.len()
                                )));
                            }
                            let elem = t[idx as usize].clone();
                            self.set_reg(rd, elem);
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "tuple get: expected Tuple, got {}",
                                other
                            )))
                        }
                    }
                }

                // ── Option / Result ────────────────────────────
                Op::Some { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    self.set_reg(rd, Value::Tuple(vec![Value::Bool(true), v]));
                }
                Op::None { rd } => {
                    self.set_reg(rd, Value::Tuple(vec![Value::Bool(false), Value::Unit]));
                }
                Op::Ok { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    self.set_reg(
                        rd,
                        Value::Tuple(vec![Value::Bool(true), v, Value::Unit]),
                    );
                }
                Op::Err { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    self.set_reg(
                        rd,
                        Value::Tuple(vec![Value::Bool(false), Value::Unit, v]),
                    );
                }
                Op::IsSome { rd, ra } => {
                    let v = self.get_reg(ra);
                    let is_some = matches!(v, Value::Tuple(t) if t.first() == Some(&Value::Bool(true)));
                    self.set_reg(rd, Value::Bool(is_some));
                }
                Op::Unwrap { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    match v {
                        Value::Tuple(t) if t.first() == Some(&Value::Bool(true)) => {
                            let inner = t.get(1).cloned().unwrap_or(Value::Unit);
                            self.set_reg(rd, inner);
                        }
                        _ => {
                            return Err(InterpError::new("unwrap called on None"));
                        }
                    }
                }

                // ── Misc ───────────────────────────────────────
                Op::ToString { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    self.set_reg(rd, Value::String(v.to_string()));
                }
                Op::Trap { msg } => {
                    let proto = &self.program.functions
                        [self.stack.last().unwrap().proto_idx as usize];
                    let msg_str = match &proto.constants[msg as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => "unknown trap".to_string(),
                    };
                    return Err(InterpError::new(msg_str));
                }
                Op::Nop => {}

                // ── Not yet implemented ────────────────────────
                _ => {
                    return Err(InterpError::new(format!(
                        "bytecode VM: opcode {:?} not yet implemented",
                        op
                    )));
                }
            }
        }
    }

    // ── Builtin dispatch ─────────────────────────────────────

    fn call_builtin(
        &mut self,
        idx: BuiltinIdx,
        args: &[Value],
    ) -> Result<Value, InterpError> {
        let name = &self.program.builtin_names[idx as usize];
        match name.as_str() {
            "println" => {
                let s = args
                    .iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                self.stdout.push_str(&s);
                self.stdout.push('\n');
                eprintln!("{}", s);
                Ok(Value::Unit)
            }
            "print" => {
                let s = args
                    .iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                self.stdout.push_str(&s);
                eprint!("{}", s);
                Ok(Value::Unit)
            }
            "len" => {
                if args.len() != 1 {
                    return Err(InterpError::new("len expects 1 argument"));
                }
                let len = match &args[0] {
                    Value::List(l) => l.len(),
                    Value::String(s) => s.len(),
                    Value::Tuple(t) => t.len(),
                    Value::Set(s) => s.len(),
                    other => {
                        return Err(InterpError::new(format!(
                            "len: unsupported type {}",
                            other
                        )))
                    }
                };
                Ok(Value::Int(len as i64))
            }
            "push" => {
                if args.len() != 2 {
                    return Err(InterpError::new("push expects 2 arguments"));
                }
                // push modifies the list in place — but our args are clones.
                // For bytecode, we handle push via ListPush opcode instead.
                // This fallback is for direct builtin calls.
                Err(InterpError::new(
                    "push: use ListPush opcode in bytecode (direct builtin call not supported)",
                ))
            }
            "to_string" => {
                if args.len() != 1 {
                    return Err(InterpError::new("to_string expects 1 argument"));
                }
                Ok(Value::String(args[0].to_string()))
            }
            "str" => {
                if args.len() != 1 {
                    return Err(InterpError::new("str expects 1 argument"));
                }
                Ok(Value::String(args[0].to_string()))
            }
            "int" | "float" | "abs" => {
                Err(InterpError::new(format!(
                    "bytecode builtin '{}' not yet implemented",
                    name
                )))
            }
            _ => Err(InterpError::new(format!(
                "bytecode: unknown builtin '{}'",
                name
            ))),
        }
    }

    /// Get captured stdout (for testing).
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    // ── Register access helpers ──────────────────────────────

    fn get_reg(&self, r: Reg) -> &Value {
        &self.stack.last().unwrap().regs[r as usize]
    }

    fn get_reg_mut(&mut self, r: Reg) -> &mut Value {
        &mut self.stack.last_mut().unwrap().regs[r as usize]
    }

    fn set_reg(&mut self, r: Reg, v: Value) {
        self.stack.last_mut().unwrap().regs[r as usize] = v;
    }

    fn get_int(&self, r: Reg) -> Result<i64, InterpError> {
        match self.get_reg(r) {
            Value::Int(v) => Ok(*v),
            other => Err(InterpError::new(format!(
                "expected Int, got {}",
                other
            ))),
        }
    }

    fn get_int2(&self, ra: Reg, rb: Reg) -> Result<(i64, i64), InterpError> {
        Ok((self.get_int(ra)?, self.get_int(rb)?))
    }

    fn get_float(&self, r: Reg) -> Result<f64, InterpError> {
        match self.get_reg(r) {
            Value::Float(v) => Ok(*v),
            other => Err(InterpError::new(format!(
                "expected Float, got {}",
                other
            ))),
        }
    }

    fn get_float2(&self, ra: Reg, rb: Reg) -> Result<(f64, f64), InterpError> {
        Ok((self.get_float(ra)?, self.get_float(rb)?))
    }

    fn check_float(&self, v: f64, op: &str) -> Result<(), InterpError> {
        if v.is_nan() || v.is_infinite() {
            return Err(InterpError::float_error(format!(
                "invalid floating-point result from {}",
                op
            )));
        }
        Ok(())
    }

    fn load_const(&self, proto: &FunctionProto, idx: ConstIdx) -> Value {
        match &proto.constants[idx as usize] {
            ConstValue::Int(v) => Value::Int(*v),
            ConstValue::Float(v) => Value::Float(*v),
            ConstValue::Bool(v) => Value::Bool(*v),
            ConstValue::Str(v) => Value::String(v.clone()),
            ConstValue::Unit => Value::Unit,
        }
    }
}
