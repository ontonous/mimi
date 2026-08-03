//! Register-based bytecode virtual machine for Mimi.
//!
//! Execution model:
//! - Each function call creates a Frame with a register file (Vec<Value>)
//! - Instructions operate on registers by index
//! - The VM dispatch loop is a single `match` on Op — no AST walking
//! - Arithmetic is typed (AddInt vs AddFloat) — zero runtime type dispatch

use super::instr::*;
use super::registry::{self, BuiltinRegistry};
use crate::ast::Lit;
use crate::ffi::FfiContract;
use crate::interp::error::InterpError;
use crate::interp::ffi_runtime::{FfiClosureRunner, FfiRuntime};
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
    /// Caller registers to write back `mut` parameter values to after the
    /// callee returns. One entry per callee mut_param_indices entry, in the
    /// same order (set by Op::MutateSetup).
    mutate_writebacks: Option<Vec<Reg>>,
    /// v0.34.13 (clause 6): record-FIELD writeback targets for payload
    /// member-level `mutate self.field` borrow. Each entry = (obj_reg,
    /// field_name); on callee Ret the final parameter value is RecordSet
    /// into caller.regs[obj_reg] at that field. Set by Op::MutateSetupField.
    /// `mutate_writebacks` (register targets) and this are mutually
    /// exclusive — a given call uses one or the other.
    mutate_field_writebacks: Option<Vec<(Reg, String)>>,
    /// Flow-transition context for this frame (None for ordinary calls).
    /// Used to absorb runtime panics into a Fault value (v0.29.12).
    flow_tx: Option<FlowTxCtx>,
    /// Pre-call parameter snapshots for `old(x)` in ensures contracts.
    /// Only populated when the function has_ensures (avoids allocation otherwise).
    old_snapshots: Vec<Value>,
    /// v0.34.10a (SD-9, H2 fix): per-frame `ieee_float { }` nesting depth.
    /// When > 0, float arithmetic + builtins in THIS frame skip the finiteness
    /// trap (NaN/Inf allowed, IEEE 754). Per-frame (not VM-global) so an early
    /// `return` inside an ieee block — whose IeeeExit is unreachable — cannot
    /// leak the suspended-trap state into the caller or later calls. Mirrors
    /// codegen, where ieee_depth is a per-function compile-time field.
    ieee_depth: usize,
}

/// Context captured when a flow transition frame is entered. Used to
/// convert runtime panics inside the transition body into a Fault value
/// (with persistent-field shadowing; @transactional removed v0.34.1).
struct FlowTxCtx {
    /// Flow name (for diagnostics / persistent lookups).
    flow_name: String,
    /// From-state name (becomes Fault.last_state).
    from_state: String,
    /// The from-state payload as passed (pre-transition).
    from_payload: Value,
    /// Persistent field names declared on the flow.
    persistent_fields: Vec<String>,
}

/// The bytecode VM.
pub struct BytecodeVM<'a> {
    /// The compiled program.
    program: &'a BytecodeProgram,
    /// Call stack of frames.
    stack: Vec<Frame>,
    /// Captured stdout output (for testing).
    stdout: String,
    /// Shared stdout capture buffer (for actor worker threads).
    /// When set, append_stdout writes to this buffer instead of (or in addition to)
    /// the local stdout field. Actor workers receive the spawning VM's buffer.
    stdout_capture: Option<std::sync::Arc<std::sync::Mutex<String>>>,
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
    /// Reusable register buffers (frame regs Vec capacity is preserved across
    /// calls, so deep recursion does not re-malloc per frame).
    free_regs: Vec<Vec<Value>>,
    /// Shared FFI execution context (0.33 Phase D FFI forwarding): extern
    /// function tables, loaded shared libraries, contract verification.
    ffi_runtime: FfiRuntime,
    /// Quote assembly stack (0.33 Phase F): nodes pushed by Quote* ops.
    quote_stack: Vec<crate::interp::value::QuotedAst>,
    /// Variable captures collected by QuoteCapture, consumed by ast_eval.
    pub(crate) quote_captures: std::collections::HashMap<String, Value>,
    /// Runtime contract checking (requires/ensures). Default true (tree-walker parity).
    pub verify_contracts: bool,
}

const MAX_DEPTH: usize = 768;

/// Read a register value as f64 (Float directly, Int widened). Used by the
/// float arithmetic ops to release the frame borrow before calling the
/// `&self` method `check_float` (H1 fix).
fn reg_as_f64(v: &Value) -> Result<f64, InterpError> {
    match v {
        Value::Float(f) => Ok(*f),
        Value::Int(i) => Ok(*i as f64),
        other => Err(InterpError::new(format!("expected Float, got {}", other))),
    }
}

impl<'a> BytecodeVM<'a> {
    pub fn new(program: &'a BytecodeProgram) -> Self {
        BytecodeVM {
            program,
            stack: Vec::with_capacity(64),
            stdout: String::new(),
            stdout_capture: None,
            depth: 0,
            stop_depth: 0,
            registry: registry::create_registry(),
            max_children: program.max_children,
            spawn_count: 0,
            cli_args: Vec::new(),
            exit_requested: None,
            free_regs: Vec::new(),
            // Shared FFI execution context (0.33 Phase D FFI forwarding).
            // Built from the program AST when available (the compiler always
            // stores it); hand-assembled test programs fall back to empty
            // tables. Contract verification is disabled until the bytecode
            // engine implements contract-expression eval (see
            // FfiClosureRunner::eval_contract_expr).
            ffi_runtime: match program.ast.as_ref() {
                Some(file) => {
                    let mut rt = FfiRuntime::from_file(file);
                    rt.verify_ffi = false;
                    rt
                }
                None => FfiRuntime::from_parts(
                    std::collections::HashMap::new(),
                    std::collections::HashMap::new(),
                    std::collections::HashMap::new(),
                ),
            },
            quote_stack: Vec::new(),
            quote_captures: std::collections::HashMap::new(),
            verify_contracts: true,
        }
    }

