//! Bytecode instruction set for the Mimi register-based VM.
//!
//! Design principles:
//! - Register-based: instructions reference registers by u16 index
//! - Typed arithmetic: separate Int/Float variants (static type → zero dispatch)
//! - Constant pool: literals indexed by u32 into per-function pool
//! - Relative jumps: i32 offsets from current instruction
//! - Call convention: args in consecutive registers, result in rd

/// Register index (u16: max 65536 locals per function — far more than needed).
pub type Reg = u16;

/// Constant pool index.
pub type ConstIdx = u32;

/// Function prototype index (into the program's function table).
pub type FuncIdx = u32;

/// Builtin function index.
pub type BuiltinIdx = u32;

/// Jump offset (relative to current instruction position).
pub type JumpOff = i32;

/// Field index in a record type (resolved at compile time).
pub type FieldIdx = u16;

/// Enum variant ordinal.
pub type VariantIdx = u16;

/// Instruction opcodes.
///
/// Naming convention:
/// - `LOAD_*` / `STORE_*`: memory ↔ register
/// - `MOV`: register → register
/// - `*_INT` / `*_FLOAT`: typed arithmetic (no runtime dispatch)
/// - `JMP*`: control flow
/// - `CALL*`: function invocation
/// - `NEW_*`: heap object construction
/// - `GET_*` / `SET_*`: field/element access
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    // ═══════════════════════════════════════════════════════════
    // Constants & moves
    // ═══════════════════════════════════════════════════════════

    /// rd = constant_pool[idx]
    LoadConst { rd: Reg, idx: ConstIdx },
    /// rd = Value::Unit
    LoadUnit { rd: Reg },
    /// rd = Value::Bool(true)
    LoadTrue { rd: Reg },
    /// rd = Value::Bool(false)
    LoadFalse { rd: Reg },
    /// rd = rs (shallow copy; Value::Int/Float/Bool are Copy)
    Mov { rd: Reg, rs: Reg },

    // ═══════════════════════════════════════════════════════════
    // Integer arithmetic (checked: trap on overflow per SD-7)
    // ═══════════════════════════════════════════════════════════

    AddInt { rd: Reg, ra: Reg, rb: Reg },
    SubInt { rd: Reg, ra: Reg, rb: Reg },
    MulInt { rd: Reg, ra: Reg, rb: Reg },
    /// Trap on rb == 0 (E0801 per SD-8)
    DivInt { rd: Reg, ra: Reg, rb: Reg },
    /// Trap on rb == 0
    ModInt { rd: Reg, ra: Reg, rb: Reg },
    NegInt { rd: Reg, ra: Reg },

    // ═══════════════════════════════════════════════════════════
    // Float arithmetic (finiteness invariant per SD-9)
    // ═══════════════════════════════════════════════════════════

    AddFloat { rd: Reg, ra: Reg, rb: Reg },
    SubFloat { rd: Reg, ra: Reg, rb: Reg },
    MulFloat { rd: Reg, ra: Reg, rb: Reg },
    DivFloat { rd: Reg, ra: Reg, rb: Reg },
    NegFloat { rd: Reg, ra: Reg },

    // ═══════════════════════════════════════════════════════════
    // Mixed int/float (int operand promoted to f64)
    // ═══════════════════════════════════════════════════════════

    IntToFloat { rd: Reg, ra: Reg },

    // ═══════════════════════════════════════════════════════════
    // Comparison (rd = Bool)
    // ═══════════════════════════════════════════════════════════

    EqInt { rd: Reg, ra: Reg, rb: Reg },
    NeInt { rd: Reg, ra: Reg, rb: Reg },
    LtInt { rd: Reg, ra: Reg, rb: Reg },
    GtInt { rd: Reg, ra: Reg, rb: Reg },
    LeInt { rd: Reg, ra: Reg, rb: Reg },
    GeInt { rd: Reg, ra: Reg, rb: Reg },

    EqFloat { rd: Reg, ra: Reg, rb: Reg },
    LtFloat { rd: Reg, ra: Reg, rb: Reg },
    GtFloat { rd: Reg, ra: Reg, rb: Reg },
    LeFloat { rd: Reg, ra: Reg, rb: Reg },
    GeFloat { rd: Reg, ra: Reg, rb: Reg },

    /// Generic equality (Value::eq — strings, lists, records, etc.)
    Eq { rd: Reg, ra: Reg, rb: Reg },
    Ne { rd: Reg, ra: Reg, rb: Reg },

    // ═══════════════════════════════════════════════════════════
    // Bitwise (integers only)
    // ═══════════════════════════════════════════════════════════

    BitAnd { rd: Reg, ra: Reg, rb: Reg },
    BitOr { rd: Reg, ra: Reg, rb: Reg },
    BitXor { rd: Reg, ra: Reg, rb: Reg },
    Shl { rd: Reg, ra: Reg, rb: Reg },
    Shr { rd: Reg, ra: Reg, rb: Reg },
    BitNot { rd: Reg, ra: Reg },

    // ═══════════════════════════════════════════════════════════
    // Logical
    // ═══════════════════════════════════════════════════════════

    Not { rd: Reg, ra: Reg },
    /// rd = ra && rb (logical AND, short-circuit not needed at bytecode level)
    And { rd: Reg, ra: Reg, rb: Reg },
    /// rd = ra || rb (logical OR)
    Or { rd: Reg, ra: Reg, rb: Reg },

    // ═══════════════════════════════════════════════════════════
    // String
    // ═══════════════════════════════════════════════════════════

    /// rd = concat(ra, rb) — string concatenation
    ConcatStr { rd: Reg, ra: Reg, rb: Reg },

    // ═══════════════════════════════════════════════════════════
    // Control flow
    // ═══════════════════════════════════════════════════════════

    /// Unconditional jump: pc += offset
    Jmp { offset: JumpOff },
    /// Jump if ra is truthy: if truthy(ra) { pc += offset }
    JmpIf { offset: JumpOff, ra: Reg },
    /// Jump if ra is falsy: if !truthy(ra) { pc += offset }
    JmpIfNot { offset: JumpOff, ra: Reg },

    // ═══════════════════════════════════════════════════════════
    // Function calls
    // ═══════════════════════════════════════════════════════════

    /// Call user function: rd = func[idx](args[0..argc])
    /// Arguments are in registers [args_base .. args_base + argc).
    Call {
        rd: Reg,
        func: FuncIdx,
        args_base: Reg,
        argc: u16,
    },
    /// Call builtin: rd = builtin[idx](args[0..argc])
    CallBuiltin {
        rd: Reg,
        builtin: BuiltinIdx,
        args_base: Reg,
        argc: u16,
    },
    /// Indirect call through a closure value in register `callee`:
    /// rd = ra(args[0..argc])
    CallIndirect {
        rd: Reg,
        callee: Reg,
        args_base: Reg,
        argc: u16,
    },
    /// Return from function: return ra
    Ret { ra: Reg },
    /// Return Unit
    RetUnit,

    // ═══════════════════════════════════════════════════════════
    // Data structures
    // ═══════════════════════════════════════════════════════════

    /// rd = new List with `capacity` pre-allocated slots
    NewList { rd: Reg, capacity: u32 },
    /// Append rb to list in ra
    ListPush { ra: Reg, rb: Reg },
    /// rd = ra[rb] (list index; rb is Int)
    ListGet { rd: Reg, ra: Reg, rb: Reg },
    /// ra[rb] = rc (list set)
    ListSet { ra: Reg, rb: Reg, rc: Reg },
    /// rd = len(ra)
    Len { rd: Reg, ra: Reg },

    /// rd = new Tuple with `arity` elements from [base .. base+arity)
    NewTuple { rd: Reg, base: Reg, arity: u16 },
    /// rd = ra.idx (tuple field access)
    TupleGet { rd: Reg, ra: Reg, idx: FieldIdx },

    /// rd = new Record of type `type_name_idx` with fields from [base..base+count)
    NewRecord {
        rd: Reg,
        type_name: ConstIdx,
        base: Reg,
        count: u16,
    },
    /// rd = ra.field[field_idx]
    RecordGet { rd: Reg, ra: Reg, field: FieldIdx },
    /// ra.field[field_idx] = rb
    RecordSet { ra: Reg, field: FieldIdx, rb: Reg },

    /// rd = new Map (empty)
    NewMap { rd: Reg },
    /// rd = new Set (empty)
    NewSet { rd: Reg },

    // ═══════════════════════════════════════════════════════════
    // Enum / pattern matching
    // ═══════════════════════════════════════════════════════════

    /// rd = Variant(type_name, variant_idx, payload[0..arity])
    NewVariant {
        rd: Reg,
        type_name: ConstIdx,
        variant: VariantIdx,
        base: Reg,
        arity: u16,
    },
    /// rd = variant_tag(ra) — extract tag as Int
    VariantTag { rd: Reg, ra: Reg },
    /// rd = variant_payload(ra, idx) — extract payload field
    VariantPayload { rd: Reg, ra: Reg, idx: FieldIdx },
    /// rd = is_variant(ra, tag) — check if ra is a Variant with the given tag
    IsVariant { rd: Reg, ra: Reg, tag: ConstIdx },
    /// rd = variant_get(ra, idx) — extract payload field (alias for VariantPayload)
    VariantGet { rd: Reg, ra: Reg, idx: u16 },

    // ═══════════════════════════════════════════════════════════
    // Option / Result
    // ═══════════════════════════════════════════════════════════

    /// rd = Some(ra)
    Some { rd: Reg, ra: Reg },
    /// rd = None
    None { rd: Reg },
    /// rd = Ok(ra)
    Ok { rd: Reg, ra: Reg },
    /// rd = Err(ra)
    Err { rd: Reg, ra: Reg },
    /// rd = is_some(ra)
    IsSome { rd: Reg, ra: Reg },
    /// rd = unwrap(ra) — trap if None
    Unwrap { rd: Reg, ra: Reg },

    // ═══════════════════════════════════════════════════════════
    // Closures
    // ═══════════════════════════════════════════════════════════

    /// rd = new Closure(proto_idx, captures[0..capture_count])
    NewClosure {
        rd: Reg,
        proto: FuncIdx,
        captures_base: Reg,
        capture_count: u16,
    },

    // ═══════════════════════════════════════════════════════════
    // Concurrency
    // ═══════════════════════════════════════════════════════════

    /// rd = spawn(func_idx, args[0..argc])
    Spawn {
        rd: Reg,
        func: FuncIdx,
        args_base: Reg,
        argc: u16,
    },
    /// rd = await(ra)
    Await { rd: Reg, ra: Reg },

    // ═══════════════════════════════════════════════════════════
    // Misc
    // ═══════════════════════════════════════════════════════════

    /// rd = ra as f64 / i32 / etc. (type cast)
    Cast { rd: Reg, ra: Reg, target: u8 },
    /// rd = to_string(ra)
    ToString { rd: Reg, ra: Reg },
    /// rd = typeof(ra) as string
    TypeOf { rd: Reg, ra: Reg },
    /// Trap with message from constant pool
    Trap { msg: ConstIdx },
    /// No operation
    Nop,
}

