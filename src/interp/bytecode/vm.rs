//! Register-based bytecode virtual machine for Mimi.
//!
//! Execution model:
//! - Each function call creates a Frame with a register file (Vec<Value>)
//! - Instructions operate on registers by index
//! - The VM dispatch loop is a single `match` on Op — no AST walking
//! - Arithmetic is typed (AddInt vs AddFloat) — zero runtime type dispatch

use super::instr::*;
use super::registry::{self, BuiltinRegistry};
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
    /// When true, wrap the return value in Ok(...) on Ret.
    /// Used by FlowTransition for transitions with `fails` clause.
    wrap_ok: bool,
    /// Source state value for `fails` transitions.
    /// Used to construct Err((source, error)) on failure.
    flow_source_state: Option<Value>,
    /// True when this frame is returning from a `?` operator.
    /// Used by wrap_ok to distinguish `?` Err (rejection) from a
    /// final-expression Err (success producing an Err value).
    early_return: bool,
    /// When set, a fault handler's instruction index. If a builtin call
    /// fails or `?` triggers RetEarly, execution jumps here instead of
    /// propagating the error. Set by Op::SetFaultPc, cleared by ClearFaultPc.
    fault_pc: Option<usize>,
    /// When fault_pc intercepts RetEarly, saves the error-value register
    /// so the fault handler can re-emit RetEarly after compensations.
    fault_reg: Option<Reg>,
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
    /// When > 0, exec_loop returns when depth drops below this value.
    /// Used by call_closure to stop when the closure frame returns.
    /// 0 = normal mode (return when stack is empty).
    stop_depth: usize,
    /// Builtin function registry (D1: declarative, not giant match).
    registry: BuiltinRegistry,
    /// Actor spawn quota (None = unlimited).
    pub max_children: Option<usize>,
    /// Total actors spawned in this VM session.
    pub spawn_count: usize,
    /// User CLI arguments (args passed after the program filename).
    pub cli_args: Vec<String>,
    /// When set, the VM should terminate with this exit code.
    exit_requested: Option<i64>,
}

const MAX_DEPTH: usize = 768;

impl<'a> BytecodeVM<'a> {
    pub fn new(program: &'a BytecodeProgram) -> Self {
        BytecodeVM {
            program,
            stack: Vec::with_capacity(64),
            stdout: String::new(),
            depth: 0,
            stop_depth: 0,
            registry: registry::create_registry(),
            max_children: program.max_children,
            spawn_count: 0,
            cli_args: Vec::new(),
            exit_requested: None,
        }
    }

    /// Request the VM to terminate with the given exit code.
    /// Called by the `exit()` builtin.
    pub fn request_exit(&mut self, code: i64) {
        self.exit_requested = Some(code);
    }

    /// Set the user CLI arguments that `args()` / `cli_args()` builtins return.
    pub fn with_cli_args(mut self, cli_args: Vec<String>) -> Self {
        self.cli_args = cli_args;
        self
    }

    /// Run the program from the entry point. Returns the exit code.
    pub fn run(&mut self) -> Result<i64, InterpError> {
        let entry = self.program.entry;
        self.push_frame(entry, Vec::new(), None)?;
        let result = self.exec_loop();
        // Enrich errors with function name + line (D5/D12).
        let result = result.map_err(|e| self.enrich_error(e));
        match result {
            Ok(Value::Int(code)) => Ok(code),
            Ok(Value::Unit) => Ok(0),
            Ok(other) => Err(InterpError::new(format!(
                "main returned non-integer: {}",
                other
            ))),
            Err(e) => Err(e),
        }
    }

    /// Run the program and return the full Value (not just exit code).
    /// Used by test infrastructure that needs the actual return value.
    pub fn run_value(&mut self) -> Result<Value, InterpError> {
        let entry = self.program.entry;
        self.push_frame(entry, Vec::new(), None)?;
        let result = self.exec_loop();
        result.map_err(|e| self.enrich_error(e))
    }

    /// Take captured stdout (consumes the buffer, leaves empty string).
    pub fn take_stdout(&mut self) -> String {
        std::mem::take(&mut self.stdout)
    }

    /// Enrich an error with the current frame's function name and source line.
    fn enrich_error(&self, err: InterpError) -> InterpError {
        if let Some(frame) = self.stack.last() {
            let proto = &self.program.functions[frame.proto_idx as usize];
            let err = err.in_func(proto.name.clone());
            let pc = if frame.pc > 0 { frame.pc - 1 } else { 0 };
            if let Some(&line) = proto.line_table.get(pc) {
                if line > 0 {
                    return err.at_line(line);
                }
            }
            err
        } else {
            err
        }
    }

    /// Push a new function frame onto the call stack.
    fn push_frame(
        &mut self,
        func_idx: FuncIdx,
        args: Vec<Value>,
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

        if args.len() != proto.param_count as usize {
            return Err(InterpError::new(format!(
                "function '{}' expects {} argument(s), got {}",
                proto.name, proto.param_count, args.len()
            )));
        }

        let mut regs = args;
        regs.resize(reg_count, Value::Unit);

        self.stack.push(Frame {
            regs,
            pc: 0,
            proto_idx: func_idx,
            return_reg,
            wrap_ok: false,
            flow_source_state: None,
            early_return: false,
            fault_pc: None,
            fault_reg: None,
        });
        Ok(())
    }

    /// Push a frame with wrap_ok flag (for flow transitions with `fails`).
    fn push_frame_wrap_ok(
        &mut self,
        func_idx: FuncIdx,
        args: Vec<Value>,
        return_reg: Option<Reg>,
        source_state: Value,
    ) -> Result<(), InterpError> {
        self.push_frame(func_idx, args, return_reg)?;
        let frame = self.stack.last_mut().unwrap();
        frame.wrap_ok = true;
        frame.flow_source_state = Some(source_state);
        Ok(())
    }