    /// Access the compiled program (for builtins that need type info).
    pub fn program(&self) -> &'a BytecodeProgram {
        self.program
    }

    /// Request the VM to terminate with the given exit code.
    /// Called by the `exit()` builtin.
    pub fn request_exit(&mut self, code: i64) {
        self.exit_requested = Some(code);
    }

    /// Pop the top frame, recycling its register buffer into the free pool.
    fn pop_frame(&mut self) {
        let frame = self
            .stack
            .pop()
            .expect("bytecode: exec stack empty (VM invariant violated)");
        if frame.regs.capacity() > 0 {
            self.free_regs.push(frame.regs);
        }
    }

    /// Top frame — VM invariant: the exec stack is non-empty while running.
    #[inline(always)]
    fn cur_frame(&self) -> &Frame {
        mimi_debug_assert!(!self.stack.is_empty(), "bytecode: exec stack empty");
        match self.stack.last() {
            Some(f) => f,
            None => unreachable!("bytecode: exec stack empty (VM invariant violated)"),
        }
    }

    /// Mutable top frame — VM invariant: the exec stack is non-empty while running.
    #[inline(always)]
    fn cur_frame_mut(&mut self) -> &mut Frame {
        mimi_debug_assert!(!self.stack.is_empty(), "bytecode: exec stack empty");
        match self.stack.last_mut() {
            Some(f) => f,
            None => unreachable!("bytecode: exec stack empty (VM invariant violated)"),
        }
    }

    /// Set the user CLI arguments that `args()` / `cli_args()` builtins return.
    pub fn with_cli_args(mut self, cli_args: Vec<String>) -> Self {
        self.cli_args = cli_args;
        self
    }

    /// Enable or disable FFI contract verification at runtime.
    pub fn set_verify_ffi(&mut self, verify: bool) {
        self.ffi_runtime.verify_ffi = verify;
    }

    /// Run the program from the entry point. Returns the exit code.
    pub fn run(&mut self) -> Result<i64, InterpError> {
        let entry = self.program.entry;
        self.push_frame(entry, Vec::new(), None)?;
        let result = loop {
            match self.exec_loop() {
                Ok(v) => break Ok(v),
                Err(e) => {
                    if self.absorb_flow_fault(&e) {
                        continue;
                    }
                    break Err(self.enrich_error(e));
                }
            }
        };
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
        loop {
            match self.exec_loop() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if self.absorb_flow_fault(&e) {
                        continue;
                    }
                    return Err(self.enrich_error(e));
                }
            }
        }
    }

    /// Flow fault absorption (v0.29.12): when a runtime panic escapes from a
    /// flow transition body, convert it into a Fault value instead of an
    /// error. The Fault is written into the transition frame's return
    /// register and all frames up to and including the transition are
    /// popped, so execution resumes at the caller with the Fault value.
    ///
    /// Returns false when the error should propagate:
    /// - no flow transition on the stack
    /// - the panic already happened in Fault (no re-absorption)
    /// - the error is a programming error, not a runtime panic
    fn absorb_flow_fault(&mut self, e: &InterpError) -> bool {
        let Some(idx) = self.stack.iter().rposition(|f| f.flow_tx.is_some()) else {
            return false;
        };
        let (flow_name, from_state, from_payload, persistent) = {
            let ctx = self.stack[idx].flow_tx.as_ref().expect("checked above");
            if ctx.from_state == "Fault" {
                return false;
            }
            (
                ctx.flow_name.clone(),
                ctx.from_state.clone(),
                ctx.from_payload.clone(),
                ctx.persistent_fields.clone(),
            )
        };
        if !is_runtime_panic(e) {
            return false;
        }
        // Draft = the transition's `self` (register 0) — mutated in place by
        // the body. Non-transactional (all flows, since @transactional was
        // abolished in v0.34.1): the draft survives; only dirty persistent
        // fields are zeroed below so recover→reset restores defaults.
        let mut restored = self.stack[idx].regs.first().cloned().unwrap_or(Value::Unit);
        // v0.29.13/14: recover degrades to reset when persistent fields
        // were dirtied during the turn that produced this Fault. Zero them
        // in the Fault shadow so the injected recover verb restores defaults
        // instead of the dirty draft.
        if !persistent.is_empty() {
            let entry_fields = record_fields_of(&from_payload);
            let draft_fields = record_fields_of(&restored);
            let dirty = persistent.iter().any(|name| {
                match (entry_fields.get(name), draft_fields.get(name)) {
                    (Some(old), Some(cur)) => !crate::interp::value::values_equal(cur, old),
                    _ => false,
                }
            });
            if dirty {
                if let Value::Record(_, fields) = &mut restored {
                    for name in &persistent {
                        if let Some(v) = fields.get_mut(name) {
                            *v = default_value_for_runtime(v);
                        }
                    }
                }
            }
        }
        let event = format!("panic:{}", e.code());
        let mut fault = crate::flow_matrix::make_fault_value(&from_state, &event, "");
        // v0.34.18b typed-fault parity: a `fault T` flow's Fault record carries a
        // defaulted `error: T` field. The codegen backend builds it from
        // type_defs["Fault"]; mirror that here so absorbed panics match L1.
        if let Some(err_ty) = self.program.flow_fault_type.get(&flow_name) {
            let err_val = default_record_value(err_ty, &self.program.record_fields);
            if let Value::Record(_, fields) = &mut fault {
                fields.insert("error".to_string(), err_val);
            }
        }
        shadow_persistent_into_fault(&mut fault, &restored, &persistent);
        let return_reg = self.stack[idx].return_reg;
        let popped = self.stack.len() - idx;
        self.stack.truncate(idx);
        self.depth = self.depth.saturating_sub(popped);
        if let Some(rd) = return_reg {
            if let Some(frame) = self.stack.last_mut() {
                frame.regs[rd as usize] = fault;
            }
        }
        true
    }

    /// Take captured stdout (consumes the buffer, leaves empty string).
    /// If a shared stdout_capture buffer is set, reads from it.
    pub fn take_stdout(&mut self) -> String {
        if let Some(buf) = &self.stdout_capture {
            if let Ok(mut g) = buf.lock() {
                let result = g.clone();
                g.clear();
                return result;
            }
        }
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
                proto.name,
                proto.param_count,
                args.len()
            )));
        }

        // Contract pre-condition check (0.33 Phase F).
        if self.verify_contracts && proto.has_requires {
            self.check_requires(func_idx, &args)?;
        }

        // Snapshot params for old(x) in ensures (before args is consumed).
        let old_snapshots = if self.verify_contracts && proto.has_ensures {
            args.clone()
        } else {
            Vec::new()
        };

        let regs = match self.free_regs.pop() {
            Some(mut buf) => {
                buf.clear();
                buf.reserve(reg_count);
                buf.extend(args);
                buf.resize(reg_count, Value::Unit);
                buf
            }
            None => {
                let mut buf = Vec::with_capacity(reg_count);
                buf.extend(args);
                buf.resize(reg_count, Value::Unit);
                buf
            }
        };

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
            mutate_writebacks: None,
            mutate_field_writebacks: None,
            flow_tx: None,
            old_snapshots,
            ieee_depth: 0,
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
        let frame = self.cur_frame_mut();
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

            let frame = self.cur_frame();
            let proto = &self.program.functions[frame.proto_idx as usize];

            if frame.pc >= proto.code.len() {
                // Fell off the end — implicit return Unit.
                // Observable under MIMI_VERBOSE so a compiler emitting a bad
                // pc is not silently masked by the fall-off-end path.
                if std::env::var("MIMI_VERBOSE").is_ok() {
                    eprintln!(
                        "[bytecode] fn#{} fell off end at pc={} (len={}) — implicit return",
                        frame.proto_idx,
                        frame.pc,
                        proto.code.len()
                    );
                }
                let return_reg = frame.return_reg;
                let wrap_ok = frame.wrap_ok;
                self.pop_frame();
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
            self.cur_frame_mut().pc += 1;

            match op {
                // ── Constants & moves ──────────────────────────
                Op::LoadConst { rd, idx } => {
                    let val = self.load_const(proto, idx);
                    self.cur_frame_mut().regs[rd as usize] = val;
                }
                Op::LoadUnit { rd } => self.cur_frame_mut().regs[rd as usize] = Value::Unit,
                Op::LoadTrue { rd } => self.cur_frame_mut().regs[rd as usize] = Value::Bool(true),
                Op::LoadFalse { rd } => self.cur_frame_mut().regs[rd as usize] = Value::Bool(false),
                Op::Mov { rd, rs } => {
                    if rd != rs {
                        let frame = self.cur_frame_mut();
                        frame.regs[rd as usize] = frame.regs[rs as usize].clone();
                    }
                }
                Op::DerefValue { rd, ra } => {
                    let val = self.get_reg(ra).clone();
                    let inner = match &val {
                        Value::Shared(arc) => arc
                            .read()
                            .map_err(|e| {
                                InterpError::new(format!("shared read lock failed: {}", e))
                            })?
                            .clone(),
                        Value::LocalShared(rc) => {
                            rc.lock().unwrap_or_else(|e| e.into_inner()).clone()
                        }
                        Value::WeakShared(weak) => {
                            let strong = weak.upgrade();
                            match &strong {
                                Some(a) => a
                                    .read()
                                    .map_err(|e| {
                                        InterpError::new(format!("shared read lock failed: {}", e))
                                    })?
                                    .clone(),
                                None => Value::Unit,
                            }
                        }
                        _ => val,
                    };
                    self.cur_frame_mut().regs[rd as usize] = inner;
                }

                // ── Integer arithmetic ─────────────────────────
                // Fast path: both Int (common case in loops). Fallback: Float/String.
                // Single frame borrow per op: reads + write happen inside one
                // last_mut region to cut per-op boundary checks.
                Op::AddInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => {
                            Value::Int(a.checked_add(*b).ok_or_else(|| {
                                InterpError::integer_overflow("integer addition overflow")
                            })?)
                        }
                        (Value::String(a), Value::String(b)) => Value::String(format!("{a}{b}")),
                        _ => {
                            let af = value_to_f64(&frame.regs[ra as usize])?;
                            let bf = value_to_f64(&frame.regs[rb as usize])?;
                            let r = af + bf;
                            if r.is_nan() || r.is_infinite() {
                                return Err(InterpError::float_error(
                                    "invalid floating-point result from +",
                                ));
                            }
                            Value::Float(r)
                        }
                    };
                    frame.regs[rd as usize] = result;
                }
                Op::SubInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => {
                            Value::Int(a.checked_sub(*b).ok_or_else(|| {
                                InterpError::integer_overflow("integer subtraction overflow")
                            })?)
                        }
                        _ => {
                            let af = value_to_f64(&frame.regs[ra as usize])?;
                            let bf = value_to_f64(&frame.regs[rb as usize])?;
                            let r = af - bf;
                            if r.is_nan() || r.is_infinite() {
                                return Err(InterpError::float_error(
                                    "invalid floating-point result from -",
                                ));
                            }
                            Value::Float(r)
                        }
                    };
                    frame.regs[rd as usize] = result;
                }
                Op::MulInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => {
                            Value::Int(a.checked_mul(*b).ok_or_else(|| {
                                InterpError::integer_overflow("integer multiplication overflow")
                            })?)
                        }
                        _ => {
                            let af = value_to_f64(&frame.regs[ra as usize])?;
                            let bf = value_to_f64(&frame.regs[rb as usize])?;
                            let r = af * bf;
                            if r.is_nan() || r.is_infinite() {
                                return Err(InterpError::float_error(
                                    "invalid floating-point result from *",
                                ));
                            }
                            Value::Float(r)
                        }
                    };
                    frame.regs[rd as usize] = result;
                }
                Op::DivInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => {
                            if *b == 0 {
                                return Err(InterpError::div_by_zero());
                            }
                            Value::Int(a.checked_div(*b).ok_or_else(|| {
                                InterpError::integer_overflow("integer division overflow")
                            })?)
                        }
                        _ => {
                            let af = value_to_f64(&frame.regs[ra as usize])?;
                            let bf = value_to_f64(&frame.regs[rb as usize])?;
                            if bf == 0.0 {
                                return Err(InterpError::div_by_zero());
                            }
                            let r = af / bf;
                            if r.is_nan() || r.is_infinite() {
                                return Err(InterpError::float_error(
                                    "invalid floating-point result from /",
                                ));
                            }
                            Value::Float(r)
                        }
                    };
                    frame.regs[rd as usize] = result;
                }
                Op::ModInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => {
                            if *b == 0 {
                                return Err(InterpError::div_by_zero());
                            }
                            Value::Int(a.checked_rem(*b).ok_or_else(|| {
                                InterpError::integer_overflow("integer remainder overflow")
                            })?)
                        }
                        _ => {
                            let a = value_to_f64(&frame.regs[ra as usize])?;
                            let b = value_to_f64(&frame.regs[rb as usize])?;
                            if b == 0.0 {
                                return Err(InterpError::div_by_zero());
                            }
                            let r = a % b;
                            if r.is_nan() || r.is_infinite() {
                                return Err(InterpError::float_error(
                                    "invalid floating-point result from %",
                                ));
                            }
                            Value::Float(r)
                        }
                    };
                    frame.regs[rd as usize] = result;
                }
                Op::NegInt { rd, ra } => {
                    let frame = self.cur_frame_mut();
                    let result = match &frame.regs[ra as usize] {
                        Value::Float(a) => {
                            let r = -*a;
                            if r.is_nan() || r.is_infinite() {
                                return Err(InterpError::float_error(
                                    "invalid floating-point result from neg",
                                ));
                            }
                            Value::Float(r)
                        }
                        other => {
                            let a = match other {
                                Value::Int(v) => *v,
                                other => {
                                    return Err(InterpError::new(format!(
                                        "expected number, found {}",
                                        other
                                    )))
                                }
                            };
                            Value::Int(a.checked_neg().ok_or_else(|| {
                                InterpError::integer_overflow("integer negation overflow")
                            })?)
                        }
                    };
                    frame.regs[rd as usize] = result;
                }

                // ── Float arithmetic ───────────────────────────
                Op::AddFloat { rd, ra, rb } => {
                    let (a, b) = {
                        let frame = self.cur_frame();
                        (
                            reg_as_f64(&frame.regs[ra as usize])?,
                            reg_as_f64(&frame.regs[rb as usize])?,
                        )
                    };
                    let r = a + b;
                    // H1 fix (SD-9): route through check_float so `ieee_float { }`
                    // suspends the finiteness trap for basic arithmetic too
                    // (was: hardcoded is_nan/is_infinite, ignoring ieee_depth).
                    self.check_float(r, "+")?;
                    self.cur_frame_mut().regs[rd as usize] = Value::Float(r);
                }
                Op::SubFloat { rd, ra, rb } => {
                    let (a, b) = {
                        let frame = self.cur_frame();
                        (
                            reg_as_f64(&frame.regs[ra as usize])?,
                            reg_as_f64(&frame.regs[rb as usize])?,
                        )
                    };
                    let r = a - b;
                    self.check_float(r, "-")?;
                    self.cur_frame_mut().regs[rd as usize] = Value::Float(r);
                }
                Op::MulFloat { rd, ra, rb } => {
                    let (a, b) = {
                        let frame = self.cur_frame();
                        (
                            reg_as_f64(&frame.regs[ra as usize])?,
                            reg_as_f64(&frame.regs[rb as usize])?,
                        )
                    };
                    let r = a * b;
                    self.check_float(r, "*")?;
                    self.cur_frame_mut().regs[rd as usize] = Value::Float(r);
                }
                Op::DivFloat { rd, ra, rb } => {
                    let (a, b) = {
                        let frame = self.cur_frame();
                        (
                            reg_as_f64(&frame.regs[ra as usize])?,
                            reg_as_f64(&frame.regs[rb as usize])?,
                        )
                    };
                    // H1 fix (SD-9): IEEE 754 division by zero yields ±Inf, so
                    // skip the div-by-zero trap inside `ieee_float { }`.
                    if b == 0.0 && self.cur_frame().ieee_depth == 0 {
                        return Err(InterpError::div_by_zero());
                    }
                    let r = a / b;
                    self.check_float(r, "/")?;
                    self.cur_frame_mut().regs[rd as usize] = Value::Float(r);
                }
                Op::NegFloat { rd, ra } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Float(v) => *v,
                        Value::Int(v) => *v as f64,
                        other => {
                            return Err(InterpError::new(format!("expected Float, got {}", other)))
                        }
                    };
                    frame.regs[rd as usize] = Value::Float(-a);
                }
                Op::IntToFloat { rd, ra } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    frame.regs[rd as usize] = Value::Float(a as f64);
                }

                // ── Comparison ─────────────────────────────────
                Op::EqInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => a == b,
                        (a, b) => crate::interp::values_equal(a, b),
                    };
                    frame.regs[rd as usize] = Value::Bool(result);
                }
                Op::NeInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => a != b,
                        (a, b) => !crate::interp::values_equal(a, b),
                    };
                    frame.regs[rd as usize] = Value::Bool(result);
                }
                Op::LtInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => a < b,
                        (Value::String(a), Value::String(b)) => a < b,
                        (a, b) => a.to_string() < b.to_string(),
                    };
                    frame.regs[rd as usize] = Value::Bool(result);
                }
                Op::GtInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => a > b,
                        (Value::String(a), Value::String(b)) => a > b,
                        (a, b) => a.to_string() > b.to_string(),
                    };
                    frame.regs[rd as usize] = Value::Bool(result);
                }
                Op::LeInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => a <= b,
                        (Value::String(a), Value::String(b)) => a <= b,
                        (a, b) => a.to_string() <= b.to_string(),
                    };
                    frame.regs[rd as usize] = Value::Bool(result);
                }
                Op::GeInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Int(a), Value::Int(b)) => a >= b,
                        (Value::String(a), Value::String(b)) => a >= b,
                        (a, b) => a.to_string() >= b.to_string(),
                    };
                    frame.regs[rd as usize] = Value::Bool(result);
                }
                Op::EqFloat { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let (a, b) = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Float(a), Value::Float(b)) => (*a, *b),
                        (a, b) => {
                            return Err(InterpError::new(format!(
                                "expected Float, got {} and {}",
                                a, b
                            )))
                        }
                    };
                    frame.regs[rd as usize] = Value::Bool(a == b);
                }
                Op::LtFloat { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let (a, b) = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Float(a), Value::Float(b)) => (*a, *b),
                        (a, b) => {
                            return Err(InterpError::new(format!(
                                "expected Float, got {} and {}",
                                a, b
                            )))
                        }
                    };
                    frame.regs[rd as usize] = Value::Bool(a < b);
                }
                Op::GtFloat { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let (a, b) = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Float(a), Value::Float(b)) => (*a, *b),
                        (a, b) => {
                            return Err(InterpError::new(format!(
                                "expected Float, got {} and {}",
                                a, b
                            )))
                        }
                    };
                    frame.regs[rd as usize] = Value::Bool(a > b);
                }
                Op::LeFloat { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let (a, b) = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Float(a), Value::Float(b)) => (*a, *b),
                        (a, b) => {
                            return Err(InterpError::new(format!(
                                "expected Float, got {} and {}",
                                a, b
                            )))
                        }
                    };
                    frame.regs[rd as usize] = Value::Bool(a <= b);
                }
                Op::GeFloat { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let (a, b) = match (&frame.regs[ra as usize], &frame.regs[rb as usize]) {
                        (Value::Float(a), Value::Float(b)) => (*a, *b),
                        (a, b) => {
                            return Err(InterpError::new(format!(
                                "expected Float, got {} and {}",
                                a, b
                            )))
                        }
                    };
                    frame.regs[rd as usize] = Value::Bool(a >= b);
                }
                Op::Eq { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = crate::interp::values_equal(
                        &frame.regs[ra as usize],
                        &frame.regs[rb as usize],
                    );
                    frame.regs[rd as usize] = Value::Bool(result);
                }
                Op::Ne { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = !crate::interp::values_equal(
                        &frame.regs[ra as usize],
                        &frame.regs[rb as usize],
                    );
                    frame.regs[rd as usize] = Value::Bool(result);
                }

                // ── Bitwise ────────────────────────────────────
                Op::BitAnd { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    let b = match &frame.regs[rb as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    frame.regs[rd as usize] = Value::Int(a & b);
                }
                Op::BitOr { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    let b = match &frame.regs[rb as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    frame.regs[rd as usize] = Value::Int(a | b);
                }
                Op::BitXor { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    let b = match &frame.regs[rb as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    frame.regs[rd as usize] = Value::Int(a ^ b);
                }
                Op::Shl { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    let b = match &frame.regs[rb as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    let r = u32::try_from(b)
                        .ok()
                        .and_then(|s| a.checked_shl(s))
                        .ok_or_else(|| InterpError::integer_overflow("shift overflow in <<"))?;
                    frame.regs[rd as usize] = Value::Int(r);
                }
                Op::Shr { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    let b = match &frame.regs[rb as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    let r = u32::try_from(b)
                        .ok()
                        .and_then(|s| a.checked_shr(s))
                        .ok_or_else(|| InterpError::integer_overflow("shift overflow in >>"))?;
                    frame.regs[rd as usize] = Value::Int(r);
                }
                Op::PowInt { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    let b = match &frame.regs[rb as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    let exp = u32::try_from(b).map_err(|_| {
                        InterpError::new(format!("negative exponent {} in integer power", b))
                    })?;
                    let r = a
                        .checked_pow(exp)
                        .ok_or_else(|| InterpError::integer_overflow("integer power overflow"))?;
                    frame.regs[rd as usize] = Value::Int(r);
                }
                Op::PowFloat { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Float(v) => *v,
                        Value::Int(v) => *v as f64,
                        other => {
                            return Err(InterpError::new(format!("expected Float, got {}", other)))
                        }
                    };
                    let b = match &frame.regs[rb as usize] {
                        Value::Float(v) => *v,
                        Value::Int(v) => *v as f64,
                        other => {
                            return Err(InterpError::new(format!("expected Float, got {}", other)))
                        }
                    };
                    let r = a.powf(b);
                    if r.is_nan() || r.is_infinite() {
                        return Err(InterpError::float_error(
                            "invalid floating-point result from pow",
                        ));
                    }
                    frame.regs[rd as usize] = Value::Float(r);
                }
                Op::BitNot { rd, ra } => {
                    let frame = self.cur_frame_mut();
                    let a = match &frame.regs[ra as usize] {
                        Value::Int(v) => *v,
                        other => {
                            return Err(InterpError::new(format!("expected Int, got {}", other)))
                        }
                    };
                    frame.regs[rd as usize] = Value::Int(!a);
                }
                Op::Not { rd, ra } => {
                    let frame = self.cur_frame_mut();
                    let v = &frame.regs[ra as usize];
                    frame.regs[rd as usize] = Value::Bool(!crate::interp::is_truthy(v));
                }
                Op::And { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    if crate::interp::is_truthy(&frame.regs[ra as usize]) {
                        let b = crate::interp::is_truthy(&frame.regs[rb as usize]);
                        frame.regs[rd as usize] = Value::Bool(b);
                    } else {
                        frame.regs[rd as usize] = Value::Bool(false);
                    }
                }
                Op::Or { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    if crate::interp::is_truthy(&frame.regs[ra as usize]) {
                        frame.regs[rd as usize] = Value::Bool(true);
                    } else {
                        let b = crate::interp::is_truthy(&frame.regs[rb as usize]);
                        frame.regs[rd as usize] = Value::Bool(b);
                    }
                }

                // ── String ─────────────────────────────────────
                Op::ConcatStr { rd, ra, rb } => {
                    let frame = self.cur_frame_mut();
                    let result = format!("{}{}", frame.regs[ra as usize], frame.regs[rb as usize]);
                    frame.regs[rd as usize] = Value::String(result);
                }
                Op::StrAppend { ra, rb } => {
                    let suffix = self.get_reg(rb).to_string();
                    let target = self.get_reg_mut(ra);
                    match target {
                        Value::String(s) => s.push_str(&suffix),
                        other => {
                            let base = other.to_string();
                            *other = Value::String(format!("{}{}", base, suffix));
                        }
                    }
                }

                // ── Control flow ───────────────────────────────
                Op::Jmp { offset } => {
                    let frame = self.cur_frame_mut();
                    let pc = frame.pc as i32;
                    let new_pc = pc + offset;
                    if new_pc < 0 {
                        return Err(InterpError::new(format!(
                            "Jmp underflow: pc={} offset={}",
                            pc, offset
                        )));
                    }
                    if new_pc as usize > proto.code.len() {
                        return Err(InterpError::new(format!(
                            "Jmp overflow: pc={} offset={} len={}",
                            pc,
                            offset,
                            proto.code.len()
                        )));
                    }
                    frame.pc = new_pc as usize;
                }
                Op::JmpIf { offset, ra } => {
                    if crate::interp::is_truthy(self.get_reg(ra)) {
                        let frame = self.cur_frame_mut();
                        let pc = frame.pc as i32;
                        let new_pc = pc + offset;
                        if new_pc < 0 {
                            return Err(InterpError::new(format!(
                                "JmpIf underflow: pc={} offset={}",
                                pc, offset
                            )));
                        }
                        if new_pc as usize > proto.code.len() {
                            return Err(InterpError::new(format!(
                                "JmpIf overflow: pc={} offset={} len={}",
                                pc,
                                offset,
                                proto.code.len()
                            )));
                        }
                        frame.pc = new_pc as usize;
                    }
                }
                Op::JmpIfNot { offset, ra } => {
                    if !crate::interp::is_truthy(self.get_reg(ra)) {
                        let frame = self.cur_frame_mut();
                        let pc = frame.pc as i32;
                        let new_pc = pc + offset;
                        if new_pc < 0 {
                            return Err(InterpError::new(format!(
                                "JmpIfNot underflow: pc={} offset={}",
                                pc, offset
                            )));
                        }
                        if new_pc as usize > proto.code.len() {
                            return Err(InterpError::new(format!(
                                "JmpIfNot overflow: pc={} offset={} len={}",
                                pc,
                                offset,
                                proto.code.len()
                            )));
                        }
                        frame.pc = new_pc as usize;
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
                Op::MutateSetup { regs_base, count } => {
                    let mut targets = Vec::with_capacity(count as usize);
                    for i in 0..count {
                        match self.get_reg(regs_base + i) {
                            Value::Int(reg) => targets.push(*reg as Reg),
                            _ => {
                                self.cur_frame_mut().mutate_writebacks = None;
                                targets.clear();
                                break;
                            }
                        }
                    }
                    if !targets.is_empty() {
                        self.cur_frame_mut().mutate_writebacks = Some(targets);
                    }
                }
                Op::MutateSetupField { regs_base, count } => {
                    // v0.34.13: each target = (obj_reg, field_name) in 2
                    // consecutive registers.
                    let mut targets = Vec::with_capacity(count as usize);
                    let mut ok = true;
                    for i in 0..count {
                        let obj_slot = regs_base + (i * 2) as Reg;
                        let field_slot = obj_slot + 1;
                        match (self.get_reg(obj_slot), self.get_reg(field_slot)) {
                            (Value::Int(obj_reg), Value::String(field)) => {
                                targets.push((*obj_reg as Reg, field.clone()));
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok && !targets.is_empty() {
                        self.cur_frame_mut().mutate_field_writebacks = Some(targets);
                    } else {
                        self.cur_frame_mut().mutate_field_writebacks = None;
                    }
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
                            if let Some(handler_pc) =
                                self.stack.last_mut().and_then(|f| f.fault_pc.take())
                            {
                                self.cur_frame_mut().pc = handler_pc;
                            } else {
                                return Err(e);
                            }
                        }
                    }
                }
                Op::CallExtern {
                    rd,
                    extern_idx,
                    args_base,
                    argc,
                } => {
                    let args: Vec<Value> = (0..argc)
                        .map(|i| self.get_reg(args_base + i).clone())
                        .collect();
                    let result = self.call_extern_idx(extern_idx, args);
                    match result {
                        Ok(v) => self.set_reg(rd, v),
                        Err(e) => {
                            if let Some(handler_pc) =
                                self.stack.last_mut().and_then(|f| f.fault_pc.take())
                            {
                                self.cur_frame_mut().pc = handler_pc;
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
                // ── Quote assembly (0.33 Phase F) ──
                Op::QuotePushLit { const_idx } => {
                    let lit = match self.program.functions[self.cur_frame().proto_idx as usize]
                        .constants
                        .get(const_idx as usize)
                    {
                        Some(ConstValue::Int(v)) => Lit::Int(*v),
                        Some(ConstValue::Float(v)) => Lit::Float(*v),
                        Some(ConstValue::Bool(v)) => Lit::Bool(*v),
                        Some(ConstValue::Str(v)) => Lit::String(v.clone()),
                        Some(ConstValue::Unit) => Lit::Unit,
                        Some(ConstValue::Type(_))
                        | Some(ConstValue::QuoteAst(_))
                        | Some(ConstValue::LambdaSpec { .. })
                        | Some(ConstValue::Pattern(_))
                        | Some(ConstValue::StrVec(_))
                        | None => {
                            return Err(InterpError::new("QuotePushLit: constant is not a literal"))
                        }
                    };
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Literal(lit));
                }
                Op::QuotePushIdent { str_idx } => {
                    let name = self.const_str(str_idx)?.to_string();
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Ident(name));
                }
                Op::QuoteInterpPush { rs } => {
                    let v = self.get_reg(rs).clone();
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Interpolate(Box::new(v)));
                }
                Op::QuoteAstPush { rs } => {
                    let v = self.get_reg(rs).clone();
                    match v {
                        Value::QuoteAst(q) => {
                            self.quote_stack.push(*q);
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "QuoteAstPush: expected QuoteAst value, got {}",
                                other
                            )))
                        }
                    }
                }
                Op::QuoteCapture { str_idx, reg } => {
                    let name = self.const_str(str_idx)?.to_string();
                    self.quote_captures.insert(name, self.get_reg(reg).clone());
                }
                Op::QuoteBlock { n } => {
                    let items = self.quote_pop_n(n)?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Block(items));
                }
                Op::QuoteList { n } => {
                    let items = self.quote_pop_n(n)?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::List(items));
                }
                Op::QuoteTuple { n } => {
                    let items = self.quote_pop_n(n)?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Tuple(items));
                }
                Op::QuoteBinary { op } => {
                    let r = self.quote_pop()?;
                    let l = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Binary(
                            op,
                            Box::new(l),
                            Box::new(r),
                        ));
                }
                Op::QuoteUnary { op } => {
                    let e = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Unary(op, Box::new(e)));
                }
                Op::QuoteCall { argc } => {
                    let args = self.quote_pop_n(argc)?;
                    let callee = self.quote_pop()?;
                    self.quote_stack.push(crate::interp::value::QuotedAst::Call(
                        Box::new(callee),
                        args,
                    ));
                }
                Op::QuoteField { str_idx } => {
                    let name = self.const_str(str_idx)?.to_string();
                    let obj = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Field(Box::new(obj), name));
                }
                Op::QuoteIndex => {
                    let idx = self.quote_pop()?;
                    let obj = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Index(
                            Box::new(obj),
                            Box::new(idx),
                        ));
                }
                Op::QuoteIf { has_else } => {
                    let else_node = if has_else {
                        Some(Box::new(self.quote_pop()?))
                    } else {
                        None
                    };
                    let then_node = self.quote_pop()?;
                    let cond = self.quote_pop()?;
                    self.quote_stack.push(crate::interp::value::QuotedAst::If(
                        Box::new(cond),
                        Box::new(then_node),
                        else_node,
                    ));
                }
                Op::QuoteLet { str_idx } => {
                    let name = self.const_str(str_idx)?.to_string();
                    let value = self.quote_pop()?;
                    self.quote_stack.push(crate::interp::value::QuotedAst::Let {
                        name,
                        value: Box::new(value),
                    });
                }
                Op::QuoteCast { type_idx } => {
                    let ty = match self.program.functions[self.cur_frame().proto_idx as usize]
                        .constants
                        .get(type_idx as usize)
                    {
                        Some(ConstValue::Type(t)) => t.clone(),
                        _ => return Err(InterpError::new("QuoteCast: constant is not a type")),
                    };
                    let inner = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Cast(Box::new(inner), ty));
                }
                Op::QuoteExprStmt => {
                    let e = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::ExprStmt(Box::new(e)));
                }
                Op::QuoteReturn { has_value } => {
                    let inner = if has_value {
                        Some(Box::new(self.quote_pop()?))
                    } else {
                        None
                    };
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Return(inner));
                }
                Op::QuoteWhile => {
                    let body = self.quote_pop()?;
                    let cond = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::While(
                            Box::new(cond),
                            Box::new(body),
                        ));
                }
                Op::QuoteWhileLet { pat_idx } => {
                    let pat = match self.program.functions[self.cur_frame().proto_idx as usize]
                        .constants
                        .get(pat_idx as usize)
                    {
                        Some(ConstValue::Pattern(p)) => p.clone(),
                        _ => {
                            return Err(InterpError::new(
                                "QuoteWhileLet: constant is not a pattern",
                            ))
                        }
                    };
                    let body = self.quote_pop()?;
                    let init = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::WhileLet {
                            pat,
                            init: Box::new(init),
                            body: Box::new(body),
                        });
                }
                Op::QuoteBreak { has_value } => {
                    let inner = if has_value {
                        Some(Box::new(self.quote_pop()?))
                    } else {
                        None
                    };
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Break(inner));
                }
                Op::QuoteContinue => {
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Continue);
                }
                Op::QuoteLambda { spec_idx } => {
                    let (params, ret, body, free_vars) = match self.program.functions
                        [self.cur_frame().proto_idx as usize]
                        .constants
                        .get(spec_idx as usize)
                    {
                        Some(ConstValue::LambdaSpec {
                            params,
                            ret,
                            body,
                            free_vars,
                        }) => (params.clone(), ret.clone(), body.clone(), free_vars.clone()),
                        _ => {
                            return Err(InterpError::new(
                                "QuoteLambda: constant is not a lambda spec",
                            ))
                        }
                    };
                    // Capture only the lambda's free variables from quote_captures
                    // (prevents cross-quote contamination).
                    let captured: std::collections::HashMap<String, Value> = free_vars
                        .iter()
                        .filter_map(|name| {
                            self.quote_captures
                                .get(name)
                                .map(|v| (name.clone(), v.clone()))
                        })
                        .collect();
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Lambda {
                            params,
                            ret,
                            body,
                            captured,
                        });
                }
                Op::QuoteFor { var_idx } => {
                    let var = self.const_str(var_idx)?.to_string();
                    let body = self.quote_pop()?;
                    let iter = self.quote_pop()?;
                    self.quote_stack.push(crate::interp::value::QuotedAst::For(
                        var,
                        Box::new(iter),
                        Box::new(body),
                    ));
                }
                Op::QuoteAssign => {
                    let value = self.quote_pop()?;
                    let target = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Assign(
                            Box::new(target),
                            Box::new(value),
                        ));
                }
                Op::QuoteLoop => {
                    let body = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Loop(Box::new(body)));
                }
                Op::QuoteRecord {
                    n,
                    names_idx,
                    ty_idx,
                } => {
                    let proto = &self.program.functions[self.cur_frame().proto_idx as usize];
                    let names = match proto.constants.get(names_idx as usize) {
                        Some(ConstValue::StrVec(v)) => v.clone(),
                        _ => {
                            return Err(InterpError::new(
                                "QuoteRecord: names constant is not a StrVec",
                            ))
                        }
                    };
                    let ty = match proto.constants.get(ty_idx as usize) {
                        Some(ConstValue::Str(s)) if s.is_empty() => None,
                        Some(ConstValue::Str(s)) => Some(s.clone()),
                        _ => None,
                    };
                    let values = self.quote_pop_n(n)?;
                    let fields: Vec<crate::interp::value::RecordFieldExprQuoted> = names
                        .iter()
                        .zip(values.into_iter())
                        .map(
                            |(name, value)| crate::interp::value::RecordFieldExprQuoted {
                                name: name.clone(),
                                value,
                            },
                        )
                        .collect();
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Record { ty, fields });
                }
                Op::QuoteTry => {
                    let e = self.quote_pop()?;
                    self.quote_stack
                        .push(crate::interp::value::QuotedAst::Try(Box::new(e)));
                }
                Op::QuoteResult { rd } => {
                    let node = self.quote_pop()?;
                    self.set_reg(rd, Value::QuoteAst(Box::new(node)));
                }
                Op::RetEarly { ra } => {
                    // Check fault handler before returning.
                    if let Some(handler_pc) = self.stack.last_mut().and_then(|f| f.fault_pc.take())
                    {
                        if let Some(frame) = self.stack.last_mut() {
                            frame.fault_reg = Some(ra);
                        }
                        self.cur_frame_mut().pc = handler_pc;
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
                    let frame = self.cur_frame();
                    let return_reg = frame.return_reg;
                    let wrap_ok = frame.wrap_ok;
                    let mut_param_vals = self.collect_mut_param_vals();
                    let v = if wrap_ok {
                        Value::Variant("Ok".to_string(), vec![Value::Unit])
                    } else {
                        Value::Unit
                    };
                    self.pop_frame();
                    self.depth -= 1;
                    if self.stack.is_empty() || (stop > 0 && self.depth < stop) {
                        return Ok(v);
                    }
                    if let Some(rd) = return_reg {
                        self.set_reg(rd, v);
                    }
                    if !mut_param_vals.is_empty() {
                        self.apply_mutate_writeback(&mut_param_vals);
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
                                        idx_raw,
                                        l.len()
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
                            let chars: Vec<char> = s.chars().collect();
                            let idx = if idx_raw < 0 {
                                let wrapped = chars.len() as i64 + idx_raw;
                                if wrapped < 0 {
                                    return Err(InterpError::new(format!(
                                        "string index {} out of bounds (len {})",
                                        idx_raw,
                                        chars.len()
                                    )));
                                }
                                wrapped as usize
                            } else {
                                idx_raw as usize
                            };
                            if idx >= chars.len() {
                                return Err(InterpError::new(format!(
                                    "string index {} out of bounds (len {})",
                                    idx_raw,
                                    chars.len()
                                )));
                            }
                            Value::String(chars[idx].to_string())
                        }
                        Value::Set(s) => {
                            let idx = if idx_raw < 0 {
                                let wrapped = s.len() as i64 + idx_raw;
                                if wrapped < 0 {
                                    return Err(InterpError::new(format!(
                                        "set index {} out of bounds (len {})",
                                        idx_raw,
                                        s.len()
                                    )));
                                }
                                wrapped as usize
                            } else {
                                idx_raw as usize
                            };
                            if idx >= s.len() {
                                return Err(InterpError::new(format!(
                                    "set index {} out of bounds (len {})",
                                    idx_raw,
                                    s.len()
                                )));
                            }
                            s[idx].clone()
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "index: expected List, String, or Set, got {}",
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
                        Value::String(s) => s.chars().count(),
                        Value::Tuple(t) => t.len(),
                        Value::Set(s) => s.len(),
                        Value::Record(_, fields) => fields.len(),
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
                    let elems: Vec<Value> =
                        (0..arity).map(|i| self.get_reg(base + i).clone()).collect();
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
                        // Newtype .0 accessor: unwrap the inner value.
                        // Newtypes are represented as Variant(name, [inner]) in bytecode.
                        Value::Newtype(_, inner) if idx == 0 => {
                            self.set_reg(rd, *inner);
                        }
                        Value::Variant(_, payload) if idx == 0 && payload.len() == 1 => {
                            self.set_reg(rd, payload[0].clone());
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
                            if s.is_empty() {
                                None
                            } else {
                                Some(s.clone())
                            }
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
                                        idx,
                                        proto.constants.len()
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
                        // Auto-unwrap Shared/Ref/RefMut to access inner record.
                        Value::Shared(inner) | Value::Ref(inner) | Value::RefMut(inner) => {
                            let guard = inner.read().map_err(|e| {
                                InterpError::new(format!("shared lock failed: {}", e))
                            })?;
                            match &*guard {
                                Value::Record(_, fields) => {
                                    fields.get(&field_name).cloned().ok_or_else(|| {
                                        InterpError::new(format!(
                                            "record has no field '{}'",
                                            field_name
                                        ))
                                    })?
                                }
                                other => {
                                    return Err(InterpError::new(format!(
                                        "record get: shared inner is not a Record, got {}",
                                        other
                                    )))
                                }
                            }
                        }
                        // Auto-unwrap Variant payload (Some(record), Ok(record)).
                        Value::Variant(_, payload) if payload.len() == 1 => match &payload[0] {
                            Value::Record(_, fields) => {
                                fields.get(&field_name).cloned().ok_or_else(|| {
                                    InterpError::new(format!(
                                        "record has no field '{}'",
                                        field_name
                                    ))
                                })?
                            }
                            other => {
                                return Err(InterpError::new(format!(
                                    "record get: variant payload is not a Record, got {}",
                                    other
                                )))
                            }
                        },
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
                            handle
                                .inner
                                .write()
                                .map_err(|e| InterpError::new(format!("actor lock failed: {}", e)))?
                                .fields
                                .insert(field_name, value);
                        }
                        // Auto-unwrap Shared/Ref/RefMut for field assignment.
                        Value::Shared(inner) | Value::Ref(inner) | Value::RefMut(inner) => {
                            let inner_clone = inner.clone();
                            let mut guard = inner_clone.write().map_err(|e| {
                                InterpError::new(format!("shared lock failed: {}", e))
                            })?;
                            match &mut *guard {
                                Value::Record(_, fields) => {
                                    fields.insert(field_name, value);
                                }
                                other => {
                                    return Err(InterpError::new(format!(
                                        "record set: shared inner is not a Record, got {}",
                                        other
                                    )))
                                }
                            }
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "record set: expected Record or Actor, got {}",
                                other
                            )))
                        }
                    }
                }
                Op::TupleSet { ra, idx, rb } => {
                    let idx_name = match &proto.constants[idx as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => String::new(),
                    };
                    let idx: usize = idx_name.parse().unwrap_or(usize::MAX);
                    let value = self.get_reg(rb).clone();
                    let tuple = self.get_reg_mut(ra);
                    match tuple {
                        Value::Tuple(t) => {
                            if idx >= t.len() {
                                return Err(InterpError::new(format!(
                                    "tuple set: index {} out of bounds (len {})",
                                    idx,
                                    t.len()
                                )));
                            }
                            t[idx] = value;
                        }
                        // Newtype inner set: `.0 = v`.
                        Value::Newtype(_, inner) if idx == 0 => {
                            **inner = value;
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "tuple set: expected Tuple, got {}",
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
                        _ => return Err(InterpError::new("map_get: expected (Map, String key)")),
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
                        _ => return Err(InterpError::new("map_set: expected (Map, String key)")),
                    }
                }
                Op::MapContains { rd, ra, rb } => {
                    let key = self.get_reg(rb).clone();
                    let contains = match (self.get_reg(ra), &key) {
                        (Value::Record(_, fields), Value::String(k)) => fields.contains_key(k),
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
                        _ => return Err(InterpError::new("set_add: expected Set")),
                    }
                }
                Op::SetContains { rd, ra, rb } => {
                    let set = self.get_reg(ra).clone();
                    let val = self.get_reg(rb).clone();
                    match &set {
                        Value::Set(s) => {
                            self.set_reg(rd, Value::Bool(s.contains(&val)));
                        }
                        _ => return Err(InterpError::new("set_contains: expected Set")),
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
                        Value::BytecodeClosure {
                            proto: proto_idx,
                            captured,
                        } => {
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
                                let value = captured
                                    .get(name)
                                    .ok_or_else(|| {
                                        InterpError::new(format!(
                                            "CallIndirect: missing capture '{}'",
                                            name
                                        ))
                                    })?
                                    .clone();
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
                            // Fallback: tree-walker Closure (from ast_eval / quote).
                            // 0.33 Phase F: evaluate via bytecode comptime evaluator.
                            if let Value::Closure {
                                params,
                                body,
                                captured,
                                ..
                            } = &other
                            {
                                let args: Vec<Value> = (0..argc)
                                    .map(|i| self.get_reg(args_base + i).clone())
                                    .collect();
                                if args.len() != params.len() {
                                    return Err(InterpError::wrong_arg_count(format!(
                                        "closure expects {} argument(s), got {}",
                                        params.len(),
                                        args.len()
                                    )));
                                }
                                let file = self.program.ast.clone().ok_or_else(|| {
                                    InterpError::new("call indirect: no program AST for Closure")
                                })?;
                                // Seed comptime values: captured vars + params.
                                let mut comptime_values = captured.clone();
                                for (p, a) in params.iter().zip(args.iter()) {
                                    comptime_values.insert(p.name.clone(), a.clone());
                                }
                                let v = crate::interp::bytecode::compiler::eval_comptime_block_bytecode(
                                    file.as_ref(),
                                    body,
                                    &comptime_values,
                                )
                                .map_err(InterpError::new)?;
                                self.set_reg(rd, v);
                            } else {
                                return Err(InterpError::new(format!(
                                    "call indirect: expected BytecodeClosure, got {}",
                                    other
                                )));
                            }
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
                    let payload: Vec<Value> =
                        (0..arity).map(|i| self.get_reg(base + i).clone()).collect();
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
                        // v0.34.15: flow states are Record(Some(state_name), _)
                        // — multi-target transition results carry the target
                        // state name as the record tag, so match arms on
                        // `Small { v }` / `Large { v }` must IsVariant-match
                        // against records too.
                        Value::Record(Some(name), _) => name == &expected_tag,
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
                Op::PatternField { rd, ra, field } => {
                    // v0.34.15: field extraction for match arms. Flow states
                    // are Record(Some(name), HashMap) — extract by field name.
                    // Variants keep positional _0.._N semantics.
                    let v = self.get_reg(ra).clone();
                    let field_name = match &proto.constants[field as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => String::new(),
                    };
                    match v {
                        Value::Record(_, fields) => {
                            if let Some(value) = fields.get(&field_name) {
                                self.set_reg(rd, value.clone());
                            } else {
                                return Err(InterpError::new(format!(
                                    "pattern field '{field_name}' absent from record"
                                )));
                            }
                        }
                        Value::Variant(_, vals) => {
                            let idx = field_name
                                .strip_prefix('_')
                                .and_then(|n| n.parse::<usize>().ok())
                                .unwrap_or(0);
                            if idx >= vals.len() {
                                return Err(InterpError::new(format!(
                                    "pattern field index {idx} out of bounds (arity {})",
                                    vals.len()
                                )));
                            }
                            self.set_reg(rd, vals[idx].clone());
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "pattern field: expected Record or Variant, got {other}"
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
                Op::NewCap { rd, name } => {
                    let proto = &self.program.functions[self.cur_frame().proto_idx as usize];
                    let cap_str = match &proto.constants[name as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => "unknown_cap".to_string(),
                    };
                    // Components stored as comma-separated string.
                    let components: Vec<String> =
                        cap_str.split(',').map(|s| s.to_string()).collect();
                    self.set_reg(rd, Value::Cap(components));
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
                            other => {
                                return Err(InterpError::new(format!(
                                    "cannot cast {} to i64",
                                    other
                                )))
                            }
                        },
                        1 => match v {
                            Value::Float(f) => Value::Float(f),
                            Value::Int(i) => Value::Float(i as f64),
                            other => {
                                return Err(InterpError::new(format!(
                                    "cannot cast {} to f64",
                                    other
                                )))
                            }
                        },
                        _ => {
                            return Err(InterpError::new(format!(
                                "Cast: unknown target {}",
                                target
                            )))
                        }
                    };
                    self.set_reg(rd, result);
                }
                Op::TypeOf { rd, ra } => {
                    let v = self.get_reg(ra);
                    // Match tree-walker's value_type_name semantics.
                    let name = match v {
                        Value::Int(_) => "i32".to_string(),
                        Value::Float(_) => "f64".to_string(),
                        Value::Bool(_) => "bool".to_string(),
                        Value::String(_) => "string".to_string(),
                        Value::Unit => "unit".to_string(),
                        Value::List(_) => "list".to_string(),
                        Value::Set(_) => "set".to_string(),
                        Value::Array(_) => "array".to_string(),
                        Value::Tuple(_) => "tuple".to_string(),
                        Value::Variant(tag, _) => tag.clone(),
                        Value::Record(Some(name), _) => name.clone(),
                        Value::Record(None, _) => "record".to_string(),
                        Value::Newtype(name, _) => name.clone(),
                        Value::Shared(_) => "shared".to_string(),
                        Value::Actor(_) => "actor".to_string(),
                        _ => crate::interp::type_name(v).to_string(),
                    };
                    self.set_reg(rd, Value::String(name));
                }
                Op::Trap { msg } => {
                    let proto = &self.program.functions[self.cur_frame().proto_idx as usize];
                    let msg_str = match &proto.constants[msg as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => "unknown trap".to_string(),
                    };
                    return Err(InterpError::new(msg_str));
                }
                Op::Nop => {}
                Op::IeeeEnter => self.cur_frame_mut().ieee_depth += 1,
                Op::IeeeExit => {
                    let frame = self.cur_frame_mut();
                    frame.ieee_depth = frame.ieee_depth.saturating_sub(1);
                }

                // ── Actor / Flow / Session (Phase D) ──────────
                Op::ActorSpawn { rd, actor } => {
                    let proto = &self.program.functions[self.cur_frame().proto_idx as usize];
                    let actor_name = match &proto.constants[actor as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => return Err(InterpError::new("ActorSpawn: invalid actor name")),
                    };
                    let val = self.spawn_actor(&actor_name, false)?;
                    self.set_reg(rd, val);
                }

                Op::ActorSpawnDetached { rd, actor } => {
                    let proto = &self.program.functions[self.cur_frame().proto_idx as usize];
                    let actor_name = match &proto.constants[actor as usize] {
                        ConstValue::Str(s) => s.clone(),
                        _ => {
                            return Err(InterpError::new("ActorSpawnDetached: invalid actor name"))
                        }
                    };
                    let val = self.spawn_actor(&actor_name, true)?;
                    self.set_reg(rd, val);
                }

                Op::FlowTransition {
                    rd,
                    flow,
                    method,
                    args_base,
                    argc,
                } => {
                    let proto = &self.program.functions[self.cur_frame().proto_idx as usize];
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
                        other => {
                            return Err(InterpError::new(format!(
                                "FlowTransition: first arg must be a state Record, got {}",
                                other
                            )))
                        }
                    };
                    // Look up the compiled transition function.
                    let key = (flow_name.clone(), method_name.clone(), from_state.clone());
                    let func_idx = self
                        .program
                        .flow_transition_funcs
                        .get(&key)
                        .copied()
                        .ok_or_else(|| {
                            InterpError::new(format!(
                                "no transition {}::{} from state {}",
                                flow_name, method_name, from_state
                            ))
                        })?;
                    // Collect args.
                    let args: Vec<Value> = (0..argc)
                        .map(|i| self.get_reg(args_base + i).clone())
                        .collect();
                    // From-state payload for fault shadowing (captured before
                    // args is moved into the frame).
                    let from_payload = args.first().cloned().unwrap_or(Value::Unit);
                    // Call the transition function.
                    // If the transition has a `fails` clause, wrap the result:
                    // success → Ok(result), failure → Err((source, error)).
                    if self.program.flow_fails_transitions.contains(&key) {
                        let state_val = self.get_reg(args_base).clone();
                        self.push_frame_wrap_ok(func_idx, args, Some(rd), state_val)?;
                    } else {
                        self.push_frame(func_idx, args, Some(rd))?;
                    }
                    // Record flow-transition context for fault absorption
                    // (runtime panics → Fault value, v0.29.12).
                    {
                        let persistent = self.program.flow_persistent.get(&flow_name).cloned();
                        let frame = self.cur_frame_mut();
                        frame.flow_tx = Some(FlowTxCtx {
                            flow_name,
                            from_state,
                            from_payload,
                            persistent_fields: persistent.unwrap_or_default(),
                        });
                    }
                }

                Op::DynMethodCall {
                    rd,
                    method,
                    args_base,
                    argc,
                } => {
                    let proto = &self.program.functions[self.cur_frame().proto_idx as usize];
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
                                let func_idx = self
                                    .program
                                    .actor_method_funcs
                                    .get(&key)
                                    .copied()
                                    .ok_or_else(|| {
                                    InterpError::new(format!(
                                        "self-call: actor method '{}' not found",
                                        method_name
                                    ))
                                })?;
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
                                let result = rx
                                    .recv()
                                    .map_err(|_| InterpError::new("actor worker thread died"))??;
                                self.set_reg(rd, result);
                            }
                        }
                        Value::WeakShared(weak) if method_name == "upgrade" => {
                            match weak.upgrade() {
                                Some(arc) => self.set_reg(rd, Value::Shared(arc)),
                                None => {
                                    self.set_reg(rd, Value::Variant("None".to_string(), vec![]))
                                }
                            }
                        }
                        Value::Shared(arc) if method_name == "deref" => {
                            let inner = arc.read().map_err(|e| {
                                InterpError::new(format!("shared read lock failed: {}", e))
                            })?;
                            self.set_reg(rd, inner.clone());
                        }
                        // Capability methods (split, drop).
                        Value::Cap(components) => {
                            let result: Result<Value, InterpError> = match method_name.as_str() {
                                "split" => {
                                    // Split combined cap into tuple of individual caps.
                                    if components.len() <= 1 {
                                        Err(InterpError::new(
                                            "split() requires a combined capability (e.g., cap FullAccess = Read + Write)",
                                        ))
                                    } else {
                                        let parts: Vec<Value> = components
                                            .iter()
                                            .map(|c| Value::Cap(vec![c.clone()]))
                                            .collect();
                                        Ok(Value::Tuple(parts))
                                    }
                                }
                                "drop" => Ok(Value::Unit),
                                _ => {
                                    return Err(InterpError::new(format!(
                                        "cannot call method '{}' on Cap",
                                        method_name
                                    )));
                                }
                            };
                            self.set_reg(rd, result?);
                        }
                        // Set built-in methods (remove, insert, is_empty, contains, size, to_list).
                        Value::Set(items) => {
                            let result: Result<Value, InterpError> = match method_name.as_str() {
                                "remove" => {
                                    let elem = self.get_reg(args_base + 1).clone();
                                    let new_set: Vec<Value> = items
                                        .iter()
                                        .filter(|v| !crate::interp::values_equal(v, &elem))
                                        .cloned()
                                        .collect();
                                    Ok(Value::Set(new_set))
                                }
                                "insert" => {
                                    let elem = self.get_reg(args_base + 1).clone();
                                    let mut new_set = items.clone();
                                    if !new_set
                                        .iter()
                                        .any(|v| crate::interp::values_equal(v, &elem))
                                    {
                                        new_set.push(elem);
                                    }
                                    Ok(Value::Set(new_set))
                                }
                                "is_empty" => Ok(Value::Bool(items.is_empty())),
                                "contains" => {
                                    let elem = self.get_reg(args_base + 1).clone();
                                    Ok(Value::Bool(
                                        items.iter().any(|v| crate::interp::values_equal(v, &elem)),
                                    ))
                                }
                                "size" | "len" => Ok(Value::Int(items.len() as i64)),
                                "to_list" => Ok(Value::List(items.clone())),
                                _ => {
                                    return Err(InterpError::new(format!(
                                        "cannot call method '{}' on Set",
                                        method_name
                                    )));
                                }
                            };
                            self.set_reg(rd, result?);
                        }
                        // Option/Result built-in methods (matches tree-walker call.rs:1178+).
                        // dyn Trait dispatch: method resolution by concrete
                        // record type name (tree-walker Value::DynTrait arm).
                        Value::Record(Some(concrete_type), _) => {
                            let mangled = format!("{}_{}", concrete_type, method_name);
                            if let Some(func_idx) = self.program.function_index(&mangled) {
                                let args: Vec<Value> = (0..argc)
                                    .map(|i| self.get_reg(args_base + i).clone())
                                    .collect();
                                let func_idx = func_idx as FuncIdx;
                                self.push_frame(func_idx, args, Some(rd))
                                    .map_err(|e| self.enrich_error(e))?;
                                // Continue loop — new frame is now active.
                            } else {
                                return Err(InterpError::new(format!(
                                    "cannot call method '{}' on record {} (no impl found)",
                                    method_name, concrete_type
                                )));
                            }
                        }
                        Value::Variant(tag, payload) => {
                            let result = match (tag.as_str(), method_name.as_str()) {
                                // unwrap / expect
                                ("Some" | "Ok", "unwrap") | ("Some" | "Ok", "expect") => {
                                    Ok(payload.first().cloned().unwrap_or(Value::Unit))
                                }
                                ("None", "unwrap") | ("None", "expect") => {
                                    Err(InterpError::new("called unwrap() on None"))
                                }
                                ("Err", "unwrap") | ("Err", "expect") => {
                                    let msg = payload
                                        .first()
                                        .map(|v| format!("called unwrap() on Err({})", v))
                                        .unwrap_or_else(|| "called unwrap() on Err".to_string());
                                    Err(InterpError::new(msg))
                                }
                                // is_some / is_none / is_ok / is_err
                                ("Some", "is_some") => Ok(Value::Bool(true)),
                                ("None", "is_some") => Ok(Value::Bool(false)),
                                ("Some", "is_none") => Ok(Value::Bool(false)),
                                ("None", "is_none") => Ok(Value::Bool(true)),
                                ("Ok", "is_ok") => Ok(Value::Bool(true)),
                                ("Err", "is_ok") => Ok(Value::Bool(false)),
                                ("Ok", "is_err") => Ok(Value::Bool(false)),
                                ("Err", "is_err") => Ok(Value::Bool(true)),
                                // and_then: call closure with payload on Some/Ok, pass through on None/Err
                                ("Some" | "Ok", "and_then") => {
                                    let closure_reg = args_base + 1;
                                    let closure = self.get_reg(closure_reg).clone();
                                    let arg = payload.first().cloned().unwrap_or(Value::Unit);
                                    self.call_closure(&closure, &[arg])
                                }
                                ("None", "and_then") => Ok(Value::Variant("None".into(), vec![])),
                                ("Err", "and_then") => Ok(receiver.clone()),
                                // map: transform payload on Some/Ok, pass through on None/Err
                                ("Some", "map") => {
                                    let closure_reg = args_base + 1;
                                    let closure = self.get_reg(closure_reg).clone();
                                    let arg = payload.first().cloned().unwrap_or(Value::Unit);
                                    let mapped = self.call_closure(&closure, &[arg])?;
                                    Ok(Value::Variant("Some".into(), vec![mapped]))
                                }
                                ("Ok", "map") => {
                                    let closure_reg = args_base + 1;
                                    let closure = self.get_reg(closure_reg).clone();
                                    let arg = payload.first().cloned().unwrap_or(Value::Unit);
                                    let mapped = self.call_closure(&closure, &[arg])?;
                                    Ok(Value::Variant("Ok".into(), vec![mapped]))
                                }
                                ("None", "map") => Ok(Value::Variant("None".into(), vec![])),
                                ("Err", "map") => Ok(receiver.clone()),
                                // map_err: transform error on Err, pass through on Ok/Some
                                ("Err", "map_err") => {
                                    let closure_reg = args_base + 1;
                                    let closure = self.get_reg(closure_reg).clone();
                                    let arg = payload.first().cloned().unwrap_or(Value::Unit);
                                    let mapped = self.call_closure(&closure, &[arg])?;
                                    Ok(Value::Variant("Err".into(), vec![mapped]))
                                }
                                ("Ok", "map_err") | ("Some", "map_err") => Ok(receiver.clone()),
                                // unwrap_or / value_or: payload on Some/Ok, default on None/Err
                                ("Some" | "Ok", "unwrap_or") | ("Some" | "Ok", "value_or") => {
                                    Ok(payload.first().cloned().unwrap_or(Value::Unit))
                                }
                                ("None" | "Err", "unwrap_or") | ("None" | "Err", "value_or") => {
                                    Ok(self.get_reg(args_base + 1).clone())
                                }
                                // ok_or: Option → Result (Some(v) → Ok(v), None → Err(e))
                                ("Some", "ok_or") => {
                                    Ok(Value::Variant("Ok".into(), payload.clone()))
                                }
                                ("None", "ok_or") => {
                                    let err_val = self.get_reg(args_base + 1).clone();
                                    Ok(Value::Variant("Err".into(), vec![err_val]))
                                }
                                _ => {
                                    return Err(InterpError::new(format!(
                                        "cannot call method '{}' on {}",
                                        method_name, receiver
                                    )));
                                }
                            };
                            self.set_reg(rd, result?);
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

                Op::SharedSet { ra, rb } => {
                    let val = self.get_reg(rb).clone();
                    let target = self.get_reg(ra).clone();
                    match target {
                        Value::Shared(arc) | Value::Ref(arc) | Value::RefMut(arc) => {
                            let mut guard = arc.write().map_err(|e| {
                                InterpError::new(format!("shared write lock failed: {}", e))
                            })?;
                            *guard = val;
                        }
                        Value::LocalShared(rc) => {
                            let mut guard = rc.lock().map_err(|e| {
                                InterpError::new(format!("local_shared lock failed: {}", e))
                            })?;
                            *guard = val;
                        }
                        other => {
                            return Err(InterpError::new(format!(
                                "cannot write through non-shared reference: {}",
                                other
                            )))
                        }
                    }
                }

                Op::WeakNew { rd, ra } => {
                    let v = self.get_reg(ra).clone();
                    let weak = match v {
                        Value::Shared(arc) => Value::WeakShared(std::sync::Arc::downgrade(&arc)),
                        _ => {
                            return Err(InterpError::new(format!(
                                "weak requires a shared value, got {}",
                                v
                            )))
                        }
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
                        return Err(InterpError::new("FaultRetEarly: no fault_reg set"));
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
    pub(crate) fn call_closure(
        &mut self,
        closure: &Value,
        args: &[Value],
    ) -> Result<Value, InterpError> {
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
            _ => Err(InterpError::new("call_closure: expected BytecodeClosure")),
        }
    }

    // ── FFI forwarding (0.33 Phase D) ────────────────────────

    /// Execute an extern (FFI) function call through the shared FfiRuntime.
    /// The VM itself acts as the closure-execution engine (`self_as_runner`).
    fn call_extern_idx(&mut self, extern_idx: u16, args: Vec<Value>) -> Result<Value, InterpError> {
        let name = self
            .program
            .extern_names
            .get(extern_idx as usize)
            .cloned()
            .ok_or_else(|| InterpError::new(format!("extern index {} out of range", extern_idx)))?;
        let extern_func = self
            .ffi_runtime
            .extern_funcs
            .get(&name)
            .cloned()
            .ok_or_else(|| InterpError::new(format!("extern function '{}' not found", name)))?;
        let contract = self
            .ffi_runtime
            .ffi_contracts
            .get(&name)
            .cloned()
            .unwrap_or_else(|| FfiContract::from_extern(&extern_func));
        let runner_ptr = Self::vm_as_runner(self);
        self.ffi_runtime
            .call_extern_with_runner_ptr(&extern_func, &contract, args, runner_ptr)
            .map_err(|e| InterpError::new(e.to_string()))
    }

    /// Convert `&mut self` into the raw engine pointer used by `FfiRuntime`.
    ///
    /// SAFETY: the returned pointer erases the `'a` lifetime to `'static`.
    /// Sound ONLY because the pointer is used synchronously inside a single
    /// C call that completes before `self`'s borrow ends (CRITICAL #4
    /// analysis in ffi_runtime.rs). Never stored or dereferenced after
    /// `call_extern_with_runner_ptr` returns. Isolated in this function so
    /// no borrow of `self` escapes into `self.ffi_runtime`.
    fn vm_as_runner(this: &mut Self) -> *mut (dyn FfiClosureRunner + 'static) {
        let runner: &mut (dyn FfiClosureRunner + '_) = this;
        let ptr: *mut (dyn FfiClosureRunner + '_) = runner as *mut (dyn FfiClosureRunner + '_);
        // SAFETY: same-size transmute (fat pointer to fat pointer); lifetime
        // erasure is sound per the function-level SAFETY contract.
        unsafe { std::mem::transmute(ptr) }
    }

    /// Call a function by index with the given arguments.
    /// Used by actor worker threads to execute actor methods.
    pub fn call_function(
        &mut self,
        func_idx: FuncIdx,
        args: &[Value],
    ) -> Result<Value, InterpError> {
        self.push_frame(func_idx, args.to_vec(), None)?;
        let prev_stop = self.stop_depth;
        self.stop_depth = self.depth;
        let result = self.exec_loop();
        self.stop_depth = prev_stop;
        result.map_err(|e| self.enrich_error(e))
    }

    /// Call a function by name (convenience wrapper for tests).
    /// Looks up the function index from the program's function table.
    pub fn call_named(&mut self, name: &str, args: Vec<Value>) -> Result<Value, InterpError> {
        let idx = self
            .program
            .functions
            .iter()
            .position(|f| f.name == name)
            .ok_or_else(|| InterpError::new(format!("function '{}' not found", name)))?;
        self.call_function(idx as FuncIdx, &args)
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
        // Collect contract args BEFORE mem::replace (ra may alias a param register).
        let contract_args = if self.verify_contracts && !is_early_return {
            let frame = self.cur_frame();
            let proto = &self.program.functions[frame.proto_idx as usize];
            if proto.has_ensures {
                Some((
                    frame.proto_idx,
                    (0..proto.param_count as usize)
                        .map(|i| frame.regs[i].clone())
                        .collect::<Vec<_>>(),
                    frame.old_snapshots.clone(),
                ))
            } else {
                None
            }
        } else {
            None
        };

        // v0.34.13: collect mutate-param values BEFORE mem::replace — if the
        // return register aliases a mutate parameter (e.g. `RET r0` where the
        // param lives at r0), replace would leave Unit in the param slot and
        // the write-back would silently store Unit into the caller.
        let mut_param_vals = self.collect_mut_param_vals();

        // Move value out of register (frame is about to be popped — no clone needed).
        let mut v = std::mem::replace(self.get_reg_mut(ra), Value::Unit);
        let frame = self.cur_frame();
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
                v = Value::Variant("Err".to_string(), vec![Value::Tuple(vec![src, error])]);
            } else {
                v = Value::Variant("Ok".to_string(), vec![v]);
            }
        }

        // Contract post-condition check (0.33 Phase F).
        if let Some((proto_idx, args, old_snapshots)) = contract_args {
            if !wrap_ok {
                self.check_ensures(proto_idx, &args, &old_snapshots, &v)?;
            }
        }

        self.pop_frame();
        self.depth -= 1;
        if self.stack.is_empty() || (stop > 0 && self.depth < stop) {
            return Ok(Some(v));
        }
        if let Some(rd) = return_reg {
            self.set_reg(rd, v);
        }
        if !mut_param_vals.is_empty() {
            self.apply_mutate_writeback(&mut_param_vals);
        }
        Ok(None)
    }

    /// Check `requires` contracts for a function call (0.33 Phase F).
    /// Uses a temporary tree-walker to evaluate contract expressions
    /// (same pattern as builtin_ast_eval).
    fn check_requires(&mut self, func_idx: FuncIdx, args: &[Value]) -> Result<(), InterpError> {
        let proto = &self.program.functions[func_idx as usize];
        if proto.requires_funcs.is_empty() {
            return Ok(());
        }
        let name = proto.name.clone();
        let contract_funcs = proto.requires_funcs.clone();
        for cidx in contract_funcs {
            let cond = self.call_function(cidx, args)?;
            if !crate::interp::value::is_truthy(&cond) {
                return Err(InterpError::contract_violation(format!(
                    "requires condition failed for '{}': {}",
                    name, cond
                )));
            }
        }
        Ok(())
    }

    /// Check `ensures` contracts for a function return (0.33 Phase F).
    fn check_ensures(
        &mut self,
        func_idx: FuncIdx,
        _args: &[Value],
        old_snapshots: &[Value],
        result: &Value,
    ) -> Result<(), InterpError> {
        let proto = &self.program.functions[func_idx as usize];
        if proto.ensures_funcs.is_empty() {
            return Ok(());
        }
        let name = proto.name.clone();
        let contract_funcs = proto.ensures_funcs.clone();
        // Ensures mini-functions take pre-call param snapshots + result.
        // old(x) compiles to just evaluating x, so we pass old_snapshots
        // (pre-call values) as the parameter arguments. This ensures old(x)
        // resolves to the pre-call value even for mut parameters.
        let mut ensures_args = old_snapshots.to_vec();
        ensures_args.push(result.clone());
        for cidx in contract_funcs {
            let cond = self.call_function(cidx, &ensures_args)?;
            if !crate::interp::value::is_truthy(&cond) {
                return Err(InterpError::contract_violation(format!(
                    "ensures condition failed for '{}': {}",
                    name, cond
                )));
            }
        }
        Ok(())
    }

    /// Capture the current frame's final `mut` parameter values
    /// (before the frame is destroyed by pop).
    fn collect_mut_param_vals(&self) -> Vec<Value> {
        let frame = self.cur_frame();
        let proto = &self.program.functions[frame.proto_idx as usize];
        if proto.mut_param_indices.is_empty() {
            Vec::new()
        } else {
            proto
                .mut_param_indices
                .iter()
                .map(|&i| frame.regs[i as usize].clone())
                .collect()
        }
    }

    /// Write captured `mut` parameter values back to the caller's
    /// registered target registers (mutate-parameter reference ABI).
    fn apply_mutate_writeback(&mut self, vals: &[Value]) {
        if let Some(caller) = self.stack.last_mut() {
            if let Some(targets) = caller.mutate_writebacks.take() {
                for (val, &target) in vals.iter().zip(targets.iter()) {
                    mimi_debug_assert!(
                        (target as usize) < caller.regs.len(),
                        "mutate writeback target {} out of bounds (len {})",
                        target,
                        caller.regs.len()
                    );
                    caller.regs[target as usize] = val.clone();
                }
            }
            // v0.34.13 (clause 6): payload member-level borrow — RecordSet
            // the final parameter value back into the caller's payload slot.
            if let Some(field_targets) = caller.mutate_field_writebacks.take() {
                for (val, (obj_reg, field)) in vals.iter().zip(field_targets.iter()) {
                    let obj = caller.regs.get_mut(*obj_reg as usize);
                    if let Some(Value::Record(_, fields)) = obj {
                        fields.insert(field.clone(), val.clone());
                    }
                }
            }
        }
    }

    // ── Builtin dispatch (D1: registry, not giant match) ─────

    fn call_builtin(&mut self, idx: BuiltinIdx, args: &[Value]) -> Result<Value, InterpError> {
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
    /// If a shared stdout_capture buffer is set, writes there instead of local stdout.
    pub fn append_stdout(&mut self, s: &str) {
        if let Some(buf) = &self.stdout_capture {
            if let Ok(mut g) = buf.lock() {
                g.push_str(s);
            }
        } else {
            self.stdout.push_str(s);
        }
    }

    /// Enable stdout capture with a new shared buffer.
    pub fn enable_stdout_capture(&mut self) {
        self.stdout_capture = Some(std::sync::Arc::new(std::sync::Mutex::new(String::new())));
    }

    /// Set the stdout capture buffer directly. Actor workers receive the
    /// spawning VM's buffer this way.
    pub fn set_stdout_buf(&mut self, buf: std::sync::Arc<std::sync::Mutex<String>>) {
        self.stdout_capture = Some(buf);
    }

    /// Get the stdout capture buffer (for passing to actor workers).
    pub fn stdout_buf(&self) -> Option<std::sync::Arc<std::sync::Mutex<String>>> {
        self.stdout_capture.clone()
    }

    /// Get captured stdout (for testing).
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    // ── Register access helpers (D8: centralized Value conversion) ──

    pub(crate) fn get_reg(&self, r: Reg) -> &Value {
        let frame = self.cur_frame();
        let idx = r as usize;
        mimi_debug_assert!(
            idx < frame.regs.len(),
            "register {} out of bounds (len {})",
            idx,
            frame.regs.len()
        );
        &frame.regs[idx]
    }

    pub(crate) fn get_reg_mut(&mut self, r: Reg) -> &mut Value {
        let len = self.cur_frame().regs.len();
        let idx = r as usize;
        mimi_debug_assert!(idx < len, "register {} out of bounds (len {})", idx, len);
        &mut self.cur_frame_mut().regs[idx]
    }

    pub(crate) fn set_reg(&mut self, r: Reg, v: Value) {
        let idx = r as usize;
        let len = self.cur_frame().regs.len();
        mimi_debug_assert!(idx < len, "register {} out of bounds (len {})", idx, len);
        self.cur_frame_mut().regs[idx] = v;
    }

    /// Look up a Str constant of the current frame's prototype.
    fn const_str(&self, idx: ConstIdx) -> Result<&str, InterpError> {
        match self.program.functions[self.cur_frame().proto_idx as usize]
            .constants
            .get(idx as usize)
        {
            Some(ConstValue::Str(s)) => Ok(s),
            _ => Err(InterpError::new("expected Str constant")),
        }
    }

    /// Pop the top of the quote assembly stack.
    fn quote_pop(&mut self) -> Result<crate::interp::value::QuotedAst, InterpError> {
        self.quote_stack
            .pop()
            .ok_or_else(|| InterpError::new("quote assembly: stack underflow"))
    }

    /// Pop `n` nodes (top-first), leaving them in popped order.
    fn quote_pop_n(&mut self, n: u16) -> Result<Vec<crate::interp::value::QuotedAst>, InterpError> {
        if (n as usize) > self.quote_stack.len() {
            return Err(InterpError::new(format!(
                "quote assembly: stack underflow (need {}, have {})",
                n,
                self.quote_stack.len()
            )));
        }
        let split = self.quote_stack.len() - n as usize;
        Ok(self.quote_stack.split_off(split))
    }

    pub(crate) fn get_int(&self, r: Reg) -> Result<i64, InterpError> {
        match self.get_reg(r) {
            Value::Int(v) => Ok(*v),
            other => Err(InterpError::new(format!("expected Int, got {}", other))),
        }
    }

    pub(crate) fn get_int2(&self, ra: Reg, rb: Reg) -> Result<(i64, i64), InterpError> {
        Ok((self.get_int(ra)?, self.get_int(rb)?))
    }

    pub(crate) fn get_float(&self, r: Reg) -> Result<f64, InterpError> {
        match self.get_reg(r) {
            Value::Float(v) => Ok(*v),
            Value::Int(v) => Ok(*v as f64), // Auto-coerce Int → Float
            other => Err(InterpError::new(format!("expected Float, got {}", other))),
        }
    }

    pub(crate) fn get_float2(&self, ra: Reg, rb: Reg) -> Result<(f64, f64), InterpError> {
        Ok((self.get_float(ra)?, self.get_float(rb)?))
    }

    pub(crate) fn get_bool(&self, r: Reg) -> Result<bool, InterpError> {
        match self.get_reg(r) {
            Value::Bool(v) => Ok(*v),
            other => Err(InterpError::new(format!("expected Bool, got {}", other))),
        }
    }

    pub(crate) fn get_str(&self, r: Reg) -> Result<String, InterpError> {
        match self.get_reg(r) {
            Value::String(v) => Ok(v.clone()),
            other => Err(InterpError::new(format!("expected String, got {}", other))),
        }
    }

    pub(crate) fn get_list(&self, r: Reg) -> Result<Vec<Value>, InterpError> {
        match self.get_reg(r) {
            Value::List(v) => Ok(v.clone()),
            other => Err(InterpError::new(format!("expected List, got {}", other))),
        }
    }

    pub(crate) fn check_float(&self, v: f64, op: &str) -> Result<(), InterpError> {
        // v0.34.10a (SD-9): inside `ieee_float { }` the finiteness invariant
        // is suspended — NaN/Inf are legitimate IEEE 754 values there.
        // H2 fix: ieee_depth is per-frame, so a callee's suspended state never
        // leaks into the caller (and an early return inside an ieee block,
        // whose IeeeExit is unreachable, dies with the frame).
        if self.cur_frame().ieee_depth > 0 {
            return Ok(());
        }
        if v.is_nan() || v.is_infinite() {
            return Err(InterpError::float_error(format!(
                "invalid floating-point result from {}",
                op
            )));
        }
        Ok(())
    }

    fn load_const(&self, proto: &FunctionProto, idx: ConstIdx) -> Value {
        mimi_debug_assert!(
            (idx as usize) < proto.constants.len(),
            "constant index {} out of bounds (len {})",
            idx,
            proto.constants.len()
        );
        match &proto.constants[idx as usize] {
            ConstValue::Int(v) => Value::Int(*v),
            ConstValue::Float(v) => Value::Float(*v),
            ConstValue::Bool(v) => Value::Bool(*v),
            ConstValue::Str(v) => Value::String(v.clone()),
            ConstValue::Unit => Value::Unit,
            ConstValue::Type(t) => Value::String(format!("<type {:?}>", t)),
            ConstValue::QuoteAst(q) => Value::QuoteAst(q.clone()),
            ConstValue::LambdaSpec { .. } => Value::Unit,
            ConstValue::Pattern(_) => Value::Unit,
            ConstValue::StrVec(_) => Value::Unit,
        }
    }

    // ── Actor spawn helper ───────────────────────────────────

    /// Evaluate a simple init expression for actor field initialization.
    /// Handles literals, negative literals, lists, tuples, records, and common builtins.
    fn eval_init_expr(expr: &crate::ast::Expr) -> Option<Value> {
        use crate::ast::{Expr, Lit, UnOp};
        match expr {
            Expr::Literal(Lit::Int(n)) => Some(Value::Int(*n)),
            Expr::Literal(Lit::Float(f)) => Some(Value::Float(*f)),
            Expr::Literal(Lit::Bool(b)) => Some(Value::Bool(*b)),
            Expr::Literal(Lit::String(s)) => Some(Value::String(s.clone())),
            Expr::Literal(Lit::Unit) => Some(Value::Unit),
            // Negative literals: -42, -3.14
            Expr::Unary(UnOp::Neg, inner) => match Self::eval_init_expr(inner.unlocated())? {
                Value::Int(n) => Some(Value::Int(-n)),
                Value::Float(f) => Some(Value::Float(-f)),
                _ => None,
            },
            // List literal: [expr, expr, ...]
            Expr::List(elems) => {
                let mut items = Vec::new();
                for elem in elems {
                    items.push(Self::eval_init_expr(elem.unlocated())?);
                }
                Some(Value::List(items))
            }
            // Tuple literal: (expr, expr, ...)
            Expr::Tuple(elems) => {
                let mut items = Vec::new();
                for elem in elems {
                    items.push(Self::eval_init_expr(elem.unlocated())?);
                }
                Some(Value::Tuple(items))
            }
            // Record literal: Type { field: expr, ... }
            Expr::Record { ty, fields } => {
                let mut map = std::collections::HashMap::new();
                for field in fields {
                    let val = Self::eval_init_expr(field.value.unlocated())?;
                    map.insert(field.name.clone(), val);
                }
                Some(Value::Record(ty.clone(), map))
            }
            // Common builtin calls: map_new(), set_new(), etc.
            Expr::Call(callee, args) if args.is_empty() => {
                if let Expr::Ident(name) = callee.unlocated() {
                    match name.as_str() {
                        "map_new" => Some(Value::Record(None, std::collections::HashMap::new())),
                        "set_new" => Some(Value::Set(Vec::new())),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None, // complex init: not supported
        }
    }

    /// Spawn an actor by name, reusing the ActorHandle infrastructure.
    /// The actor's worker thread uses BytecodeVM internally (v0.33 migration).
    /// Main program and actor workers both run on bytecode.
    pub(crate) fn spawn_actor(
        &mut self,
        actor_name: &str,
        detached: bool,
    ) -> Result<Value, InterpError> {
        use crate::interp::value::{ActorHandle, ActorInstance};
        use std::collections::HashMap;

        // Check spawn quota.
        if let Some(max) = self.max_children {
            if self.spawn_count >= max {
                return Err(InterpError::new(
                    "QuotaExceeded: spawn would exceed @max_children limit",
                ));
            }
        }

        let actor_def = self
            .program
            .actor_defs
            .get(actor_name)
            .ok_or_else(|| InterpError::new(format!("actor '{}' not found", actor_name)))?;

        // Initialize fields with defaults.
        let mut fields = HashMap::new();
        for field in &actor_def.fields {
            let value = match field.ty.unlocated() {
                crate::ast::Type::Name(n, _) if n == "i32" || n == "i64" => Value::Int(0),
                crate::ast::Type::Name(n, _) if n == "f64" => Value::Float(0.0),
                crate::ast::Type::Name(n, _) if n == "bool" => Value::Bool(false),
                crate::ast::Type::Name(n, _) if n == "string" => Value::String(String::new()),
                crate::ast::Type::Name(n, _) if n == "List" || n == "Vec" => {
                    Value::List(Vec::new())
                }
                crate::ast::Type::Name(n, _) if n == "Map" => {
                    Value::Record(None, std::collections::HashMap::new())
                }
                crate::ast::Type::Name(n, _) if n == "Set" => Value::Set(Vec::new()),
                _ => Value::Unit,
            };
            // If there's an init expression that's a simple literal, evaluate it.
            let value = if let Some(init) = &field.init {
                Self::eval_init_expr(init.unlocated()).unwrap_or(value)
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
                                        crate::ast::Type::Name(n, _)
                                            if n == "i32" || n == "i64" =>
                                        {
                                            Value::Int(0)
                                        }
                                        crate::ast::Type::Name(n, _) if n == "f64" => {
                                            Value::Float(0.0)
                                        }
                                        crate::ast::Type::Name(n, _) if n == "bool" => {
                                            Value::Bool(false)
                                        }
                                        crate::ast::Type::Name(n, _) if n == "string" => {
                                            Value::String(String::new())
                                        }
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
                if id == 0 {
                    None
                } else {
                    Some(id)
                }
            }),
            is_detached: detached,
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
        let handle = ActorHandle::new_bytecode(instance, program, bc_prog, self.stdout_buf());
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
            "value_to_f64: expected numeric type, got {:?}",
            other
        ))),
    }
}

/// True when the error is a runtime panic (div-by-zero, overflow, OOB, …)
/// that a flow transition absorbs into a Fault. Programming errors
/// (undefined names, arity mismatches, …) always propagate.
fn is_runtime_panic(e: &InterpError) -> bool {
    matches!(
        e,
        InterpError::DivisionByZero(_)
            | InterpError::IntegerOverflow(_)
            | InterpError::IndexOutOfBounds(_)
            | InterpError::NonExhaustiveMatch(_)
            | InterpError::FloatError(_)
            | InterpError::SliceError(_)
            | InterpError::ContractViolation(_)
    )
}

/// Copy persistent field values from `from` into the Fault record's fields
/// (mirror of tree-walker shadow_persistent_into_fault).
fn shadow_persistent_into_fault(fault: &mut Value, from: &Value, persistent: &[String]) {
    if persistent.is_empty() {
        return;
    }
    let (Value::Record(_, from_fields), Value::Record(_, fault_fields)) = (from, fault) else {
        return;
    };
    for name in persistent {
        if let Some(v) = from_fields.get(name) {
            fault_fields.insert(name.clone(), v.clone());
        }
    }
}

/// Extract the field map of a Record value (empty map otherwise).
fn record_fields_of(v: &Value) -> std::collections::HashMap<String, Value> {
    match v {
        Value::Record(_, fields) => fields.clone(),
        _ => std::collections::HashMap::new(),
    }
}

/// Default value for a runtime sample (mirror of tree-walker
/// default_value_for_runtime).
fn default_value_for_runtime(sample: &Value) -> Value {
    match sample {
        Value::Int(_) => Value::Int(0),
        Value::Float(_) => Value::Float(0.0),
        Value::Bool(_) => Value::Bool(false),
        Value::String(_) => Value::String(String::new()),
        Value::List(_) => Value::List(vec![]),
        Value::Unit => Value::Unit,
        other => other.clone(), // keep shape for complex types
    }
}

/// Build a default record value for a named type from `record_fields` metadata
/// (type name → [(field name, field type str)]). Used by typed-fault panic
/// absorption to default the `error` field, matching the codegen backend
/// (v0.34.18b). Nested record fields recurse; unknown/compound types default to
/// Unit (error payloads are typically flat scalar records).
fn default_record_value(
    type_name: &str,
    record_fields: &std::collections::HashMap<String, Vec<(String, String)>>,
) -> Value {
    let mut fields = std::collections::HashMap::new();
    if let Some(field_defs) = record_fields.get(type_name) {
        for (fname, fty) in field_defs {
            fields.insert(
                fname.clone(),
                default_value_for_type_str(fty, record_fields),
            );
        }
    }
    Value::Record(Some(type_name.to_string()), fields)
}

/// Default value for a field type string (from `fmt_type`).
fn default_value_for_type_str(
    ty: &str,
    record_fields: &std::collections::HashMap<String, Vec<(String, String)>>,
) -> Value {
    match ty {
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "int" => Value::Int(0),
        "f32" | "f64" | "float" => Value::Float(0.0),
        "bool" => Value::Bool(false),
        "string" => Value::String(String::new()),
        "unit" => Value::Unit,
        other => {
            if record_fields.contains_key(other) {
                default_record_value(other, record_fields)
            } else {
                Value::Unit
            }
        }
    }
}

/// FfiClosureRunner implementation: the bytecode VM can execute Mimi
/// closures (BytecodeClosure) from C callback trampolines, and provides the
/// program File for cross-thread callback evaluation.
impl<'a> FfiClosureRunner for BytecodeVM<'a> {
    fn ffi_file(&self) -> &crate::ast::File {
        self.program
            .ast
            .as_ref()
            .map(|f| f.as_ref() as &crate::ast::File)
            .expect("BytecodeVM FFI: program AST is required (compiler always sets it)")
    }

    fn apply_closure_ffi(&mut self, closure: &Value, args: Vec<Value>) -> Result<Value, String> {
        self.call_closure(closure, &args).map_err(|e| e.to_string())
    }

    fn ffi_bytecode_program(
        &self,
    ) -> Option<*const crate::interp::bytecode::instr::BytecodeProgram> {
        Some(self.program as *const crate::interp::bytecode::instr::BytecodeProgram)
    }

    fn eval_contract_expr(
        &mut self,
        _expr: &crate::ast::Expr,
        _result_binding: Option<&Value>,
    ) -> Result<Value, String> {
        Err(
            "FFI contract evaluation is not supported in the bytecode VM yet (0.33 Phase D+)"
                .to_string(),
        )
    }
}