/// A compiled function: constant pool + instruction stream + metadata.
#[derive(Debug, Clone)]
pub struct FunctionProto {
    /// Function name (for debugging / stack traces).
    pub name: String,
    /// Number of parameters.
    pub param_count: u16,
    /// Number of registers needed (params + locals + temporaries).
    pub register_count: u16,
    /// Whether this function has `mut` parameters.
    pub has_mut_params: bool,
    /// Whether this function is async.
    pub is_async: bool,
    /// Constant pool (literals, string names, type names).
    pub constants: Vec<ConstValue>,
    /// Instruction stream.
    pub code: Vec<Op>,
    /// Source line table: maps instruction index → source line (for errors).
    pub line_table: Vec<u32>,
}

/// Compile-time constant values stored in the constant pool.
#[derive(Debug, Clone)]
pub enum ConstValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    /// Unit constant (shared).
    Unit,
}

impl FunctionProto {
    pub fn new(name: String, param_count: u16) -> Self {
        FunctionProto {
            name,
            param_count,
            register_count: param_count,
            has_mut_params: false,
            is_async: false,
            constants: Vec::new(),
            code: Vec::new(),
            line_table: Vec::new(),
        }
    }

    /// Allocate a new register and return its index.
    pub fn alloc_reg(&mut self) -> Reg {
        let r = self.register_count;
        self.register_count += 1;
        r
    }