    /// Iterative dispatch loop: runs until the entry frame returns
    /// (or until depth drops below stop_depth for closure calls).
    /// Call/Ret are handled by pushing/popping frames — no Rust recursion.
    fn exec_loop(&mut self) -> Result<Value, InterpError> {
        let stop = self.stop_depth;
        loop {
            // Check for exit() builtin request.
            if let Some(code) = self.exit_requested.take() {
                return Ok(Value::Int(code));
            }

            let frame = self.stack.last().unwrap();
            let proto = &self.program.functions[frame.proto_idx as usize];

            if frame.pc >= proto.code.len() {
                // Fell off the end — implicit return Unit.
                let return_reg = frame.return_reg;
                let wrap_ok = frame.wrap_ok;
                self.stack.pop();
                self.depth -= 1;
                let v = if wrap_ok {
                    Value::Variant("Ok".to_string(), vec![Value::Unit])
                } else {
                    Value::Unit
                };
                if self.stack.is_empty() || (stop > 0 && self.depth < stop) {
                    return Ok(v);
                }
                if let Some(rd) = return_reg {
                    self.set_reg(rd, v);
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
                    if matches!(self.get_reg(ra), Value::String(_)) && matches!(self.get_reg(rb), Value::String(_)) {
                        let result = format!("{}{}", self.get_reg(ra), self.get_reg(rb));
                        self.set_reg(rd, Value::String(result));
                    } else if matches!(self.get_reg(ra), Value::Float(_)) || matches!(self.get_reg(rb), Value::Float(_)) {
                        let (af, bf) = (value_to_f64(self.get_reg(ra))?, value_to_f64(self.get_reg(rb))?);
                        let r = af + bf;
                        self.check_float(r, "+")?;
                        self.set_reg(rd, Value::Float(r));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        let r = a.checked_add(b).ok_or_else(|| {
                            InterpError::integer_overflow("integer addition overflow")
                        })?;
                        self.set_reg(rd, Value::Int(r));
                    }
                }
                Op::SubInt { rd, ra, rb } => {
                    if matches!(self.get_reg(ra), Value::Float(_)) || matches!(self.get_reg(rb), Value::Float(_)) {
                        let (af, bf) = (value_to_f64(self.get_reg(ra))?, value_to_f64(self.get_reg(rb))?);
                        let r = af - bf;
                        self.check_float(r, "-")?;
                        self.set_reg(rd, Value::Float(r));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        let r = a.checked_sub(b).ok_or_else(|| {
                            InterpError::integer_overflow("integer subtraction overflow")
                        })?;
                        self.set_reg(rd, Value::Int(r));
                    }
                }
                Op::MulInt { rd, ra, rb } => {
                    if matches!(self.get_reg(ra), Value::Float(_)) || matches!(self.get_reg(rb), Value::Float(_)) {
                        let (af, bf) = (value_to_f64(self.get_reg(ra))?, value_to_f64(self.get_reg(rb))?);
                        let r = af * bf;
                        self.check_float(r, "*")?;
                        self.set_reg(rd, Value::Float(r));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        let r = a.checked_mul(b).ok_or_else(|| {
                            InterpError::integer_overflow("integer multiplication overflow")
                        })?;
                        self.set_reg(rd, Value::Int(r));
                    }
                }
                Op::DivInt { rd, ra, rb } => {
                    if matches!(self.get_reg(ra), Value::Float(_)) || matches!(self.get_reg(rb), Value::Float(_)) {
                        let (af, bf) = (value_to_f64(self.get_reg(ra))?, value_to_f64(self.get_reg(rb))?);
                        if bf == 0.0 {
                            return Err(InterpError::div_by_zero());
                        }
                        let r = af / bf;
                        self.check_float(r, "/")?;
                        self.set_reg(rd, Value::Float(r));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        if b == 0 {
                            return Err(InterpError::div_by_zero());
                        }
                        let r = a.checked_div(b).ok_or_else(|| {
                            InterpError::integer_overflow("integer division overflow")
                        })?;
                        self.set_reg(rd, Value::Int(r));
                    }
                }
                Op::ModInt { rd, ra, rb } => {
                    if matches!(self.get_reg(ra), Value::Float(_)) || matches!(self.get_reg(rb), Value::Float(_)) {
                        let a = value_to_f64(self.get_reg(ra))?;
                        let b = value_to_f64(self.get_reg(rb))?;
                        if b == 0.0 {
                            return Err(InterpError::div_by_zero());
                        }
                        self.set_reg(rd, Value::Float(a % b));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        if b == 0 {
                            return Err(InterpError::div_by_zero());
                        }
                        let r = a.checked_rem(b).ok_or_else(|| {
                            InterpError::integer_overflow("integer remainder overflow")
                        })?;
                        self.set_reg(rd, Value::Int(r));
                    }
                }
                Op::NegInt { rd, ra } => {
                    if matches!(self.get_reg(ra), Value::Float(_)) {
                        let a = self.get_float(ra)?;
                        let r = -a;
                        self.check_float(r, "neg")?;
                        self.set_reg(rd, Value::Float(r));
                    } else {
                        let a = self.get_int(ra)?;
                        let r = a.checked_neg().ok_or_else(|| {
                            InterpError::integer_overflow("integer negation overflow")
                        })?;
                        self.set_reg(rd, Value::Int(r));
                    }
                }

                // ── Float arithmetic ───────────────────────────
                Op::AddFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    let r = a + b;
                    self.check_float(r, "+")?;
                    self.set_reg(rd, Value::Float(r));
                }
                Op::SubFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    let r = a - b;
                    self.check_float(r, "-")?;
                    self.set_reg(rd, Value::Float(r));
                }
                Op::MulFloat { rd, ra, rb } => {
                    let (a, b) = self.get_float2(ra, rb)?;
                    let r = a * b;
                    self.check_float(r, "*")?;
                    self.set_reg(rd, Value::Float(r));
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
                    if !matches!(self.get_reg(ra), Value::Int(_)) || !matches!(self.get_reg(rb), Value::Int(_)) {
                        let result = crate::interp::values_equal(self.get_reg(ra), self.get_reg(rb));
                        self.set_reg(rd, Value::Bool(result));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        self.set_reg(rd, Value::Bool(a == b));
                    }
                }
                Op::NeInt { rd, ra, rb } => {
                    if !matches!(self.get_reg(ra), Value::Int(_)) || !matches!(self.get_reg(rb), Value::Int(_)) {
                        let result = !crate::interp::values_equal(self.get_reg(ra), self.get_reg(rb));
                        self.set_reg(rd, Value::Bool(result));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        self.set_reg(rd, Value::Bool(a != b));
                    }
                }
                Op::LtInt { rd, ra, rb } => {
                    if matches!(self.get_reg(ra), Value::String(_)) || matches!(self.get_reg(rb), Value::String(_)) {
                        let result = match (self.get_reg(ra), self.get_reg(rb)) {
                            (Value::String(a), Value::String(b)) => a < b,
                            (a, b) => a.to_string() < b.to_string(),
                        };
                        self.set_reg(rd, Value::Bool(result));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        self.set_reg(rd, Value::Bool(a < b));
                    }
                }
                Op::GtInt { rd, ra, rb } => {
                    if matches!(self.get_reg(ra), Value::String(_)) || matches!(self.get_reg(rb), Value::String(_)) {
                        let result = match (self.get_reg(ra), self.get_reg(rb)) {
                            (Value::String(a), Value::String(b)) => a > b,
                            (a, b) => a.to_string() > b.to_string(),
                        };
                        self.set_reg(rd, Value::Bool(result));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        self.set_reg(rd, Value::Bool(a > b));
                    }
                }
                Op::LeInt { rd, ra, rb } => {
                    if matches!(self.get_reg(ra), Value::String(_)) || matches!(self.get_reg(rb), Value::String(_)) {
                        let result = match (self.get_reg(ra), self.get_reg(rb)) {
                            (Value::String(a), Value::String(b)) => a <= b,
                            (a, b) => a.to_string() <= b.to_string(),
                        };
                        self.set_reg(rd, Value::Bool(result));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        self.set_reg(rd, Value::Bool(a <= b));
                    }
                }
                Op::GeInt { rd, ra, rb } => {
                    if matches!(self.get_reg(ra), Value::String(_)) || matches!(self.get_reg(rb), Value::String(_)) {
                        let result = match (self.get_reg(ra), self.get_reg(rb)) {
                            (Value::String(a), Value::String(b)) => a >= b,
                            (a, b) => a.to_string() >= b.to_string(),
                        };
                        self.set_reg(rd, Value::Bool(result));
                    } else {
                        let (a, b) = self.get_int2(ra, rb)?;
                        self.set_reg(rd, Value::Bool(a >= b));
                    }
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
                    let result = crate::interp::values_equal(self.get_reg(ra), self.get_reg(rb));
                    self.set_reg(rd, Value::Bool(result));
                }
                Op::Ne { rd, ra, rb } => {
                    let result = !crate::interp::values_equal(self.get_reg(ra), self.get_reg(rb));
                    self.set_reg(rd, Value::Bool(result));
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
                    let a = self.get_int(ra)?;
                    self.set_reg(rd, Value::Int(!a));
                }
                Op::Not { rd, ra } => {
                    let v = self.get_reg(ra);
                    self.set_reg(rd, Value::Bool(!crate::interp::is_truthy(v)));
                }
                Op::And { rd, ra, rb } => {
                    if crate::interp::is_truthy(self.get_reg(ra)) {
                        let b = crate::interp::is_truthy(self.get_reg(rb));
                        self.set_reg(rd, Value::Bool(b));
                    } else {
                        self.set_reg(rd, Value::Bool(false));
                    }
                }
                Op::Or { rd, ra, rb } => {
                    if crate::interp::is_truthy(self.get_reg(ra)) {
                        self.set_reg(rd, Value::Bool(true));
                    } else {
                        let b = crate::interp::is_truthy(self.get_reg(rb));
                        self.set_reg(rd, Value::Bool(b));
                    }
                }

                // ── String ─────────────────────────────────────
                Op::ConcatStr { rd, ra, rb } => {
                    let result = format!("{}{}", self.get_reg(ra), self.get_reg(rb));
                    self.set_reg(rd, Value::String(result));
                }

                // ── Control flow ───────────────────────────────
                Op::Jmp { offset } => {
                    let pc = self.stack.last().unwrap().pc as i32;
                    let new_pc = pc + offset;
                    if new_pc < 0 {
                        return Err(InterpError::new(format!(
                            "Jmp underflow: pc={} offset={}", pc, offset
                        )));
                    }
                    self.stack.last_mut().unwrap().pc = new_pc as usize;
                }
                Op::JmpIf { offset, ra } => {
                    if crate::interp::is_truthy(self.get_reg(ra)) {
                        let pc = self.stack.last().unwrap().pc as i32;
                        let new_pc = pc + offset;
                        if new_pc < 0 {
                            return Err(InterpError::new(format!(
                                "JmpIf underflow: pc={} offset={}", pc, offset
                            )));
                        }
                        self.stack.last_mut().unwrap().pc = new_pc as usize;
                    }
                }
                Op::JmpIfNot { offset, ra } => {
                    if !crate::interp::is_truthy(self.get_reg(ra)) {
                        let pc = self.stack.last().unwrap().pc as i32;
                        let new_pc = pc + offset;
                        if new_pc < 0 {
                            return Err(InterpError::new(format!(
                                "JmpIfNot underflow: pc={} offset={}", pc, offset
                            )));
                        }
                        self.stack.last_mut().unwrap().pc = new_pc as usize;
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
                    self.push_frame(func, args, Some(rd))?;
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
                    match self.call_builtin(builtin, &args) {
                        Ok(v) => self.set_reg(rd, v),
                        Err(e) => {
                            if let Some(handler_pc) = self.stack.last_mut().and_then(|f| f.fault_pc.take()) {
                                self.stack.last_mut().unwrap().pc = handler_pc;
                            } else {
                                return Err(e);
                            }
                        }
                    }
                }
                Op::Ret { ra } => {
                    let v = self.do_return(ra, false, stop)?;
                    if let Some(v) = v {
                        return Ok(v);
                    }
                }
                Op::RetEarly { ra } => {
                    // Check fault handler before returning.
                    if let Some(handler_pc) = self.stack.last_mut().and_then(|f| f.fault_pc.take()) {
                        if let Some(frame) = self.stack.last_mut() {
                            frame.fault_reg = Some(ra);
                        }
                        self.stack.last_mut().unwrap().pc = handler_pc;
                    } else {
                        // Set early_return flag so wrap_ok treats this as a
                        // `?` rejection (not a final-expression Err value).
                        if let Some(frame) = self.stack.last_mut() {
                            frame.early_return = true;
                        }
                        let v = self.do_return(ra, true, stop)?;
                        if let Some(v) = v {
                            return Ok(v);
                        }
                    }
                }
                Op::RetUnit => {
                    let frame = self.stack.last().unwrap();
                    let return_reg = frame.return_reg;
                    let wrap_ok = frame.wrap_ok;
                    let v = if wrap_ok {
                        Value::Variant("Ok".to_string(), vec![Value::Unit])
                    } else {
                        Value::Unit
                    };
                    self.stack.pop();
                    self.depth -= 1;
                    if self.stack.is_empty() || (stop > 0 && self.depth < stop) {
                        return Ok(v);
                    }
                    if let Some(rd) = return_reg {
                        self.set_reg(rd, v);
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
                    let idx_raw = self.get_int(rb)?;
                    // Borrow the collection, extract only the element (avoid cloning entire list).
                    let v = match self.get_reg(ra) {
                        Value::List(l) => {
                            let idx = if idx_raw < 0 {
                                let wrapped = l.len() as i64 + idx_raw;
                                if wrapped < 0 {
                                    return Err(InterpError::new(format!(
                                        "index {} out of bounds (len {})",
                                        idx_raw, l.len()
                                    )));
                                }
                                wrapped as usize
                            } else {
                                idx_raw as usize
                            };
                            if idx >= l.len() {
                                return Err(InterpError::new(format!(
                                    "index {} out of bounds (len {})",
                                    idx_raw,
                                    l.len()
                                )));
                            }
                            l[idx].clone()
                        }
                        Value::String(s) => {
                            let bytes = s.as_bytes();
                            let idx = if idx_raw < 0 {
                                let wrapped = bytes.len() as i64 + idx_raw;
                                if wrapped < 0 {
                                    return Err(InterpError::new(format!(
                                        "string index {} out of bounds (len {})",
                                        idx_raw, bytes.len()
                                    )));
                                }
                                wrapped as usize
                            } else {
                                idx_raw as usize
                            };
                            if idx >= bytes.len() {
                                return Err(InterpError::new(format!(
                                    "string index {} out of bounds (len {})",
                                    idx_raw,
                                    bytes.len()
                                )));
                            }
                            Value::Int(bytes[idx] as i64)
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "index: expected List or String, got {}",
                                other
                            )))
                        }
                    };
                    self.set_reg(rd, v);
                }
                Op::ListSet { ra, rb, rc } => {
                    let idx_raw = self.get_int(rb)?;
                    if idx_raw < 0 {
                        return Err(InterpError::new(format!(
                            "negative index {} out of bounds",
                            idx_raw
                        )));
                    }
                    let idx = idx_raw as usize;
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

                // ── Record ─────────────────────────────────────
                Op::NewRecord {
                    rd,
                    type_name,
                    base,
                    count,
                } => {
                    let type_name_str = match &proto.constants.get(type_name as usize) {
                        Some(ConstValue::Str(s)) => {
                            if s.is_empty() { None } else { Some(s.clone()) }
                        }
                        _ => None,
                    };
                    // Field names are stored in constants[type_name+1..type_name+1+count].
                    let mut fields = std::collections::HashMap::new();
                    for i in 0..count {
                        let idx = (type_name + 1 + i as u32) as usize;
                        let field_name = match proto.constants.get(idx) {
                            Some(ConstValue::Str(s)) => s.clone(),
                            _ => {
                                if idx < proto.constants.len() {
                                    format!("_{}", i)
                                } else {
                                    return Err(InterpError::new(format!(
                                        "NewRecord: field constant {} out of bounds (len {})",
                                        idx, proto.constants.len()
                                    )));
                                }
                            }
                        };
                        let value = self.get_reg(base + i).clone();
                        fields.insert(field_name, value);
                    }
                    self.set_reg(rd, Value::Record(type_name_str, fields));
                }
                Op::RecordGet { rd, ra, field } => {
                    let field_name = match &proto.constants[field as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => String::new(),
                    };
                    // Borrow record, extract only the field value (avoid cloning entire record).
                    let v = match self.get_reg(ra) {
                        Value::Record(_, fields) => {
                            fields.get(&field_name).cloned().ok_or_else(|| {
                                InterpError::new(format!("record has no field '{}'", field_name))
                            })?
                        }
                        Value::Actor(handle) => {
                            let actor = handle.inner.read().map_err(|e| {
                                InterpError::new(format!("actor lock failed: {}", e))
                            })?;
                            actor.fields.get(&field_name).cloned().ok_or_else(|| {
                                InterpError::new(format!("actor has no field '{}'", field_name))
                            })?
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "record get: expected Record or Actor, got {}",
                                other
                            )))
                        }
                    };
                    self.set_reg(rd, v);
                }
                Op::RecordSet { ra, field, rb } => {
                    let field_name = match &proto.constants[field as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => String::new(),
                    };
                    let value = self.get_reg(rb).clone();
                    let record = self.get_reg_mut(ra);
                    match record {
                        Value::Record(_, fields) => {
                            fields.insert(field_name, value);
                        }
                        Value::Actor(handle) => {
                            handle.inner.write().map_err(|e| {
                                InterpError::new(format!("actor lock failed: {}", e))
                            })?.fields.insert(field_name, value);
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "record set: expected Record or Actor, got {}",
                                other
                            )))
                        }
                    }
                }

                // ── Map / Set ────────────────────────────────
                Op::NewMap { rd } => {
                    self.set_reg(rd, Value::Record(None, std::collections::HashMap::new()));
                }
                Op::NewSet { rd } => {
                    self.set_reg(rd, Value::Set(Vec::new()));
                }
                Op::MapGet { rd, ra, rb } => {
                    let key = self.get_reg(rb).clone();
                    // Borrow map, extract only the value (avoid cloning entire map).
                    let v = match (self.get_reg(ra), &key) {
                        (Value::Record(_, fields), Value::String(k)) => {
                            fields.get(k).cloned().unwrap_or(Value::Unit)
                        }
                        _ => {
                            return Err(InterpError::new(
                                "map_get: expected (Map, String key)",
                            ))
                        }
                    };
                    self.set_reg(rd, v);
                }
                Op::MapSet { ra, rb, rc } => {
                    let key = self.get_reg(rb).clone();
                    let val = self.get_reg(rc).clone();
                    let map = self.get_reg_mut(ra);
                    match (map, &key) {
                        (Value::Record(_, fields), Value::String(k)) => {
                            fields.insert(k.clone(), val);
                        }
                        _ => {
                            return Err(InterpError::new(
                                "map_set: expected (Map, String key)",
                            ))
                        }
                    }
                }
                Op::MapContains { rd, ra, rb } => {
                    let key = self.get_reg(rb).clone();
                    let contains = match (self.get_reg(ra), &key) {
                        (Value::Record(_, fields), Value::String(k)) => {
                            fields.contains_key(k)
                        }
                        _ => {
                            return Err(InterpError::new(
                                "map_contains: expected (Map, String key)",
                            ))
                        }
                    };
                    self.set_reg(rd, Value::Bool(contains));
                }
                Op::SetAdd { ra, rb } => {
                    let val = self.get_reg(rb).clone();
                    let set = self.get_reg_mut(ra);
                    match set {
                        Value::Set(s) => {
                            if !s.contains(&val) {
                                s.push(val);
                            }
                        }
                        _ => {
                            return Err(InterpError::new("set_add: expected Set"))
                        }
                    }
                }
                Op::SetContains { rd, ra, rb } => {
                    let set = self.get_reg(ra).clone();
                    let val = self.get_reg(rb).clone();
                    match &set {
                        Value::Set(s) => {
                            self.set_reg(rd, Value::Bool(s.contains(&val)));
                        }
                        _ => {
                            return Err(InterpError::new("set_contains: expected Set"))
                        }
                    }
                }

                // ── Closures ───────────────────────────────────
                Op::NewClosure {
                    rd,
                    proto: proto_idx,
                    captures_base,
                    capture_count,
                } => {
                    // Collect captured variables by name.
                    let target_proto = &self.program.functions[proto_idx as usize];
                    let mut captured = std::collections::HashMap::new();
                    for i in 0..capture_count {
                        let reg = captures_base + i;
                        let value = self.get_reg(reg).clone();
                        // Use the capture name from the proto, or fall back to index.
                        let name = target_proto
                            .capture_names
                            .get(i as usize)
                            .cloned()
                            .unwrap_or_else(|| format!("_capture_{}", i));
                        captured.insert(name, value);
                    }
                    self.set_reg(
                        rd,
                        Value::BytecodeClosure {
                            proto: proto_idx,
                            captured,
                        },
                    );
                }
                Op::CallIndirect {
                    rd,
                    callee,
                    args_base,
                    argc,
                } => {
                    let closure = self.get_reg(callee).clone();
                    match closure {
                        Value::BytecodeClosure { proto: proto_idx, captured } => {
                            // Collect arguments.
                            let args: Vec<Value> = (0..argc)
                                .map(|i| self.get_reg(args_base + i).clone())
                                .collect();

                            // Push a new frame for the closure.
                            self.push_frame(proto_idx, args, Some(rd))?;

                            // Bind captured variables in the new frame.
                            // Captures go into registers param_count..param_count+capture_count.
                            let target_proto = &self.program.functions[proto_idx as usize];
                            let param_count = target_proto.param_count as usize;
                            let frame_len = self.stack.last().map(|f| f.regs.len()).unwrap_or(0);
                            for (i, name) in target_proto.capture_names.iter().enumerate() {
                                let reg = param_count + i;
                                let value = captured.get(name).ok_or_else(|| {
                                    InterpError::new(format!(
                                        "CallIndirect: missing capture '{}'", name
                                    ))
                                })?.clone();
                                if reg >= frame_len {
                                    return Err(InterpError::new(format!(
                                        "CallIndirect: capture register {} out of bounds (len {})",
                                        reg, frame_len
                                    )));
                                }
                                if let Some(frame) = self.stack.last_mut() {
                                    frame.regs[reg] = value;
                                }
                            }
                            // Continue loop — new frame is now active.
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "call indirect: expected BytecodeClosure, got {}",
                                other
                            )))
                        }
                    }
                }

                // ── Variant (enum) ─────────────────────────────
                Op::NewVariant {
                    rd,
                    type_name,
                    variant,
                    base,
                    arity,
                } => {
                    let tag = match &proto.constants[type_name as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => {
                            // Fall back to generic variant label for
                            // multi-variant enums without per-variant labels.
                            format!("variant_{}", variant)
                        }
                    };
                    let payload: Vec<Value> = (0..arity)
                        .map(|i| self.get_reg(base + i).clone())
                        .collect();
                    self.set_reg(rd, Value::Variant(tag, payload));
                }
                Op::IsVariant { rd, ra, tag } => {
                    let v = self.get_reg(ra);
                    let expected_tag = match &proto.constants[tag as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => String::new(),
                    };
                    let matches = match v {
                        Value::Variant(name, _) => name == &expected_tag,
                        _ => false,
                    };
                    self.set_reg(rd, Value::Bool(matches));
                }
                Op::VariantGet { rd, ra, idx } => {
                    let v = self.get_reg(ra).clone();
                    match v {
                        Value::Variant(_, fields) => {
                            if (idx as usize) >= fields.len() {
                                return Err(InterpError::new(format!(
                                    "variant field index {} out of bounds (arity {})",
                                    idx,
                                    fields.len()
                                )));
                            }
                            let elem = fields[idx as usize].clone();
                            self.set_reg(rd, elem);
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "variant get: expected Variant, got {}",
                                other
                            )))
                        }
                    }
                }
                Op::VariantTag { rd, ra } => {
                    let v = self.get_reg(ra);
                    match v {
                        Value::Variant(name, _) => {
                            // Return tag as a string (for comparison).
                            self.set_reg(rd, Value::String(name.clone()));
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "variant tag: expected Variant, got {}",
                                other
                            )))
                        }
                    }
                }
                Op::VariantPayload { rd, ra, idx } => {
                    let v = self.get_reg(ra).clone();
                    match v {
                        Value::Variant(_, fields) => {
                            if (idx as usize) >= fields.len() {
                                return Err(InterpError::new(format!(
                                    "variant payload index {} out of bounds (arity {})",
                                    idx,
                                    fields.len()
                                )));
                            }
                            let elem = fields[idx as usize].clone();
                            self.set_reg(rd, elem);
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "variant payload: expected Variant, got {}",
                                other
                            )))
                        }
                    }
                }

                // ── Option / Result (Variant encoding — matches tree-walker) ──
                Op::Some { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    self.set_reg(rd, Value::Variant("Some".into(), vec![v]));
                }
                Op::None { rd } => {
                    self.set_reg(rd, Value::Variant("None".into(), vec![]));
                }
                Op::Ok { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    self.set_reg(rd, Value::Variant("Ok".into(), vec![v]));
                }
                Op::Err { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    self.set_reg(rd, Value::Variant("Err".into(), vec![v]));
                }
                Op::IsSome { rd, ra } => {
                    let v = self.get_reg(ra);
                    let is_some =
                        matches!(v, Value::Variant(name, _) if name == "Some" || name == "Ok");
                    self.set_reg(rd, Value::Bool(is_some));
                }
                Op::Unwrap { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    match v {
                        Value::Variant(name, payload) if name == "Some" || name == "Ok" => {
                            let inner = payload.into_iter().next().unwrap_or(Value::Unit);
                            self.set_reg(rd, inner);
                        }
                        Value::Variant(name, _) if name == "None" => {
                            return Err(InterpError::new("unwrap called on None"));
                        }
                        Value::Variant(name, _) if name == "Err" => {
                            return Err(InterpError::new("unwrap called on Err"));
                        }
                        _ => {
                            return Err(InterpError::new(
                                "unwrap: expected Option or Result variant",
                            ));
                        }
                    }
                }

                // ── Misc ───────────────────────────────────────
                Op::ToString { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    self.set_reg(rd, Value::String(v.to_string()));
                }
                Op::Cast { rd, ra, target } => {
                    let v = self.get_reg(ra).clone();
                    // target: 0 = i64, 1 = f64 (matching compiler convention)
                    let result = match target {
                        0 => match v {
                            Value::Int(i) => Value::Int(i),
                            Value::Float(f) => Value::Int(f as i64),
                            Value::Bool(b) => Value::Int(b as i64),
                            other => return Err(InterpError::new(format!(
                                "cannot cast {} to i64", other
                            ))),
                        },
                        1 => match v {
                            Value::Float(f) => Value::Float(f),
                            Value::Int(i) => Value::Float(i as f64),
                            other => return Err(InterpError::new(format!(
                                "cannot cast {} to f64", other
                            ))),
                        },
                        _ => return Err(InterpError::new(format!(
                            "Cast: unknown target {}",
                            target
                        ))),
                    };
                    self.set_reg(rd, result);
                }
                Op::TypeOf { rd, ra } => {
                    let v = self.get_reg(ra);
                    let name = crate::interp::type_name(v);
                    self.set_reg(rd, Value::String(name.to_string()));
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

                // ── Actor / Flow / Session (Phase D) ──────────

                Op::ActorSpawn { rd, actor } => {
                    let proto = &self.program.functions
                        [self.stack.last().unwrap().proto_idx as usize];
                    let actor_name = match &proto.constants[actor as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => return Err(InterpError::new("ActorSpawn: invalid actor name")),
                    };
                    let val = self.spawn_actor(&actor_name)?;
                    self.set_reg(rd, val);
                }

                Op::FlowTransition { rd, flow, method, args_base, argc } => {
                    let proto = &self.program.functions
                        [self.stack.last().unwrap().proto_idx as usize];
                    let flow_name = match &proto.constants[flow as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => return Err(InterpError::new("FlowTransition: invalid flow name")),
                    };
                    let method_name = match &proto.constants[method as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => return Err(InterpError::new("FlowTransition: invalid method name")),
                    };
                    // Extract from-state name from the first argument.
                    let from_state = match self.get_reg(args_base) {
                        Value::Record(Some(name), _) => name.clone(),
                        other => return Err(InterpError::new(format!(
                            "FlowTransition: first arg must be a state Record, got {}",
                            other
                        ))),
                    };
                    // Look up the compiled transition function.
                    let key = (flow_name.clone(), method_name.clone(), from_state.clone());
                    let func_idx = self.program.flow_transition_funcs.get(&key)
                        .copied()
                        .ok_or_else(|| InterpError::new(format!(
                            "no transition {}::{} from state {}",
                            flow_name, method_name, from_state
                        )))?;
                    // Collect args.
                    let args: Vec<Value> = (0..argc)
                        .map(|i| self.get_reg(args_base + i).clone())
                        .collect();
                    // Call the transition function.
                    // If the transition has a `fails` clause, wrap the result:
                    // success → Ok(result), failure → Err((source, error)).
                    if self.program.flow_fails_transitions.contains(&key) {
                        let state_val = self.get_reg(args_base).clone();
                        self.push_frame_wrap_ok(func_idx, args, Some(rd), state_val)?;
                    } else {
                        self.push_frame(func_idx, args, Some(rd))?;
                    }
                }

                Op::DynMethodCall { rd, method, args_base, argc } => {
                    let proto = &self.program.functions
                        [self.stack.last().unwrap().proto_idx as usize];
                    let method_name = match &proto.constants[method as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => return Err(InterpError::new("DynMethodCall: invalid method name")),
                    };
                    let receiver = self.get_reg(args_base).clone();
                    match &receiver {
                        Value::Actor(handle) => {
                            // Self-call detection: if the current thread is this
                            // actor's worker, call directly to avoid mailbox
                            // deadlock (same pattern as tree-walker call.rs:775).
                            let is_self_call =
                                crate::interp::value::ActorHandle::current_worker_id() == handle.id;

                            if is_self_call {
                                // Direct synchronous call: push frame and let
                                // the current exec_loop handle it (same pattern
                                // as Op::Call), avoiding nested exec_loop re-entry.
                                let actor_name = {
                                    let actor = handle.inner.read().map_err(|e| {
                                        InterpError::new(format!("actor lock failed: {}", e))
                                    })?;
                                    actor.actor_name.clone()
                                };
                                let key = (actor_name, method_name.clone());
                                let func_idx = self.program.actor_method_funcs.get(&key)
                                    .copied()
                                    .ok_or_else(|| InterpError::new(format!(
                                        "self-call: actor method '{}' not found", method_name
                                    )))?;
                                let direct_args: Vec<Value> = (0..argc)
                                    .map(|i| self.get_reg(args_base + i).clone())
                                    .collect();
                                self.push_frame(func_idx, direct_args, Some(rd))
                                    .map_err(|e| self.enrich_error(e))?;
                                // Continue loop — new frame is now active.
                            } else {
                                // Cross-actor call: enqueue and wait for result.
                                let args: Vec<Value> = (1..argc)
                                    .map(|i| self.get_reg(args_base + i).clone())
                                    .collect();
                                let rx = handle.try_enqueue(method_name, args)?;
                                let result = rx.recv().map_err(|_| {
                                    InterpError::new("actor worker thread died")
                                })??;
                                self.set_reg(rd, result);
                            }
                        }
                        Value::WeakShared(weak) if method_name == "upgrade" => {
                            match weak.upgrade() {
                                Some(arc) => self.set_reg(rd, Value::Shared(arc)),
                                None => self.set_reg(rd, Value::Variant("None".to_string(), vec![])),
                            }
                        }
                        Value::Shared(arc) if method_name == "deref" => {
                            let inner = arc.read().map_err(|e| {
                                InterpError::new(format!("shared read lock failed: {}", e))
                            })?;
                            self.set_reg(rd, inner.clone());
                        }
                        _ => {
                            return Err(InterpError::new(format!(
                                "cannot call method '{}' on {}",
                                method_name, receiver
                            )));
                        }
                    }
                }

                Op::SharedNew { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    let shared = match v {
                        Value::Shared(arc) => Value::Shared(std::sync::Arc::clone(&arc)),
                        other => Value::Shared(std::sync::Arc::new(std::sync::RwLock::new(other))),
                    };
                    self.set_reg(rd, shared);
                }

                Op::WeakNew { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    let weak = match v {
                        Value::Shared(arc) => Value::WeakShared(std::sync::Arc::downgrade(&arc)),
                        _ => return Err(InterpError::new(format!(
                            "weak requires a shared value, got {}", v
                        ))),
                    };
                    self.set_reg(rd, weak);
                }

                // ── Fault handling ──────────────────────────────
                Op::SetFaultPc { handler_pc } => {
                    if let Some(frame) = self.stack.last_mut() {
                        frame.fault_pc = Some(handler_pc as usize);
                    }
                }
                Op::ClearFaultPc => {
                    if let Some(frame) = self.stack.last_mut() {
                        frame.fault_pc = None;
                    }
                }
                Op::FaultRetEarly => {
                    let ra = self.stack.last().and_then(|f| f.fault_reg);
                    if let Some(ra) = ra {
                        if let Some(frame) = self.stack.last_mut() {
                            frame.early_return = true;
                        }
                        let v = self.do_return(ra, true, stop)?;
                        if let Some(v) = v {
                            return Ok(v);
                        }
                    } else {
                        return Err(InterpError::new(
                            "FaultRetEarly: no fault_reg set"
                        ));
                    }
                }

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

    // ── Closure call helper ──────────────────────────────────

    /// Call a closure synchronously and return the result.
    /// Used by higher-order builtins (map_list/filter_list/reduce_list/any/all).
    ///
    /// Pushes a closure frame, sets stop_depth, and delegates to exec_loop.
    /// This ensures closures can call user functions, other closures, builtins —
    /// anything the main loop supports. No separate dispatch.
    pub(crate) fn call_closure(&mut self, closure: &Value, args: &[Value]) -> Result<Value, InterpError> {
        match closure {
            Value::BytecodeClosure {
                proto: proto_idx,
                captured,
            } => {
                // Push a new frame for the closure.
                self.push_frame(*proto_idx, args.to_vec(), None)?;

                // Bind captured variables in the new frame.
                let target_proto = &self.program.functions[*proto_idx as usize];
                let param_count = target_proto.param_count as usize;
                for (i, name) in target_proto.capture_names.iter().enumerate() {
                    if let Some(value) = captured.get(name) {
                        let reg = param_count + i;
                        if let Some(frame) = self.stack.last_mut() {
                            if reg < frame.regs.len() {
                                frame.regs[reg] = value.clone();
                            }
                        }
                    }
                }

                // Set stop_depth so exec_loop returns when this frame pops.
                let prev_stop = self.stop_depth;
                self.stop_depth = self.depth; // depth was incremented by push_frame
                let result = self.exec_loop();
                self.stop_depth = prev_stop;
                result.map_err(|e| self.enrich_error(e))
            }
            _ => Err(InterpError::new(
                "call_closure: expected BytecodeClosure",
            )),
        }
    }

    /// Call a function by index with the given arguments.
    /// Used by actor worker threads to execute actor methods.
    pub fn call_function(&mut self, func_idx: FuncIdx, args: &[Value]) -> Result<Value, InterpError> {
        self.push_frame(func_idx, args.to_vec(), None)?;
        let prev_stop = self.stop_depth;
        self.stop_depth = self.depth;
        let result = self.exec_loop();
        self.stop_depth = prev_stop;
        result.map_err(|e| self.enrich_error(e))
    }

    /// Call a function with wrap_ok semantics (for `fails` transitions).
    /// On return, Op::Ret wraps the value in Ok/Err Variant matching
    /// the tree-walker's eval_flow_transition return convention.
    pub fn call_function_wrap_ok(
        &mut self,
        func_idx: FuncIdx,
        args: &[Value],
        source_state: Value,
    ) -> Result<Value, InterpError> {
        self.push_frame_wrap_ok(func_idx, args.to_vec(), None, source_state)?;
        let prev_stop = self.stop_depth;
        self.stop_depth = self.depth;
        let result = self.exec_loop();
        self.stop_depth = prev_stop;
        result
    }

    /// Shared return path for Op::Ret and Op::RetEarly.
    /// Returns Ok(Some(v)) if execution should stop (empty stack or stop_depth),
    /// Ok(None) if execution should continue (caller frame can receive the value).
    fn do_return(
        &mut self,
        ra: Reg,
        is_early_return: bool,
        stop: usize,
    ) -> Result<Option<Value>, InterpError> {
        let mut v = self.get_reg(ra).clone();
        let frame = self.stack.last().unwrap();
        let return_reg = frame.return_reg;
        let wrap_ok = frame.wrap_ok;
        let source_state = frame.flow_source_state.clone();
        if wrap_ok {
            if is_early_return {
                // `?` triggered rejection: unwrap Err(x) → Err((source, x))
                let error = match &v {
                    Value::Variant(_, payload) => payload.first().cloned().unwrap_or(Value::Unit),
                    other => other.clone(),
                };
                let src = source_state.unwrap_or(Value::Unit);
                v = Value::Variant("Err".to_string(), vec![
                    Value::Tuple(vec![src, error]),
                ]);
            } else {
                v = Value::Variant("Ok".to_string(), vec![v]);
            }
        }
        self.stack.pop();
        self.depth -= 1;
        if self.stack.is_empty() || (stop > 0 && self.depth < stop) {
            return Ok(Some(v));
        }
        if let Some(rd) = return_reg {
            self.set_reg(rd, v);
        }
        Ok(None)
    }

    // ── Builtin dispatch (D1: registry, not giant match) ─────

    fn call_builtin(
        &mut self,
        idx: BuiltinIdx,
        args: &[Value],
    ) -> Result<Value, InterpError> {
        let (func, arity, name) = self.registry.get_func(idx);
        if arity != usize::MAX && args.len() != arity {
            return Err(InterpError::new(format!(
                "{} expects {} argument(s), got {}",
                name,
                arity,
                args.len()
            )));
        }
        func(self, args)
    }

    /// Append to captured stdout (used by builtin io functions).
    pub fn append_stdout(&mut self, s: &str) {
        self.stdout.push_str(s);
    }

    /// Get captured stdout (for testing).
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    // ── Register access helpers (D8: centralized Value conversion) ──

    pub(crate) fn get_reg(&self, r: Reg) -> &Value {
        let frame = self.stack.last().unwrap();
        let idx = r as usize;
        if idx >= frame.regs.len() {
            panic!("bytecode: register {} out of bounds (len {})", idx, frame.regs.len());
        }
        &frame.regs[idx]
    }

    pub(crate) fn get_reg_mut(&mut self, r: Reg) -> &mut Value {
        let len = self.stack.last().unwrap().regs.len();
        let idx = r as usize;
        if idx >= len {
            panic!("bytecode: register {} out of bounds (len {})", idx, len);
        }
        &mut self.stack.last_mut().unwrap().regs[idx]
    }

    pub(crate) fn set_reg(&mut self, r: Reg, v: Value) {
        let idx = r as usize;
        let len = self.stack.last().unwrap().regs.len();
        if idx >= len {
            panic!("bytecode: register {} out of bounds (len {})", idx, len);
        }
        self.stack.last_mut().unwrap().regs[idx] = v;
    }

    pub(crate) fn get_int(&self, r: Reg) -> Result<i64, InterpError> {
        match self.get_reg(r) {
            Value::Int(v) => Ok(*v),
            other => Err(InterpError::new(format!(
                "expected Int, got {}",
                other
            ))),
        }
    }

    pub(crate) fn get_int2(&self, ra: Reg, rb: Reg) -> Result<(i64, i64), InterpError> {
        Ok((self.get_int(ra)?, self.get_int(rb)?))
    }

    pub(crate) fn get_float(&self, r: Reg) -> Result<f64, InterpError> {
        match self.get_reg(r) {
            Value::Float(v) => Ok(*v),
            other => Err(InterpError::new(format!(
                "expected Float, got {}",
                other
            ))),
        }
    }

    pub(crate) fn get_float2(&self, ra: Reg, rb: Reg) -> Result<(f64, f64), InterpError> {
        Ok((self.get_float(ra)?, self.get_float(rb)?))
    }

    pub(crate) fn get_bool(&self, r: Reg) -> Result<bool, InterpError> {
        match self.get_reg(r) {
            Value::Bool(v) => Ok(*v),
            other => Err(InterpError::new(format!(
                "expected Bool, got {}",
                other
            ))),
        }
    }

    pub(crate) fn get_str(&self, r: Reg) -> Result<String, InterpError> {
        match self.get_reg(r) {
            Value::String(v) => Ok(v.clone()),
            other => Err(InterpError::new(format!(
                "expected String, got {}",
                other
            ))),
        }
    }

    pub(crate) fn get_list(&self, r: Reg) -> Result<Vec<Value>, InterpError> {
        match self.get_reg(r) {
            Value::List(v) => Ok(v.clone()),
            other => Err(InterpError::new(format!(
                "expected List, got {}",
                other
            ))),
        }
    }

    pub(crate) fn check_float(&self, v: f64, op: &str) -> Result<(), InterpError> {
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

    // ── Actor spawn helper ───────────────────────────────────

    /// Spawn an actor by name, reusing the ActorHandle infrastructure.
    /// The actor's worker thread uses BytecodeVM internally (v0.33 migration).
    /// Main program and actor workers both run on bytecode.
    pub(crate) fn spawn_actor(&mut self, actor_name: &str) -> Result<Value, InterpError> {
        use crate::interp::value::{ActorInstance, ActorHandle};
        use std::collections::HashMap;

        // Check spawn quota.
        if let Some(max) = self.max_children {
            if self.spawn_count >= max {
                return Err(InterpError::new(
                    "QuotaExceeded: spawn would exceed @max_children limit",
                ));
            }
        }

        let actor_def = self.program.actor_defs.get(actor_name)
            .ok_or_else(|| InterpError::new(format!("actor '{}' not found", actor_name)))?;

        // Initialize fields with defaults.
        let mut fields = HashMap::new();
        for field in &actor_def.fields {
            let value = match field.ty.unlocated() {
                crate::ast::Type::Name(n, _) if n == "i32" || n == "i64" => Value::Int(0),
                crate::ast::Type::Name(n, _) if n == "f64" => Value::Float(0.0),
                crate::ast::Type::Name(n, _) if n == "bool" => Value::Bool(false),
                crate::ast::Type::Name(n, _) if n == "string" => Value::String(String::new()),
                _ => Value::Unit,
            };
            // If there's an init expression that's a simple literal, evaluate it.
            let value = if let Some(init) = &field.init {
                match init.unlocated() {
                    crate::ast::Expr::Literal(crate::ast::Lit::Int(n)) => Value::Int(*n),
                    crate::ast::Expr::Literal(crate::ast::Lit::Float(f)) => Value::Float(*f),
                    crate::ast::Expr::Literal(crate::ast::Lit::Bool(b)) => Value::Bool(*b),
                    crate::ast::Expr::Literal(crate::ast::Lit::String(s)) => Value::String(s.clone()),
                    _ => value, // complex init: use default (tree-walker eval not available)
                }
            } else {
                value
            };
            fields.insert(field.name.clone(), value);
        }

        // Initialize flow_state for actors that run a flow.
        let flow_state = if let Some(flow_name) = &actor_def.runs_flow {
            self.program.flow_defs.get(flow_name).and_then(|flow| {
                flow.states.first().map(|root_state| {
                    let fields: HashMap<String, Value> = root_state
                        .payload
                        .as_ref()
                        .map(|payload| {
                            payload
                                .iter()
                                .map(|f| {
                                    let default_val = match f.ty.unlocated() {
                                        crate::ast::Type::Name(n, _) if n == "i32" || n == "i64" => Value::Int(0),
                                        crate::ast::Type::Name(n, _) if n == "f64" => Value::Float(0.0),
                                        crate::ast::Type::Name(n, _) if n == "bool" => Value::Bool(false),
                                        crate::ast::Type::Name(n, _) if n == "string" => Value::String(String::new()),
                                        _ => Value::Unit,
                                    };
                                    (f.name.clone(), default_val)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Value::Record(Some(root_state.name.clone()), fields)
                })
            })
        } else {
            None
        };

        let instance = ActorInstance {
            actor_name: actor_name.to_string(),
            fields,
            methods: actor_def.methods.clone(),
            runs_flow: actor_def.runs_flow.clone(),
            flow_state,
            faulted: false,
            peer_links: Vec::new(),
            parent_id: crate::interp::value::CURRENT_ACTOR_ID.with(|id| {
                let id = id.get();
                if id == 0 { None } else { Some(id) }
            }),
            is_detached: false,
            producers: Vec::new(),
        };

        let program = self.program.ast.clone().unwrap_or_else(|| {
            std::sync::Arc::new(crate::ast::File {
                sources: crate::span::SourceRegistry::default(),
                imports: Vec::new(),
                items: Vec::new(),
                implicit_single: false,
            })
        });
        let bc_prog = std::sync::Arc::new(self.program.clone());
        let handle = ActorHandle::new_bytecode(instance, program, bc_prog);
        self.spawn_count += 1;
        Ok(Value::Actor(handle))
    }
}

/// Convert a Value to f64 (for runtime int/float dispatch fallback).
fn value_to_f64(v: &Value) -> Result<f64, InterpError> {
    match v {
        Value::Float(f) => Ok(*f),
        Value::Int(i) => Ok(*i as f64),
        other => Err(InterpError::new(format!(
            "value_to_f64: expected numeric type, got {:?}", other
        ))),
    }
}
