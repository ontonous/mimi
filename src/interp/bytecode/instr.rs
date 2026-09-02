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
    LoadConst {
        rd: Reg,
        idx: ConstIdx,
    },
    /// rd = Value::Unit
    LoadUnit {
        rd: Reg,
    },
    /// rd = Value::Bool(true)
    LoadTrue {
        rd: Reg,
    },
    /// rd = Value::Bool(false)
    LoadFalse {
        rd: Reg,
    },
    /// rd = rs (shallow copy; Value::Int/Float/Bool are Copy)
    Mov {
        rd: Reg,
        rs: Reg,
    },
    /// rd = rs and consume the source register.  This is the runtime
    /// realization of canonical MIR `Move` for a non-Copy value.
    Move {
        rd: Reg,
        rs: Reg,
    },
    /// rd = clone(rs) using the TypeDesc-selected ownership glue.
    Clone {
        rd: Reg,
        rs: Reg,
    },
    /// Release the value held by ra and mark the register unavailable.
    Drop {
        ra: Reg,
    },
    /// Release a canonical aggregate after the MIR TypeDesc validator has
    /// proved its recursive drop plan and physical arity.
    DropAggregate {
        ra: Reg,
        arity: u16,
    },
    /// Consume an owned Option/Result variant after the MIR TypeDesc
    /// validator has proved the complete variant drop-shape table.
    DropVariant {
        ra: Reg,
        shapes: ConstIdx,
    },

    // ═══════════════════════════════════════════════════════════
    // Integer arithmetic (checked: trap on overflow per SD-7)
    // ═══════════════════════════════════════════════════════════
    AddInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    SubInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    MulInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// Trap on rb == 0 (E0801 per SD-8)
    DivInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// Trap on rb == 0
    ModInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    NegInt {
        rd: Reg,
        ra: Reg,
    },

    // ═══════════════════════════════════════════════════════════
    // Float arithmetic (finiteness invariant per SD-9)
    // ═══════════════════════════════════════════════════════════
    AddFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    SubFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    MulFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    DivFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    NegFloat {
        rd: Reg,
        ra: Reg,
    },

    // ═══════════════════════════════════════════════════════════
    // Mixed int/float (int operand promoted to f64)
    // ═══════════════════════════════════════════════════════════
    IntToFloat {
        rd: Reg,
        ra: Reg,
    },

    // ═══════════════════════════════════════════════════════════
    // Comparison (rd = Bool)
    // ═══════════════════════════════════════════════════════════
    EqInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    NeInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    LtInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    GtInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    LeInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    GeInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },

    EqFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    LtFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    GtFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    LeFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    GeFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },

    /// Generic equality (Value::eq — strings, lists, records, etc.)
    Eq {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    Ne {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },

    // ═══════════════════════════════════════════════════════════
    // Bitwise (integers only)
    // ═══════════════════════════════════════════════════════════
    BitAnd {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    BitOr {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    BitXor {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    Shl {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    Shr {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// i32 width-fidelity guard (0.34.34, SD-7 / L1 alignment).
    /// Trap (E0802) if the Int in `rd` is outside [i32::MIN, i32::MAX].
    /// `kind` selects the codegen-matching message:
    /// 0 = addition, 1 = subtraction, 2 = multiplication, 3 = generic.
    /// Emitted after int arithmetic whose declared result type is i32
    /// (the VM stores all ints as i64; codegen computes native i32 with
    /// checked overflow — without this guard the VM silently wraps at i64).
    CheckI32 {
        rd: Reg,
        kind: u8,
    },
    /// i32 division/remainder guard: trap if ra == i32::MIN && rb == -1.
    /// In i64 arithmetic MIN_i32 / -1 does not overflow, but codegen's
    /// native i32 checked sdiv/srem traps with
    /// "integer division overflow (MIN / -1)" — this aligns the VM.
    CheckI32DivRem {
        ra: Reg,
        rb: Reg,
    },
    /// Truncate the Int in `rd` to i32 with wrap-around (no trap).
    /// Parity with codegen semantics where the result narrows to the
    /// declared i32 width: integer `**` on i32 operands (runtime pow
    /// computes i64 then narrows — observed: 2**31 wraps to i32::MIN),
    /// and constant-folded values landing in i32 bindings.
    WrapI32 {
        rd: Reg,
    },
    /// Mask the shift-amount register in place: rb = (rb as u64 & mask) as i64.
    /// Parity with codegen/hardware shift semantics (x86 SHL/SAR and
    /// aarch64 LSL/ASR mask the amount modulo the operand width); also
    /// prevents LLVM from folding unmasked out-of-range shifts to poison
    /// at O1. mask is 31 (i32) or 63 (i64).
    MaskShiftAmt {
        rb: Reg,
        mask: u8,
    },
    /// rd = ra ** rb (integer power, checked overflow)
    PowInt {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// rd = ra ** rb (float power via powf)
    PowFloat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    BitNot {
        rd: Reg,
        ra: Reg,
    },

    // ═══════════════════════════════════════════════════════════
    // Logical
    // ═══════════════════════════════════════════════════════════
    Not {
        rd: Reg,
        ra: Reg,
    },
    /// rd = ra && rb (logical AND, short-circuit not needed at bytecode level)
    And {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// rd = ra || rb (logical OR)
    Or {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },

    // ═══════════════════════════════════════════════════════════
    // String
    // ═══════════════════════════════════════════════════════════
    /// rd = concat(ra, rb) — string concatenation
    ConcatStr {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },

    /// ra += rb — in-place string append (avoids O(n²) realloc in loops)
    StrAppend {
        ra: Reg,
        rb: Reg,
    },

    // ═══════════════════════════════════════════════════════════
    // Control flow
    // ═══════════════════════════════════════════════════════════
    /// Unconditional jump: pc += offset
    Jmp {
        offset: JumpOff,
    },
    /// Jump if ra is truthy: if truthy(ra) { pc += offset }
    JmpIf {
        offset: JumpOff,
        ra: Reg,
    },
    /// Jump if ra is falsy: if !truthy(ra) { pc += offset }
    JmpIfNot {
        offset: JumpOff,
        ra: Reg,
    },

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
    /// Canonical-MIR call: consume the materialized argument range into the
    /// callee frame. Legacy bytecode keeps `Call`'s clone-compatible ABI.
    CallMove {
        rd: Reg,
        func: FuncIdx,
        args_base: Reg,
        argc: u16,
    },
    /// Unwrap a Shared/LocalShared/WeakShared value (`*x` on shared vars).
    /// Other values pass through unchanged (value semantics).
    DerefValue {
        rd: Reg,
        ra: Reg,
    },
    /// Record mutate-parameter writeback targets for the next Call.
    /// `count` registers at [regs_base .. regs_base + count) hold the
    /// CALLER's register numbers of the `mut` arguments (one per
    /// function mut_param_indices entry, in the same order). On Ret,
    /// the callee's final parameter values are written back to those
    /// caller registers (mutate-parameter reference ABI, 0.33 Phase F).
    MutateSetup {
        regs_base: Reg,
        count: u16,
    },
    /// v0.34.13: mutate writeback targets that are record FIELDS of the
    /// caller (payload member-level borrow, clause 6). `count` targets, each
    /// occupying 2 registers at [regs_base ..): (obj_reg, field_const_idx).
    /// On callee Ret, the final parameter value is RecordSet into
    /// caller.regs[obj_reg] at field — writes `mutate self.field` back to
    /// the payload slot (golden §3.3; previously silently dropped).
    MutateSetupField {
        regs_base: Reg,
        count: u16,
    },
    /// Call builtin: rd = builtin[idx](args[0..argc])
    CallBuiltin {
        rd: Reg,
        builtin: BuiltinIdx,
        args_base: Reg,
        argc: u16,
    },
    /// Call extern (FFI) function: rd = extern(idx)(args[0..argc]).
    /// The index refers to BytecodeProgram::extern_names; the name is
    /// resolved against the shared `FfiRuntime` table at runtime
    /// (0.33 Phase D: FFI forwarding).
    CallExtern {
        rd: Reg,
        extern_idx: u16,
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
    // ═══════════════════════════════════════════════════════════
    // Quote assembly (0.33 Phase F: quote!/interpolation)
    // ═══════════════════════════════════════════════════════════
    // `quote! { ... }` builds a QuotedAst value at runtime. The compiler
    // emits these stack-machine ops against `BytecodeVM::quote_stack`;
    // QuoteResult pops the finished node into a register. All field
    // operands are Copy (Reg/u16/BinOp/UnOp/ConstIdx) so Op stays Copy.
    /// push QuotedAst::Literal from constant pool (Int/Float/Bool/String/Unit)
    QuotePushLit {
        const_idx: ConstIdx,
    },
    /// push QuotedAst::Ident(name) — name is Str in the constant pool
    QuotePushIdent {
        str_idx: ConstIdx,
    },
    /// push QuotedAst::Interpolate(regs[rs])
    QuoteInterpPush {
        rs: Reg,
    },
    /// `rs` holds a QuoteAst value → push its inner QuotedAst (nested quote)
    QuoteAstPush {
        rs: Reg,
    },
    /// record (name ← reg) capture for ast_eval env; name is Str in constants
    QuoteCapture {
        str_idx: ConstIdx,
        reg: Reg,
    },
    /// pop n nodes → push QuotedAst::Block
    QuoteBlock {
        n: u16,
    },
    /// pop n nodes → push QuotedAst::List
    QuoteList {
        n: u16,
    },
    /// pop n nodes → push QuotedAst::Tuple
    QuoteTuple {
        n: u16,
    },
    /// pop (rhs, lhs) → push QuotedAst::Binary(op, lhs, rhs)
    QuoteBinary {
        op: crate::ast::BinOp,
    },
    /// pop e → push QuotedAst::Unary(op, e)
    QuoteUnary {
        op: crate::ast::UnOp,
    },
    /// pop (argc args, callee) → push QuotedAst::Call(callee, args)
    QuoteCall {
        argc: u16,
    },
    /// pop obj → push QuotedAst::Field(obj, name)
    QuoteField {
        str_idx: ConstIdx,
    },
    /// pop (idx, obj) → push QuotedAst::Index(obj, idx)
    QuoteIndex,
    /// pop (else_node, then_node, cond) → push QuotedAst::If(cond, then, else_)
    QuoteIf {
        has_else: bool,
    },
    /// pop e → push QuotedAst::Try(e)
    QuoteTry,
    /// pop value → push QuotedAst::Let { name, value }
    QuoteLet {
        str_idx: ConstIdx,
    },
    /// pop inner → push QuotedAst::Cast(inner, ty); ty is ConstValue::Type
    QuoteCast {
        type_idx: ConstIdx,
    },
    /// pop e → push QuotedAst::ExprStmt(e)
    QuoteExprStmt,
    /// pop e → push QuotedAst::Return(Some(e)); or Return(None) when empty
    QuoteReturn {
        has_value: bool,
    },
    /// pop (body, cond) → push QuotedAst::While(cond, body)
    QuoteWhile,
    /// pop (body, init) → push QuotedAst::WhileLet { pat, init, body }; pat is Pattern in constants
    QuoteWhileLet {
        pat_idx: ConstIdx,
    },
    /// pop value (if has_value) → push QuotedAst::Break
    QuoteBreak {
        has_value: bool,
    },
    /// push QuotedAst::Continue (no pop)
    QuoteContinue,
    /// push QuotedAst::Lambda from LambdaSpec constant + quote_captures
    QuoteLambda {
        spec_idx: ConstIdx,
    },
    /// pop (body, iter) → push QuotedAst::For(var, iter, body); var is Str in constants
    QuoteFor {
        var_idx: ConstIdx,
    },
    /// pop (value, target) → push QuotedAst::Assign(target, value)
    QuoteAssign,
    /// pop body → push QuotedAst::Loop(body)
    QuoteLoop,
    /// pop n field values → push QuotedAst::Record; names from StrVec, ty from Str (empty=None)
    QuoteRecord {
        n: u16,
        names_idx: ConstIdx,
        ty_idx: ConstIdx,
    },
    /// pop quote_stack top → rd
    QuoteResult {
        rd: Reg,
    },
    /// Return from function: return ra
    Ret {
        ra: Reg,
    },
    /// Return Unit
    RetUnit,
    /// Early return from `?` operator: return ra, marking it as a rejection
    /// for `fails` transitions (distinguishes `?` Err from final-expression Err).
    RetEarly {
        ra: Reg,
    },

    // ═══════════════════════════════════════════════════════════
    // Data structures
    // ═══════════════════════════════════════════════════════════
    /// rd = new List with `capacity` pre-allocated slots
    NewList {
        rd: Reg,
        capacity: u32,
    },
    /// Append rb to list in ra
    ListPush {
        ra: Reg,
        rb: Reg,
    },
    /// rd = list(ra).pop() — IN-PLACE mutation of the caller's list binding
    /// with write-back semantics (ruling (a), audit fix #14). Removes and
    /// returns the last element; traps on empty (E0803-flavored message).
    /// Mirrors Op::ListPush's register-mutating special case: the builtin
    /// `pop` clones (value semantics) and cannot write back through a cloned
    /// argument, so the compiler emits this op for `pop(var)` on a known
    /// local variable (see compiler.rs compile_call special case).
    ListPop {
        rd: Reg,
        ra: Reg,
    },
    /// rd = ra[rb] (list index; rb is Int)
    ListGet {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// ra[rb] = rc (list set)
    ListSet {
        ra: Reg,
        rb: Reg,
        rc: Reg,
    },
    /// rd = len(ra)
    Len {
        rd: Reg,
        ra: Reg,
    },

    /// rd = new Tuple with `arity` elements from [base .. base+arity)
    NewTuple {
        rd: Reg,
        base: Reg,
        arity: u16,
    },
    /// rd = new Tuple by consuming elements from [base .. base+arity).
    /// Canonical MIR uses this op for non-Copy product construction; the
    /// legacy `NewTuple` keeps its clone-compatible behavior.
    NewTupleMove {
        rd: Reg,
        base: Reg,
        arity: u16,
    },
    /// rd = ra.idx (tuple field access)
    TupleGet {
        rd: Reg,
        ra: Reg,
        idx: FieldIdx,
    },

    /// rd = new Record of type `type_name_idx` with fields from [base..base+count)
    /// Field names are stored in constants[type_name..type_name+count].
    NewRecord {
        rd: Reg,
        type_name: ConstIdx,
        base: Reg,
        count: u16,
    },
    /// rd = new Record by consuming fields from [base..base+count).
    /// Field names are stored in constants[type_name..type_name+count].
    /// This is the runtime realization of canonical MIR aggregate move.
    NewRecordMove {
        rd: Reg,
        type_name: ConstIdx,
        base: Reg,
        count: u16,
    },
    /// rd = clone of ra, then fields from [base..base+count) override by name.
    /// Field names are stored in constants[type_name..type_name+count].
    UpdateRecord {
        rd: Reg,
        type_name: ConstIdx,
        ra: Reg,
        base: Reg,
        count: u16,
    },
    /// rd = ra.field[field_name] — field name is a string constant
    RecordGet {
        rd: Reg,
        ra: Reg,
        field: ConstIdx,
    },
    /// rd = consume ra and move ra.field[field_name] out of the record.
    /// The canonical MIR contract guarantees that no non-Copy residual field
    /// remains; field name is still resolved from the TypeDesc by the MIR
    /// adapter before this op is emitted.
    RecordMoveGet {
        rd: Reg,
        ra: Reg,
        field: ConstIdx,
    },
    /// ra.field[field_name] = rb — field name is a string constant
    RecordSet {
        ra: Reg,
        field: ConstIdx,
        rb: Reg,
    },
    /// ra[idx] = rb — tuple element set (numeric field "0"/"1"/...)
    TupleSet {
        ra: Reg,
        idx: ConstIdx,
        rb: Reg,
    },

    /// rd = new Map (empty)
    NewMap {
        rd: Reg,
    },
    /// rd = new Set (empty)
    NewSet {
        rd: Reg,
    },
    /// rd = map_get(ra, rb) — get value for key rb from map ra
    MapGet {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// map_set(ra, rb, rc) — set key rb to value rc in map ra
    MapSet {
        ra: Reg,
        rb: Reg,
        rc: Reg,
    },
    /// rd = map_contains(ra, rb) — check if map ra contains key rb
    MapContains {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// set_add(ra, rb) — add value rb to set ra
    SetAdd {
        ra: Reg,
        rb: Reg,
    },
    /// rd = set_contains(ra, rb) — check if set ra contains value rb
    SetContains {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// Canonical MIR Set operations. These are separate from the legacy
    /// in-place SetAdd path: insert/remove consume the receiver register and
    /// return the transformed owned Set in `rd`.
    MirSetNew {
        rd: Reg,
    },
    MirSetSize {
        rd: Reg,
        ra: Reg,
    },
    MirSetIsEmpty {
        rd: Reg,
        ra: Reg,
    },
    MirSetContains {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    MirSetInsert {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    MirSetRemove {
        rd: Reg,
        ra: Reg,
        rb: Reg,
    },
    /// rd = sorted scalar list copy of Set ra. The MIR TypeDesc fixes both
    /// the Set<T> and List<T> element ABI; the VM must not expose HashSet
    /// iteration order as observable semantics.
    MirSetToList {
        rd: Reg,
        ra: Reg,
    },
    /// rd = canonical List.len(ra). The MIR validator proves the List
    /// element ABI and the i32 result contract before this opcode is emitted.
    MirListLen {
        rd: Reg,
        ra: Reg,
    },

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
        /// Present only for canonical MIR emission; legacy bytecode leaves
        /// this unset and keeps its historical compiler contract.
        shapes: Option<ConstIdx>,
    },
    /// rd = consume payload[0..arity] and build a tagged owned variant.
    NewVariantMove {
        rd: Reg,
        type_name: ConstIdx,
        variant: VariantIdx,
        base: Reg,
        arity: u16,
        /// Present only for canonical MIR emission; legacy bytecode leaves
        /// this unset and keeps its historical compiler contract.
        shapes: Option<ConstIdx>,
    },
    /// Consume the canonical active variant and move all payload fields into
    /// `base..base+arity`. `variant_tag` is the tag constant selected from
    /// the MIR TypeDesc; the VM checks it before moving anything. The
    /// canonical MIR SwitchMove adapter then drops unbound fields and
    /// transfers bound fields to block parameters.
    DestructureVariantMove {
        ra: Reg,
        base: Reg,
        arity: u16,
        variant_tag: ConstIdx,
    },
    /// rd = variant_tag(ra) — extract tag as Int
    VariantTag {
        rd: Reg,
        ra: Reg,
    },
    /// rd = variant_payload(ra, idx) — extract payload field
    VariantPayload {
        rd: Reg,
        ra: Reg,
        idx: FieldIdx,
    },
    /// rd = is_variant(ra, tag) — check if ra is a Variant with the given tag
    IsVariant {
        rd: Reg,
        ra: Reg,
        tag: ConstIdx,
    },
    /// rd = variant_get(ra, idx) — extract payload field (alias for VariantPayload)
    VariantGet {
        rd: Reg,
        ra: Reg,
        idx: u16,
    },
    /// v0.34.15: extract a pattern field by NAME. Flow states are
    /// Record(Some(name), HashMap) — multi-target match arms name fields
    /// (`Small { v }`), so index-based VariantGet cannot extract them. VM:
    /// Record → fields.get(name); Variant → _0.._N positional mapping.
    PatternField {
        rd: Reg,
        ra: Reg,
        field: u16,
    },

    // ═══════════════════════════════════════════════════════════
    // Option / Result
    // ═══════════════════════════════════════════════════════════
    /// rd = Some(ra)
    Some {
        rd: Reg,
        ra: Reg,
    },
    /// rd = None
    None {
        rd: Reg,
    },
    /// rd = Cap([name]) — create a capability value.
    NewCap {
        rd: Reg,
        name: ConstIdx,
    },
    /// rd = Ok(ra)
    Ok {
        rd: Reg,
        ra: Reg,
    },
    /// rd = Err(ra)
    Err {
        rd: Reg,
        ra: Reg,
    },
    /// rd = is_some(ra)
    IsSome {
        rd: Reg,
        ra: Reg,
    },
    /// rd = unwrap(ra) — trap if None
    Unwrap {
        rd: Reg,
        ra: Reg,
    },

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
    Await {
        rd: Reg,
        ra: Reg,
    },

    // ═══════════════════════════════════════════════════════════
    // Misc
    // ═══════════════════════════════════════════════════════════
    /// rd = ra as f64 / i32 / etc. (type cast)
    Cast {
        rd: Reg,
        ra: Reg,
        target: u8,
    },
    /// rd = to_string(ra)
    ToString {
        rd: Reg,
        ra: Reg,
    },
    /// rd = typeof(ra) as string
    TypeOf {
        rd: Reg,
        ra: Reg,
    },
    /// Trap with message from constant pool
    Trap {
        msg: ConstIdx,
    },
    /// H-9 (Wave-2): runtime non-exhaustive-match panic (E0805). Emitted by the
    /// compiler at the fall-through point of a `match` whose arms can all miss
    /// (previously `LoadUnit` — silent Unit). Zero operands: the op diverges, so
    /// no result register is written (the register the compiler allocated for
    /// the match result simply stays dead on this path, mirroring codegen's
    /// mimi_match_panic abort). The VM raises
    /// `InterpError::non_exhaustive_match` — code E0805, member of
    /// `is_runtime_panic`, so flow transitions absorb it into
    /// `Fault("panic:E0805")` exactly like codegen's fallible-multi-target path.
    NonExhaustiveMatch,

    // ═══════════════════════════════════════════════════════════
    // Actor / Flow / Session (Phase D)
    // ═══════════════════════════════════════════════════════════
    /// rd = spawn actor by name (constant pool string).
    /// detached=true → ActorSpawnDetached: the child survives SystemKill
    /// cascade (0.34.23 §12 actor 决策：interp 此前恒 false，双端级联语义
    /// 不一致)。
    ActorSpawn {
        rd: Reg,
        actor: ConstIdx,
    },
    ActorSpawnDetached {
        rd: Reg,
        actor: ConstIdx,
    },
    /// rd = flow transition dispatch: flow.method(args[0..argc]).
    /// args[0] is the from-state value; runtime extracts state name
    /// and looks up the compiled transition function.
    FlowTransition {
        rd: Reg,
        flow: ConstIdx,
        method: ConstIdx,
        args_base: Reg,
        argc: u16,
    },
    /// rd = dynamic method call: receiver.method(args[1..argc]).
    /// args[0] is the receiver. Runtime dispatch:
    /// - Value::Actor → try_enqueue
    /// - Value::Record → look up function
    /// - otherwise → error
    DynMethodCall {
        rd: Reg,
        method: ConstIdx,
        args_base: Reg,
        argc: u16,
    },

    /// rd = Shared(ra) — wrap value in Arc<RwLock<>>.
    SharedNew {
        rd: Reg,
        ra: Reg,
    },
    /// *ra = rb — write through Shared/Ref/RefMut reference.
    SharedSet {
        ra: Reg,
        rb: Reg,
    },
    /// rd = Weak(ra) — downgrade Shared to Weak reference.
    WeakNew {
        rd: Reg,
        ra: Reg,
    },

    /// No operation
    Nop,

    /// v0.34.10a (SD-9): enter `ieee_float { }` — suspend float finiteness
    /// trapping for the enclosing block. Paired with IeeeExit.
    IeeeEnter,
    /// Leave the innermost `ieee_float { }` block.
    IeeeExit,

    // ═══════════════════════════════════════════════════════════
    // Fault handling (OnFailure compensation)
    // ═══════════════════════════════════════════════════════════
    /// Set the current frame's fault handler PC.
    /// When a builtin call or `?` operator triggers a fault, execution
    /// jumps to `handler_pc` instead of returning the error.
    /// handler_pc is an absolute instruction index (not a relative offset).
    SetFaultPc {
        handler_pc: u32,
    },
    /// Clear the fault handler (normal scope exit — no compensation).
    ClearFaultPc,
    /// Like RetEarly but reads the register from frame.fault_reg (set
    /// by RetEarly when redirected to fault handler). Used at the end
    /// of OnFailure compensation code to re-emit the original error.
    FaultRetEarly,
}

impl Op {
    /// Destination register for pure single-destination compute ops.
    ///
    /// Returns `Some(rd)` only for ops that write exactly one register and
    /// have no side effects (no memory mutation, no control-flow, no trap
    /// that a caller could observe differently after copy-propagation). Used
    /// by the peephole pass (R3) to fuse `op rd=X; MOV rd=Y, rs=X` into
    /// `op rd=Y`. Returns `None` for everything else (conservative).
    pub fn dest_reg(&self) -> Option<Reg> {
        use Op::*;
        Some(match self {
            LoadConst { rd, .. } | LoadUnit { rd } | LoadTrue { rd } | LoadFalse { rd } => *rd,
            Mov { rd, .. } => *rd,
            AddInt { rd, .. }
            | SubInt { rd, .. }
            | MulInt { rd, .. }
            | DivInt { rd, .. }
            | ModInt { rd, .. }
            | NegInt { rd, .. }
            | AddFloat { rd, .. }
            | SubFloat { rd, .. }
            | MulFloat { rd, .. }
            | DivFloat { rd, .. }
            | NegFloat { rd, .. }
            | PowInt { rd, .. }
            | PowFloat { rd, .. }
            | IntToFloat { rd, .. }
            | Cast { rd, .. }
            | EqInt { rd, .. }
            | NeInt { rd, .. }
            | LtInt { rd, .. }
            | GtInt { rd, .. }
            | LeInt { rd, .. }
            | GeInt { rd, .. }
            | EqFloat { rd, .. }
            | LtFloat { rd, .. }
            | GtFloat { rd, .. }
            | LeFloat { rd, .. }
            | GeFloat { rd, .. }
            | Eq { rd, .. }
            | Ne { rd, .. }
            | BitAnd { rd, .. }
            | BitOr { rd, .. }
            | BitXor { rd, .. }
            | Shl { rd, .. }
            | Shr { rd, .. }
            | BitNot { rd, .. }
            | Not { rd, .. }
            | And { rd, .. }
            | Or { rd, .. }
            | ConcatStr { rd, .. }
            | Len { rd, .. }
            | NewList { rd, .. }
            | NewTuple { rd, .. }
            | NewRecord { rd, .. }
            | UpdateRecord { rd, .. }
            | NewVariant { rd, .. }
            | NewVariantMove { rd, .. }
            | NewMap { rd, .. }
            | NewSet { rd, .. }
            | ListGet { rd, .. }
            | ListPop { rd, .. }
            | MirSetToList { rd, .. }
            | MirListLen { rd, .. }
            | TupleGet { rd, .. }
            | RecordGet { rd, .. }
            | RecordMoveGet { rd, .. }
            | VariantTag { rd, .. }
            | VariantPayload { rd, .. }
            | VariantGet { rd, .. }
            | MapGet { rd, .. }
            | MapContains { rd, .. }
            | SetContains { rd, .. }
            | Some { rd, .. }
            | None { rd, .. }
            | NewCap { rd, .. }
            | Ok { rd, .. }
            | Err { rd, .. }
            | IsSome { rd, .. }
            | Unwrap { rd, .. }
            | IsVariant { rd, .. }
            | PatternField { rd, .. }
            | NewClosure { rd, .. }
            | Spawn { rd, .. }
            | Await { rd, .. }
            | ToString { rd, .. }
            | TypeOf { rd, .. }
            | SharedNew { rd, .. }
            | WeakNew { rd, .. }
            | DerefValue { rd, .. }
            | Call { rd, .. }
            | CallMove { rd, .. }
            | CallBuiltin { rd, .. }
            | CallExtern { rd, .. }
            | CallIndirect { rd, .. }
            | DynMethodCall { rd, .. }
            | FlowTransition { rd, .. }
            | ActorSpawn { rd, .. }
            | ActorSpawnDetached { rd, .. } => *rd,
            _ => return None,
        })
    }

    /// Clone this op with a different destination register. Only valid for
    /// ops where `dest_reg()` is `Some` — otherwise returns `self` unchanged.
    pub fn with_dest(&self, new_rd: Reg) -> Op {
        let mut op = *self;
        match &mut op {
            Op::LoadConst { rd, .. } => *rd = new_rd,
            Op::LoadUnit { rd } => *rd = new_rd,
            Op::LoadTrue { rd } => *rd = new_rd,
            Op::LoadFalse { rd } => *rd = new_rd,
            Op::Mov { rd, .. } => *rd = new_rd,
            Op::AddInt { rd, .. }
            | Op::SubInt { rd, .. }
            | Op::MulInt { rd, .. }
            | Op::DivInt { rd, .. }
            | Op::ModInt { rd, .. }
            | Op::NegInt { rd, .. }
            | Op::AddFloat { rd, .. }
            | Op::SubFloat { rd, .. }
            | Op::MulFloat { rd, .. }
            | Op::DivFloat { rd, .. }
            | Op::NegFloat { rd, .. }
            | Op::PowInt { rd, .. }
            | Op::PowFloat { rd, .. }
            | Op::IntToFloat { rd, .. }
            | Op::Cast { rd, .. }
            | Op::EqInt { rd, .. }
            | Op::NeInt { rd, .. }
            | Op::LtInt { rd, .. }
            | Op::GtInt { rd, .. }
            | Op::LeInt { rd, .. }
            | Op::GeInt { rd, .. }
            | Op::EqFloat { rd, .. }
            | Op::LtFloat { rd, .. }
            | Op::GtFloat { rd, .. }
            | Op::LeFloat { rd, .. }
            | Op::GeFloat { rd, .. }
            | Op::Eq { rd, .. }
            | Op::Ne { rd, .. }
            | Op::BitAnd { rd, .. }
            | Op::BitOr { rd, .. }
            | Op::BitXor { rd, .. }
            | Op::Shl { rd, .. }
            | Op::Shr { rd, .. }
            | Op::BitNot { rd, .. }
            | Op::Not { rd, .. }
            | Op::And { rd, .. }
            | Op::Or { rd, .. }
            | Op::ConcatStr { rd, .. }
            | Op::Len { rd, .. }
            | Op::NewList { rd, .. }
            | Op::NewTuple { rd, .. }
            | Op::NewRecord { rd, .. }
            | Op::UpdateRecord { rd, .. }
            | Op::NewVariant { rd, .. }
            | Op::NewVariantMove { rd, .. }
            | Op::NewMap { rd, .. }
            | Op::NewSet { rd, .. }
            | Op::ListGet { rd, .. }
            | Op::ListPop { rd, .. }
            | Op::TupleGet { rd, .. }
            | Op::RecordGet { rd, .. }
            | Op::RecordMoveGet { rd, .. }
            | Op::VariantTag { rd, .. }
            | Op::VariantPayload { rd, .. }
            | Op::VariantGet { rd, .. }
            | Op::MapGet { rd, .. }
            | Op::MapContains { rd, .. }
            | Op::SetContains { rd, .. }
            | Op::Some { rd, .. }
            | Op::None { rd, .. }
            | Op::NewCap { rd, .. }
            | Op::Ok { rd, .. }
            | Op::Err { rd, .. }
            | Op::IsSome { rd, .. }
            | Op::Unwrap { rd, .. }
            | Op::IsVariant { rd, .. }
            | Op::PatternField { rd, .. }
            | Op::NewClosure { rd, .. }
            | Op::Spawn { rd, .. }
            | Op::Await { rd, .. }
            | Op::ToString { rd, .. }
            | Op::TypeOf { rd, .. }
            | Op::SharedNew { rd, .. }
            | Op::WeakNew { rd, .. }
            | Op::DerefValue { rd, .. }
            | Op::Call { rd, .. }
            | Op::CallMove { rd, .. }
            | Op::CallBuiltin { rd, .. }
            | Op::CallExtern { rd, .. }
            | Op::CallIndirect { rd, .. }
            | Op::DynMethodCall { rd, .. }
            | Op::FlowTransition { rd, .. }
            | Op::ActorSpawn { rd, .. }
            | Op::ActorSpawnDetached { rd, .. } => *rd = new_rd,
            _ => {}
        }
        op
    }

    /// True if this op reads `reg` in any operand register.
    pub fn reads_reg(&self, reg: Reg) -> bool {
        use Op::*;
        // Range read: base..base+count.
        fn reads_range(base: Reg, count: u16, reg: Reg) -> bool {
            reg >= base && reg < base + count
        }
        match self {
            // Ops that read no register:
            LoadConst { .. }
            | LoadUnit { .. }
            | LoadTrue { .. }
            | LoadFalse { .. }
            | None { .. }
            | NewMap { .. }
            | NewSet { .. }
            | MirSetNew { .. }
            | NewCap { .. }
            | Nop
            | Jmp { .. }
            | SetFaultPc { .. }
            | ClearFaultPc
            | IeeeEnter
            | IeeeExit
            | Trap { .. }
            | RetUnit
            | FaultRetEarly => false,
            Mov { rs, .. } | Move { rs, .. } | Clone { rs, .. } => *rs == reg,
            Drop { ra } | DropAggregate { ra, .. } | DropVariant { ra, .. } => *ra == reg,
            DestructureVariantMove { ra, .. } => *ra == reg,
            AddInt { ra, rb, .. }
            | SubInt { ra, rb, .. }
            | MulInt { ra, rb, .. }
            | DivInt { ra, rb, .. }
            | ModInt { ra, rb, .. }
            | AddFloat { ra, rb, .. }
            | SubFloat { ra, rb, .. }
            | MulFloat { ra, rb, .. }
            | DivFloat { ra, rb, .. }
            | EqInt { ra, rb, .. }
            | NeInt { ra, rb, .. }
            | LtInt { ra, rb, .. }
            | GtInt { ra, rb, .. }
            | LeInt { ra, rb, .. }
            | GeInt { ra, rb, .. }
            | EqFloat { ra, rb, .. }
            | LtFloat { ra, rb, .. }
            | GtFloat { ra, rb, .. }
            | LeFloat { ra, rb, .. }
            | GeFloat { ra, rb, .. }
            | Eq { ra, rb, .. }
            | Ne { ra, rb, .. }
            | BitAnd { ra, rb, .. }
            | BitOr { ra, rb, .. }
            | BitXor { ra, rb, .. }
            | Shl { ra, rb, .. }
            | Shr { ra, rb, .. }
            | PowInt { ra, rb, .. }
            | PowFloat { ra, rb, .. }
            | ConcatStr { ra, rb, .. }
            | StrAppend { ra, rb, .. }
            | CheckI32DivRem { ra, rb, .. }
            | ListPush { ra, rb, .. }
            | ListGet { ra, rb, .. }
            | ListSet { ra, rb, .. }
            | MapGet { ra, rb, .. }
            | MapSet { ra, rb, .. }
            | MapContains { ra, rb, .. }
            | SetAdd { ra, rb, .. }
            | SetContains { ra, rb, .. }
            | MirSetInsert { ra, rb, .. }
            | MirSetRemove { ra, rb, .. }
            | RecordSet { ra, rb, .. }
            | TupleSet { ra, rb, .. }
            | SharedSet { ra, rb, .. } => *ra == reg || *rb == reg,
            NegInt { ra, .. }
            | NegFloat { ra, .. }
            | BitNot { ra, .. }
            | Not { ra, .. }
            | IntToFloat { ra, .. }
            | Cast { ra, .. }
            | DerefValue { ra, .. }
            | Some { ra, .. }
            | Ok { ra, .. }
            | Err { ra, .. }
            | IsSome { ra, .. }
            | Unwrap { ra, .. }
            | Await { ra, .. }
            | ToString { ra, .. }
            | TypeOf { ra, .. }
            | IsVariant { ra, .. }
            | PatternField { ra, .. }
            | VariantTag { ra, .. }
            | VariantPayload { ra, .. }
            | VariantGet { ra, .. }
            | TupleGet { ra, .. }
            | RecordGet { ra, .. }
            | RecordMoveGet { ra, .. }
            | Len { ra, .. }
            | ListPop { ra, .. }
            | MirSetSize { ra, .. }
            | MirSetIsEmpty { ra, .. }
            | MirSetContains { ra, .. }
            | MirSetToList { ra, .. }
            | MirListLen { ra, .. }
            | SharedNew { ra, .. }
            | WeakNew { ra, .. }
            | Ret { ra, .. }
            | RetEarly { ra, .. }
            | JmpIf { ra, .. }
            | JmpIfNot { ra, .. } => *ra == reg,
            MaskShiftAmt { rb, .. } => *rb == reg,
            CheckI32 { rd, .. } | WrapI32 { rd, .. } => *rd == reg,
            And { ra, rb, .. } | Or { ra, rb, .. } => *ra == reg || *rb == reg,
            Call {
                args_base, argc, ..
            }
            | CallMove {
                args_base, argc, ..
            }
            | CallBuiltin {
                args_base, argc, ..
            }
            | CallExtern {
                args_base, argc, ..
            }
            | DynMethodCall {
                args_base, argc, ..
            }
            | FlowTransition {
                args_base, argc, ..
            }
            | Spawn {
                args_base, argc, ..
            } => reads_range(*args_base, *argc, reg),
            CallIndirect {
                callee,
                args_base,
                argc,
                ..
            } => *callee == reg || reads_range(*args_base, *argc, reg),
            NewClosure {
                captures_base,
                capture_count,
                ..
            } => reads_range(*captures_base, *capture_count, reg),
            MutateSetup {
                regs_base, count, ..
            }
            | MutateSetupField {
                regs_base, count, ..
            } => reads_range(*regs_base, *count, reg),
            NewTuple { base, arity, .. }
            | NewTupleMove { base, arity, .. }
            | NewVariant { base, arity, .. }
            | NewVariantMove { base, arity, .. } => reads_range(*base, *arity, reg),
            NewRecord { base, count, .. } | NewRecordMove { base, count, .. } => {
                reads_range(*base, *count, reg)
            }
            UpdateRecord {
                ra, base, count, ..
            } => *ra == reg || reads_range(*base, *count, reg),
            // Conservative: any op not enumerated above may read the register.
            _ => true,
        }
    }
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
    /// Parameter positions (indices into the arg list) that are `mut`,
    /// in declaration order. Used for mutate-parameter writeback.
    pub mut_param_indices: Vec<u16>,
    /// Whether this function is async.
    pub is_async: bool,
    /// Constant pool (literals, string names, type names).
    pub constants: Vec<ConstValue>,
    /// Instruction stream.
    pub code: Vec<Op>,
    /// Source line table: maps instruction index → source line (for errors).
    pub line_table: Vec<u32>,
    /// Captured variable names (for closures). Index i corresponds to register param_count + i.
    pub capture_names: Vec<String>,
    /// O(1) contract flags (0.33 Phase F: runtime contract checking).
    pub has_requires: bool,
    pub has_ensures: bool,
    /// Parameter names (for contract expression binding).
    pub param_names: Vec<String>,
    /// Compiled contract expression function indices (0.33 Phase F: native contract eval).
    /// Each entry is a mini-function that takes the parent's params (+ result for ensures)
    /// and returns a bool. Empty vec if no contracts.
    pub requires_funcs: Vec<FuncIdx>,
    pub ensures_funcs: Vec<FuncIdx>,
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
    /// Type constant (quote Cast targets; 0.33 Phase F).
    Type(crate::ast::Type),
    /// Quoted AST constant (comptime quote! results inlined at compile time).
    QuoteAst(Box<crate::interp::value::QuotedAst>),
    /// Lambda specification for quote context (params + ret + body + free var names).
    LambdaSpec {
        params: Vec<crate::ast::Param>,
        ret: Option<crate::ast::Type>,
        body: crate::ast::Block,
        free_vars: Vec<String>,
    },
    /// Pattern constant (for QuoteWhileLet).
    Pattern(crate::ast::Pattern),
    /// String vector constant (for QuoteRecord field names).
    StrVec(Vec<String>),
    /// Canonical variant shapes encoded for the bytecode physical ABI.
    /// Each entry carries the active tag, semantic discriminant, and payload
    /// arity copied only from a validated MIR TypeDesc table; the VM must
    /// reject tags or payloads outside it.
    VariantShapes(Vec<VariantShape>),
}

/// One canonical variant shape in the bytecode physical contract.
#[derive(Debug, Clone)]
pub struct VariantShape {
    pub tag: String,
    pub discriminant: VariantIdx,
    pub arity: u16,
}

impl FunctionProto {
    pub fn new(name: String, param_count: u16) -> Self {
        FunctionProto {
            name,
            param_count,
            register_count: param_count,
            has_mut_params: false,
            mut_param_indices: Vec::new(),
            is_async: false,
            constants: Vec::new(),
            code: Vec::new(),
            line_table: Vec::new(),
            capture_names: Vec::new(),
            has_requires: false,
            has_ensures: false,
            param_names: Vec::new(),
            requires_funcs: Vec::new(),
            ensures_funcs: Vec::new(),
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

    /// Add a constant WITHOUT deduplication. Used for record field names
    /// which must be contiguous after the type name in the constant pool.
    pub fn add_const_raw(&mut self, val: ConstValue) -> ConstIdx {
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
    /// Extern (FFI) function names, indexed by Op::CallExtern::extern_idx
    /// (0.33 Phase D FFI forwarding).
    pub extern_names: Vec<String>,
    /// Actor definitions (for spawn at runtime).
    pub actor_defs: std::collections::HashMap<String, crate::ast::ActorDef>,
    /// Flow definitions (for transition dispatch).
    pub flow_defs: std::collections::HashMap<String, crate::ast::FlowDef>,
    /// Flow transition function indices: (flow_name, transition_name, from_state) → FuncIdx.
    pub flow_transition_funcs: std::collections::HashMap<(String, String, String), FuncIdx>,
    /// Transitions with `fails` clause: result must be wrapped in Ok(...).
    pub flow_fails_transitions: std::collections::HashSet<(String, String, String)>,
    /// Actor method function indices: (actor_name, method_name) → FuncIdx.
    pub actor_method_funcs: std::collections::HashMap<(String, String), FuncIdx>,
    /// Global max_children limit extracted from flow @max_children annotations.
    pub max_children: Option<usize>,
    /// Flow persistent fields: flow_name → field names (for Fault shadowing).
    pub flow_persistent: std::collections::HashMap<String, Vec<String>>,
    /// Flow typed-fault error type: flow_name → error type name (from `fault T`).
    /// Used by panic absorption to add a defaulted `error` field to the Fault
    /// record, matching the codegen backend (v0.34.18b typed-fault parity).
    pub flow_fault_type: std::collections::HashMap<String, String>,
    /// Type definitions: type_name → kind (for type_fields / type_variants).
    pub type_defs: std::collections::HashMap<String, crate::ast::TypeDefKind>,
    /// The original AST (for actor worker threads that use tree-walker internally).
    pub ast: Option<std::sync::Arc<crate::ast::File>>,
    /// Record field types: type_name → [(field_name, field_type_str)].
    /// Used by from_json_typed for recursive field coercion.
    pub record_fields: std::collections::HashMap<String, Vec<(String, String)>>,
}

impl BytecodeProgram {
    /// Look up a function index by name.
    pub fn function_index(&self, name: &str) -> Option<FuncIdx> {
        self.functions
            .iter()
            .position(|f| f.name == name)
            .map(|i| i as FuncIdx)
    }
}