    /// Add a constant to the pool, returning its index.
    /// Deduplicates strings and integers.
    pub fn add_const(&mut self, val: ConstValue) -> ConstIdx {
        // Dedup for common cases
        for (i, existing) in self.constants.iter().enumerate() {
            if std::mem::discriminant(existing) == std::mem::discriminant(&val) {
                match (&existing, &val) {
                    (ConstValue::Int(a), ConstValue::Int(b)) if a == b => return i as ConstIdx,
                    (ConstValue::Str(a), ConstValue::Str(b)) if a == b => return i as ConstIdx,
                    (ConstValue::Float(a), ConstValue::Float(b)) if a == b => return i as ConstIdx,
                    _ => {}
                }
            }
        }
        self.constants.push(val);
        (self.constants.len() - 1) as ConstIdx
    }

    /// Emit an instruction and return its index.
    pub fn emit(&mut self, op: Op) -> usize {
        self.code.push(op);
        self.code.len() - 1
    }

    /// Patch a jump instruction at `idx` with the correct offset
    /// to jump to the current end of code.
    pub fn patch_jump(&mut self, idx: usize) {
        let target = self.code.len() as i32;
        self.patch_jump_to(idx, target as usize);
    }

    /// Patch a jump instruction at `idx` to jump to an explicit target instruction.
    pub fn patch_jump_to(&mut self, idx: usize, target: usize) {
        let origin = idx as i32;
        let offset = target as i32 - origin - 1; // -1: relative to next instruction
        match &mut self.code[idx] {
            Op::Jmp { offset: o } => *o = offset,
            Op::JmpIf { offset: o, .. } => *o = offset,
            Op::JmpIfNot { offset: o, .. } => *o = offset,
            _ => {}
        }
    }
}

/// A compiled program: all function prototypes + global metadata.
#[derive(Debug, Clone)]
pub struct BytecodeProgram {
    /// Function prototypes indexed by FuncIdx.
    pub functions: Vec<FunctionProto>,
    /// Entry point (main function) index.
    pub entry: FuncIdx,
    /// Builtin function name → BuiltinIdx mapping.
    pub builtin_names: Vec<String>,
}
