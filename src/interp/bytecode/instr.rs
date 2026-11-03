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
    /// Typed canonical Actor operations. The instruction constant is the
    /// checker-owned MIR identity, sealed with the complete function body.
    MirActorSpawn {
        rd: Reg,
        descriptor: u32,
        instruction: ConstIdx,
    },
    MirActorCall {
        rd: Reg,
        ra: Reg,
        descriptor: u32,
        method: u32,
        instruction: ConstIdx,
        args_base: Reg,
        argc: u16,
    },
    MirActorFieldLoad {
        rd: Reg,
        state: Reg,
        descriptor: u32,
        field: u32,
        instruction: ConstIdx,
    },
    MirActorFieldStore {
        state: Reg,
        value: Reg,
        descriptor: u32,
        field: u32,
        instruction: ConstIdx,
    },
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
    /// Convert a signed integer directly to binary32, then store its exact
    /// promoted value in the VM's f64-backed Float representation.
    IntToFloat32 {
        rd: Reg,
        ra: Reg,
    },
    FloatNarrow {
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
    /// Retired receiptless extern-call encoding. It is retained as a
    /// compatibility/inspection shape so forged or old bytecode can receive
    /// a stable fail-closed diagnostic; production compilers never emit it
    /// and the VM never resolves the index through host libraries.
    CallExtern {
        rd: Reg,
        extern_idx: u16,
        args_base: Reg,
        argc: u16,
    },
    /// Call a checker-owned scalar FFI descriptor. A missing descriptor is
    /// an error; this instruction never consults compatibility extern names.
    CallCanonicalExtern {
        rd: Reg,
        extern_idx: u16,
        /// Constant-pool index for the checker-owned MIR instruction identity.
        /// The VM uses this alongside the current frame owner to prevent a
        /// forged descriptor-table index from retargeting another call site.
        instruction: ConstIdx,
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
        /// `Some` is mandatory for canonical MIR List projections. `None`
        /// remains the explicit legacy bytecode compatibility path.
        contract: Option<ConstIdx>,
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
    /// rd = Unicode scalar count of a checked String receiver.
    /// Kept distinct from legacy Len so checked String.len validates a String
    /// receiver without altering collection or raw-AST compatibility paths.
    StringScalarLen {
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
        /// Optional canonical MIR tuple projection receipt. `None` is kept
        /// for legacy bytecode only; canonical MIR emission must populate it.
        contract: Option<ConstIdx>,
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
    /// rd = consumes ra and moves fields from [base..base+count) by name.
    /// Unlike `UpdateRecord`, no source record or update value is cloned.
    UpdateRecordMove {
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
        /// Optional canonical MIR record projection receipt. `None` is kept
        /// for legacy bytecode only; canonical MIR emission must populate it.
        contract: Option<ConstIdx>,
    },
    /// rd = consume ra and move ra.field[field_name] out of the record.
    /// The canonical MIR contract guarantees that no non-Copy residual field
    /// remains; field name is still resolved from the TypeDesc by the MIR
    /// adapter before this op is emitted.
    RecordMoveGet {
        rd: Reg,
        ra: Reg,
        field: ConstIdx,
        /// Optional canonical MIR record projection receipt. `None` is kept
        /// for legacy bytecode only; canonical MIR emission must populate it.
        contract: Option<ConstIdx>,
    },
    /// rd = consume ra, move one record field out, and drop every residual
    /// field named by the TypeDesc-derived receipt constant.
    RecordMoveDropGet {
        rd: Reg,
        ra: Reg,
        field: ConstIdx,
        contract: ConstIdx,
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
        /// `Some` is mandatory for canonical MIR List operations. `None`
        /// remains the explicit legacy bytecode compatibility path.
        contract: Option<ConstIdx>,
    },
    /// rd = an independent reversed scalar List copied from ra. The MIR
    /// receipt proves that the source remains borrowed and the result owns a
    /// complete List value; this is not an in-place mutation opcode.
    MirListReverse {
        rd: Reg,
        ra: Reg,
        /// Canonical MIR always supplies a ListOperationShape here.
        contract: Option<ConstIdx>,
    },
    /// rd = a fresh scalar List formed by consuming and appending ra, rb.
    /// The canonical receipt proves both inputs share one List element ABI;
    /// this opcode is not the legacy generic ListExt dispatcher.
    MirListConcat {
        rd: Reg,
        ra: Reg,
        rb: Reg,
        /// Canonical MIR always supplies a ListOperationShape here.
        contract: Option<ConstIdx>,
    },
    /// Canonical MIR MapRoot operations. The root is an opaque value separate
    /// from legacy `NewMap`/`MapSet`: Set consumes `ra` and creates a fresh
    /// `rd` from the static key constant and i32 `value` register.
    MirMapRootNew {
        rd: Reg,
    },
    MirMapRootSet {
        rd: Reg,
        ra: Reg,
        key: ConstIdx,
        value: Reg,
    },
    MirMapRootRemove {
        rd: Reg,
        ra: Reg,
        key: ConstIdx,
    },
    MirMapRootSize {
        rd: Reg,
        ra: Reg,
    },
    MirMapRootContains {
        rd: Reg,
        ra: Reg,
        key: ConstIdx,
    },
    MirMapRootDrop {
        ra: Reg,
    },
    /// The separately receipted MapAnyOwner v1 operations. These use a
    /// distinct opcode family and runtime value so a legacy MapRoot receipt
    /// can never authorize a typed Any-owner lifecycle.
    MirMapAnyOwnerV1New {
        rd: Reg,
    },
    MirMapAnyOwnerV1Set {
        rd: Reg,
        ra: Reg,
        key: ConstIdx,
        value: Reg,
    },
    MirMapAnyOwnerV1Remove {
        rd: Reg,
        ra: Reg,
        key: ConstIdx,
    },
    MirMapAnyOwnerV1Size {
        rd: Reg,
        ra: Reg,
    },
    MirMapAnyOwnerV1Contains {
        rd: Reg,
        ra: Reg,
        key: ConstIdx,
    },
    MirMapAnyOwnerV1Drop {
        ra: Reg,
    },
    /// Independent source-Map owner lifecycle profile. Unlike MapRoot and
    /// MapAnyOwner v1, updates preserve the source root and use a dynamic
    /// String key register. This opcode family has its own bytecode receipt
    /// and runtime Value identity.
    MirOrdinaryMapOwnerLifecycleI32StringV1New {
        rd: Reg,
    },
    /// Fresh independent root; the source remains live.
    MirOrdinaryMapOwnerLifecycleI32StringV1Clone {
        rd: Reg,
        ra: Reg,
    },
    MirOrdinaryMapOwnerLifecycleI32StringV1Set {
        rd: Reg,
        ra: Reg,
        key: Reg,
        value: Reg,
    },
    MirOrdinaryMapOwnerLifecycleI32StringV1Remove {
        rd: Reg,
        ra: Reg,
        key: Reg,
    },
    MirOrdinaryMapOwnerLifecycleI32StringV1Size {
        rd: Reg,
        ra: Reg,
    },
    MirOrdinaryMapOwnerLifecycleI32StringV1Contains {
        rd: Reg,
        ra: Reg,
        key: Reg,
    },
    MirOrdinaryMapOwnerLifecycleI32StringV1Drop {
        ra: Reg,
    },
    /// Borrow one Checker-receipted lifecycle-v1 Map root and return an
    /// independently owned canonical JSON String. The root stays live.
    MirOrdinaryMapOwnerLifecycleObserverV1ToJson {
        rd: Reg,
        ra: Reg,
    },
    /// Borrow one Checker-receipted lifecycle-v1 Map root and print its
    /// canonical sorted JSON representation followed by one newline.
    MirOrdinaryMapOwnerLifecycleObserverV1Println {
        rd: Reg,
        ra: Reg,
    },
    /// Persistent ordinary-Map profile with typed owned reads. Set and
    /// Remove read `ra` and produce a distinct result root. Get operations
    /// carry the TypeDesc-derived canonical Result variant shape table.
    MirOrdinaryMapOwnedGetI32StringV1New {
        rd: Reg,
    },
    /// Fresh independent root; the source remains live.
    MirOrdinaryMapOwnedGetI32StringV1Clone {
        rd: Reg,
        ra: Reg,
    },
    MirOrdinaryMapOwnedGetI32StringV1Set {
        rd: Reg,
        ra: Reg,
        key: Reg,
        value: Reg,
    },
    MirOrdinaryMapOwnedGetI32StringV1Remove {
        rd: Reg,
        ra: Reg,
        key: Reg,
    },
    MirOrdinaryMapOwnedGetI32StringV1Size {
        rd: Reg,
        ra: Reg,
    },
    MirOrdinaryMapOwnedGetI32StringV1Contains {
        rd: Reg,
        ra: Reg,
        key: Reg,
    },
    MirOrdinaryMapOwnedGetI32StringV1GetI32 {
        rd: Reg,
        ra: Reg,
        key: Reg,
        shapes: ConstIdx,
    },
    MirOrdinaryMapOwnedGetI32StringV1GetString {
        rd: Reg,
        ra: Reg,
        key: Reg,
        shapes: ConstIdx,
    },
    /// A fresh `(found, owned Any)` snapshot; missing uses a fresh owned i32 zero.
    MirOrdinaryMapOwnedGetI32StringV1GetAny {
        rd: Reg,
        ra: Reg,
        key: Reg,
    },
    /// Borrow a typed ordinary Map root and return a move-owned `List<Any>`
    /// snapshot. List elements are independent managed Any owners.
    MirOrdinaryMapOwnedGetI32StringV1Values {
        rd: Reg,
        ra: Reg,
    },
    /// Dedicated descriptor-checked Any operation. The complete checker contract
    /// is retained in the constant pool and pinned by the independent profile receipt.
    MirManagedAnyOp {
        rd: Option<Reg>,
        ra: Reg,
        operation: crate::core::mir::MirManagedAnyOperation,
        contract: ConstIdx,
    },
    /// Borrow a canonical owned Any and exact String peer for a sealed
    /// equality/inequality operation. Neither input register is consumed.
    MirManagedAnyStringCompare {
        rd: Reg,
        any: Reg,
        string: Reg,
        equal: bool,
    },
    MirOrdinaryMapOwnedGetI32StringV1Drop {
        ra: Reg,
    },
    /// Independent concrete-record ordinary Map/Any owner-v2 profile. The
    /// payload boundary is validated by the MIR adapter (i32 or Label{text:
    /// String}); opcodes remain distinct from every v1 owner profile.
    MirOrdinaryMapOwnedAnyV2New {
        rd: Reg,
    },
    /// Pending receipt-bound New returning canonical Ok(root)/Err(Capacity).
    /// A distinct opcode prevents a bare-root receipt from authorizing it.
    MirOrdinaryMapOwnedAnyV2NewResult {
        rd: Reg,
    },
    MirOrdinaryMapOwnedAnyV2Clone {
        rd: Reg,
        ra: Reg,
    },
    MirOrdinaryMapOwnedAnyV2Set {
        rd: Reg,
        ra: Reg,
        key: Reg,
        value: Reg,
    },
    MirOrdinaryMapOwnedAnyV2Remove {
        rd: Reg,
        ra: Reg,
        key: Reg,
    },
    MirOrdinaryMapOwnedAnyV2Size {
        rd: Reg,
        ra: Reg,
    },
    MirOrdinaryMapOwnedAnyV2Contains {
        rd: Reg,
        ra: Reg,
        key: Reg,
    },
    /// Fresh `(found, owned Any-v2)` snapshot. Missing keys produce an
    /// independently owned canonical i32 zero; the Map root stays live.
    MirOrdinaryMapOwnedAnyV2GetAny {
        rd: Reg,
        ra: Reg,
        key: Reg,
    },
    /// Exact cross-call `std::maps::get_or_default<i32>` operation. The
    /// adapter binds this opcode to the independent Checker/MIR receipt; the
    /// VM performs V2 GetAny and checked i32 unpack, selects the caller
    /// default only on a miss, and settles the temporary GetAny pair.
    MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 {
        rd: Reg,
        ra: Reg,
        key: Reg,
        default: Reg,
    },
    /// Exact cross-call `std::maps::get_or_default<String>` operation. This
    /// opcode has a distinct Checker/MIR binding from the i32 specialization.
    /// The VM copies a String hit or the selected default on a miss; the
    /// canonical i32-zero Any token is only a miss sentinel.
    MirOrdinaryMapOwnedAnyV2GetOrDefaultString {
        rd: Reg,
        ra: Reg,
        key: Reg,
        default: Reg,
    },
    /// Print a Checker-sealed projection of a borrowed GetAny pair. The
    /// opcode reads the pair without consuming it and writes Unit to `rd`.
    MirOrdinaryMapOwnedAnyV2Println {
        rd: Reg,
        pair: Reg,
    },
    /// Borrow the owned Any-v2 payload from one Checker/MIR-bound GetAny
    /// pair and copy it into its exact Core-authorized i32, String, or concrete
    /// one-String-field record target type. The Core result-settlement receipt
    /// remains the owner of the pair.
    MirOrdinaryMapOwnedAnyV2Unpack {
        rd: Reg,
        ra: Reg,
    },
    /// Copy the exactly selected element of a Checker-receipted ordinary
    /// Map/Any owner-v2 Values snapshot into its exact i32, String, or Label
    /// target type. The Values snapshot remains live.
    MirOrdinaryMapOwnedAnyV2ValuesUnpack {
        rd: Reg,
        ra: Reg,
        index: Reg,
    },
    /// Materialize the exact owner-v2 Map keys as an independently owned,
    /// UTF-8-byte-sorted ordinary List<String>. A private KeysSnapshot route
    /// anchor binds this operation to one Checker/MIR source pair.
    MirOrdinaryMapOwnedAnyV2Keys {
        rd: Reg,
        ra: Reg,
    },
    /// Copy the literal-zero element from one exact KeysSnapshot List into a
    /// fresh owned String. This opcode is not a generic List index operation.
    MirOrdinaryMapOwnedAnyV2KeysUnpack {
        rd: Reg,
        ra: Reg,
    },
    /// Materialize Mimi user argument zero as an owned, length-bearing
    /// String. This opcode is emitted only for the dedicated DynamicKeysGet
    /// Checker/MIR route and is guarded by its private bytecode anchor.
    MirOrdinaryMapOwnedAnyV2DynamicArg0 {
        rd: Reg,
    },
    /// Snapshot the ordinary Map/Any owner-v2 Values result into an ordinary
    /// bytecode List of independently owned V2 payload identities. The
    /// Checker/MIR Values receipt binds this site and its exact Drop.
    MirOrdinaryMapOwnedAnyV2Values {
        rd: Reg,
        ra: Reg,
    },
    MirOrdinaryMapOwnedAnyV2IsEmpty {
        rd: Reg,
        ra: Reg,
    },
    MirOrdinaryMapOwnedAnyV2Drop {
        ra: Reg,
    },
    /// Explicitly settle the receipted V2 Values List. This stays separate
    /// from generic List Drop because the elements carry opaque V2 owners.
    MirOrdinaryMapOwnedAnyV2ValuesDrop {
        ra: Reg,
    },
    /// Deep-clone a Checker-typed owner-v2 Values snapshot into an independent
    /// list of independently owned managed payloads.
    MirOrdinaryMapOwnedAnyV2ValuesClone {
        rd: Reg,
        ra: Reg,
    },
    /// rd = canonical Option/Result predicate(ra). The receipt is mandatory
    /// for MIR bytecode and carries the TypeDesc-selected discriminant.
    MirVariantPredicate {
        rd: Reg,
        ra: Reg,
        predicate: crate::core::mir::MirVariantPredicate,
        contract: Option<ConstIdx>,
    },
    /// rd = canonical Option/Result.unwrap_or(ra, rb). The receipt carries
    /// both active tag identities and either the Copy payload ABI or the
    /// explicit consuming managed-payload contract; no legacy tag lookup is
    /// permitted for this MIR opcode.
    MirVariantProjectOr {
        rd: Reg,
        ra: Reg,
        rb: Reg,
        contract: Option<ConstIdx>,
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
    /// `base..base+arity`. `variant_tag` and `shapes` are selected from the
    /// MIR TypeDesc; the VM checks active identity and physical shape before
    /// moving anything. The canonical MIR SwitchMove adapter then drops
    /// unbound fields and transfers bound fields to block parameters.
    DestructureVariantMove {
        ra: Reg,
        base: Reg,
        arity: u16,
        variant_tag: ConstIdx,
        shapes: ConstIdx,
    },
    /// Consume a canonical tuple and move every element into `base..base+arity`.
    /// The shape constant is copied from the MIR tuple receipt and is checked
    /// before the source register is replaced.
    DestructureTupleMove {
        ra: Reg,
        base: Reg,
        arity: u16,
        shape: ConstIdx,
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
    /// rd = variant_get(ra, idx) — extract a canonical payload field without
    /// consuming the source. `variant_tag` and `shapes` are selected from
    /// the MIR TypeDesc; the VM checks the active identity and physical
    /// payload shape before reading the field.
    VariantGet {
        rd: Reg,
        ra: Reg,
        idx: u16,
        variant_tag: ConstIdx,
        shapes: ConstIdx,
    },
    /// rd = variant_move_get(ra, idx) — validate the active canonical
    /// variant, consume the source register, and move out one owned payload.
    /// The shape table and expected tag are TypeDesc receipts, not inferred
    /// from the VM value.
    VariantMoveGet {
        rd: Reg,
        ra: Reg,
        idx: u16,
        variant_tag: ConstIdx,
        shapes: ConstIdx,
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
        /// Exact checker-selected source state for checked transitions.
        /// Raw compatibility transitions leave this empty.
        checked_source_state: Option<ConstIdx>,
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
            | IntToFloat32 { rd, .. }
            | FloatNarrow { rd, .. }
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
            | StringScalarLen { rd, .. }
            | NewList { rd, .. }
            | NewTuple { rd, .. }
            | NewRecord { rd, .. }
            | UpdateRecord { rd, .. }
            | UpdateRecordMove { rd, .. }
            | NewVariant { rd, .. }
            | NewVariantMove { rd, .. }
            | NewMap { rd, .. }
            | NewSet { rd, .. }
            | ListGet { rd, .. }
            | ListPop { rd, .. }
            | MirSetToList { rd, .. }
            | MirListLen { rd, .. }
            | MirListReverse { rd, .. }
            | MirListConcat { rd, .. }
            | MirMapRootRemove { rd, .. }
            | MirMapRootContains { rd, .. }
            | MirMapAnyOwnerV1Remove { rd, .. }
            | MirMapAnyOwnerV1Contains { rd, .. }
            | MirOrdinaryMapOwnerLifecycleI32StringV1Clone { rd, .. }
            | MirOrdinaryMapOwnerLifecycleI32StringV1Remove { rd, .. }
            | MirOrdinaryMapOwnerLifecycleI32StringV1Contains { rd, .. }
            | MirOrdinaryMapOwnerLifecycleObserverV1ToJson { rd, .. }
            | MirOrdinaryMapOwnerLifecycleObserverV1Println { rd, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Clone { rd, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Remove { rd, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Contains { rd, .. }
            | MirOrdinaryMapOwnedGetI32StringV1GetI32 { rd, .. }
            | MirOrdinaryMapOwnedGetI32StringV1GetString { rd, .. }
            | MirOrdinaryMapOwnedGetI32StringV1GetAny { rd, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Values { rd, .. }
            | MirOrdinaryMapOwnedAnyV2Clone { rd, .. }
            | MirOrdinaryMapOwnedAnyV2Remove { rd, .. }
            | MirOrdinaryMapOwnedAnyV2Contains { rd, .. }
            | MirOrdinaryMapOwnedAnyV2GetAny { rd, .. }
            | MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { rd, .. }
            | MirOrdinaryMapOwnedAnyV2GetOrDefaultString { rd, .. }
            | MirOrdinaryMapOwnedAnyV2Println { rd, .. }
            | MirOrdinaryMapOwnedAnyV2Unpack { rd, .. }
            | MirOrdinaryMapOwnedAnyV2ValuesUnpack { rd, .. }
            | MirOrdinaryMapOwnedAnyV2Values { rd, .. }
            | MirOrdinaryMapOwnedAnyV2Keys { rd, .. }
            | MirOrdinaryMapOwnedAnyV2KeysUnpack { rd, .. }
            | MirOrdinaryMapOwnedAnyV2DynamicArg0 { rd }
            | MirOrdinaryMapOwnedAnyV2ValuesClone { rd, .. }
            | MirOrdinaryMapOwnedAnyV2IsEmpty { rd, .. }
            | MirVariantPredicate { rd, .. }
            | MirVariantProjectOr { rd, .. }
            | TupleGet { rd, .. }
            | RecordGet { rd, .. }
            | RecordMoveGet { rd, .. }
            | RecordMoveDropGet { rd, .. }
            | VariantTag { rd, .. }
            | VariantPayload { rd, .. }
            | VariantGet { rd, .. }
            | VariantMoveGet { rd, .. }
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
            | CallCanonicalExtern { rd, .. }
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
            | Op::IntToFloat32 { rd, .. }
            | Op::FloatNarrow { rd, .. }
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
            | Op::StringScalarLen { rd, .. }
            | Op::NewList { rd, .. }
            | Op::NewTuple { rd, .. }
            | Op::NewRecord { rd, .. }
            | Op::UpdateRecord { rd, .. }
            | Op::UpdateRecordMove { rd, .. }
            | Op::NewVariant { rd, .. }
            | Op::NewVariantMove { rd, .. }
            | Op::NewMap { rd, .. }
            | Op::NewSet { rd, .. }
            | Op::ListGet { rd, .. }
            | Op::ListPop { rd, .. }
            | Op::MirMapRootRemove { rd, .. }
            | Op::MirMapRootContains { rd, .. }
            | Op::MirMapAnyOwnerV1Remove { rd, .. }
            | Op::MirMapAnyOwnerV1Contains { rd, .. }
            | Op::MirOrdinaryMapOwnerLifecycleI32StringV1Clone { rd, .. }
            | Op::MirOrdinaryMapOwnerLifecycleI32StringV1Remove { rd, .. }
            | Op::MirOrdinaryMapOwnerLifecycleI32StringV1Contains { rd, .. }
            | Op::MirOrdinaryMapOwnerLifecycleObserverV1ToJson { rd, .. }
            | Op::MirOrdinaryMapOwnerLifecycleObserverV1Println { rd, .. }
            | Op::MirOrdinaryMapOwnedGetI32StringV1Clone { rd, .. }
            | Op::MirOrdinaryMapOwnedGetI32StringV1Remove { rd, .. }
            | Op::MirOrdinaryMapOwnedGetI32StringV1Contains { rd, .. }
            | Op::MirOrdinaryMapOwnedGetI32StringV1GetI32 { rd, .. }
            | Op::MirOrdinaryMapOwnedGetI32StringV1GetString { rd, .. }
            | Op::MirOrdinaryMapOwnedGetI32StringV1GetAny { rd, .. }
            | Op::MirOrdinaryMapOwnedGetI32StringV1Values { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2Clone { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2Remove { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2Contains { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2GetAny { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultString { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2Println { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2Unpack { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2Values { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2Keys { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2KeysUnpack { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2DynamicArg0 { rd }
            | Op::MirOrdinaryMapOwnedAnyV2ValuesClone { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2IsEmpty { rd, .. }
            | Op::TupleGet { rd, .. }
            | Op::RecordGet { rd, .. }
            | Op::RecordMoveGet { rd, .. }
            | Op::RecordMoveDropGet { rd, .. }
            | Op::VariantTag { rd, .. }
            | Op::VariantPayload { rd, .. }
            | Op::VariantGet { rd, .. }
            | Op::VariantMoveGet { rd, .. }
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
            | Op::CallCanonicalExtern { rd, .. }
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
            MirActorSpawn { .. } => false,
            MirActorCall {
                ra,
                args_base,
                argc,
                ..
            } => {
                *ra == reg
                    || (u32::from(reg) >= u32::from(*args_base)
                        && u32::from(reg) < u32::from(*args_base) + u32::from(*argc))
            }
            MirActorFieldLoad { state, .. } => *state == reg,
            MirActorFieldStore { state, value, .. } => *state == reg || *value == reg,
            // Ops that read no register:
            LoadConst { .. }
            | LoadUnit { .. }
            | LoadTrue { .. }
            | LoadFalse { .. }
            | None { .. }
            | NewMap { .. }
            | NewSet { .. }
            | MirSetNew { .. }
            | MirMapRootNew { .. }
            | MirMapAnyOwnerV1New { .. }
            | MirOrdinaryMapOwnerLifecycleI32StringV1New { .. }
            | MirOrdinaryMapOwnedGetI32StringV1New { .. }
            | MirOrdinaryMapOwnedAnyV2New { .. }
            | MirOrdinaryMapOwnedAnyV2NewResult { .. }
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
            DestructureVariantMove { ra, .. } | DestructureTupleMove { ra, .. } => *ra == reg,
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
            | MirListConcat { ra, rb, .. }
            | RecordSet { ra, rb, .. }
            | TupleSet { ra, rb, .. }
            | SharedSet { ra, rb, .. } => *ra == reg || *rb == reg,
            MirMapRootSet { ra, value, .. } => *ra == reg || *value == reg,
            MirMapAnyOwnerV1Set { ra, value, .. } => *ra == reg || *value == reg,
            MirOrdinaryMapOwnerLifecycleI32StringV1Set { ra, key, value, .. } => {
                *ra == reg || *key == reg || *value == reg
            }
            MirOrdinaryMapOwnedGetI32StringV1Set { ra, key, value, .. } => {
                *ra == reg || *key == reg || *value == reg
            }
            MirOrdinaryMapOwnerLifecycleI32StringV1Remove { ra, key, .. }
            | MirOrdinaryMapOwnerLifecycleI32StringV1Contains { ra, key, .. } => {
                *ra == reg || *key == reg
            }
            MirOrdinaryMapOwnedGetI32StringV1Remove { ra, key, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Contains { ra, key, .. }
            | MirOrdinaryMapOwnedGetI32StringV1GetI32 { ra, key, .. }
            | MirOrdinaryMapOwnedGetI32StringV1GetString { ra, key, .. }
            | MirOrdinaryMapOwnedGetI32StringV1GetAny { ra, key, .. } => *ra == reg || *key == reg,
            MirOrdinaryMapOwnedGetI32StringV1Values { ra, .. } => *ra == reg,
            MirOrdinaryMapOwnedAnyV2Set { ra, key, value, .. } => {
                *ra == reg || *key == reg || *value == reg
            }
            MirOrdinaryMapOwnedAnyV2Println { pair, .. } => *pair == reg,
            MirOrdinaryMapOwnedAnyV2Unpack { ra, .. } => *ra == reg,
            MirOrdinaryMapOwnedAnyV2ValuesUnpack { ra, index, .. } => *ra == reg || *index == reg,
            MirOrdinaryMapOwnedAnyV2Keys { ra, .. }
            | MirOrdinaryMapOwnedAnyV2KeysUnpack { ra, .. } => *ra == reg,
            MirOrdinaryMapOwnedAnyV2Remove { ra, key, .. }
            | MirOrdinaryMapOwnedAnyV2Contains { ra, key, .. }
            | MirOrdinaryMapOwnedAnyV2GetAny { ra, key, .. } => *ra == reg || *key == reg,
            MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 {
                ra, key, default, ..
            }
            | MirOrdinaryMapOwnedAnyV2GetOrDefaultString {
                ra, key, default, ..
            } => *ra == reg || *key == reg || *default == reg,
            MirOrdinaryMapOwnedAnyV2Values { ra, .. }
            | MirOrdinaryMapOwnedAnyV2IsEmpty { ra, .. } => *ra == reg,
            NegInt { ra, .. }
            | NegFloat { ra, .. }
            | BitNot { ra, .. }
            | Not { ra, .. }
            | IntToFloat { ra, .. }
            | IntToFloat32 { ra, .. }
            | FloatNarrow { ra, .. }
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
            | VariantMoveGet { ra, .. }
            | TupleGet { ra, .. }
            | RecordGet { ra, .. }
            | RecordMoveGet { ra, .. }
            | RecordMoveDropGet { ra, .. }
            | Len { ra, .. }
            | StringScalarLen { ra, .. }
            | ListPop { ra, .. }
            | MirSetSize { ra, .. }
            | MirSetIsEmpty { ra, .. }
            | MirSetContains { ra, .. }
            | MirSetToList { ra, .. }
            | MirListLen { ra, .. }
            | MirListReverse { ra, .. }
            | MirMapRootRemove { ra, .. }
            | MirMapRootSize { ra, .. }
            | MirMapRootContains { ra, .. }
            | MirMapRootDrop { ra }
            | MirMapAnyOwnerV1Remove { ra, .. }
            | MirMapAnyOwnerV1Size { ra, .. }
            | MirMapAnyOwnerV1Contains { ra, .. }
            | MirMapAnyOwnerV1Drop { ra }
            | MirOrdinaryMapOwnerLifecycleI32StringV1Clone { ra, .. }
            | MirOrdinaryMapOwnerLifecycleI32StringV1Size { ra, .. }
            | MirOrdinaryMapOwnerLifecycleI32StringV1Drop { ra }
            | MirOrdinaryMapOwnerLifecycleObserverV1ToJson { ra, .. }
            | MirOrdinaryMapOwnerLifecycleObserverV1Println { ra, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Clone { ra, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Size { ra, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Drop { ra }
            | MirOrdinaryMapOwnedAnyV2Clone { ra, .. }
            | MirOrdinaryMapOwnedAnyV2Size { ra, .. }
            | MirOrdinaryMapOwnedAnyV2Drop { ra }
            | MirOrdinaryMapOwnedAnyV2ValuesDrop { ra }
            | MirOrdinaryMapOwnedAnyV2ValuesClone { ra, .. }
            | MirManagedAnyOp { ra, .. }
            | MirVariantPredicate { ra, .. }
            | SharedNew { ra, .. }
            | WeakNew { ra, .. }
            | Ret { ra, .. }
            | RetEarly { ra, .. }
            | JmpIf { ra, .. }
            | JmpIfNot { ra, .. } => *ra == reg,
            MirManagedAnyStringCompare { any, string, .. } => *any == reg || *string == reg,
            MirVariantProjectOr { ra, rb, .. } => *ra == reg || *rb == reg,
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
            | CallCanonicalExtern {
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
            UpdateRecordMove {
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
    /// Exact checker identity plus selected Any opcode, separate from legacy dynamic Any.
    ManagedAnyContract(CanonicalManagedAnyBytecodeContract),
    /// Canonical record projection receipts encoded for the bytecode physical
    /// ABI. The MIR adapter copies this only from a validated TypeDesc.
    RecordProjection(RecordProjectionShape),
    /// Canonical record move projection with an explicit residual drop plan.
    RecordMoveDropProjection(RecordMoveDropProjectionShape),
    /// Canonical tuple projection receipts encoded for the bytecode physical
    /// ABI. The MIR adapter copies this only from a validated TypeDesc.
    TupleProjection(TupleProjectionShape),
    /// Canonical full tuple destructure receipt encoded for the bytecode
    /// physical ABI.
    TupleDestructure(TupleDestructureShape),
    /// Canonical List index projection receipts encoded for the bytecode
    /// physical ABI. Legacy ListGet has no receipt and remains separate.
    ListProjection(ListProjectionShape),
    /// Canonical List operation receipts encoded for the bytecode physical
    /// ABI. Legacy MirListLen has no receipt and remains separate.
    ListOperation(ListOperationShape),
    /// Canonical Option/Result predicate receipt encoded for the bytecode
    /// physical ABI.
    VariantPredicate(VariantPredicateShape),
    /// Canonical Result.unwrap_or receipt encoded for the bytecode physical
    /// ABI. The fallback arm and selected payload remain checker identities.
    VariantProjectionFallback(VariantProjectionFallbackShape),
}

/// Fully materialized checker receipt for one canonical managed Any operation.
#[derive(Debug, Clone)]
pub struct CanonicalManagedAnyBytecodeContract {
    pub operation: crate::core::mir::MirManagedAnyOperation,
    pub contract: crate::core::mir::MirManagedAnyContract,
}

/// One canonical variant shape in the bytecode physical contract.
///
/// The nominal and variant IDs are carried alongside the physical tag,
/// discriminant, and arity.  A tag alone is not a semantic identity: the
/// runtime must retain the checker-owned IDs until a consuming operation has
/// validated its TypeDesc contract.
#[derive(Debug, Clone)]
pub struct VariantShape {
    /// Canonical nominal identity copied from the MIR TypeDesc family.
    pub nominal: crate::core::ir::NominalTypeId,
    /// Canonical variant identity copied from the MIR TypeDesc descriptor.
    pub variant: crate::core::NodeId,
    pub tag: String,
    pub discriminant: VariantIdx,
    pub arity: u16,
}

/// Canonical Option/Result predicate receipt copied from MIR TypeDesc.
#[derive(Debug, Clone)]
pub struct VariantPredicateShape {
    pub variant_ty: crate::core::ResolvedTypeId,
    pub result_ty: crate::core::ResolvedTypeId,
    pub nominal: crate::core::ir::NominalTypeId,
    pub variant: crate::core::NodeId,
    pub variant_name: String,
    pub alternate_variant: crate::core::NodeId,
    pub alternate_variant_name: String,
    pub predicate: crate::core::mir::MirVariantPredicate,
    pub discriminant: VariantIdx,
}

/// Canonical Copy Option/Result payload-or-fallback receipt for MIR bytecode.
#[derive(Debug, Clone)]
pub struct VariantProjectionFallbackShape {
    pub source_ty: crate::core::ResolvedTypeId,
    pub result_ty: crate::core::ResolvedTypeId,
    pub fallback_ty: crate::core::ResolvedTypeId,
    pub nominal: crate::core::ir::NominalTypeId,
    pub variant: crate::core::NodeId,
    pub variant_name: String,
    pub discriminant: VariantIdx,
    pub fallback_variant: crate::core::NodeId,
    pub fallback_variant_name: String,
    pub fallback_discriminant: VariantIdx,
    pub fallback_arity: u16,
    pub field: crate::core::NodeId,
    pub field_index: u16,
    pub arity: u16,
    /// `true` for the managed Move fallback island.  The VM must consume the
    /// source aggregate and fallback register and may not clone either
    /// payload; Copy receipts keep the historical read-only behavior.
    pub consuming: bool,
}

/// One canonical record field projection in the bytecode physical contract.
/// The field type remains in the core MIR receipt; the VM needs the nominal,
/// field name, declaration-order index, and arity to reject forged records
/// before it reads or consumes a source register.
#[derive(Debug, Clone)]
pub struct RecordProjectionShape {
    pub nominal: crate::core::ir::NominalTypeId,
    pub field: crate::core::NodeId,
    pub name: String,
    pub index: FieldIdx,
    pub arity: u16,
}

#[derive(Debug, Clone)]
pub struct RecordMoveDropProjectionShape {
    pub base: RecordProjectionShape,
    pub residual: Vec<RecordResidualDropShape>,
}

#[derive(Debug, Clone)]
pub struct RecordResidualDropShape {
    pub field: crate::core::NodeId,
    pub name: String,
    pub index: FieldIdx,
    pub ty: crate::core::ResolvedTypeId,
    pub glue: crate::core::mir::types::MirGlueKind,
}

/// One canonical tuple field projection in the bytecode physical contract.
/// The structural source type remains in the core MIR receipt; the physical
/// shape carries that identity while the VM validates Tuple, index, and arity
/// before reading the untyped runtime vector.
#[derive(Debug, Clone)]
pub struct TupleProjectionShape {
    pub tuple_ty: crate::core::ResolvedTypeId,
    pub index: FieldIdx,
    pub arity: u16,
}

/// One canonical full tuple move/destructure shape. Element identities are
/// retained for auditability; the VM additionally checks the physical arity.
#[derive(Debug, Clone)]
pub struct TupleDestructureShape {
    pub tuple_ty: crate::core::ResolvedTypeId,
    pub element_tys: Vec<crate::core::ResolvedTypeId>,
}

/// One canonical read-only List index projection in the bytecode physical
/// contract. The type identities remain attached so the VM cannot treat a
/// generic list/string/set index as a canonical MIR List operation.
#[derive(Debug, Clone)]
pub struct ListProjectionShape {
    pub list_ty: crate::core::ResolvedTypeId,
    pub element_ty: crate::core::ResolvedTypeId,
    pub index_ty: crate::core::ResolvedTypeId,
    pub result_ty: crate::core::ResolvedTypeId,
    pub mode: crate::core::mir::types::MirListIndexProjectionMode,
}

/// One canonical read-only List operation in the bytecode physical contract.
/// The operation and type identities remain attached so the VM cannot treat a
/// generic handle operation as canonical MIR List.len.
#[derive(Debug, Clone)]
pub struct ListOperationShape {
    pub list_ty: crate::core::ResolvedTypeId,
    pub element_ty: crate::core::ResolvedTypeId,
    pub result_ty: crate::core::ResolvedTypeId,
    pub argument_ty: Option<crate::core::ResolvedTypeId>,
    pub operation: crate::core::mir::MirListOperation,
    pub mode: crate::core::mir::types::MirListOperationMode,
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

    /// Try to allocate a new register without wrapping the `u16` bytecode
    /// register ABI.  The legacy compiler still uses `alloc_reg`; canonical
    /// MIR consumers use this checked entry point so malformed or unusually
    /// large programs fail during emission instead of producing an invalid
    /// register index.
    pub fn try_alloc_reg(&mut self) -> Option<Reg> {
        let r = self.register_count;
        let next = r.checked_add(1)?;
        self.register_count = next;
        Some(r)
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

    fn checked_const_idx(len: usize) -> Result<ConstIdx, &'static str> {
        ConstIdx::try_from(len).map_err(|_| "constant pool index exceeds u32 ABI")
    }

    /// Checked constant-pool insertion used by canonical emitters.
    ///
    /// The legacy [`Self::add_const`] API remains available for compatibility;
    /// canonical MIR construction uses this result-returning variant so a
    /// malformed or oversized pool cannot wrap an index into an unrelated
    /// constant.
    pub(crate) fn try_add_const(&mut self, val: ConstValue) -> Result<ConstIdx, &'static str> {
        // Dedup for common cases, preserving the legacy equality policy while
        // checking the physical index before returning it.
        for (i, existing) in self.constants.iter().enumerate() {
            if std::mem::discriminant(existing) == std::mem::discriminant(&val) {
                match (&existing, &val) {
                    (ConstValue::Int(a), ConstValue::Int(b)) if a == b => {
                        return Self::checked_const_idx(i);
                    }
                    (ConstValue::Str(a), ConstValue::Str(b)) if a == b => {
                        return Self::checked_const_idx(i);
                    }
                    (ConstValue::Float(a), ConstValue::Float(b)) if a == b => {
                        return Self::checked_const_idx(i);
                    }
                    _ => {}
                }
            }
        }
        let index = Self::checked_const_idx(self.constants.len())?;
        self.constants.push(val);
        Ok(index)
    }

    /// Add a constant WITHOUT deduplication. Used for record field names
    /// which must be contiguous after the type name in the constant pool.
    pub fn add_const_raw(&mut self, val: ConstValue) -> ConstIdx {
        self.constants.push(val);
        (self.constants.len() - 1) as ConstIdx
    }

    /// Checked non-deduplicating constant-pool insertion used by canonical
    /// record construction, where field names must remain contiguous.
    pub(crate) fn try_add_const_raw(&mut self, val: ConstValue) -> Result<ConstIdx, &'static str> {
        let index = Self::checked_const_idx(self.constants.len())?;
        self.constants.push(val);
        Ok(index)
    }

    /// Emit an instruction and return its index.
    pub fn emit(&mut self, op: Op) -> usize {
        self.code.push(op);
        self.code.len() - 1
    }

    /// Patch a jump instruction at `idx` with the correct offset
    /// to jump to the current end of code.
    pub fn patch_jump(&mut self, idx: usize) {
        let target = self.code.len();
        let _ = self.try_patch_jump_to(idx, target);
    }

    /// Patch a jump instruction at `idx` to jump to an explicit target instruction.
    pub fn patch_jump_to(&mut self, idx: usize, target: usize) {
        let _ = self.try_patch_jump_to(idx, target);
    }

    /// Checked variant of [`Self::patch_jump_to`] used by canonical emitters.
    /// Invalid instruction indices, non-jump instructions, and offsets outside
    /// the bytecode `i32` jump ABI are reported instead of panicking or
    /// silently leaving an unpatched instruction behind.
    pub(crate) fn try_patch_jump_to(
        &mut self,
        idx: usize,
        target: usize,
    ) -> Result<(), &'static str> {
        let next = idx
            .checked_add(1)
            .ok_or("jump patch origin overflows usize")?;
        let distance = if target >= next {
            i64::try_from(target - next).map_err(|_| "jump patch offset exceeds i64 ABI")?
        } else {
            -i64::try_from(next - target).map_err(|_| "jump patch offset exceeds i64 ABI")?
        };
        let offset = i32::try_from(distance).map_err(|_| "jump patch offset exceeds i32 ABI")?;
        let op = self
            .code
            .get_mut(idx)
            .ok_or("jump patch instruction index out of range")?;
        match op {
            Op::Jmp { offset: o } => *o = offset,
            Op::JmpIf { offset: o, .. } => *o = offset,
            Op::JmpIfNot { offset: o, .. } => *o = offset,
            _ => return Err("jump patch target is not a jump instruction"),
        }
        Ok(())
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
    /// Extern declaration names retained for old bytecode inspection.
    /// `CallExtern` is rejected by the VM and this table is never used for
    /// host symbol resolution.
    /// (0.33 Phase D FFI forwarding).
    pub extern_names: Vec<String>,
    /// Checker-owned scalar FFI descriptors materialized from Canonical MIR.
    /// Canonical bytecode execution uses this table directly and never needs
    /// an AST or legacy `ExternFunc` declaration.  Compatibility bytecode
    /// leaves the table empty and uses `extern_names` instead.
    pub canonical_ffi: Vec<CanonicalFfiDescriptor>,
    /// Internal source binding for every canonical FFI call site.  The
    /// compiler records this snapshot before the program crosses the VM
    /// boundary so public bytecode fields cannot swap a call's descriptor and
    /// instruction identity together without being detected.
    pub(crate) canonical_ffi_bindings: Vec<CanonicalFfiBinding>,
    /// Private compiler snapshot for canonical MIR programs that contain
    /// Checker-owned MapRoot actions. Compatibility bytecode has no snapshot;
    /// the VM then rejects any injected MIR MapRoot opcode.
    pub(crate) canonical_map_root_receipt: Option<CanonicalMapRootBytecodeReceipt>,
    /// Independent private snapshot for Checker-owned MapAnyOwner v1
    /// actions. This profile-specific receipt cannot authorize raw MapRoot
    /// opcodes or values.
    pub(crate) canonical_map_any_owner_v1_receipt: Option<CanonicalMapAnyOwnerV1BytecodeReceipt>,
    /// Route receipt anchor for canonical MIR bytecode. Compatibility
    /// bytecode leaves this unset; receipt-bound consumers copy the checked
    /// admission snapshot here so VM children and re-entry share one source
    /// of truth.
    pub(crate) canonical_ffi_route_receipt: Option<crate::core::mir::CanonicalMirRouteReceipt>,
    /// Sealed typed Actor manifest emitted only by the AST-free MIR adapter.
    pub(crate) canonical_actors: Option<super::actor::CanonicalActorManifest>,
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

/// Private admission snapshot for the public, mutable bytecode tables used by
/// the closed MapRoot profile. The entry index and full `FunctionProto` table
/// are captured because bytecode register writes around a MapRoot opcode also
/// determine its ownership and value flow. Current MapRoot bytecode does not
/// consult the program-level actor/flow/type metadata. This is an in-process
/// provenance check, not a serialized or cryptographic bytecode format.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalMapRootBytecodeReceipt {
    expected_has_map_root: bool,
    expected_program: String,
    /// Independent canonical-MIR presence marker for the Any/CFG snapshot.
    /// This marker lives beside, rather than inside, the Map-owner receipt
    /// carrier so deleting that carrier or its CFG child cannot be hidden by
    /// a still-valid Map snapshot.
    expected_managed_any_cfg_receipt: bool,
}

impl CanonicalMapRootBytecodeReceipt {
    pub(crate) fn capture(entry: FuncIdx, functions: &[FunctionProto]) -> Self {
        let expected_has_map_root = functions.iter().any(function_has_map_root_ops);
        Self {
            expected_has_map_root,
            expected_program: if expected_has_map_root {
                map_root_program_snapshot(entry, functions)
            } else {
                String::new()
            },
            expected_managed_any_cfg_receipt: false,
        }
    }

    pub(crate) fn require_managed_any_cfg_receipt(&mut self) {
        self.expected_managed_any_cfg_receipt = true;
    }

    pub(crate) fn expects_managed_any_cfg_receipt(&self) -> bool {
        self.expected_managed_any_cfg_receipt
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_has_map_root && functions.iter().any(function_has_map_any_owner_v1_ops) {
            return Err(
                "canonical MapRoot receipt cannot authorize MapAnyOwner v1 bytecode".into(),
            );
        }
        if self.expected_has_map_root
            && functions
                .iter()
                .any(function_has_ordinary_map_owner_lifecycle_i32_string_v1_ops)
        {
            return Err(
                "MapRoot receipt cannot authorize ordinary Map owner lifecycle v1 bytecode".into(),
            );
        }
        if self.expected_has_map_root
            && functions
                .iter()
                .any(function_has_ordinary_map_owned_get_i32_string_v1_ops)
        {
            return Err(
                "MapRoot receipt cannot authorize ordinary Map owned-Get v1 bytecode".into(),
            );
        }
        if self.expected_has_map_root
            && functions
                .iter()
                .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err(
                "MapRoot receipt cannot authorize ordinary Map/Any owner-v2 bytecode".into(),
            );
        }
        let has_map_root = functions.iter().any(function_has_map_root_ops);
        if !self.expected_has_map_root && !has_map_root {
            return Ok(());
        }
        if !self.expected_has_map_root && has_map_root {
            return Err("canonical MapRoot bytecode has no Checker-owned MIR receipt".into());
        }
        if !has_map_root || map_root_program_snapshot(entry, functions) != self.expected_program {
            return Err(
                "canonical MapRoot bytecode disagrees with its Checker-owned receipt snapshot"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Compiler-owned VM snapshot for a Checker-sealed generic ManagedAny
/// identity instance. This is kept separate from the ordinary Map owner
/// receipts: a ManagedAny TypeDesc alone never authorizes a generic call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalManagedAnyIdentityInstanceBinding {
    pub(crate) instance_id: String,
    pub(crate) template: String,
    /// The source generic argument (DynamicAny), not the specialized ABI ID.
    pub(crate) type_argument: String,
    pub(crate) managed_type_argument: String,
    pub(crate) owner: String,
    pub(crate) function: FuncIdx,
    pub(crate) parameter: String,
    pub(crate) parameter_type: String,
    pub(crate) clone_result: String,
    pub(crate) result_type: String,
    pub(crate) clone_instruction: String,
    pub(crate) clone_origin: String,
    pub(crate) drop_instruction: String,
    pub(crate) drop_origin: String,
}

/// A direct generic identity call as emitted by the MIR adapter. The
/// immediately preceding `Move` is pinned to establish opaque-owner transfer
/// into the one-register `CallMove` argument window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalManagedAnyIdentityCallBinding {
    pub(crate) instance_id: String,
    pub(crate) caller: String,
    pub(crate) caller_index: FuncIdx,
    pub(crate) instruction: String,
    pub(crate) callee: String,
    pub(crate) callee_index: FuncIdx,
    pub(crate) type_argument: String,
    pub(crate) argument: String,
    pub(crate) argument_type: String,
    pub(crate) result: String,
    pub(crate) result_type: String,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) args_base: Reg,
    pub(crate) argc: u16,
    pub(crate) transfer_pc: u32,
    pub(crate) transfer_rd: Reg,
    pub(crate) transfer_rs: Reg,
}

/// One source MIR function whose signature, values, or call boundary contains
/// DynamicAny/ManagedAny.  The source type summary is Checker/MIR-derived;
/// the bytecode index/name binding prevents a public function-table edit from
/// detaching that source census from its emitted function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalManagedAnyCfgFunctionBinding {
    pub(crate) owner: String,
    pub(crate) function_index: FuncIdx,
    pub(crate) source_signature: String,
}

/// A direct Any-bearing MIR call or receipted ClosureInvoke paired with its
/// exact emitted CallMove site.  Existing identity/trap receipts further bind
/// their specialized instance and transfer Move; this census records all
/// other source call-edge identities and keeps ClosureInvoke distinct from an
/// ordinary direct call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalManagedAnyCfgCallBinding {
    pub(crate) caller: String,
    pub(crate) caller_index: FuncIdx,
    pub(crate) source_block: String,
    pub(crate) source_instruction: String,
    pub(crate) source_kind: String,
    pub(crate) callee: String,
    pub(crate) callee_index: FuncIdx,
    pub(crate) source_type_arguments: Vec<String>,
    pub(crate) source_arguments: Vec<String>,
    pub(crate) source_result: Option<String>,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) args_base: Reg,
    pub(crate) argc: u16,
}

/// Compiler-owned receipt for the exact instance table, callee bodies, and
/// call edges. A complete bytecode snapshot makes tampering fail during VM
/// preflight, before output or other user effects.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalManagedAnyIdentityBytecodeReceipt {
    profile_version: u16,
    instances: Vec<CanonicalManagedAnyIdentityInstanceBinding>,
    calls: Vec<CanonicalManagedAnyIdentityCallBinding>,
    expected_program: String,
}

impl CanonicalManagedAnyIdentityBytecodeReceipt {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        instances: Vec<CanonicalManagedAnyIdentityInstanceBinding>,
        calls: Vec<CanonicalManagedAnyIdentityCallBinding>,
    ) -> Self {
        let expected_program = if instances.is_empty() {
            String::new()
        } else {
            managed_any_identity_program_snapshot(entry, functions)
        };
        Self {
            profile_version: 1,
            instances,
            calls,
            expected_program,
        }
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.profile_version != 1 {
            return Err(
                "ManagedAny generic identity receipt has an unsupported profile version".into(),
            );
        }
        let mut candidates = std::collections::BTreeSet::new();
        for (index, function) in functions.iter().enumerate() {
            if function_is_managed_any_identity_candidate(function) {
                candidates.insert(
                    FuncIdx::try_from(index)
                        .map_err(|_| "ManagedAny generic identity callee index exceeds u32 ABI")?,
                );
            }
        }
        let instance_functions = self
            .instances
            .iter()
            .map(|instance| instance.function)
            .collect::<std::collections::BTreeSet<_>>();
        if self.instances.is_empty() {
            if !self.calls.is_empty() || !candidates.is_empty() {
                return Err(
                    "ManagedAny generic identity bytecode has no Checker instance receipt".into(),
                );
            }
            return Ok(());
        }
        if self.calls.is_empty() {
            return Err("ManagedAny generic identity receipt has no direct call site".into());
        }
        if candidates != instance_functions {
            return Err(
                "ManagedAny generic identity candidate/callee set disagrees with Checker instances"
                    .into(),
            );
        }
        if managed_any_identity_program_snapshot(entry, functions) != self.expected_program {
            return Err(
                "ManagedAny generic identity bytecode disagrees with its private Checker snapshot"
                    .into(),
            );
        }

        let managed_type = crate::core::mir::types::managed_any_i32_string_v1_type_id();
        let managed_type_text = managed_type.as_str();
        let mut instance_ids = std::collections::BTreeSet::new();
        let mut callee_indices = std::collections::BTreeSet::new();
        for instance in &self.instances {
            if instance.instance_id.trim().is_empty()
                || instance.template.trim().is_empty()
                || instance.type_argument.trim().is_empty()
                || instance.type_argument == instance.managed_type_argument
                || instance.managed_type_argument != managed_type_text
                || instance.owner.trim().is_empty()
                || instance.parameter.trim().is_empty()
                || instance.parameter_type != managed_type_text
                || instance.clone_result.trim().is_empty()
                || instance.result_type != managed_type_text
                || instance.clone_instruction.trim().is_empty()
                || instance.clone_origin.trim().is_empty()
                || instance.drop_instruction.trim().is_empty()
                || instance.drop_origin.trim().is_empty()
                || !instance_ids.insert(instance.instance_id.clone())
                || !callee_indices.insert(instance.function)
            {
                return Err(
                    "ManagedAny generic identity instance binding is incomplete or duplicated"
                        .into(),
                );
            }
            let function = functions.get(instance.function as usize).ok_or_else(|| {
                format!(
                    "ManagedAny generic identity callee {} is outside the bytecode function table",
                    instance.function
                )
            })?;
            validate_managed_any_identity_callee(function, instance)?;
        }

        let by_function = self
            .instances
            .iter()
            .map(|instance| (instance.function, instance))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut expected_sites = std::collections::BTreeSet::new();
        for call in &self.calls {
            if call.instance_id.trim().is_empty()
                || call.caller.trim().is_empty()
                || call.instruction.trim().is_empty()
                || call.callee.trim().is_empty()
                || call.type_argument.trim().is_empty()
                || call.argument.trim().is_empty()
                || call.argument_type != managed_type_text
                || call.result.trim().is_empty()
                || call.result_type != managed_type_text
                || call.argc != 1
                || call.pc == 0
                || call.transfer_pc.checked_add(1) != Some(call.pc)
                || call.transfer_rd != call.args_base
                || !expected_sites.insert((call.caller_index, call.pc))
            {
                return Err("ManagedAny generic identity call binding is incomplete, duplicated, or not owner-moving".into());
            }
            let instance = by_function.get(&call.callee_index).ok_or_else(|| {
                format!(
                    "ManagedAny generic identity call at {} pc {} has no Checker instance",
                    call.caller, call.pc
                )
            })?;
            if call.instance_id != instance.instance_id
                || call.callee != instance.owner
                || call.type_argument != instance.type_argument
            {
                return Err(
                    "ManagedAny generic identity call does not match its source instance receipt"
                        .into(),
                );
            }
            let caller = functions.get(call.caller_index as usize).ok_or_else(|| {
                format!(
                    "ManagedAny generic identity caller {} is outside the bytecode function table",
                    call.caller_index
                )
            })?;
            let register_count = caller.register_count as usize;
            let argument_end = (call.args_base as usize).checked_add(call.argc as usize);
            if caller.name != call.caller
                || call.rd as usize >= register_count
                || call.transfer_rd as usize >= register_count
                || call.transfer_rs as usize >= register_count
                || argument_end.is_none_or(|end| end > register_count)
            {
                return Err(
                    "ManagedAny generic identity caller identity or owner-transfer registers disagree with its function frame".into(),
                );
            }
            match caller.code.get(call.pc as usize) {
                Some(Op::CallMove {
                    rd,
                    func,
                    args_base,
                    argc,
                }) if *rd == call.rd
                    && *func == call.callee_index
                    && *args_base == call.args_base
                    && *argc == call.argc => {}
                _ => {
                    return Err(
                        "ManagedAny generic identity call no longer matches its CallMove binding"
                            .into(),
                    );
                }
            }
            match caller.code.get(call.transfer_pc as usize) {
                Some(Op::Move { rd, rs }) if *rd == call.transfer_rd && *rs == call.transfer_rs => {
                }
                _ => {
                    return Err(
                        "ManagedAny generic identity call argument is not the receipted owner Move"
                            .into(),
                    );
                }
            }
        }

        let mut actual_sites = std::collections::BTreeSet::new();
        for (caller_index, caller) in functions.iter().enumerate() {
            for (pc, op) in caller.code.iter().enumerate() {
                match op {
                    Op::CallMove { func, .. } if callee_indices.contains(func) => {
                        let pc = u32::try_from(pc)
                            .map_err(|_| "ManagedAny generic identity call pc exceeds u32 ABI")?;
                        let caller_index = FuncIdx::try_from(caller_index).map_err(|_| {
                            "ManagedAny generic identity caller index exceeds u32 ABI"
                        })?;
                        if !actual_sites.insert((caller_index, pc)) {
                            return Err(
                                "ManagedAny generic identity call site is duplicated".into()
                            );
                        }
                    }
                    Op::Call { func, .. } if callee_indices.contains(func) => {
                        return Err("ManagedAny generic identity direct call must use owner-moving CallMove".into());
                    }
                    Op::NewClosure { proto, .. } if callee_indices.contains(proto) => {
                        return Err(
                            "ManagedAny generic identity callee cannot escape through a closure"
                                .into(),
                        );
                    }
                    Op::Spawn { func, .. } if callee_indices.contains(func) => {
                        return Err("ManagedAny generic identity callee cannot be invoked as a spawned task".into());
                    }
                    _ => {}
                }
            }
        }
        if actual_sites != expected_sites {
            return Err("ManagedAny generic identity bytecode has a missing or unreceipted direct call site".into());
        }
        for instance in &self.instances {
            if !self
                .calls
                .iter()
                .any(|call| call.callee_index == instance.function)
            {
                return Err(
                    "ManagedAny generic identity instance is not referenced by a direct call"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

fn managed_any_identity_program_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile=managed-any-generic-identity;version=1;entry={entry};functions={}",
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

pub(crate) fn function_is_managed_any_identity_candidate(function: &FunctionProto) -> bool {
    if !function.name.starts_with("function:mir:instance:")
        || function.param_count != 1
        || function.is_async
    {
        return false;
    }
    matches!(function.code.as_slice(), [
        Op::MirManagedAnyOp {
            rd: Some(clone_rd), ra: clone_ra,
            operation: crate::core::mir::MirManagedAnyOperation::Clone, ..
        },
        Op::MirManagedAnyOp {
            rd: None, ra: drop_ra,
            operation: crate::core::mir::MirManagedAnyOperation::Drop, ..
        },
        Op::Ret { ra: return_ra },
    ] if clone_ra == drop_ra && clone_rd == return_ra)
}

fn validate_managed_any_identity_callee(
    function: &FunctionProto,
    instance: &CanonicalManagedAnyIdentityInstanceBinding,
) -> Result<(), String> {
    if function.name != instance.owner
        || function.param_count != 1
        || function.is_async
        || function.register_count < 2
    {
        return Err("ManagedAny generic identity callee owner/signature disagrees with its Checker instance".into());
    }
    let [Op::MirManagedAnyOp {
        rd: Some(clone_rd),
        ra: clone_ra,
        operation: crate::core::mir::MirManagedAnyOperation::Clone,
        contract: clone_contract,
    }, Op::MirManagedAnyOp {
        rd: None,
        ra: drop_ra,
        operation: crate::core::mir::MirManagedAnyOperation::Drop,
        contract: drop_contract,
    }, Op::Ret { ra: return_ra }] = function.code.as_slice()
    else {
        return Err("ManagedAny generic identity callee must be exactly Clone; Drop(parameter); Return(clone)".into());
    };
    if *clone_ra != 0
        || *drop_ra != *clone_ra
        || *clone_rd == *clone_ra
        || *clone_rd >= function.register_count
        || *return_ra != *clone_rd
    {
        return Err(
            "ManagedAny generic identity callee does not preserve the checked owner schedule"
                .into(),
        );
    }
    let clone = managed_any_contract_from_constant(function, *clone_contract, "Clone")?;
    let drop = managed_any_contract_from_constant(function, *drop_contract, "Drop")?;
    let managed_type = crate::core::mir::types::managed_any_i32_string_v1_type_id();
    if clone.operation != crate::core::mir::MirManagedAnyOperation::Clone
        || clone.contract.owner.0 != instance.owner
        || clone.contract.origin.0 != instance.clone_origin
        || clone.contract.source.as_str() != instance.parameter
        || clone.contract.result.as_ref().map(|id| id.as_str())
            != Some(instance.clone_result.as_str())
        || clone.contract.source_type != managed_type
        || clone.contract.target_type.as_ref() != Some(&managed_type)
        || clone.contract.expected_kind.is_some()
        || drop.operation != crate::core::mir::MirManagedAnyOperation::Drop
        || drop.contract.owner.0 != instance.owner
        || drop.contract.origin.0 != instance.drop_origin
        || drop.contract.source.as_str() != instance.parameter
        || drop.contract.result.is_some()
        || drop.contract.source_type != managed_type
        || drop.contract.target_type.is_some()
        || drop.contract.expected_kind.is_some()
    {
        return Err(
            "ManagedAny generic identity callee operation constants disagree with the Checker seal"
                .into(),
        );
    }
    Ok(())
}

fn managed_any_contract_from_constant<'a>(
    function: &'a FunctionProto,
    index: ConstIdx,
    operation: &str,
) -> Result<&'a CanonicalManagedAnyBytecodeContract, String> {
    match function.constants.get(index as usize) {
        Some(ConstValue::ManagedAnyContract(contract))
            if contract.contract.schema_version == 1
                && contract.contract.descriptor_version == 1
                && contract.contract.owner.0 == function.name =>
        {
            Ok(contract)
        }
        Some(_) => Err(format!(
            "ManagedAny generic identity {operation} has a missing or wrong contract constant"
        )),
        None => Err(format!(
            "ManagedAny generic identity {operation} contract constant is out of range"
        )),
    }
}

/// Checker instance binding for the separate generic ManagedAny trap-cleanup
/// profile. The source argument remains DynamicAny while the executable
/// parameter and call argument use canonical ManagedAny.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalManagedAnyTrapCleanupInstanceBinding {
    pub(crate) instance_id: String,
    pub(crate) template: String,
    pub(crate) type_argument: String,
    pub(crate) managed_type_argument: String,
    pub(crate) owner: String,
    pub(crate) function: FuncIdx,
    pub(crate) trap: crate::core::mir::MirManagedAnyTrapKind,
    pub(crate) parameter: String,
    pub(crate) parameter_type: String,
    pub(crate) trap_result: String,
    pub(crate) result_type: String,
    pub(crate) drop_instruction: String,
    pub(crate) drop_origin: String,
    pub(crate) trap_instruction: String,
    pub(crate) trap_origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalManagedAnyTrapCleanupCallBinding {
    pub(crate) instance_id: String,
    pub(crate) caller: String,
    pub(crate) caller_index: FuncIdx,
    pub(crate) instruction: String,
    pub(crate) callee: String,
    pub(crate) callee_index: FuncIdx,
    pub(crate) type_argument: String,
    pub(crate) argument: String,
    pub(crate) argument_type: String,
    pub(crate) result: String,
    pub(crate) result_type: String,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) args_base: Reg,
    pub(crate) argc: u16,
    pub(crate) transfer_pc: u32,
    pub(crate) transfer_rd: Reg,
    pub(crate) transfer_rs: Reg,
}

/// Private bytecode receipt for generic callees that consume their canonical
/// ManagedAny parameter immediately before one sealed, guaranteed i32 trap.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalManagedAnyTrapCleanupBytecodeReceipt {
    profile_version: u16,
    instances: Vec<CanonicalManagedAnyTrapCleanupInstanceBinding>,
    calls: Vec<CanonicalManagedAnyTrapCleanupCallBinding>,
    expected_program: String,
}

impl CanonicalManagedAnyTrapCleanupBytecodeReceipt {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        instances: Vec<CanonicalManagedAnyTrapCleanupInstanceBinding>,
        calls: Vec<CanonicalManagedAnyTrapCleanupCallBinding>,
    ) -> Self {
        let expected_program = if instances.is_empty() {
            String::new()
        } else {
            managed_any_trap_cleanup_program_snapshot(entry, functions)
        };
        Self {
            profile_version: 1,
            instances,
            calls,
            expected_program,
        }
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.profile_version != 1 {
            return Err(
                "ManagedAny trap-cleanup receipt has an unsupported profile version".into(),
            );
        }
        let mut candidates = std::collections::BTreeSet::new();
        for (index, function) in functions.iter().enumerate() {
            if function_is_managed_any_trap_cleanup_candidate(function) {
                candidates.insert(
                    FuncIdx::try_from(index)
                        .map_err(|_| "ManagedAny trap-cleanup callee index exceeds u32 ABI")?,
                );
            }
        }
        let instance_functions = self
            .instances
            .iter()
            .map(|instance| instance.function)
            .collect::<std::collections::BTreeSet<_>>();
        if self.instances.is_empty() {
            if !self.calls.is_empty() || !candidates.is_empty() {
                return Err(
                    "ManagedAny trap-cleanup bytecode has no Checker instance receipt".into(),
                );
            }
            return Ok(());
        }
        if self.calls.is_empty() {
            return Err("ManagedAny trap-cleanup receipt has no direct call site".into());
        }
        if candidates != instance_functions {
            return Err(
                "ManagedAny trap-cleanup candidate/callee set disagrees with Checker instances"
                    .into(),
            );
        }
        if managed_any_trap_cleanup_program_snapshot(entry, functions) != self.expected_program {
            return Err(
                "ManagedAny trap-cleanup bytecode disagrees with its private Checker snapshot"
                    .into(),
            );
        }

        let managed_type = crate::core::mir::types::managed_any_i32_string_v1_type_id();
        let managed_type_text = managed_type.as_str();
        let i32_type = crate::core::ir::primitive_type_id(crate::core::ir::PrimitiveType::I32);
        let i32_type_text = i32_type.as_str();
        let mut instance_ids = std::collections::BTreeSet::new();
        let mut callee_indices = std::collections::BTreeSet::new();
        for instance in &self.instances {
            if instance.instance_id.trim().is_empty()
                || instance.template.trim().is_empty()
                || instance.type_argument.trim().is_empty()
                || instance.type_argument == instance.managed_type_argument
                || instance.managed_type_argument != managed_type_text
                || instance.instance_id
                    != format!("instance:{}<{}>", instance.template, instance.type_argument)
                || instance.owner.trim().is_empty()
                || instance.owner != format!("function:mir:{}", instance.instance_id)
                || instance.parameter.trim().is_empty()
                || instance.parameter_type != managed_type_text
                || instance.trap_result.trim().is_empty()
                || instance.result_type != i32_type_text
                || instance.drop_instruction
                    != format!(
                        "inst:managed_any_trap_cleanup_drop:{}",
                        instance.instance_id
                    )
                || instance.drop_origin.trim().is_empty()
                || instance.trap_instruction != format!("inst:binary:{}", instance.trap_origin)
                || !instance_ids.insert(instance.instance_id.clone())
                || !callee_indices.insert(instance.function)
            {
                return Err(
                    "ManagedAny trap-cleanup instance binding is incomplete or duplicated".into(),
                );
            }
            let function = functions.get(instance.function as usize).ok_or_else(|| {
                format!(
                    "ManagedAny trap-cleanup callee {} is outside the bytecode function table",
                    instance.function
                )
            })?;
            validate_managed_any_trap_cleanup_callee(function, instance)?;
        }

        let by_function = self
            .instances
            .iter()
            .map(|instance| (instance.function, instance))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut expected_sites = std::collections::BTreeSet::new();
        for call in &self.calls {
            if call.instance_id.trim().is_empty()
                || call.caller.trim().is_empty()
                || call.instruction.trim().is_empty()
                || call.callee.trim().is_empty()
                || call.type_argument.trim().is_empty()
                || call.argument.trim().is_empty()
                || call.argument_type != managed_type_text
                || call.result.trim().is_empty()
                || call.result_type != i32_type_text
                || call.argc != 1
                || call.pc == 0
                || call.transfer_pc.checked_add(1) != Some(call.pc)
                || call.transfer_rd != call.args_base
                || !expected_sites.insert((call.caller_index, call.pc))
            {
                return Err("ManagedAny trap-cleanup call binding is incomplete, duplicated, or not owner-moving".into());
            }
            let instance = by_function.get(&call.callee_index).ok_or_else(|| {
                format!(
                    "ManagedAny trap-cleanup call at {} pc {} has no Checker instance",
                    call.caller, call.pc
                )
            })?;
            if call.instance_id != instance.instance_id
                || call.callee != instance.owner
                || call.type_argument != instance.type_argument
            {
                return Err(
                    "ManagedAny trap-cleanup call does not match its source instance receipt"
                        .into(),
                );
            }
            let caller = functions.get(call.caller_index as usize).ok_or_else(|| {
                format!(
                    "ManagedAny trap-cleanup caller {} is outside the bytecode function table",
                    call.caller_index
                )
            })?;
            let register_count = caller.register_count as usize;
            let argument_end = (call.args_base as usize).checked_add(call.argc as usize);
            if caller.name != call.caller
                || call.rd as usize >= register_count
                || call.transfer_rd as usize >= register_count
                || call.transfer_rs as usize >= register_count
                || argument_end.is_none_or(|end| end > register_count)
            {
                return Err(
                    "ManagedAny trap-cleanup caller identity or owner-transfer registers disagree with its function frame".into(),
                );
            }
            match caller.code.get(call.pc as usize) {
                Some(Op::CallMove {
                    rd,
                    func,
                    args_base,
                    argc,
                }) if *rd == call.rd
                    && *func == call.callee_index
                    && *args_base == call.args_base
                    && *argc == call.argc => {}
                _ => {
                    return Err(
                        "ManagedAny trap-cleanup call no longer matches its CallMove binding"
                            .into(),
                    );
                }
            }
            match caller.code.get(call.transfer_pc as usize) {
                Some(Op::Move { rd, rs }) if *rd == call.transfer_rd && *rs == call.transfer_rs => {
                }
                _ => {
                    return Err(
                        "ManagedAny trap-cleanup argument is not the receipted owner Move".into(),
                    );
                }
            }
        }

        let mut actual_sites = std::collections::BTreeSet::new();
        for (caller_index, caller) in functions.iter().enumerate() {
            for (pc, op) in caller.code.iter().enumerate() {
                match op {
                    Op::CallMove { func, .. } if callee_indices.contains(func) => {
                        let pc = u32::try_from(pc)
                            .map_err(|_| "ManagedAny trap-cleanup call pc exceeds u32 ABI")?;
                        let caller_index = FuncIdx::try_from(caller_index)
                            .map_err(|_| "ManagedAny trap-cleanup caller index exceeds u32 ABI")?;
                        if !actual_sites.insert((caller_index, pc)) {
                            return Err("ManagedAny trap-cleanup call site is duplicated".into());
                        }
                    }
                    Op::Call { func, .. } if callee_indices.contains(func) => {
                        return Err(
                            "ManagedAny trap-cleanup direct call must use owner-moving CallMove"
                                .into(),
                        );
                    }
                    Op::NewClosure { proto, .. } if callee_indices.contains(proto) => {
                        return Err(
                            "ManagedAny trap-cleanup callee cannot escape through a closure".into(),
                        );
                    }
                    Op::Spawn { func, .. } if callee_indices.contains(func) => {
                        return Err(
                            "ManagedAny trap-cleanup callee cannot be invoked as a spawned task"
                                .into(),
                        );
                    }
                    _ => {}
                }
            }
        }
        if actual_sites != expected_sites {
            return Err(
                "ManagedAny trap-cleanup bytecode has a missing or unreceipted direct call site"
                    .into(),
            );
        }
        for instance in &self.instances {
            if !self
                .calls
                .iter()
                .any(|call| call.callee_index == instance.function)
            {
                return Err(
                    "ManagedAny trap-cleanup instance is not referenced by a direct call".into(),
                );
            }
        }
        Ok(())
    }
}

fn managed_any_trap_cleanup_program_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile=managed-any-trap-cleanup;version=1;entry={entry};functions={}",
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

pub(crate) fn function_is_managed_any_trap_cleanup_candidate(function: &FunctionProto) -> bool {
    if !function.name.starts_with("function:mir:instance:")
        || function.param_count != 1
        || function.is_async
        || !function.capture_names.is_empty()
    {
        return false;
    }
    let code = function.code.as_slice();
    if code.len() != 8
        || !matches!(&code[0], Op::LoadConst { .. })
        || !matches!(&code[1], Op::Mov { .. })
        // Division's checked body materializes `1` then clones the bound
        // zero; overflow's source expression is `MAX + 1`, so MIR emits the
        // clone before the literal addend. Both are the same sealed
        // four-operation setup and are distinguished by the trap tail below.
        || !(matches!(&code[2], Op::LoadConst { .. }) && matches!(&code[3], Op::Mov { .. })
            || matches!(&code[2], Op::Mov { .. }) && matches!(&code[3], Op::LoadConst { .. }))
        || !matches!(
            &code[4],
            Op::MirManagedAnyOp {
                rd: None,
                operation: crate::core::mir::MirManagedAnyOperation::Drop,
                ..
            }
        )
    {
        return false;
    }
    matches!(
        &code[5..],
        [Op::CheckI32DivRem { .. }, Op::DivInt { .. }, Op::Ret { .. }]
            | [
                Op::AddInt { .. },
                Op::CheckI32 { kind: 0, .. },
                Op::Ret { .. }
            ]
    )
}

fn validate_managed_any_trap_cleanup_callee(
    function: &FunctionProto,
    instance: &CanonicalManagedAnyTrapCleanupInstanceBinding,
) -> Result<(), String> {
    if function.name != instance.owner
        || function.param_count != 1
        || function.is_async
        || function.has_mut_params
        || !function.mut_param_indices.is_empty()
        || !function.capture_names.is_empty()
        || function.has_requires
        || function.has_ensures
        || !function.requires_funcs.is_empty()
        || !function.ensures_funcs.is_empty()
        || function.register_count != 6
        || !function.param_names.is_empty()
    {
        return Err(
            "ManagedAny trap-cleanup callee owner/signature disagrees with its Checker instance"
                .into(),
        );
    }
    let (drop_ra, drop_contract) =
        match function.code.get(4) {
            Some(Op::MirManagedAnyOp {
                rd: None,
                ra,
                operation: crate::core::mir::MirManagedAnyOperation::Drop,
                contract,
            }) => (ra, contract),
            _ => return Err(
                "ManagedAny trap-cleanup callee has no receipted parameter Drop before its trap"
                    .into(),
            ),
        };
    let (initial_rd, bound_rd, one_rd, read_rd, trap_rd, trap_ra, trap_rb, terminal_shape_ok) =
        match instance.trap {
            crate::core::mir::MirManagedAnyTrapKind::DivisionByZeroI32 => {
                let [Op::LoadConst {
                    rd: initial_rd,
                    idx: initial_idx,
                }, Op::Mov {
                    rd: bound_rd,
                    rs: bound_rs,
                }, Op::LoadConst {
                    rd: one_rd,
                    idx: one_idx,
                }, Op::Mov {
                    rd: read_rd,
                    rs: read_rs,
                }, Op::MirManagedAnyOp {
                    rd: None,
                    ra: scheduled_drop_ra,
                    operation: crate::core::mir::MirManagedAnyOperation::Drop,
                    contract: scheduled_drop_contract,
                }, Op::CheckI32DivRem {
                    ra: check_ra,
                    rb: check_rb,
                }, Op::DivInt {
                    rd: div_rd,
                    ra: div_ra,
                    rb: div_rb,
                }, Op::Ret { ra: return_ra }] = function.code.as_slice()
                else {
                    return Err(
                        "ManagedAny division trap callee has unexpected bytecode operations".into(),
                    );
                };
                let constants_ok = bytecode_int_constant(function, *initial_idx, 0)
                    && bytecode_int_constant(function, *one_idx, 1);
                let links_ok = scheduled_drop_ra == drop_ra
                    && scheduled_drop_contract == drop_contract
                    && *bound_rs == *initial_rd
                    && *read_rs == *bound_rd
                    && *check_ra == *one_rd
                    && *check_rb == *read_rd
                    && *div_ra == *check_ra
                    && *div_rb == *check_rb
                    && *return_ra == *div_rd;
                (
                    *initial_rd,
                    *bound_rd,
                    *one_rd,
                    *read_rd,
                    *div_rd,
                    *div_ra,
                    *div_rb,
                    constants_ok && links_ok,
                )
            }
            crate::core::mir::MirManagedAnyTrapKind::SignedOverflowI32 => {
                // The source `MAX + 1` can be emitted in either setup order:
                // a Clone followed by the literal addend (the direct MIR
                // lowering), or the literal followed by the Clone (a legacy
                // register scheduler). Both are accepted only with the same
                // sealed links and constants below.
                let (
                    initial_rd,
                    initial_idx,
                    bound_rd,
                    bound_rs,
                    one_rd,
                    one_idx,
                    read_rd,
                    read_rs,
                    scheduled_drop_ra,
                    scheduled_drop_contract,
                    add_rd,
                    add_ra,
                    add_rb,
                    check_rd,
                    return_ra,
                ) =
                    match function.code.as_slice() {
                        [Op::LoadConst {
                            rd: initial_rd,
                            idx: initial_idx,
                        }, Op::Mov {
                            rd: bound_rd,
                            rs: bound_rs,
                        }, Op::Mov {
                            rd: read_rd,
                            rs: read_rs,
                        }, Op::LoadConst {
                            rd: one_rd,
                            idx: one_idx,
                        }, Op::MirManagedAnyOp {
                            rd: None,
                            ra: scheduled_drop_ra,
                            operation: crate::core::mir::MirManagedAnyOperation::Drop,
                            contract: scheduled_drop_contract,
                        }, Op::AddInt {
                            rd: add_rd,
                            ra: add_ra,
                            rb: add_rb,
                        }, Op::CheckI32 {
                            rd: check_rd,
                            kind: 0,
                        }, Op::Ret { ra: return_ra }] => (
                            initial_rd,
                            initial_idx,
                            bound_rd,
                            bound_rs,
                            one_rd,
                            one_idx,
                            read_rd,
                            read_rs,
                            scheduled_drop_ra,
                            scheduled_drop_contract,
                            add_rd,
                            add_ra,
                            add_rb,
                            check_rd,
                            return_ra,
                        ),
                        [Op::LoadConst {
                            rd: initial_rd,
                            idx: initial_idx,
                        }, Op::Mov {
                            rd: bound_rd,
                            rs: bound_rs,
                        }, Op::LoadConst {
                            rd: one_rd,
                            idx: one_idx,
                        }, Op::Mov {
                            rd: read_rd,
                            rs: read_rs,
                        }, Op::MirManagedAnyOp {
                            rd: None,
                            ra: scheduled_drop_ra,
                            operation: crate::core::mir::MirManagedAnyOperation::Drop,
                            contract: scheduled_drop_contract,
                        }, Op::AddInt {
                            rd: add_rd,
                            ra: add_ra,
                            rb: add_rb,
                        }, Op::CheckI32 {
                            rd: check_rd,
                            kind: 0,
                        }, Op::Ret { ra: return_ra }] => (
                            initial_rd,
                            initial_idx,
                            bound_rd,
                            bound_rs,
                            one_rd,
                            one_idx,
                            read_rd,
                            read_rs,
                            scheduled_drop_ra,
                            scheduled_drop_contract,
                            add_rd,
                            add_ra,
                            add_rb,
                            check_rd,
                            return_ra,
                        ),
                        _ => return Err(
                            "ManagedAny overflow trap callee has unexpected bytecode operations"
                                .into(),
                        ),
                    };
                let constants_ok = bytecode_int_constant(function, *initial_idx, i32::MAX as i64)
                    && bytecode_int_constant(function, *one_idx, 1);
                let links_ok = scheduled_drop_ra == drop_ra
                    && scheduled_drop_contract == drop_contract
                    && *bound_rs == *initial_rd
                    && *read_rs == *bound_rd
                    && *add_ra == *read_rd
                    && *add_rb == *one_rd
                    && *check_rd == *add_rd
                    && *return_ra == *add_rd;
                (
                    *initial_rd,
                    *bound_rd,
                    *one_rd,
                    *read_rd,
                    *add_rd,
                    *add_ra,
                    *add_rb,
                    constants_ok && links_ok,
                )
            }
        };
    if !terminal_shape_ok
        || *drop_ra != 0
        || initial_rd == 0
        || bound_rd == 0
        || one_rd == 0
        || read_rd == 0
        || initial_rd == bound_rd
        || bound_rd == read_rd
        || one_rd == read_rd
        || trap_rd == 0
        || trap_rd >= function.register_count
        || trap_ra >= function.register_count
        || trap_rb >= function.register_count
        || [initial_rd, bound_rd, one_rd, read_rd]
            .iter()
            .any(|reg| *reg >= function.register_count)
        || [initial_rd, bound_rd, one_rd, read_rd, trap_rd]
            .iter()
            .enumerate()
            .any(|(index, reg)| {
                [initial_rd, bound_rd, one_rd, read_rd, trap_rd][..index].contains(reg)
            })
    {
        return Err(
            "ManagedAny trap-cleanup callee does not preserve the exact checked i32 trap schedule"
                .into(),
        );
    }
    let contract =
        managed_any_contract_from_constant(function, *drop_contract, "trap-cleanup Drop")?;
    let managed_type = crate::core::mir::types::managed_any_i32_string_v1_type_id();
    if contract.operation != crate::core::mir::MirManagedAnyOperation::Drop
        || contract.contract.owner.0 != instance.owner
        || contract.contract.origin.0 != instance.drop_origin
        || contract.contract.source.as_str() != instance.parameter
        || contract.contract.result.is_some()
        || contract.contract.source_type != managed_type
        || contract.contract.target_type.is_some()
        || contract.contract.expected_kind.is_some()
        || function.constants.len() != 3
    {
        return Err("ManagedAny trap-cleanup Drop constant disagrees with the Checker seal".into());
    }
    Ok(())
}

fn bytecode_int_constant(function: &FunctionProto, index: ConstIdx, expected: i64) -> bool {
    matches!(function.constants.get(index as usize), Some(ConstValue::Int(value)) if *value == expected)
}

/// Independent private snapshot for the separately versioned typed
/// Any-owner bytecode profile. Its validation explicitly rejects legacy
/// MapRoot operations.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalMapAnyOwnerV1BytecodeReceipt {
    expected_has_map_any_owner_v1: bool,
    expected_program: String,
    /// Third, independent profile receipt. The separate type, namespace, and
    /// snapshot prevent this carrier from lending MapAnyOwner v1 authority to
    /// ordinary source-Map owner opcodes. It is carried here only to avoid
    /// changing the public BytecodeProgram construction surface used by
    /// compatibility compiler entry points.
    ordinary_map_owner_lifecycle_i32_string_v1:
        Option<CanonicalOrdinaryMapOwnerLifecycleBytecodeReceipt>,
    ordinary_map_owned_get_i32_string_v1: Option<CanonicalOrdinaryMapOwnedGetBytecodeReceipt>,
    ordinary_map_owned_any_v2: Option<CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt>,
    /// Independent pending-L1 anchor; old V2/bare-root snapshots cannot
    /// authorize the recoverable Result opcode.
    new_result_anchor: Option<CanonicalOrdinaryMapNewResultBytecodeAnchor>,
    /// Independent route identity for the narrowly admitted Values field
    /// move profile. This anchor is minted only by the adapter that has
    /// validated the exact CheckedProgram/MIR pair, and binds that route
    /// receipt to the complete emitted bytecode table.
    values_field_move_route_anchor: Option<CanonicalValuesFieldMoveRouteBytecodeAnchor>,
    empty_root_root_queries_v1_route_anchor: Option<CanonicalEmptyRootRootQueriesV1RouteAnchor>,
    /// Independent route identity for the post-snapshot persistent Set
    /// replacement profile. This anchor is minted only by the adapter which
    /// has validated the exact CheckedProgram/MIR pair.
    values_snapshot_replacement_route_anchor:
        Option<CanonicalValuesSnapshotReplacementRouteBytecodeAnchor>,
    /// Independent route identity for the mixed snapshot profile. This is a
    /// separate private anchor so neither an existing Values route nor a
    /// generic MIR receipt can authorize this profile.
    values_mixed_snapshot_route_anchor: Option<CanonicalValuesMixedSnapshotRouteBytecodeAnchor>,
    /// Independent route identity for the exact Values local-index i32
    /// profile. Its own anchor binds the Checker/MIR route manifest to the
    /// complete emitted bytecode table.
    values_local_index_i32_route_anchor: Option<CanonicalValuesLocalIndexI32RouteBytecodeAnchor>,
    /// Independent anchor for the sorted heterogeneous Values index-one
    /// String extraction profile.
    values_heterogeneous_string_index1_route_anchor:
        Option<CanonicalValuesHeterogeneousStringIndex1RouteBytecodeAnchor>,
    /// Independent identity for the exact Checker/MIR owner-v2 Keys
    /// snapshot route. It binds both Keys operations and the complete emitted
    /// bytecode table to the route receipt.
    keys_snapshot_route_anchor: Option<CanonicalKeysSnapshotRouteBytecodeAnchor>,
    /// Exact argv[0] → dynamic key → Keys/GetAny route anchor, kept
    /// independent of both static KeysSnapshot and computed-key profiles.
    dynamic_keys_get_route_anchor: Option<CanonicalDynamicKeysGetRouteBytecodeAnchor>,
    /// Independent route identity for the Values mixed-snapshot-remove
    /// profile. The anchor binds its own exact Checker manifest and full
    /// emitted bytecode table.
    values_mixed_snapshot_remove_route_anchor:
        Option<CanonicalValuesMixedSnapshotRemoveRouteBytecodeAnchor>,
    /// Independent identity for the narrowly admitted cross-call Values
    /// producer/caller profile. The private anchor binds the Checker/MIR
    /// route receipt and typed producer/caller receipts to exact emitted
    /// Values, direct Call, owner Drop, and scalar return sites.
    values_producer_caller_route_anchor: Option<CanonicalValuesProducerCallerRouteBytecodeAnchor>,
    /// Independent identity for the Map-root producer/caller profile. It
    /// owns a different receipt and binding table from the Values bridge.
    map_root_producer_caller_route_anchor:
        Option<CanonicalMapRootProducerCallerRouteBytecodeAnchor>,
    /// Independent identity for the returned Map-root → GetAny → checked
    /// String-unpack caller route. It cannot borrow the Map-root/Size anchor.
    map_root_get_any_caller_route_anchor: Option<CanonicalMapRootGetAnyCallerRouteBytecodeAnchor>,
    /// Independent route identity for the computed-key Get profile. Its
    /// Checker/MIR route receipt and complete bytecode table are sealed
    /// separately from generic V2 and all Values routes.
    computed_key_get_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2ComputedKeyGetRouteBytecodeAnchor>,
    /// Independent identity for the computed String-key owner-chain route.
    /// The Checker/MIR receipt and a complete bytecode snapshot are sealed
    /// separately from the simpler GetAny computed-key profile.
    computed_string_key_owner_chain_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainRouteBytecodeAnchor>,
    /// Independent route identity for the exact Checker-sealed two-GetAny
    /// zero-miss i32 profile and its complete emitted bytecode table.
    ordinary_map_owned_any_v2_zero_miss_i32_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2ZeroMissI32RouteBytecodeAnchor>,
    /// Independent identity for the exact Checker-sealed GetOrDefault<i32>
    /// specialization and its complete emitted bytecode table.
    ordinary_map_owned_any_v2_get_or_default_i32_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32RouteBytecodeAnchor>,
    /// Independent Checker/MIR route identity for the exact String
    /// specialization and its complete emitted bytecode table.
    ordinary_map_owned_any_v2_get_or_default_string_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringRouteBytecodeAnchor>,
    /// Independent route identity for the exact Checker-sealed persistent
    /// Remove / checked missing-key GetAny profile.
    ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2RemovedKeyCheckedMissRouteBytecodeAnchor>,
    /// Independent route identity for the exact Checker-sealed CheckedGet
    /// profile. Its MIR route receipt and complete emitted bytecode table are
    /// sealed separately from generic V2 and other narrow routes.
    checked_get_route_anchor: Option<CanonicalOrdinaryMapOwnedAnyV2CheckedGetRouteBytecodeAnchor>,
    /// Independent identity for the exact Checker-sealed V2 IsEmpty route.
    /// The Checker-aware adapter joins the source callsite/action to MIR
    /// before capture; this anchor binds that complete route receipt to the
    /// emitted bytecode table.
    is_empty_route_anchor: Option<CanonicalOrdinaryMapOwnedAnyV2IsEmptyRouteBytecodeAnchor>,
    /// Independent identity for the exact mixed i32/Label/i32 RecordUnit
    /// operation census and its complete emitted bytecode snapshot.
    record_unit_route_anchor: Option<CanonicalOrdinaryMapOwnedAnyV2RecordUnitRouteBytecodeAnchor>,
    /// Independent identity for the exact no-query MapSetDropControl route.
    map_set_drop_control_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2MapSetDropControlRouteBytecodeAnchor>,
    /// Independent identity for the exact branch-local persistent-Set
    /// diamond. This anchor cannot borrow the straight-line Set/drop route.
    branch_local_set_diamond_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2BranchLocalSetDiamondRouteBytecodeAnchor>,
    /// Independent route identity for the exact Checker-sealed V2
    /// CloneIsolation profile and its full emitted bytecode table.
    clone_isolation_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2CloneIsolationRouteBytecodeAnchor>,
    /// Independent route identity for the exact persistent-root alias
    /// replacement profile. This anchor is separate from CloneIsolation so
    /// neither profile can authorize the other.
    ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor:
        Option<CanonicalOrdinaryMapOwnedAnyV2PersistentRootAliasReplacementRouteBytecodeAnchor>,
    /// Independent route identity for the exact Checker-sealed direct
    /// `println(pair.1)` profile. The operation-level println anchor above
    /// proves emitted sites; this route anchor binds the admitted profile to
    /// the complete bytecode table.
    direct_print_route_anchor: Option<CanonicalOrdinaryMapOwnedAnyV2DirectPrintRouteBytecodeAnchor>,
    /// Exact primitive-Map observer profile. Its anchor binds the narrow
    /// Checker observer receipts, route receipt, operation sites, operands,
    /// entry, and full emitted program independently of the lifecycle ledger.
    ordinary_map_owner_observer_v1: Option<CanonicalOrdinaryMapOwnerObserverV1BytecodeAnchor>,
    managed_any_identity: Option<CanonicalManagedAnyIdentityBytecodeReceipt>,
    managed_any_trap_cleanup: Option<CanonicalManagedAnyTrapCleanupBytecodeReceipt>,
    /// Independent whole-program Any/CFG bytecode seal.  The expected bit is
    /// deliberately separate from this optional child so deleting the CFG
    /// receipt cannot be masked by a still-valid Map snapshot.
    expected_managed_any_cfg_receipt: bool,
    managed_any_cfg: Option<CanonicalManagedAnyCfgBytecodeReceipt>,
}

/// Distinct private proof plus immutable emitted-program/manifest anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalEmptyRootRootQueriesV1RouteAnchor {
    proof: crate::core::mir::MapEmptyRootRootQueriesV1MirReceipt,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    entry: FuncIdx,
    program: String,
    manifest: String,
}

/// Private VM-boundary identity for the Checker-sealed Values field-move
/// route. Keeping both the canonical manifest and the emitted bytecode
/// snapshot here means a route label alone can never authorize a mutable
/// bytecode program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesFieldMoveRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the Values snapshot replacement route.
/// This separate anchor prevents a field-move admission or generic MIR route
/// receipt from authorizing the replacement profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesSnapshotReplacementRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the Values mixed-snapshot route. It
/// captures both the canonical MIR route receipt and the complete emitted
/// bytecode table, independently of the existing Values route anchors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesMixedSnapshotRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed Values local
/// index i32 route. This profile cannot borrow another Values route anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesLocalIndexI32RouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the two-entry heterogeneous Values
/// profile. It cannot borrow the local-index i32 anchor or broad V2 receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesHeterogeneousStringIndex1RouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private bytecode identity for the one source-bound owner-v2 Keys snapshot
/// route. It records both emitted operation sites and seals the full program
/// table so neither opcode can be transplanted into generic List code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalKeysSnapshotRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
    keys_site: (u32, Reg, Reg),
    unpack_site: (u32, Reg, Reg),
}

/// Private identity for the dedicated DynamicKeysGet route. It binds the
/// fixed argv[0] opcode plus the route's Keys and KeysUnpack sites to the
/// exact MIR receipt and the complete bytecode program snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalDynamicKeysGetRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
    arg0_site: (u32, Reg),
    keys_site: (u32, Reg, Reg),
    unpack_site: (u32, Reg, Reg),
}

/// Private VM-boundary identity for the Values mixed-snapshot-remove route.
/// It is distinct from both snapshot anchors and binds the exact route
/// manifest to the complete bytecode table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesMixedSnapshotRemoveRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Values producer→caller route.
/// The MIR receipt and proof vector authorize only the single direct-call
/// shape; site bindings independently map that proof to emitted bytecode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesProducerCallerRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesProducerCallerMirReceipt>,
    bindings: Vec<CanonicalValuesProducerCallerBytecodeBinding>,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// One exact emitted instruction belonging to the Values cross-call String
/// owner proof. The site vector is ordered by the proof's producer/caller
/// instruction identities and stores the actual opcode so VM validation can
/// recheck the complete source-to-bytecode projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesCrosscallStringCallerBytecodeSite {
    pub(crate) owner: crate::core::NodeId,
    pub(crate) function: FuncIdx,
    pub(crate) instruction: crate::core::mir::MirInstructionId,
    pub(crate) pc: u32,
    pub(crate) op: Op,
}

/// Exact bytecode projection for the Values source/caller ownership pair.
/// Owner IDs and digest identities are retained independently of the generic
/// Values receipt tables so a local receipt cannot impersonate this route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesCrosscallStringCallerBytecodeBinding {
    pub(crate) producer_owner: crate::core::NodeId,
    pub(crate) caller_owner: crate::core::NodeId,
    pub(crate) call_target: crate::core::NodeId,
    pub(crate) producer_function: FuncIdx,
    pub(crate) caller_function: FuncIdx,
    pub(crate) proof_digest: String,
    pub(crate) mir_digest: String,
    pub(crate) type_desc_digest: String,
    pub(crate) payload_selection_digest: String,
    pub(crate) string_type_desc_digest: String,
    pub(crate) payload_type_desc_digest: String,
    /// Literal source moved into the producer's owned String payload local.
    /// It is derived from the exact MIR Move source and tied to the Checker
    /// expected String literal by the route proof.
    pub(crate) producer_payload_initializer_instruction: crate::core::mir::MirInstructionId,
    pub(crate) sites: Vec<CanonicalValuesCrosscallStringCallerBytecodeSite>,
}

/// Dedicated route anchor for the Values cross-call String caller. This
/// retains the Core/MIR proof, route manifest, independent site bindings, and
/// full emitted table snapshot; none of these fields share storage with
/// same-function Values owner receipts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesCrosscallStringCallerRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt>,
    bindings: Vec<CanonicalValuesCrosscallStringCallerBytecodeBinding>,
    expected_manifest: String,
    expected_bindings: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Separate bytecode binding for the versioned Values cross-call route which
/// selects the second of two sorted String payloads. V1 remains a fixed
/// 22-site index-zero binding; this binding independently seals its expanded
/// producer Set/drop chains and literal index one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesCrosscallStringCallerIndex1BytecodeBinding {
    pub(crate) producer_owner: crate::core::NodeId,
    pub(crate) caller_owner: crate::core::NodeId,
    pub(crate) call_target: crate::core::NodeId,
    pub(crate) producer_function: FuncIdx,
    pub(crate) caller_function: FuncIdx,
    pub(crate) proof_digest: String,
    pub(crate) mir_digest: String,
    pub(crate) type_desc_digest: String,
    pub(crate) payload_selection_digest: String,
    pub(crate) string_type_desc_digest: String,
    pub(crate) payload_type_desc_digest: String,
    pub(crate) producer_payload_initializer_instructions: Vec<crate::core::mir::MirInstructionId>,
    /// Each V2 Set borrows its String key and cloned String payload. These
    /// exact temporary Drops are separate from the Checker-bound source
    /// payload Drops in the MIR receipt and must remain bound to their Set
    /// operands at the VM boundary.
    pub(crate) producer_set_temporary_drops:
        Vec<CanonicalValuesCrosscallStringCallerIndex1SetTemporaryDrops>,
    pub(crate) sites: Vec<CanonicalValuesCrosscallStringCallerBytecodeSite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesCrosscallStringCallerIndex1SetTemporaryDrops {
    pub(crate) set_instruction: crate::core::mir::MirInstructionId,
    pub(crate) key_drop_instruction: crate::core::mir::MirInstructionId,
    pub(crate) key_value: crate::core::mir::MirValueId,
    pub(crate) payload_drop_instruction: crate::core::mir::MirInstructionId,
    pub(crate) payload_value: crate::core::mir::MirValueId,
}

/// Independent route anchor for the V2 literal-index-one Values bridge.
/// Its profile, manifest, site snapshot, and whole-program bytecode snapshot
/// are kept apart from the frozen V1 zero-index anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesCrosscallStringCallerIndex1RouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt>,
    bindings: Vec<CanonicalValuesCrosscallStringCallerIndex1BytecodeBinding>,
    expected_manifest: String,
    expected_bindings: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the independently admitted Map-root
/// producer/caller bridge. It has its own proof vector, site table, manifest,
/// and emitted-program snapshot; the Values bridge cannot supply this
/// authority even though both routes contain a producer return and a direct
/// caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalMapRootProducerCallerRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootProducerCallerMirReceipt>,
    bindings: Vec<CanonicalMapRootProducerCallerBytecodeBinding>,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Exact bytecode projection of one Map-root producer/caller Checker/MIR
/// receipt. Every ownership-sensitive Map opcode and transfer Move is tied
/// to a MIR instruction identity and a concrete register/PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalMapRootProducerCallerBytecodeBinding {
    pub(crate) producer_owner: crate::core::NodeId,
    pub(crate) caller_owner: crate::core::NodeId,
    pub(crate) producer_function: FuncIdx,
    pub(crate) caller_function: FuncIdx,
    pub(crate) call_target: FuncIdx,
    pub(crate) producer_new_instruction: crate::core::mir::MirInstructionId,
    pub(crate) producer_set_instruction: crate::core::mir::MirInstructionId,
    pub(crate) producer_predecessor_drop_instruction: crate::core::mir::MirInstructionId,
    pub(crate) producer_payload_drop_instruction: crate::core::mir::MirInstructionId,
    pub(crate) producer_return_instruction: crate::core::mir::MirInstructionId,
    pub(crate) caller_call_instruction: crate::core::mir::MirInstructionId,
    pub(crate) caller_binding_instruction: crate::core::mir::MirInstructionId,
    pub(crate) caller_size_instruction: crate::core::mir::MirInstructionId,
    pub(crate) caller_size_binding_instruction: crate::core::mir::MirInstructionId,
    pub(crate) caller_root_drop_instruction: crate::core::mir::MirInstructionId,
    pub(crate) caller_scalar_return_instruction: crate::core::mir::MirInstructionId,
    pub(crate) initial_root: crate::core::mir::MirValueId,
    pub(crate) final_root: crate::core::mir::MirValueId,
    pub(crate) producer_return_value: crate::core::mir::MirValueId,
    pub(crate) caller_call_result: crate::core::mir::MirValueId,
    pub(crate) caller_root: crate::core::mir::MirValueId,
    pub(crate) caller_size_result: crate::core::mir::MirValueId,
    pub(crate) caller_size_binding: crate::core::mir::MirValueId,
    pub(crate) caller_scalar_return: crate::core::mir::MirValueId,
    pub(crate) set_key: crate::core::mir::MirValueId,
    pub(crate) set_payload: crate::core::mir::MirValueId,
    pub(crate) root_type: crate::core::ResolvedTypeId,
    pub(crate) scalar_type: crate::core::ResolvedTypeId,
    pub(crate) producer_new_pc: u32,
    pub(crate) producer_new_rd: Reg,
    pub(crate) producer_set_pc: u32,
    pub(crate) producer_set_rd: Reg,
    pub(crate) producer_set_ra: Reg,
    pub(crate) producer_set_key: Reg,
    pub(crate) producer_set_payload: Reg,
    pub(crate) producer_predecessor_drop_pc: u32,
    pub(crate) producer_predecessor_drop_ra: Reg,
    pub(crate) producer_payload_drop_pc: u32,
    pub(crate) producer_payload_drop_ra: Reg,
    pub(crate) producer_return_move_pc: u32,
    pub(crate) producer_return_move_rd: Reg,
    pub(crate) producer_return_move_rs: Reg,
    pub(crate) producer_return_pc: u32,
    pub(crate) producer_return_ra: Reg,
    pub(crate) caller_call_pc: u32,
    pub(crate) caller_call_rd: Reg,
    pub(crate) caller_binding_move_pc: u32,
    pub(crate) caller_binding_move_rd: Reg,
    pub(crate) caller_binding_move_rs: Reg,
    pub(crate) caller_size_pc: u32,
    pub(crate) caller_size_rd: Reg,
    pub(crate) caller_size_ra: Reg,
    pub(crate) caller_size_binding_move_pc: u32,
    pub(crate) caller_size_binding_move_rd: Reg,
    pub(crate) caller_size_binding_move_rs: Reg,
    pub(crate) caller_root_drop_pc: u32,
    pub(crate) caller_root_drop_ra: Reg,
    pub(crate) caller_scalar_return_move_pc: u32,
    pub(crate) caller_scalar_return_move_rd: Reg,
    pub(crate) caller_scalar_return_move_rs: Reg,
    pub(crate) caller_return_pc: u32,
    pub(crate) caller_return_ra: Reg,
}

/// Independent bytecode projection for the returned Map-root → direct
/// GetAny → checked String-unpack route. This is a separate typed binding
/// from the Map-root/Size producer-caller route above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalMapRootGetAnyCallerBytecodeSite {
    pub(crate) instruction: crate::core::mir::MirInstructionId,
    pub(crate) function: FuncIdx,
    pub(crate) pc: u32,
    pub(crate) op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalMapRootGetAnyCallerBytecodeBinding {
    pub(crate) producer_owner: crate::core::NodeId,
    pub(crate) caller_owner: crate::core::NodeId,
    pub(crate) producer_function: FuncIdx,
    pub(crate) caller_function: FuncIdx,
    pub(crate) call_target: FuncIdx,
    /// Producer New/Set/root Drop/String Drop/return Move, followed by caller
    /// Call/root-binding Move/GetAny/pair-binding Move/root Drop/unpack/
    /// String-binding Move/pair Drop/String Drop/scalar literal, all mapped to
    /// one exact opcode each.
    pub(crate) sites: Vec<CanonicalMapRootGetAnyCallerBytecodeSite>,
    pub(crate) producer_return_pc: u32,
    pub(crate) caller_return_pc: u32,
    pub(crate) root_type: crate::core::ResolvedTypeId,
    pub(crate) string_type: crate::core::ResolvedTypeId,
    pub(crate) scalar_type: crate::core::ResolvedTypeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalMapRootGetAnyCallerRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerMirReceipt>,
    bindings: Vec<CanonicalMapRootGetAnyCallerBytecodeBinding>,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Exact bytecode projection of one Checker/MIR producer→caller receipt.
/// Stored independently of the route receipt so VM preflight rechecks every
/// ownership-sensitive opcode and the direct callee index before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalValuesProducerCallerBytecodeBinding {
    pub(crate) producer_owner: crate::core::NodeId,
    pub(crate) caller_owner: crate::core::NodeId,
    pub(crate) producer_values_instruction: crate::core::mir::MirInstructionId,
    pub(crate) producer_binding_instruction: crate::core::mir::MirInstructionId,
    pub(crate) producer_return_instruction: crate::core::mir::MirInstructionId,
    pub(crate) producer_values_result: crate::core::mir::MirValueId,
    pub(crate) producer_snapshot_value: crate::core::mir::MirValueId,
    pub(crate) producer_return_value: crate::core::mir::MirValueId,
    pub(crate) call_instruction: crate::core::mir::MirInstructionId,
    pub(crate) caller_binding_instruction: crate::core::mir::MirInstructionId,
    pub(crate) call_result: crate::core::mir::MirValueId,
    pub(crate) caller_binding: crate::core::mir::MirValueId,
    pub(crate) owner_drop_instruction: crate::core::mir::MirInstructionId,
    pub(crate) owner_drop_value: crate::core::mir::MirValueId,
    pub(crate) scalar_return_value: crate::core::mir::MirValueId,
    pub(crate) scalar_return_node: crate::core::NodeId,
    pub(crate) scalar_return_literal: Option<crate::core::ir::ResolvedLiteral>,
    pub(crate) len_instruction: Option<crate::core::mir::MirInstructionId>,
    pub(crate) len_result: Option<crate::core::mir::MirValueId>,
    pub(crate) len_binding_instruction: Option<crate::core::mir::MirInstructionId>,
    pub(crate) len_binding: Option<crate::core::mir::MirValueId>,
    pub(crate) caller_return_move_instruction: Option<crate::core::mir::MirInstructionId>,
    pub(crate) owner_type: crate::core::ResolvedTypeId,
    /// Checker/MIR bridge identities. The source and surface TypeDesc IDs are
    /// intentionally distinct from `owner_type`, which remains the runtime
    /// V2 Values TypeDesc used by MIR and the VM.
    pub(crate) producer_checker_snapshot_type: crate::core::ResolvedTypeId,
    pub(crate) producer_surface_return_type: crate::core::ResolvedTypeId,
    pub(crate) call_surface_result_type: crate::core::ResolvedTypeId,
    pub(crate) producer_return_cast_node: crate::core::NodeId,
    pub(crate) producer_return_source_node: crate::core::NodeId,
    pub(crate) producer_return_source_type: crate::core::ResolvedTypeId,
    pub(crate) producer_return_target_type: crate::core::ResolvedTypeId,
    pub(crate) producer_return_conversion_kind: crate::core::ir::CheckedConversionKind,
    pub(crate) len_receiver: Option<crate::core::mir::MirValueId>,
    pub(crate) len_receiver_type: Option<crate::core::ResolvedTypeId>,
    pub(crate) len_receiver_clone_instruction: Option<crate::core::mir::MirInstructionId>,
    pub(crate) len_receiver_drop_instruction: Option<crate::core::mir::MirInstructionId>,
    pub(crate) producer_function: FuncIdx,
    pub(crate) caller_function: FuncIdx,
    pub(crate) producer_values_pc: u32,
    pub(crate) producer_values_rd: Reg,
    pub(crate) producer_values_ra: Reg,
    pub(crate) producer_binding_move_pc: u32,
    pub(crate) producer_binding_move_rd: Reg,
    pub(crate) producer_binding_move_rs: Reg,
    pub(crate) producer_return_move_pc: u32,
    pub(crate) producer_return_move_rd: Reg,
    pub(crate) producer_return_move_rs: Reg,
    pub(crate) producer_return_pc: u32,
    pub(crate) producer_return_ra: Reg,
    pub(crate) call_pc: u32,
    pub(crate) call_rd: Reg,
    pub(crate) call_target: FuncIdx,
    pub(crate) caller_binding_move_pc: u32,
    pub(crate) caller_binding_rd: Reg,
    pub(crate) caller_binding_rs: Reg,
    pub(crate) owner_drop_pc: u32,
    pub(crate) owner_drop_ra: Reg,
    pub(crate) caller_return_pc: u32,
    pub(crate) caller_return_ra: Reg,
    pub(crate) caller_return_literal_instruction: Option<crate::core::mir::MirInstructionId>,
    pub(crate) caller_return_literal_pc: Option<u32>,
    pub(crate) producer_result_type: crate::core::ResolvedTypeId,
    pub(crate) call_result_type: crate::core::ResolvedTypeId,
    pub(crate) caller_return_type: crate::core::ResolvedTypeId,
    pub(crate) len_pc: Option<u32>,
    pub(crate) len_rd: Option<Reg>,
    pub(crate) len_ra: Option<Reg>,
    pub(crate) len_receiver_clone_pc: Option<u32>,
    pub(crate) len_receiver_clone_rd: Option<Reg>,
    pub(crate) len_receiver_clone_ra: Option<Reg>,
    pub(crate) len_receiver_drop_pc: Option<u32>,
    pub(crate) len_receiver_drop_ra: Option<Reg>,
    pub(crate) len_binding_move_pc: Option<u32>,
    pub(crate) len_binding_move_rd: Option<Reg>,
    pub(crate) len_binding_move_rs: Option<Reg>,
    pub(crate) caller_return_move_pc: Option<u32>,
    pub(crate) caller_return_move_rd: Option<Reg>,
    pub(crate) caller_return_move_rs: Option<Reg>,
}

/// Private VM-boundary identity for the Checker-sealed computed-key Get
/// route. The route receipt commits to the exact MIR and Checker ownership
/// source receipt; the bytecode snapshot seals the executable translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2ComputedKeyGetRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the full computed String-key owner chain.
/// It seals the exact route manifest and complete emitted program so generic
/// V2 bytecode cannot borrow this profile's narrow execution permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed CheckedGet
/// route. The MIR route receipt commits to the Checker-owned Map/Any action
/// chain, while this snapshot seals its executable bytecode translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2CheckedGetRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed two-GetAny
/// zero-miss i32 route. The full bytecode snapshot seals its executable
/// translation to the canonical route receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2ZeroMissI32RouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the dedicated, Checker/MIR-bound
/// `get_or_default<i32>` operation. Its typed site binding is sealed
/// separately in the V2 receipt; this anchor binds the route manifest to the
/// complete emitted bytecode table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32RouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the dedicated, Checker/MIR-bound
/// `get_or_default<String>` operation. This cannot reuse the i32 anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed persistent
/// Remove / checked-miss route. The full bytecode snapshot binds its emitted
/// Set, Remove, GetAny, canonical i32 unpack and settlement instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2RemovedKeyCheckedMissRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed V2 IsEmpty
/// route. The full route receipt carries the MIR/ownership digests produced
/// after the Checker source-action join, while the snapshot seals the exact
/// emitted instruction sites and operands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2IsEmptyRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed mixed
/// i32/Label/i32 RecordUnit profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2RecordUnitRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed no-query
/// MapSetDropControl route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2MapSetDropControlRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed
/// branch-local persistent-Set diamond. The route receipt binds the
/// Checker/MIR proof, while the snapshot seals its complete emitted control
/// flow, operation operands, and register boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2BranchLocalSetDiamondRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed V2
/// CloneIsolation route. Its receipt binds the Checker/MIR authorization,
/// while the snapshot seals the complete emitted bytecode program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2CloneIsolationRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact Checker-sealed persistent-root
/// alias replacement profile. It owns a distinct route receipt and complete
/// bytecode snapshot from CloneIsolation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2PersistentRootAliasReplacementRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

/// Private VM-boundary identity for the exact V2 DirectPrint route. The
/// receipt is checked against MIR before capture, then its manifest and the
/// emitted bytecode snapshot are replayed at every public VM entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2DirectPrintRouteBytecodeAnchor {
    expected_profile: &'static str,
    receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_manifest: String,
    expected_entry: FuncIdx,
    expected_program: String,
}

fn ordinary_map_owned_any_v2_direct_print_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

impl CanonicalOrdinaryMapOwnedAnyV2DirectPrintRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "DirectPrint bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt
            .manifest_round_trip()
            .map_err(|message| format!("DirectPrint route manifest replay failed: {message}"))?;
        if replayed != *receipt {
            return Err("DirectPrint route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt
            .manifest_text()
            .map_err(|message| format!("DirectPrint route manifest render failed: {message}"))?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: ordinary_map_owned_any_v2_direct_print_bytecode_snapshot(
                entry, functions,
            ),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "DirectPrint bytecode route profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("DirectPrint route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "DirectPrint route receipt disagrees with its immutable manifest anchor".into(),
            );
        }
        if entry != self.expected_entry
            || ordinary_map_owned_any_v2_direct_print_bytecode_snapshot(entry, functions)
                != self.expected_program
        {
            return Err("DirectPrint bytecode disagrees with its exact route snapshot".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND.into();
    }
}

/// Independent bytecode anchor for the borrowed V2 payload Println opcode.
/// It repeats the whole-program snapshot and entry identity so this output
/// effect has its own removable, fail-closed authorization slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2PrintlnAnchor {
    pub(crate) profile: &'static str,
    pub(crate) expected_entry: FuncIdx,
    pub(crate) expected_program: String,
    pub(crate) payload_selection_digest: String,
    pub(crate) bindings:
        std::collections::BTreeMap<FuncIdx, Vec<CanonicalOrdinaryMapOwnedAnyV2PrintlnBinding>>,
    expected_bindings: String,
}

fn computed_key_get_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn computed_string_key_owner_chain_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn checked_get_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn ordinary_map_owned_any_v2_zero_miss_i32_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn ordinary_map_owned_any_v2_removed_key_checked_miss_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn is_empty_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn record_unit_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn record_unit_opcode_counts(
    functions: &[FunctionProto],
) -> (usize, usize, usize, usize, usize, usize, usize) {
    let mut new_count = 0;
    let mut set_count = 0;
    let mut remove_count = 0;
    let mut size_count = 0;
    let mut contains_count = 0;
    let mut drop_count = 0;
    let mut other_v2_count = 0;
    for op in functions.iter().flat_map(|function| &function.code) {
        match op {
            Op::MirOrdinaryMapOwnedAnyV2New { .. } => new_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Set { .. } => set_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Remove { .. } => remove_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Size { .. } => size_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Contains { .. } => contains_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Drop { .. } => drop_count += 1,
            op if matches!(
                op,
                Op::MirOrdinaryMapOwnedAnyV2Clone { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2GetAny { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Println { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Unpack { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Values { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Keys { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2KeysUnpack { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2IsEmpty { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2ValuesClone { .. }
            ) =>
            {
                other_v2_count += 1
            }
            _ => {}
        }
    }
    (
        new_count,
        set_count,
        remove_count,
        size_count,
        contains_count,
        drop_count,
        other_v2_count,
    )
}

fn map_set_drop_control_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn map_set_drop_control_opcode_counts(functions: &[FunctionProto]) -> (usize, usize, usize, usize) {
    let mut new_count = 0;
    let mut set_count = 0;
    let mut drop_count = 0;
    let mut other_v2_count = 0;
    for op in functions.iter().flat_map(|function| &function.code) {
        match op {
            Op::MirOrdinaryMapOwnedAnyV2New { .. } => new_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Set { .. } => set_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Drop { .. } => drop_count += 1,
            op if matches!(
                op,
                Op::MirOrdinaryMapOwnedAnyV2Clone { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Remove { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Size { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Contains { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2GetAny { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Println { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Unpack { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Values { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Keys { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2KeysUnpack { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2IsEmpty { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2ValuesClone { .. }
            ) =>
            {
                other_v2_count += 1
            }
            _ => {}
        }
    }
    (new_count, set_count, drop_count, other_v2_count)
}

fn branch_local_set_diamond_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn branch_local_set_diamond_opcode_counts(
    functions: &[FunctionProto],
) -> (usize, usize, usize, usize, usize) {
    let mut new_count = 0;
    let mut set_count = 0;
    let mut size_count = 0;
    let mut drop_count = 0;
    let mut other_v2_count = 0;
    for op in functions.iter().flat_map(|function| &function.code) {
        match op {
            Op::MirOrdinaryMapOwnedAnyV2New { .. } => new_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Set { .. } => set_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Size { .. } => size_count += 1,
            Op::MirOrdinaryMapOwnedAnyV2Drop { .. } => drop_count += 1,
            op if matches!(
                op,
                Op::MirOrdinaryMapOwnedAnyV2Clone { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Remove { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Contains { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2GetAny { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Println { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Unpack { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Values { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2Keys { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2KeysUnpack { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2IsEmpty { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }
                    | Op::MirOrdinaryMapOwnedAnyV2ValuesClone { .. }
            ) =>
            {
                other_v2_count += 1
            }
            _ => {}
        }
    }
    (new_count, set_count, size_count, drop_count, other_v2_count)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchLocalSetDiamondOpcode {
    Set { result: Reg, source: Reg },
    Size { source: Reg },
    Drop { source: Reg },
}

fn branch_local_set_diamond_path_ops(
    function: &FunctionProto,
    start: usize,
    base_drop_pc: usize,
    branch_pc: usize,
) -> Result<(Vec<usize>, Vec<BranchLocalSetDiamondOpcode>), String> {
    let mut visited = std::collections::BTreeSet::new();
    let mut path = Vec::new();
    let mut route_ops = Vec::new();
    let mut pc = start;
    loop {
        if pc == base_drop_pc {
            if let Some(Op::MirOrdinaryMapOwnedAnyV2Drop { ra }) = function.code.get(pc) {
                route_ops.push(BranchLocalSetDiamondOpcode::Drop { source: *ra });
            } else {
                return Err("branch-local Set diamond join is not the base-root Drop".into());
            }
            return Ok((path, route_ops));
        }
        if pc >= function.code.len() || !visited.insert(pc) {
            return Err("branch-local Set diamond arm escapes its boundary or loops".into());
        }
        path.push(pc);
        match &function.code[pc] {
            Op::MirOrdinaryMapOwnedAnyV2Set { rd, ra, .. } => {
                route_ops.push(BranchLocalSetDiamondOpcode::Set {
                    result: *rd,
                    source: *ra,
                });
            }
            Op::MirOrdinaryMapOwnedAnyV2Size { ra, .. } => {
                route_ops.push(BranchLocalSetDiamondOpcode::Size { source: *ra });
            }
            Op::MirOrdinaryMapOwnedAnyV2Drop { ra } => {
                route_ops.push(BranchLocalSetDiamondOpcode::Drop { source: *ra });
            }
            Op::MirOrdinaryMapOwnedAnyV2New { .. }
            | Op::MirOrdinaryMapOwnedAnyV2Clone { .. }
            | Op::MirOrdinaryMapOwnedAnyV2Remove { .. }
            | Op::MirOrdinaryMapOwnedAnyV2Contains { .. }
            | Op::MirOrdinaryMapOwnedAnyV2GetAny { .. }
            | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
            | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
            | Op::MirOrdinaryMapOwnedAnyV2Println { .. }
            | Op::MirOrdinaryMapOwnedAnyV2Unpack { .. }
            | Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }
            | Op::MirOrdinaryMapOwnedAnyV2Values { .. }
            | Op::MirOrdinaryMapOwnedAnyV2Keys { .. }
            | Op::MirOrdinaryMapOwnedAnyV2KeysUnpack { .. }
            | Op::MirOrdinaryMapOwnedAnyV2IsEmpty { .. }
            | Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }
            | Op::MirOrdinaryMapOwnedAnyV2ValuesClone { .. } => {
                return Err("branch-local Set diamond arm contains a disallowed V2 opcode".into());
            }
            Op::Jmp { offset } => {
                let next = pc.checked_add(1).ok_or_else(|| {
                    "branch-local Set diamond jump boundary overflowed".to_string()
                })?;
                let target = i64::try_from(next)
                    .ok()
                    .and_then(|next| next.checked_add(i64::from(*offset)))
                    .and_then(|target| usize::try_from(target).ok())
                    .filter(|target| *target < function.code.len())
                    .ok_or_else(|| {
                        "branch-local Set diamond jump escapes the function".to_string()
                    })?;
                pc = target;
                continue;
            }
            Op::JmpIf { .. } | Op::JmpIfNot { .. } => {
                return Err(if pc == branch_pc {
                    "branch-local Set diamond arm begins at the control split"
                } else {
                    "branch-local Set diamond arm contains an unexpected nested branch"
                }
                .into());
            }
            Op::Ret { .. }
            | Op::RetUnit
            | Op::RetEarly { .. }
            | Op::Trap { .. }
            | Op::NonExhaustiveMatch => {
                return Err(
                    "branch-local Set diamond arm exits before the common base Drop".into(),
                );
            }
            _ => {}
        }
        pc = pc
            .checked_add(1)
            .ok_or_else(|| "branch-local Set diamond fallthrough overflowed".to_string())?;
    }
}

/// Check the operation census and its emitted control-flow/operand lineage
/// before the route receipt is allowed to authorize the mutable bytecode
/// table. The full snapshot below then seals every instruction and constant.
fn validate_branch_local_set_diamond_bytecode(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> Result<(), String> {
    let counts = branch_local_set_diamond_opcode_counts(functions);
    if counts != (1, 2, 2, 3, 0) {
        return Err(format!(
            "branch-local Set diamond requires exactly New=1, Set=2, Size=2, Drop=3, other V2=0, found {counts:?}"
        ));
    }
    if entry != 0 || functions.len() != 1 {
        return Err(
            "branch-local Set diamond requires the single main bytecode function at entry zero"
                .into(),
        );
    }
    let function = &functions[0];
    if function.name != "function:main" {
        return Err("branch-local Set diamond bytecode function is not main".into());
    }

    let mut new_site = None;
    let mut sets = Vec::new();
    let mut sizes = Vec::new();
    let mut drops = Vec::new();
    let mut branches = Vec::new();
    for (pc, op) in function.code.iter().enumerate() {
        match op {
            Op::MirOrdinaryMapOwnedAnyV2New { rd } => new_site = Some((pc, *rd)),
            Op::MirOrdinaryMapOwnedAnyV2Set { rd, ra, key, value } => {
                sets.push((pc, *rd, *ra, *key, *value))
            }
            Op::MirOrdinaryMapOwnedAnyV2Size { rd, ra } => sizes.push((pc, *rd, *ra)),
            Op::MirOrdinaryMapOwnedAnyV2Drop { ra } => drops.push((pc, *ra)),
            Op::JmpIfNot { offset, ra } => {
                branches.push((pc, *offset, *ra));
            }
            Op::JmpIf { .. } => {
                return Err(
                    "branch-local Set diamond requires the canonical JmpIfNot split".into(),
                );
            }
            _ => {}
        }
    }
    let (new_pc, base_root) =
        new_site.ok_or_else(|| "branch-local Set diamond has no New opcode".to_string())?;
    let [(set0_pc, set0_root, set0_source, key0, value0), (set1_pc, set1_root, set1_source, key1, value1)] =
        sets.as_slice()
    else {
        return Err("branch-local Set diamond does not have two Set operand sites".into());
    };
    let [(size0_pc, size0_result, size0_root), (size1_pc, size1_result, size1_root)] =
        sizes.as_slice()
    else {
        return Err("branch-local Set diamond does not have two Size operand sites".into());
    };
    if drops.len() != 3 {
        return Err("branch-local Set diamond does not have three Drop operand sites".into());
    }
    let [(branch_pc, branch_offset, branch_condition)] = branches.as_slice() else {
        return Err("branch-local Set diamond requires one conditional control split".into());
    };

    let in_bounds = |reg: Reg| reg < function.register_count;
    let root_registers = [base_root, *set0_root, *set1_root];
    let size_registers = [*size0_result, *size1_result];
    let base_drop_sites = drops
        .iter()
        .copied()
        .filter(|(_, root)| *root == base_root)
        .collect::<Vec<_>>();
    let child_drop_sites = drops
        .iter()
        .copied()
        .filter(|(_, root)| *root != base_root)
        .collect::<Vec<_>>();
    if root_registers.iter().any(|reg| !in_bounds(*reg))
        || size_registers.iter().any(|reg| !in_bounds(*reg))
        || [key0, key1, value0, value1, branch_condition]
            .iter()
            .any(|reg| !in_bounds(**reg))
        || root_registers[1] == root_registers[0]
        || root_registers[2] == root_registers[0]
        || root_registers[1] == root_registers[2]
        || size_registers[0] == size_registers[1]
        || size_registers
            .iter()
            .any(|reg| root_registers.contains(reg))
        || base_drop_sites.len() != 1
        || child_drop_sites.len() != 2
    {
        return Err(
            "branch-local Set diamond has aliased or out-of-bounds bytecode operands".into(),
        );
    }
    if *set0_source != base_root
        || *set1_source != base_root
        || *size0_root != *set0_root
        || *size1_root != *set1_root
        || child_drop_sites
            .iter()
            .map(|(_, root)| *root)
            .collect::<std::collections::BTreeSet<_>>()
            != std::collections::BTreeSet::from([*set0_root, *set1_root])
    {
        return Err(
            "branch-local Set diamond Set/Size/Drop operands break the exact root lineage".into(),
        );
    }
    if [*set0_pc, *set1_pc, *size0_pc, *size1_pc]
        .iter()
        .any(|pc| *pc >= function.code.len())
        || *branch_pc <= new_pc
        || *branch_pc >= *set0_pc.min(set1_pc)
    {
        return Err("branch-local Set diamond operation sites cross a block boundary".into());
    }

    let fallthrough = (*branch_pc)
        .checked_add(1)
        .filter(|pc| *pc < function.code.len())
        .ok_or_else(|| "branch-local Set diamond control split has no fallthrough".to_string())?;
    let target = i64::try_from(fallthrough)
        .ok()
        .and_then(|fallthrough| fallthrough.checked_add(i64::from(*branch_offset)))
        .and_then(|target| usize::try_from(target).ok())
        .filter(|target| *target < function.code.len())
        .ok_or_else(|| "branch-local Set diamond control split escapes the function".to_string())?;
    if target == fallthrough {
        return Err("branch-local Set diamond control split has identical arm targets".into());
    }
    let (base_drop_pc, _) = base_drop_sites[0];
    let (left_path, left_ops) =
        branch_local_set_diamond_path_ops(function, fallthrough, base_drop_pc, *branch_pc)?;
    let (right_path, right_ops) =
        branch_local_set_diamond_path_ops(function, target, base_drop_pc, *branch_pc)?;
    let expected_for = |set_root: Reg| {
        vec![
            BranchLocalSetDiamondOpcode::Set {
                result: set_root,
                source: base_root,
            },
            BranchLocalSetDiamondOpcode::Size { source: set_root },
            BranchLocalSetDiamondOpcode::Drop { source: set_root },
            BranchLocalSetDiamondOpcode::Drop { source: base_root },
        ]
    };
    let expected0 = expected_for(*set0_root);
    let expected1 = expected_for(*set1_root);
    if !((left_ops == expected0 && right_ops == expected1)
        || (left_ops == expected1 && right_ops == expected0))
    {
        return Err(
            "branch-local Set diamond arms do not preserve Set -> Size -> child Drop -> shared base Drop order".into(),
        );
    }
    if let Some((left_join_index, join_pc)) = left_path
        .iter()
        .enumerate()
        .find(|(_, pc)| right_path.contains(pc))
    {
        let Some(right_join_index) = right_path.iter().position(|pc| pc == join_pc) else {
            return Err("branch-local Set diamond join identity is inconsistent".into());
        };
        if left_path[left_join_index..] != right_path[right_join_index..] {
            return Err("branch-local Set diamond arms overlap before a shared join".into());
        }
    }
    Ok(())
}

fn clone_isolation_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn persistent_root_alias_replacement_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn ordinary_map_owned_any_v2_is_empty_op_count(functions: &[FunctionProto]) -> usize {
    functions
        .iter()
        .flat_map(|function| &function.code)
        .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2IsEmpty { .. }))
        .count()
}

fn ordinary_map_owned_any_v2_clone_op_count(functions: &[FunctionProto]) -> usize {
    functions
        .iter()
        .flat_map(|function| &function.code)
        .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2Clone { .. }))
        .count()
}

fn ordinary_map_owned_any_v2_zero_miss_i32_opcode_counts(
    functions: &[FunctionProto],
) -> (usize, usize, usize) {
    let mut get_any = 0;
    let mut unpack = 0;
    let mut get_or_default_i32 = 0;
    for op in functions.iter().flat_map(|function| &function.code) {
        match op {
            Op::MirOrdinaryMapOwnedAnyV2GetAny { .. } => get_any += 1,
            Op::MirOrdinaryMapOwnedAnyV2Unpack { .. } => unpack += 1,
            Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. } => get_or_default_i32 += 1,
            _ => {}
        }
    }
    (get_any, unpack, get_or_default_i32)
}

fn ordinary_map_owned_any_v2_get_or_default_i32_opcode_count(functions: &[FunctionProto]) -> usize {
    functions
        .iter()
        .flat_map(|function| &function.code)
        .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }))
        .count()
}

fn ordinary_map_owned_any_v2_get_or_default_string_opcode_count(
    functions: &[FunctionProto],
) -> usize {
    functions
        .iter()
        .flat_map(|function| &function.code)
        .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultString { .. }))
        .count()
}

fn ordinary_map_owned_any_v2_get_or_default_i32_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn ordinary_map_owned_any_v2_get_or_default_string_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn ordinary_map_owned_any_v2_removed_key_checked_miss_opcode_counts(
    functions: &[FunctionProto],
) -> (usize, usize, usize, usize, usize) {
    let mut set = 0;
    let mut remove = 0;
    let mut get_any = 0;
    let mut unpack = 0;
    let mut get_or_default_i32 = 0;
    for op in functions.iter().flat_map(|function| &function.code) {
        match op {
            Op::MirOrdinaryMapOwnedAnyV2Set { .. } => set += 1,
            Op::MirOrdinaryMapOwnedAnyV2Remove { .. } => remove += 1,
            Op::MirOrdinaryMapOwnedAnyV2GetAny { .. } => get_any += 1,
            Op::MirOrdinaryMapOwnedAnyV2Unpack { .. } => unpack += 1,
            Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. } => get_or_default_i32 += 1,
            _ => {}
        }
    }
    (set, remove, get_any, unpack, get_or_default_i32)
}

impl CanonicalOrdinaryMapOwnedAnyV2CheckedGetRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "CheckedGet bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt
            .manifest_round_trip()
            .map_err(|message| format!("CheckedGet route manifest replay failed: {message}"))?;
        if replayed != *receipt {
            return Err("CheckedGet route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt
            .manifest_text()
            .map_err(|message| format!("CheckedGet route manifest render failed: {message}"))?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: checked_get_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "CheckedGet bytecode route profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("CheckedGet route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "CheckedGet route receipt disagrees with its immutable manifest anchor".into(),
            );
        }
        if entry != self.expected_entry
            || checked_get_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err("CheckedGet bytecode disagrees with its exact MIR route snapshot".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2ZeroMissI32RouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "ZeroMissI32 bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if ordinary_map_owned_any_v2_zero_miss_i32_opcode_counts(functions) != (2, 2, 0) {
            return Err(
                "ZeroMissI32 route requires exactly two emitted GetAny and two unpack instructions"
                    .into(),
            );
        }
        let replayed = receipt
            .manifest_round_trip()
            .map_err(|message| format!("ZeroMissI32 route manifest replay failed: {message}"))?;
        if replayed != *receipt {
            return Err("ZeroMissI32 route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt
            .manifest_text()
            .map_err(|message| format!("ZeroMissI32 route manifest render failed: {message}"))?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: ordinary_map_owned_any_v2_zero_miss_i32_bytecode_snapshot(
                entry, functions,
            ),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "ZeroMissI32 bytecode route profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("ZeroMissI32 route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "ZeroMissI32 route receipt disagrees with its immutable manifest anchor".into(),
            );
        }
        if entry != self.expected_entry
            || ordinary_map_owned_any_v2_zero_miss_i32_opcode_counts(functions) != (2, 2, 0)
            || ordinary_map_owned_any_v2_zero_miss_i32_bytecode_snapshot(entry, functions)
                != self.expected_program
        {
            return Err("ZeroMissI32 bytecode disagrees with its exact MIR route snapshot".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        let replacement = if self.receipt.mir_digest.starts_with('0') {
            '1'
        } else {
            '0'
        };
        self.receipt.mir_digest = replacement.to_string().repeat(64);
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32RouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "GetOrDefault<i32> bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if ordinary_map_owned_any_v2_get_or_default_i32_opcode_count(functions) != 1 {
            return Err(
                "GetOrDefault<i32> route requires exactly one dedicated emitted operation".into(),
            );
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("GetOrDefault<i32> route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err("GetOrDefault<i32> route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("GetOrDefault<i32> route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: ordinary_map_owned_any_v2_get_or_default_i32_bytecode_snapshot(
                entry, functions,
            ),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_I32_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "GetOrDefault<i32> bytecode route profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("GetOrDefault<i32> route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "GetOrDefault<i32> route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || ordinary_map_owned_any_v2_get_or_default_i32_opcode_count(functions) != 1
            || ordinary_map_owned_any_v2_get_or_default_i32_bytecode_snapshot(entry, functions)
                != self.expected_program
        {
            return Err(
                "GetOrDefault<i32> bytecode disagrees with its exact MIR route snapshot".into(),
            );
        }
        Ok(())
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "GetOrDefault<String> bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if ordinary_map_owned_any_v2_get_or_default_string_opcode_count(functions) != 1
            || ordinary_map_owned_any_v2_get_or_default_i32_opcode_count(functions) != 0
        {
            return Err(
                "GetOrDefault<String> route requires exactly one String opcode and no i32 specialization".into(),
            );
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("GetOrDefault<String> route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err("GetOrDefault<String> route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("GetOrDefault<String> route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: ordinary_map_owned_any_v2_get_or_default_string_bytecode_snapshot(
                entry, functions,
            ),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_GET_OR_DEFAULT_STRING_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "GetOrDefault<String> bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("GetOrDefault<String> route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "GetOrDefault<String> route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || ordinary_map_owned_any_v2_get_or_default_string_opcode_count(functions) != 1
            || ordinary_map_owned_any_v2_get_or_default_i32_opcode_count(functions) != 0
            || ordinary_map_owned_any_v2_get_or_default_string_bytecode_snapshot(entry, functions)
                != self.expected_program
        {
            return Err(
                "GetOrDefault<String> bytecode disagrees with its exact MIR route snapshot".into(),
            );
        }
        Ok(())
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2RemovedKeyCheckedMissRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "RemovedKeyCheckedMiss bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if ordinary_map_owned_any_v2_removed_key_checked_miss_opcode_counts(functions)
            != (1, 1, 1, 1, 0)
        {
            return Err(
                "RemovedKeyCheckedMiss requires exactly one Set, Remove, GetAny and checked-unpack opcode".into(),
            );
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("RemovedKeyCheckedMiss route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "RemovedKeyCheckedMiss route receipt changed during manifest replay".into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("RemovedKeyCheckedMiss route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: ordinary_map_owned_any_v2_removed_key_checked_miss_bytecode_snapshot(
                entry, functions,
            ),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_REMOVED_KEY_CHECKED_MISS_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "RemovedKeyCheckedMiss bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("RemovedKeyCheckedMiss route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "RemovedKeyCheckedMiss route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || ordinary_map_owned_any_v2_removed_key_checked_miss_opcode_counts(functions)
                != (1, 1, 1, 1, 0)
            || ordinary_map_owned_any_v2_removed_key_checked_miss_bytecode_snapshot(
                entry, functions,
            ) != self.expected_program
        {
            return Err(
                "RemovedKeyCheckedMiss bytecode disagrees with its exact MIR route snapshot".into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-removed-key-miss-digest".into();
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2IsEmptyRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "IsEmpty bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if ordinary_map_owned_any_v2_is_empty_op_count(functions) != 2 {
            return Err(
                "IsEmpty route requires exactly two emitted V2 IsEmpty instructions".into(),
            );
        }
        let replayed = receipt
            .manifest_round_trip()
            .map_err(|message| format!("IsEmpty route manifest replay failed: {message}"))?;
        if replayed != *receipt {
            return Err("IsEmpty route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt
            .manifest_text()
            .map_err(|message| format!("IsEmpty route manifest render failed: {message}"))?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: is_empty_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err("IsEmpty bytecode route profile disagrees with its private anchor".into());
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("IsEmpty route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "IsEmpty route receipt disagrees with its immutable manifest anchor".into(),
            );
        }
        if entry != self.expected_entry
            || ordinary_map_owned_any_v2_is_empty_op_count(functions) != 2
            || is_empty_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err("IsEmpty bytecode disagrees with its exact Checker route snapshot".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2RecordUnitRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "RecordUnit bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let counts = record_unit_opcode_counts(functions);
        if functions.len() != 1 || counts != (1, 3, 1, 5, 3, 5, 0) {
            return Err(format!(
                "RecordUnit bytecode requires one function and exact New/Set/Remove/Size/Contains/Drop counts, found functions={} counts={counts:?}",
                functions.len()
            ));
        }
        let builtin_calls = functions
            .iter()
            .flat_map(|function| &function.code)
            .filter(|op| matches!(op, Op::CallBuiltin { .. }))
            .count();
        if builtin_calls != 1 {
            return Err(format!(
                "RecordUnit bytecode requires exactly one builtin stdout call, found {builtin_calls}"
            ));
        }
        let replayed = receipt
            .manifest_round_trip()
            .map_err(|message| format!("RecordUnit route manifest replay failed: {message}"))?;
        if replayed != *receipt {
            return Err("RecordUnit route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt
            .manifest_text()
            .map_err(|message| format!("RecordUnit route manifest render failed: {message}"))?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: record_unit_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "RecordUnit bytecode route profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("RecordUnit route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "RecordUnit route receipt disagrees with its immutable manifest anchor".into(),
            );
        }
        if entry != self.expected_entry
            || functions.len() != 1
            || record_unit_opcode_counts(functions) != (1, 3, 1, 5, 3, 5, 0)
            || functions
                .iter()
                .flat_map(|function| &function.code)
                .filter(|op| matches!(op, Op::CallBuiltin { .. }))
                .count()
                != 1
            || record_unit_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "RecordUnit bytecode disagrees with its exact Checker route snapshot".into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE.into();
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2MapSetDropControlRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "MapSetDropControl bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if map_set_drop_control_opcode_counts(functions) != (1, 1, 2, 0) {
            return Err(format!(
                "MapSetDropControl route requires exactly New, Set, and two Drop opcodes with no other V2 opcode, found {:?}",
                map_set_drop_control_opcode_counts(functions)
            ));
        }
        let replayed = receipt
            .manifest_round_trip()
            .map_err(|message| format!("MapSetDropControl manifest replay failed: {message}"))?;
        if replayed != *receipt {
            return Err("MapSetDropControl route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt
            .manifest_text()
            .map_err(|message| format!("MapSetDropControl manifest render failed: {message}"))?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: map_set_drop_control_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "MapSetDropControl bytecode profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("MapSetDropControl manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "MapSetDropControl route receipt disagrees with its immutable anchor".into(),
            );
        }
        if entry != self.expected_entry
            || map_set_drop_control_opcode_counts(functions) != (1, 1, 2, 0)
            || map_set_drop_control_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "MapSetDropControl bytecode disagrees with its exact Checker route snapshot".into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE.into();
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2BranchLocalSetDiamondRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "branch-local Set diamond bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        validate_branch_local_set_diamond_bytecode(entry, functions)?;
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("branch-local Set diamond manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "branch-local Set diamond route receipt changed during manifest replay".into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("branch-local Set diamond manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: branch_local_set_diamond_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_BRANCH_LOCAL_SET_DIAMOND_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "branch-local Set diamond bytecode profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("branch-local Set diamond manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "branch-local Set diamond route receipt disagrees with its immutable anchor".into(),
            );
        }
        validate_branch_local_set_diamond_bytecode(entry, functions)?;
        if entry != self.expected_entry
            || branch_local_set_diamond_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "branch-local Set diamond bytecode disagrees with its exact Checker route snapshot"
                    .into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_PROFILE.into();
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2CloneIsolationRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "CloneIsolation bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if ordinary_map_owned_any_v2_clone_op_count(functions) != 1 {
            return Err("CloneIsolation route requires exactly one emitted V2 Clone opcode".into());
        }
        let replayed = receipt
            .manifest_round_trip()
            .map_err(|message| format!("CloneIsolation route manifest replay failed: {message}"))?;
        if replayed != *receipt {
            return Err("CloneIsolation route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt
            .manifest_text()
            .map_err(|message| format!("CloneIsolation route manifest render failed: {message}"))?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: clone_isolation_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "CloneIsolation bytecode route profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("CloneIsolation route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "CloneIsolation route receipt disagrees with its immutable manifest anchor".into(),
            );
        }
        if entry != self.expected_entry
            || ordinary_map_owned_any_v2_clone_op_count(functions) != 1
            || clone_isolation_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "CloneIsolation bytecode disagrees with its exact Checker route snapshot".into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        let replacement = if self.receipt.mir_digest.starts_with('0') {
            '1'
        } else {
            '0'
        };
        self.receipt
            .mir_digest
            .replace_range(..1, &replacement.to_string());
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2PersistentRootAliasReplacementRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "PersistentRootAliasReplacement bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("PersistentRootAliasReplacement route requires emitted V2 bytecode".into());
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("PersistentRootAliasReplacement route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "PersistentRootAliasReplacement route receipt changed during manifest replay"
                    .into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("PersistentRootAliasReplacement route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: persistent_root_alias_replacement_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "PersistentRootAliasReplacement bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!(
                "PersistentRootAliasReplacement route manifest replay failed at VM boundary: {message}"
            )
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "PersistentRootAliasReplacement route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || persistent_root_alias_replacement_bytecode_snapshot(entry, functions)
                != self.expected_program
        {
            return Err(
                "PersistentRootAliasReplacement bytecode disagrees with its exact Checker route snapshot"
                    .into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_digest_for_test_only(&mut self) {
        let replacement = if self.receipt.mir_digest.starts_with('0') {
            '1'
        } else {
            '0'
        };
        self.receipt
            .mir_digest
            .replace_range(..1, &replacement.to_string());
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.expected_manifest.push_str(";tampered=true");
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2ComputedKeyGetRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "computed-key Get bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("computed-key Get route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err("computed-key Get route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("computed-key Get route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: computed_key_get_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "computed-key Get bytecode route profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("computed-key Get route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "computed-key Get route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || computed_key_get_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "computed-key Get bytecode disagrees with its exact MIR route snapshot".into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "ComputedStringKeyOwnerChain bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("ComputedStringKeyOwnerChain manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "ComputedStringKeyOwnerChain receipt changed during manifest replay".into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("ComputedStringKeyOwnerChain manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: computed_string_key_owner_chain_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "ComputedStringKeyOwnerChain bytecode profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("ComputedStringKeyOwnerChain manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "ComputedStringKeyOwnerChain route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || computed_string_key_owner_chain_bytecode_snapshot(entry, functions)
                != self.expected_program
        {
            return Err(
                "ComputedStringKeyOwnerChain bytecode differs from its exact MIR route snapshot"
                    .into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        let replacement = if self.receipt.mir_digest.starts_with('0') {
            '1'
        } else {
            '0'
        };
        self.receipt
            .mir_digest
            .replace_range(..1, &replacement.to_string());
    }
}

impl CanonicalValuesMixedSnapshotRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Values mixed-snapshot bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Values mixed-snapshot route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "Values mixed-snapshot route receipt changed during manifest replay".into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Values mixed-snapshot route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: values_mixed_snapshot_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "Values mixed-snapshot bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("Values mixed-snapshot route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "Values mixed-snapshot route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || values_mixed_snapshot_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "Values mixed-snapshot bytecode disagrees with its exact MIR route snapshot".into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

impl CanonicalValuesLocalIndexI32RouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Values local-index i32 bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Values local-index i32 route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "Values local-index i32 route receipt changed during manifest replay".into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Values local-index i32 route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: values_local_index_i32_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "Values local-index i32 bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("Values local-index i32 route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "Values local-index i32 route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || values_local_index_i32_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "Values local-index i32 bytecode disagrees with its exact MIR route snapshot"
                    .into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

impl CanonicalValuesHeterogeneousStringIndex1RouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "heterogeneous Values bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("heterogeneous Values route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err("heterogeneous Values route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("heterogeneous Values route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: values_heterogeneous_string_index1_bytecode_snapshot(
                entry, functions,
            ),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "heterogeneous Values bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("heterogeneous Values route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "heterogeneous Values route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || values_heterogeneous_string_index1_bytecode_snapshot(entry, functions)
                != self.expected_program
        {
            return Err(
                "heterogeneous Values bytecode disagrees with its exact MIR route snapshot".into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

fn canonical_keys_snapshot_bytecode_sites(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> Result<((u32, Reg, Reg), (u32, Reg, Reg)), String> {
    if functions.len() != 1 || entry != 0 {
        return Err("KeysSnapshot bytecode requires one entry function".into());
    }
    let function = functions
        .get(entry as usize)
        .ok_or_else(|| "KeysSnapshot entry function is absent".to_string())?;
    let keys = function
        .code
        .iter()
        .enumerate()
        .filter_map(|(pc, op)| match op {
            Op::MirOrdinaryMapOwnedAnyV2Keys { rd, ra } => Some((pc, *rd, *ra)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let unpack = function
        .code
        .iter()
        .enumerate()
        .filter_map(|(pc, op)| match op {
            Op::MirOrdinaryMapOwnedAnyV2KeysUnpack { rd, ra } => Some((pc, *rd, *ra)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [keys] = keys.as_slice() else {
        return Err("KeysSnapshot requires one unique Keys opcode".into());
    };
    let [unpack] = unpack.as_slice() else {
        return Err("KeysSnapshot requires one unique KeysUnpack opcode".into());
    };
    let keys = (
        u32::try_from(keys.0).map_err(|_| "KeysSnapshot Keys PC exceeds u32 ABI")?,
        keys.1,
        keys.2,
    );
    let unpack = (
        u32::try_from(unpack.0).map_err(|_| "KeysSnapshot KeysUnpack PC exceeds u32 ABI")?,
        unpack.1,
        unpack.2,
    );
    if keys.0 >= unpack.0 || keys.1 == keys.2 || unpack.1 == unpack.2 {
        return Err("KeysSnapshot bytecode operation order or register aliases are invalid".into());
    }
    Ok((keys, unpack))
}

impl CanonicalKeysSnapshotRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "KeysSnapshot bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt
            .manifest_round_trip()
            .map_err(|message| format!("KeysSnapshot route manifest replay failed: {message}"))?;
        if replayed != *receipt {
            return Err("KeysSnapshot route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt
            .manifest_text()
            .map_err(|message| format!("KeysSnapshot route manifest render failed: {message}"))?;
        let (keys_site, unpack_site) = canonical_keys_snapshot_bytecode_sites(entry, functions)?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: keys_snapshot_bytecode_snapshot(entry, functions),
            keys_site,
            unpack_site,
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "KeysSnapshot bytecode route profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("KeysSnapshot route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "KeysSnapshot route receipt disagrees with its immutable manifest anchor".into(),
            );
        }
        if entry != self.expected_entry
            || keys_snapshot_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err("KeysSnapshot bytecode disagrees with its exact MIR route snapshot".into());
        }
        let (keys_site, unpack_site) = canonical_keys_snapshot_bytecode_sites(entry, functions)?;
        if keys_site != self.keys_site || unpack_site != self.unpack_site {
            return Err(
                "KeysSnapshot operation sites disagree with their private route anchor".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn authorizes_keys(&self, function: FuncIdx, pc: u32, rd: Reg, ra: Reg) -> bool {
        function == self.expected_entry && self.keys_site == (pc, rd, ra)
    }

    pub(crate) fn authorizes_unpack(&self, function: FuncIdx, pc: u32, rd: Reg, ra: Reg) -> bool {
        function == self.expected_entry && self.unpack_site == (pc, rd, ra)
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

fn canonical_dynamic_keys_get_bytecode_sites(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> Result<((u32, Reg), (u32, Reg, Reg), (u32, Reg, Reg)), String> {
    if functions.len() != 1 || entry != 0 {
        return Err("DynamicKeysGet bytecode requires one entry function".into());
    }
    let function = functions
        .get(entry as usize)
        .ok_or_else(|| "DynamicKeysGet entry function is absent".to_string())?;
    let arg0 = function
        .code
        .iter()
        .enumerate()
        .filter_map(|(pc, op)| match op {
            Op::MirOrdinaryMapOwnedAnyV2DynamicArg0 { rd } => Some((pc, *rd)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let keys = function
        .code
        .iter()
        .enumerate()
        .filter_map(|(pc, op)| match op {
            Op::MirOrdinaryMapOwnedAnyV2Keys { rd, ra } => Some((pc, *rd, *ra)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let unpack = function
        .code
        .iter()
        .enumerate()
        .filter_map(|(pc, op)| match op {
            Op::MirOrdinaryMapOwnedAnyV2KeysUnpack { rd, ra } => Some((pc, *rd, *ra)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [arg0] = arg0.as_slice() else {
        return Err("DynamicKeysGet requires one unique argv[0] opcode".into());
    };
    let [keys] = keys.as_slice() else {
        return Err("DynamicKeysGet requires one unique Keys opcode".into());
    };
    let [unpack] = unpack.as_slice() else {
        return Err("DynamicKeysGet requires one unique KeysUnpack opcode".into());
    };
    let arg0 = (
        u32::try_from(arg0.0).map_err(|_| "DynamicKeysGet argv[0] PC exceeds u32 ABI")?,
        arg0.1,
    );
    let keys = (
        u32::try_from(keys.0).map_err(|_| "DynamicKeysGet Keys PC exceeds u32 ABI")?,
        keys.1,
        keys.2,
    );
    let unpack = (
        u32::try_from(unpack.0).map_err(|_| "DynamicKeysGet KeysUnpack PC exceeds u32 ABI")?,
        unpack.1,
        unpack.2,
    );
    if arg0.0 >= keys.0 || keys.0 >= unpack.0 || arg0.1 == keys.1 || keys.1 == keys.2 {
        return Err("DynamicKeysGet bytecode opcode order or register aliases are invalid".into());
    }
    Ok((arg0, keys, unpack))
}

impl CanonicalDynamicKeysGetRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "DynamicKeysGet bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt
            .manifest_round_trip()
            .map_err(|message| format!("DynamicKeysGet route manifest replay failed: {message}"))?;
        if replayed != *receipt {
            return Err("DynamicKeysGet route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt
            .manifest_text()
            .map_err(|message| format!("DynamicKeysGet route manifest render failed: {message}"))?;
        let (arg0_site, keys_site, unpack_site) =
            canonical_dynamic_keys_get_bytecode_sites(entry, functions)?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: format!("{functions:?}"),
            arg0_site,
            keys_site,
            unpack_site,
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err("DynamicKeysGet bytecode profile disagrees with its private anchor".into());
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("DynamicKeysGet route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "DynamicKeysGet route receipt disagrees with its immutable manifest anchor".into(),
            );
        }
        if entry != self.expected_entry || format!("{functions:?}") != self.expected_program {
            return Err("DynamicKeysGet bytecode differs from its exact MIR route snapshot".into());
        }
        let (arg0_site, keys_site, unpack_site) =
            canonical_dynamic_keys_get_bytecode_sites(entry, functions)?;
        if arg0_site != self.arg0_site
            || keys_site != self.keys_site
            || unpack_site != self.unpack_site
        {
            return Err(
                "DynamicKeysGet bytecode operation sites disagree with their private route anchor"
                    .into(),
            );
        }
        Ok(())
    }

    pub(crate) fn authorizes_arg0(&self, function: FuncIdx, pc: u32, rd: Reg) -> bool {
        function == self.expected_entry && self.arg0_site == (pc, rd)
    }

    pub(crate) fn authorizes_keys(&self, function: FuncIdx, pc: u32, rd: Reg, ra: Reg) -> bool {
        function == self.expected_entry && self.keys_site == (pc, rd, ra)
    }

    pub(crate) fn authorizes_unpack(&self, function: FuncIdx, pc: u32, rd: Reg, ra: Reg) -> bool {
        function == self.expected_entry && self.unpack_site == (pc, rd, ra)
    }
}

impl CanonicalValuesMixedSnapshotRemoveRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Values mixed-snapshot-remove bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Values mixed-snapshot-remove route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "Values mixed-snapshot-remove route receipt changed during manifest replay".into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Values mixed-snapshot-remove route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: values_mixed_snapshot_remove_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "Values mixed-snapshot-remove bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!(
                "Values mixed-snapshot-remove route manifest replay failed at VM boundary: {message}"
            )
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "Values mixed-snapshot-remove route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || values_mixed_snapshot_remove_bytecode_snapshot(entry, functions)
                != self.expected_program
        {
            return Err(
                "Values mixed-snapshot-remove bytecode disagrees with its exact MIR route snapshot"
                    .into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

impl CanonicalValuesSnapshotReplacementRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Values snapshot replacement bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Values snapshot replacement route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "Values snapshot replacement route receipt changed during manifest replay".into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Values snapshot replacement route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: values_snapshot_replacement_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "Values snapshot replacement bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!(
                "Values snapshot replacement route manifest replay failed at VM boundary: {message}"
            )
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "Values snapshot replacement route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || values_snapshot_replacement_bytecode_snapshot(entry, functions)
                != self.expected_program
        {
            return Err(
                "Values snapshot replacement bytecode disagrees with its exact MIR route snapshot"
                    .into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

impl CanonicalValuesFieldMoveRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Values field-move bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Values field-move route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err("Values field-move route receipt changed during manifest replay".into());
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Values field-move route manifest render failed: {message}")
        })?;
        Ok(Self {
            expected_profile,
            receipt: receipt.clone(),
            expected_manifest,
            expected_entry: entry,
            expected_program: values_field_move_bytecode_snapshot(entry, functions),
        })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "Values field-move bytecode route profile disagrees with its private anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("Values field-move route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "Values field-move route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || values_field_move_bytecode_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "Values field-move bytecode disagrees with its exact MIR route snapshot".into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_CHECKED_GET_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }
}

fn values_field_move_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn values_snapshot_replacement_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_SNAPSHOT_REPLACEMENT_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn values_mixed_snapshot_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn values_local_index_i32_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_LOCAL_INDEX_I32_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn values_heterogeneous_string_index1_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_HETEROGENEOUS_STRING_INDEX1_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn keys_snapshot_bytecode_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn values_mixed_snapshot_remove_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_MIXED_SNAPSHOT_REMOVE_PROFILE,
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn values_producer_caller_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
    bindings: &[CanonicalValuesProducerCallerBytecodeBinding],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={};sites={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE,
        functions.len(),
        bindings.len()
    );
    use std::fmt::Write as _;
    let _ = write!(snapshot, "|typed-sites={bindings:?}");
    for (index, function) in functions.iter().enumerate() {
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn map_root_producer_caller_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
    bindings: &[CanonicalMapRootProducerCallerBytecodeBinding],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={};sites={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE,
        functions.len(),
        bindings.len()
    );
    use std::fmt::Write as _;
    let _ = write!(snapshot, "|typed-sites={bindings:?}");
    for (index, function) in functions.iter().enumerate() {
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

impl CanonicalMapRootProducerCallerBytecodeBinding {
    fn agrees_with_receipt(
        &self,
        proof: &crate::core::mir::OrdinaryMapOwnedAnyV2MapRootProducerCallerMirReceipt,
    ) -> bool {
        let expected_initial_root = crate::core::mir::MirValueId::new(format!(
            "local:{}",
            proof.checker_producer.initial_root_local.0 .0
        ))
        .ok();
        let expected_final_root = crate::core::mir::MirValueId::new(format!(
            "local:{}",
            proof.checker_producer.final_root_local.0 .0
        ))
        .ok();
        let expected_payload = crate::core::mir::MirValueId::new(format!(
            "local:{}",
            proof.checker_producer.payload_local.0 .0
        ))
        .ok();
        let expected_set_key = crate::core::mir::MirValueId::new(format!(
            "expr:{}",
            proof.checker_producer.set_key_node.0
        ))
        .ok();
        let expected_caller_root = crate::core::mir::MirValueId::new(format!(
            "local:{}",
            proof.checker_caller.caller_root_local.0 .0
        ))
        .ok();
        let expected_call_result =
            crate::core::mir::MirValueId::new(format!("expr:{}", proof.checker_caller.call_node.0))
                .ok();
        let expected_size_result = crate::core::mir::MirValueId::new(format!(
            "expr:{}",
            proof.checker_caller.map_size_node.0
        ))
        .ok();
        let expected_size_binding = crate::core::mir::MirValueId::new(format!(
            "local:{}",
            proof.checker_caller.size_binding.0 .0
        ))
        .ok();
        let expected_scalar_return = crate::core::mir::MirValueId::new(format!(
            "expr:{}",
            proof.checker_caller.caller_return.0
        ))
        .ok();

        self.producer_owner == proof.producer_owner
            && self.caller_owner == proof.caller_owner
            && proof.schema
                == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_SCHEMA
            && proof.profile
                == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE
            && proof.producer_owner == proof.checker_producer.owner
            && proof.caller_owner == proof.checker_caller.caller_owner
            && proof.call_target == proof.checker_caller.call_target
            && proof.checker_caller.producer_owner == proof.producer_owner
            && proof.checker_caller.logical_root == proof.checker_producer.final_root
            && proof.checker_caller.producer_map_set_operation
                == proof.checker_producer.map_set_operation
            && proof.checker_caller.producer_return_value == proof.checker_producer.return_value
            && proof.scalar_type == proof.checker_caller.caller_return_type
            && proof.call_target == proof.producer_owner
            && self.producer_new_instruction == proof.producer_new_instruction
            && self.producer_set_instruction == proof.producer_set_instruction
            && self.producer_predecessor_drop_instruction
                == proof.producer_predecessor_drop_instruction
            && self.producer_payload_drop_instruction == proof.producer_payload_drop_instruction
            && self.producer_return_instruction == proof.producer_return_instruction
            && self.caller_call_instruction == proof.caller_call_instruction
            && self.caller_binding_instruction == proof.caller_binding_instruction
            && self.caller_size_instruction == proof.caller_size_instruction
            && self.caller_size_binding_instruction == proof.caller_size_binding_instruction
            && self.caller_root_drop_instruction == proof.caller_root_drop_instruction
            && self.caller_scalar_return_instruction == proof.caller_scalar_return_instruction
            && expected_initial_root.as_ref() == Some(&self.initial_root)
            && expected_final_root.as_ref() == Some(&self.final_root)
            && expected_payload.as_ref() == Some(&self.set_payload)
            && expected_set_key.as_ref() == Some(&self.set_key)
            && expected_final_root.as_ref() == Some(&proof.final_root)
            && self.producer_return_value == proof.producer_return_value
            && expected_call_result.as_ref() == Some(&self.caller_call_result)
            && self.caller_root == proof.caller_root
            && expected_caller_root.as_ref() == Some(&self.caller_root)
            && expected_size_result.as_ref() == Some(&self.caller_size_result)
            && expected_size_binding.as_ref() == Some(&self.caller_size_binding)
            && expected_scalar_return.as_ref() == Some(&self.caller_scalar_return)
            && self.root_type == proof.root_type
            && self.scalar_type == proof.scalar_type
    }
}

impl CanonicalMapRootProducerCallerRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootProducerCallerMirReceipt>,
        bindings: Vec<CanonicalMapRootProducerCallerBytecodeBinding>,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Map-root producer-caller bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if proofs.len() != 1 || bindings.len() != 1 {
            return Err(
                "Map-root producer-caller route must bind exactly one proof/site pair".into(),
            );
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Map-root producer-caller route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "Map-root producer-caller route receipt changed during manifest replay".into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Map-root producer-caller route manifest render failed: {message}")
        })?;
        let expected_program =
            map_root_producer_caller_bytecode_snapshot(entry, functions, &bindings);
        let anchor = Self {
            expected_profile,
            receipt: receipt.clone(),
            proofs,
            bindings,
            expected_manifest,
            expected_entry: entry,
            expected_program,
        };
        anchor.validate(entry, functions)?;
        Ok(anchor)
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_PRODUCER_CALLER_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "Map-root producer-caller bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!(
                "Map-root producer-caller route manifest replay failed at VM boundary: {message}"
            )
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "Map-root producer-caller route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if self.proofs.len() != 1
            || self.bindings.len() != 1
            || !self.bindings[0].agrees_with_receipt(&self.proofs[0])
        {
            return Err(
                "Map-root producer-caller proof/site table is empty, mismatched, or malformed"
                    .into(),
            );
        }

        let proof = &self.proofs[0];
        let binding = &self.bindings[0];
        if binding.root_type != crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
            || binding.scalar_type
                != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
        {
            return Err("Map-root producer-caller TypeDesc boundary is not canonical".into());
        }
        if entry != self.expected_entry
            || map_root_producer_caller_bytecode_snapshot(entry, functions, &self.bindings)
                != self.expected_program
        {
            return Err(
                "Map-root producer-caller bytecode disagrees with its immutable route snapshot"
                    .into(),
            );
        }
        let producer = functions
            .get(binding.producer_function as usize)
            .ok_or_else(|| {
                "Map-root producer function index is outside bytecode table".to_string()
            })?;
        let caller = functions
            .get(binding.caller_function as usize)
            .ok_or_else(|| {
                "Map-root caller function index is outside bytecode table".to_string()
            })?;
        if producer.name != binding.producer_owner.0
            || caller.name != binding.caller_owner.0
            || binding.call_target != binding.producer_function
            || proof.call_target != binding.producer_owner
        {
            return Err(
                "Map-root producer-caller bytecode function/callee identity mismatch".into(),
            );
        }

        let sites = [
            binding.producer_new_pc,
            binding.producer_set_pc,
            binding.producer_predecessor_drop_pc,
            binding.producer_payload_drop_pc,
            binding.producer_return_move_pc,
            binding.producer_return_pc,
            binding.caller_call_pc,
            binding.caller_binding_move_pc,
            binding.caller_size_pc,
            binding.caller_size_binding_move_pc,
            binding.caller_root_drop_pc,
            binding.caller_scalar_return_move_pc,
            binding.caller_return_pc,
        ];
        let pcs = sites
            .map(|pc| usize::try_from(pc).map_err(|_| "Map-root bridge PC exceeds usize"))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let [producer_new_pc, producer_set_pc, producer_predecessor_drop_pc, producer_payload_drop_pc, producer_return_move_pc, producer_return_pc, caller_call_pc, caller_binding_move_pc, caller_size_pc, caller_size_binding_move_pc, caller_root_drop_pc, caller_scalar_return_move_pc, caller_return_pc] =
            pcs.as_slice()
        else {
            unreachable!("fixed-size Map-root producer/caller PC vector")
        };
        if !(producer_new_pc < producer_set_pc
            && producer_set_pc < producer_predecessor_drop_pc
            && producer_predecessor_drop_pc < producer_payload_drop_pc
            && producer_payload_drop_pc < producer_return_move_pc
            && producer_return_move_pc < producer_return_pc
            && caller_call_pc < caller_binding_move_pc
            && caller_binding_move_pc < caller_size_pc
            && caller_size_pc < caller_size_binding_move_pc
            && caller_size_binding_move_pc < caller_root_drop_pc
            && caller_root_drop_pc < caller_scalar_return_move_pc
            && caller_scalar_return_move_pc < caller_return_pc)
        {
            return Err(
                "Map-root producer-caller bytecode ownership sites are out of route order".into(),
            );
        }
        let producer_returns = producer
            .code
            .iter()
            .filter(|op| matches!(op, Op::Ret { .. } | Op::RetUnit | Op::RetEarly { .. }))
            .count();
        let caller_returns = caller
            .code
            .iter()
            .filter(|op| matches!(op, Op::Ret { .. } | Op::RetUnit | Op::RetEarly { .. }))
            .count();
        if producer_returns != 1
            || caller_returns != 1
            || !matches!(producer.code.get(*producer_new_pc), Some(Op::MirOrdinaryMapOwnedAnyV2New { rd }) if *rd == binding.producer_new_rd)
            || !matches!(producer.code.get(*producer_set_pc), Some(Op::MirOrdinaryMapOwnedAnyV2Set { rd, ra, key, value }) if (*rd, *ra, *key, *value) == (binding.producer_set_rd, binding.producer_set_ra, binding.producer_set_key, binding.producer_set_payload))
            || !matches!(producer.code.get(*producer_predecessor_drop_pc), Some(Op::MirOrdinaryMapOwnedAnyV2Drop { ra }) if *ra == binding.producer_predecessor_drop_ra)
            || !matches!(producer.code.get(*producer_payload_drop_pc), Some(Op::Drop { ra }) if *ra == binding.producer_payload_drop_ra)
            || !matches!(producer.code.get(*producer_return_move_pc), Some(Op::Move { rd, rs }) if (*rd, *rs) == (binding.producer_return_move_rd, binding.producer_return_move_rs))
            || !matches!(producer.code.get(*producer_return_pc), Some(Op::Ret { ra }) if *ra == binding.producer_return_ra)
            || binding.caller_call_rd != binding.caller_binding_move_rs
            || !matches!(caller.code.get(*caller_call_pc), Some(Op::CallMove { rd, func, argc: 0, .. }) if (*rd, *func) == (binding.caller_call_rd, binding.call_target))
            || !matches!(caller.code.get(*caller_binding_move_pc), Some(Op::Move { rd, rs }) if (*rd, *rs) == (binding.caller_binding_move_rd, binding.caller_binding_move_rs))
            || !matches!(caller.code.get(*caller_size_pc), Some(Op::MirOrdinaryMapOwnedAnyV2Size { rd, ra }) if (*rd, *ra) == (binding.caller_size_rd, binding.caller_size_ra))
            || !matches!(caller.code.get(*caller_size_binding_move_pc), Some(Op::Mov { rd, rs }) if (*rd, *rs) == (binding.caller_size_binding_move_rd, binding.caller_size_binding_move_rs))
            || !matches!(caller.code.get(*caller_root_drop_pc), Some(Op::MirOrdinaryMapOwnedAnyV2Drop { ra }) if *ra == binding.caller_root_drop_ra)
            || !matches!(caller.code.get(*caller_scalar_return_move_pc), Some(Op::Mov { rd, rs }) if (*rd, *rs) == (binding.caller_scalar_return_move_rd, binding.caller_scalar_return_move_rs))
            || !matches!(caller.code.get(*caller_return_pc), Some(Op::Ret { ra }) if *ra == binding.caller_return_ra)
        {
            return Err("Map-root producer-caller bytecode opcode/register sites disagree with their private Checker/MIR binding".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-map-root-mir-digest".into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_call_target_for_test_only(&mut self) {
        if let Some(binding) = self.bindings.first_mut() {
            binding.call_target = binding.caller_function;
        }
    }

    #[cfg(test)]
    pub(crate) fn corrupt_root_drop_register_for_test_only(&mut self) {
        if let Some(binding) = self.bindings.first_mut() {
            binding.caller_root_drop_ra = Reg::MAX;
        }
    }
}

fn map_root_get_any_caller_bytecode_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
    bindings: &[CanonicalMapRootGetAnyCallerBytecodeBinding],
) -> String {
    let mut snapshot = format!(
        "profile={};version=1;entry={entry};functions={};sites={}",
        crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_PROFILE,
        functions.len(),
        bindings.len()
    );
    use std::fmt::Write as _;
    let _ = write!(snapshot, "|typed-sites={bindings:?}");
    for (index, function) in functions.iter().enumerate() {
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

impl CanonicalMapRootGetAnyCallerBytecodeBinding {
    fn agrees_with_receipt(
        &self,
        proof: &crate::core::mir::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerMirReceipt,
    ) -> bool {
        let expected = [
            &proof.producer_new_instruction,
            &proof.producer_set_instruction,
            &proof.producer_predecessor_drop_instruction,
            &proof.producer_payload_drop_instruction,
            &proof.producer_return_instruction,
            &proof.caller_call_instruction,
            &proof.caller_binding_instruction,
            &proof.get_any_result.instruction,
            &proof.caller_pair_binding_instruction,
            &proof.caller_root_drop_instruction,
            &proof.unpack_result.instruction,
            &proof.caller_string_binding_instruction,
            &proof.get_any_result.settlement_instruction,
            &proof.caller_string_drop_instruction,
            &proof.caller_scalar_return_instruction,
        ];
        let expected_root = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
        let expected_string =
            crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        let expected_scalar = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        let checker_string_binding = crate::core::mir::MirValueId::new(format!(
            "local:{}",
            proof.checker_caller.unpack_local.0 .0
        ))
        .ok();
        self.producer_owner == proof.producer_owner
            && self.caller_owner == proof.caller_owner
            && proof.schema
                == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_SCHEMA
            && proof.profile
                == crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_PROFILE
            && proof.call_target == proof.producer_owner
            && proof.surface_root_type == proof.checker_producer.final_root_type
            && proof.get_any_result.owner == proof.caller_owner
            && proof.get_any_result.root == proof.caller_root
            && proof.get_any_result.binding == proof.unpack_result.pair
            && proof.unpack_result.owner == proof.caller_owner
            && checker_string_binding.as_ref() == Some(&proof.caller_string)
            && proof.unpack_result.cast == proof.checker_caller.unpack_cast
            && proof.unpack_result.result_type == expected_string
            && proof.unpack_result.conversion_to == expected_string
            && proof.root_type == expected_root
            && proof.string_type == expected_string
            && proof.scalar_type == expected_scalar
            && self.root_type == proof.root_type
            && self.string_type == proof.string_type
            && self.scalar_type == proof.scalar_type
            && self.sites.len() == expected.len()
            && self
                .sites
                .iter()
                .zip(expected)
                .enumerate()
                .all(|(index, (site, instruction))| {
                    site.instruction == *instruction
                        && site.function
                            == if index < 5 {
                                self.producer_function
                            } else {
                                self.caller_function
                            }
                })
    }

    fn validate_sites(&self, functions: &[FunctionProto]) -> Result<(), String> {
        if self.sites.len() != 15 {
            return Err("Map-root GetAny-caller binding has an incomplete site table".into());
        }
        let producer = functions
            .get(self.producer_function as usize)
            .ok_or_else(|| {
                "Map-root GetAny producer index is outside bytecode table".to_string()
            })?;
        let caller = functions
            .get(self.caller_function as usize)
            .ok_or_else(|| "Map-root GetAny caller index is outside bytecode table".to_string())?;
        if producer.name != self.producer_owner.0
            || caller.name != self.caller_owner.0
            || self.producer_function == self.caller_function
            || self.call_target != self.producer_function
        {
            return Err("Map-root GetAny-caller function/callee identity mismatch".into());
        }
        let op = |index: usize| -> Result<Op, String> {
            let site = &self.sites[index];
            if site.function
                != if index < 5 {
                    self.producer_function
                } else {
                    self.caller_function
                }
            {
                return Err("Map-root GetAny-caller site belongs to another function".into());
            }
            let function = if index < 5 { producer } else { caller };
            let pc = usize::try_from(site.pc)
                .map_err(|_| "Map-root GetAny-caller PC exceeds usize".to_string())?;
            let actual = *function
                .code
                .get(pc)
                .ok_or_else(|| "Map-root GetAny-caller PC is outside function code".to_string())?;
            if actual != site.op {
                return Err("Map-root GetAny-caller opcode differs from its bound site".into());
            }
            Ok(actual)
        };
        let [Op::MirOrdinaryMapOwnedAnyV2New { rd: new_rd }, Op::MirOrdinaryMapOwnedAnyV2Set {
            rd: set_rd,
            ra: set_ra,
            key: _set_key,
            value: set_value,
        }, Op::MirOrdinaryMapOwnedAnyV2Drop {
            ra: predecessor_drop,
        }, Op::Drop { ra: payload_drop }, Op::Move {
            rd: producer_move_rd,
            rs: producer_move_rs,
        }, Op::CallMove {
            rd: call_rd,
            func,
            argc: 0,
            ..
        }, Op::Move {
            rd: caller_move_rd,
            rs: caller_move_rs,
        }, Op::MirOrdinaryMapOwnedAnyV2GetAny {
            rd: get_any_rd,
            ra: get_any_ra,
            key: _get_any_key,
        }, Op::Move {
            rd: pair_binding_rd,
            rs: pair_binding_rs,
        }, Op::MirOrdinaryMapOwnedAnyV2Drop { ra: root_drop }, Op::MirOrdinaryMapOwnedAnyV2Unpack {
            rd: unpack_rd,
            ra: unpack_ra,
        }, Op::Move {
            rd: string_binding_rd,
            rs: string_binding_rs,
        }, Op::DropAggregate {
            ra: pair_drop,
            arity: pair_arity,
        }, Op::Drop { ra: string_drop }, Op::LoadConst {
            rd: scalar_rd,
            idx: scalar_idx,
        }] = [
            op(0)?,
            op(1)?,
            op(2)?,
            op(3)?,
            op(4)?,
            op(5)?,
            op(6)?,
            op(7)?,
            op(8)?,
            op(9)?,
            op(10)?,
            op(11)?,
            op(12)?,
            op(13)?,
            op(14)?,
        ]
        else {
            let actual = self
                .sites
                .iter()
                .map(|site| (site.pc, site.op))
                .collect::<Vec<_>>();
            return Err(format!(
                "Map-root GetAny-caller bytecode operations have unexpected kinds: {actual:?}"
            ));
        };
        if self.sites[..5]
            .windows(2)
            .any(|pair| pair[0].pc >= pair[1].pc)
            || self.sites[4].pc >= self.producer_return_pc
            || self.sites[5..15]
                .windows(2)
                .any(|pair| pair[0].pc >= pair[1].pc)
            || self.sites[14].pc >= self.caller_return_pc
        {
            return Err("Map-root GetAny-caller bytecode sites are out of route order".into());
        }
        let producer_return_rd = match producer.code.get(self.producer_return_pc as usize) {
            Some(Op::Ret { ra }) => *ra,
            _ => return Err("Map-root GetAny producer has no bound root return".into()),
        };
        let caller_return_rd = match caller.code.get(self.caller_return_pc as usize) {
            Some(Op::Ret { ra }) => *ra,
            _ => return Err("Map-root GetAny caller has no bound scalar return".into()),
        };
        let mismatches = [
            ("Set source is not the allocated root", set_ra != new_rd),
            (
                "predecessor Drop is not the allocated root",
                predecessor_drop != new_rd,
            ),
            (
                "Set result is not the producer Move source",
                set_rd != producer_move_rs,
            ),
            (
                "producer Move does not feed the root return",
                producer_move_rd != producer_return_rd,
            ),
            (
                "direct call result is not the caller Move source",
                call_rd != caller_move_rs,
            ),
            (
                "direct call target differs from the receipt",
                func != self.call_target,
            ),
            (
                "caller Move does not feed GetAny and root Drop",
                caller_move_rd != get_any_ra || caller_move_rd != root_drop,
            ),
            (
                "GetAny result does not feed the pair binding Move",
                get_any_rd != pair_binding_rs,
            ),
            (
                "pair binding Move does not feed unpack and pair Drop",
                pair_binding_rd != unpack_ra || pair_binding_rd != pair_drop,
            ),
            (
                "GetAny pair Drop is not a two-field aggregate",
                pair_arity != 2,
            ),
            (
                "unpack result is not the String binding source",
                unpack_rd != string_binding_rs,
            ),
            (
                "String binding does not feed String Drop",
                string_binding_rd != string_drop,
            ),
            (
                "scalar return does not use the receipt literal",
                scalar_rd != caller_return_rd || !bytecode_int_constant(caller, scalar_idx, 0),
            ),
        ]
        .into_iter()
        .filter_map(|(message, mismatch)| mismatch.then_some(message))
        .collect::<Vec<_>>();
        if !mismatches.is_empty() {
            return Err(format!(
                "Map-root GetAny-caller bytecode transfers or settlements disagree: {}; Set.value={set_value:?}, payload.Drop={payload_drop:?}, GetAny.result={get_any_rd:?}, pair.binding=({pair_binding_rd:?} <- {pair_binding_rs:?}), unpack.pair={unpack_ra:?}, pair.Drop={pair_drop:?}",
                mismatches.join("; "),
            ));
        }
        let producer_returns = producer
            .code
            .iter()
            .filter(|op| matches!(op, Op::Ret { .. } | Op::RetUnit | Op::RetEarly { .. }))
            .count();
        let caller_returns = caller
            .code
            .iter()
            .filter(|op| matches!(op, Op::Ret { .. } | Op::RetUnit | Op::RetEarly { .. }))
            .count();
        if producer_returns != 1 || caller_returns != 1 {
            return Err("Map-root GetAny-caller bytecode has a non-unique return".into());
        }
        Ok(())
    }
}

impl CanonicalMapRootGetAnyCallerRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerMirReceipt>,
        bindings: Vec<CanonicalMapRootGetAnyCallerBytecodeBinding>,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Map-root GetAny-caller bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if proofs.len() != 1 || bindings.len() != 1 {
            return Err(
                "Map-root GetAny-caller route must bind exactly one proof/site pair".into(),
            );
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Map-root GetAny-caller manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err("Map-root GetAny-caller route changed during manifest replay".into());
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Map-root GetAny-caller manifest render failed: {message}")
        })?;
        let expected_program =
            map_root_get_any_caller_bytecode_snapshot(entry, functions, &bindings);
        let anchor = Self {
            expected_profile,
            receipt: receipt.clone(),
            proofs,
            bindings,
            expected_manifest,
            expected_entry: entry,
            expected_program,
        };
        anchor.validate(entry, functions)?;
        Ok(anchor)
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_MAP_ROOT_GET_ANY_CALLER_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "Map-root GetAny-caller profile differs from its private route anchor".into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("Map-root GetAny-caller manifest replay failed: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err("Map-root GetAny-caller manifest differs from its immutable anchor".into());
        }
        if self.proofs.len() != 1
            || self.bindings.len() != 1
            || !self.bindings[0].agrees_with_receipt(&self.proofs[0])
        {
            return Err("Map-root GetAny-caller proof/site table is malformed".into());
        }
        let binding = &self.bindings[0];
        if binding.root_type != crate::core::mir::types::ordinary_map_owned_any_v2_type_id()
            || binding.string_type
                != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
            || binding.scalar_type
                != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
        {
            return Err("Map-root GetAny-caller TypeDesc boundary is not canonical".into());
        }
        if entry != self.expected_entry
            || map_root_get_any_caller_bytecode_snapshot(entry, functions, &self.bindings)
                != self.expected_program
        {
            return Err("Map-root GetAny-caller bytecode differs from its route snapshot".into());
        }
        binding.validate_sites(functions)
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile = crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-map-root-get-any-mir-digest".into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_get_any_key_for_test_only(&mut self) {
        if let Some(site) = self
            .bindings
            .first_mut()
            .and_then(|binding| binding.sites.get_mut(7))
        {
            if let Op::MirOrdinaryMapOwnedAnyV2GetAny { key, .. } = &mut site.op {
                *key = Reg::MAX;
            }
        }
    }
}

pub(crate) fn values_producer_caller_literal_tail_matches(
    caller: &FunctionProto,
    pc: usize,
    return_register: Reg,
    literal: &crate::core::ir::ResolvedLiteral,
) -> bool {
    match literal {
        crate::core::ir::ResolvedLiteral::Int(expected) => {
            matches!(
                caller.code.get(pc),
                Some(Op::LoadConst { rd, idx })
                    if *rd == return_register
                        && matches!(
                            caller.constants.get(*idx as usize),
                            Some(ConstValue::Int(actual)) if actual == expected
                        )
            )
        }
        crate::core::ir::ResolvedLiteral::FloatBits(expected) => {
            matches!(
                caller.code.get(pc),
                Some(Op::LoadConst { rd, idx })
                    if *rd == return_register
                        && matches!(
                            caller.constants.get(*idx as usize),
                            Some(ConstValue::Float(actual)) if actual.to_bits() == *expected
                        )
            )
        }
        crate::core::ir::ResolvedLiteral::Bool(true) => matches!(
            caller.code.get(pc),
            Some(Op::LoadTrue { rd }) if *rd == return_register
        ),
        crate::core::ir::ResolvedLiteral::Bool(false) => matches!(
            caller.code.get(pc),
            Some(Op::LoadFalse { rd }) if *rd == return_register
        ),
        crate::core::ir::ResolvedLiteral::String(_) | crate::core::ir::ResolvedLiteral::Unit => {
            false
        }
    }
}

impl CanonicalValuesProducerCallerRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesProducerCallerMirReceipt>,
        bindings: Vec<CanonicalValuesProducerCallerBytecodeBinding>,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Values producer-caller bytecode anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if proofs.is_empty() || proofs.len() != bindings.len() {
            return Err(
                "Values producer-caller route must bind a nonempty one-to-one proof/site table"
                    .into(),
            );
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Values producer-caller route manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "Values producer-caller route receipt changed during manifest replay".into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Values producer-caller route manifest render failed: {message}")
        })?;
        let expected_program =
            values_producer_caller_bytecode_snapshot(entry, functions, &bindings);
        let anchor = Self {
            expected_profile,
            receipt: receipt.clone(),
            proofs,
            bindings,
            expected_manifest,
            expected_entry: entry,
            expected_program,
        };
        anchor.validate(entry, functions)?;
        Ok(anchor)
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.expected_profile
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE
            || self.receipt.profile != self.expected_profile
        {
            return Err(
                "Values producer-caller bytecode route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("Values producer-caller route manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "Values producer-caller route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if self.proofs.is_empty() || self.proofs.len() != self.bindings.len() {
            return Err(
                "Values producer-caller private proof/site table is empty or mismatched".into(),
            );
        }
        if self
            .proofs
            .iter()
            .zip(&self.bindings)
            .any(|(proof, binding)| {
                proof.validate().is_err()
                    || !proof.unpacks.is_empty()
                    || proof.checker_producer.owner != proof.producer_owner
                    || proof.checker_caller.caller_owner != proof.caller_owner
                    || proof.checker_caller.call_target != proof.call_target
                    || proof.call_target != proof.producer_owner
                    || proof.producer_implicit_return != proof.checker_producer.return_value
                    || proof.values_binding_local != proof.checker_producer.snapshot_local
                    || proof.values_binding_point != proof.checker_producer.snapshot_binding_point
                    || proof.caller_binding_local != proof.checker_caller.owner_local
                    || proof.caller_binding_local != proof.checker_caller.result_binding
                    || proof.caller_binding_point != proof.checker_caller.result_binding_point
                    || proof.owner_drop_point != proof.checker_caller.owner_drop_point
                    || proof.scalar_return_node != proof.checker_caller.caller_return
                    || proof.producer_result_type != proof.owner_type
                    || proof.call_result_type != proof.owner_type
                    || proof.producer_checker_snapshot_type != proof.checker_producer.snapshot_type
                    || proof.producer_surface_return_type
                        != proof.checker_producer.surface_return_type
                    || proof.call_surface_result_type != proof.checker_caller.result_type
                    || proof.producer_surface_return_type != proof.call_surface_result_type
                    || proof.checker_caller.owner_type != proof.producer_checker_snapshot_type
                    || proof.producer_return_cast_node
                        != proof.checker_producer.return_container_erase.cast_node
                    || proof.producer_return_source_node
                        != proof.checker_producer.return_container_erase.source_node
                    || proof.producer_return_source_type
                        != proof.checker_producer.return_container_erase.source_type
                    || proof.producer_return_target_type
                        != proof.checker_producer.return_container_erase.target_type
                    || proof.producer_return_conversion_kind
                        != proof
                            .checker_producer
                            .return_container_erase
                            .conversion_kind
                    || proof.producer_return_conversion_kind
                        != crate::core::ir::CheckedConversionKind::ContainerErase
                    || proof.producer_owner != binding.producer_owner
                    || proof.caller_owner != binding.caller_owner
                    || proof.call_target.0 != binding.producer_owner.0
                    || proof.producer_values_instruction != binding.producer_values_instruction
                    || proof.producer_values_result != binding.producer_values_result
                    || proof.producer_snapshot_value != binding.producer_snapshot_value
                    || proof.producer_return_value != binding.producer_return_value
                    || proof.producer_binding_instruction != binding.producer_binding_instruction
                    || proof.producer_return_instruction != binding.producer_return_instruction
                    || proof.call_instruction != binding.call_instruction
                    || proof.caller_binding_instruction != binding.caller_binding_instruction
                    || proof.call_result != binding.call_result
                    || proof.caller_binding != binding.caller_binding
                    || proof.owner_drop_instruction != binding.owner_drop_instruction
                    || proof.owner_drop_value != binding.owner_drop_value
                    || proof.scalar_return_value != binding.scalar_return_value
                    || proof.scalar_return_node != binding.scalar_return_node
                    || proof.checker_caller.caller_return_literal != binding.scalar_return_literal
                    || proof.producer_result_type != binding.producer_result_type
                    || proof.call_result_type != binding.call_result_type
                    || proof.owner_type != binding.owner_type
                    || proof.producer_checker_snapshot_type
                        != binding.producer_checker_snapshot_type
                    || proof.producer_surface_return_type != binding.producer_surface_return_type
                    || proof.call_surface_result_type != binding.call_surface_result_type
                    || proof.producer_return_cast_node != binding.producer_return_cast_node
                    || proof.producer_return_source_node != binding.producer_return_source_node
                    || proof.producer_return_source_type != binding.producer_return_source_type
                    || proof.producer_return_target_type != binding.producer_return_target_type
                    || proof.producer_return_conversion_kind
                        != binding.producer_return_conversion_kind
                    || proof.scalar_return_type != binding.caller_return_type
                    || binding.producer_result_type != binding.call_result_type
                    || proof.len_instruction != binding.len_instruction
                    || proof.len_result != binding.len_result
                    || proof.len_binding_instruction != binding.len_binding_instruction
                    || proof.len_binding != binding.len_binding
                    || proof.len_receiver != binding.len_receiver
                    || proof.len_receiver_type != binding.len_receiver_type
            })
        {
            return Err(
                "Values producer-caller bytecode sites disagree with their exact typed route receipts or contain unsupported unpack sites".into(),
            );
        }

        for (proof, binding) in self.proofs.iter().zip(&self.bindings) {
            let expected_len_tail_clone = crate::core::mir::MirInstructionId::new(format!(
                "inst:clone:{}",
                proof.scalar_return_node.0
            ))
            .map_err(|error| {
                format!("Values producer-caller scalar tail identity is invalid: {error}")
            })?;
            match (
                proof.checker_caller.len_observation.as_ref(),
                binding.caller_return_move_instruction.as_ref(),
            ) {
                (Some(_), Some(instruction)) if instruction == &expected_len_tail_clone => {}
                (None, None) => {}
                (Some(_), _) => {
                    return Err(
                        "Values producer-caller Len tail is not the exact Checker-bound scalar Clone identity"
                            .into(),
                    );
                }
                (None, Some(_)) => {
                    return Err(
                        "Values producer-caller literal tail must not carry a Len scalar Clone identity"
                            .into(),
                    );
                }
            }

            let expected_literal_tail_const = crate::core::mir::MirInstructionId::new(format!(
                "inst:const:{}",
                proof.scalar_return_node.0
            ))
            .map_err(|error| {
                format!("Values producer-caller literal tail identity is invalid: {error}")
            })?;
            match (
                proof.checker_caller.len_observation.as_ref(),
                proof.checker_caller.caller_return_literal.as_ref(),
                binding.caller_return_literal_instruction.as_ref(),
                binding.caller_return_literal_pc,
            ) {
                (Some(_), None, None, None) => {}
                (
                    None,
                    Some(
                        crate::core::ir::ResolvedLiteral::Int(_)
                        | crate::core::ir::ResolvedLiteral::FloatBits(_)
                        | crate::core::ir::ResolvedLiteral::Bool(_),
                    ),
                    Some(instruction),
                    Some(_),
                ) if instruction == &expected_literal_tail_const => {}
                _ => {
                    return Err(
                        "Values producer-caller scalar literal tail is not the exact Checker-bound Const identity"
                            .into(),
                    );
                }
            }
        }

        for binding in &self.bindings {
            let producer = functions
                .get(binding.producer_function as usize)
                .ok_or_else(|| {
                    "Values producer function index is outside bytecode table".to_string()
                })?;
            let caller = functions
                .get(binding.caller_function as usize)
                .ok_or_else(|| {
                    "Values caller function index is outside bytecode table".to_string()
                })?;
            if producer.name != binding.producer_owner.0
                || caller.name != binding.caller_owner.0
                || binding.call_target != binding.producer_function
            {
                return Err(
                    "Values producer-caller bytecode function/callee identity mismatch".into(),
                );
            }
            let producer_values_pc = usize::try_from(binding.producer_values_pc)
                .map_err(|_| "Values producer opcode PC exceeds usize".to_string())?;
            let producer_return_pc = usize::try_from(binding.producer_return_pc)
                .map_err(|_| "Values producer return PC exceeds usize".to_string())?;
            let producer_binding_move_pc = usize::try_from(binding.producer_binding_move_pc)
                .map_err(|_| "Values producer snapshot Move PC exceeds usize".to_string())?;
            let producer_return_move_pc = usize::try_from(binding.producer_return_move_pc)
                .map_err(|_| "Values producer tail Move PC exceeds usize".to_string())?;
            let call_pc = usize::try_from(binding.call_pc)
                .map_err(|_| "Values caller call PC exceeds usize".to_string())?;
            let caller_binding_move_pc = usize::try_from(binding.caller_binding_move_pc)
                .map_err(|_| "Values caller binding Move PC exceeds usize".to_string())?;
            let owner_drop_pc = usize::try_from(binding.owner_drop_pc)
                .map_err(|_| "Values caller Drop PC exceeds usize".to_string())?;
            let caller_return_pc = usize::try_from(binding.caller_return_pc)
                .map_err(|_| "Values caller return PC exceeds usize".to_string())?;
            if !(producer_values_pc < producer_binding_move_pc
                && producer_binding_move_pc < producer_return_move_pc
                && producer_return_move_pc < producer_return_pc
                && call_pc < caller_binding_move_pc
                && caller_binding_move_pc < owner_drop_pc
                && owner_drop_pc < caller_return_pc)
            {
                return Err(
                    "Values producer-caller bytecode ownership sites are out of route order".into(),
                );
            }
            if !matches!(
                producer.code.get(producer_values_pc),
                Some(Op::MirOrdinaryMapOwnedAnyV2Values { rd, ra })
                    if (*rd, *ra) == (binding.producer_values_rd, binding.producer_values_ra)
            ) || !matches!(
                producer.code.get(producer_binding_move_pc),
                Some(Op::Move { rd, rs })
                    if (*rd, *rs) == (binding.producer_binding_move_rd, binding.producer_binding_move_rs)
            ) || !matches!(
                producer.code.get(producer_return_move_pc),
                Some(Op::Move { rd, rs })
                    if (*rd, *rs) == (binding.producer_return_move_rd, binding.producer_return_move_rs)
            ) || !matches!(
                producer.code.get(producer_return_pc),
                Some(Op::Ret { ra }) if *ra == binding.producer_return_ra
            ) || !matches!(
                caller.code.get(call_pc),
                Some(Op::CallMove { rd, func, argc: 0, .. })
                    if (*rd, *func) == (binding.call_rd, binding.call_target)
            ) || !matches!(
                caller.code.get(caller_binding_move_pc),
                Some(Op::Move { rd, rs })
                    if (*rd, *rs) == (binding.caller_binding_rd, binding.caller_binding_rs)
            ) || !matches!(
                caller.code.get(owner_drop_pc),
                Some(Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { ra })
                    if *ra == binding.owner_drop_ra
            ) || !matches!(
                caller.code.get(caller_return_pc),
                Some(Op::Ret { ra }) if *ra == binding.caller_return_ra
            ) {
                return Err(
                    "Values producer-caller route bytecode opcode/register sites disagree with their private Checker/MIR binding".into(),
                );
            }
            match (
                binding.scalar_return_literal.as_ref(),
                binding.caller_return_literal_instruction.as_ref(),
                binding.caller_return_literal_pc,
            ) {
                (Some(literal), Some(_), Some(literal_pc)) => {
                    let literal_pc = usize::try_from(literal_pc).map_err(|_| {
                        "Values caller scalar literal tail PC exceeds usize".to_string()
                    })?;
                    if !(owner_drop_pc < literal_pc && literal_pc < caller_return_pc)
                        || !values_producer_caller_literal_tail_matches(
                            caller,
                            literal_pc,
                            binding.caller_return_ra,
                            literal,
                        )
                    {
                        return Err(
                            "Values producer-caller scalar literal tail Const/LoadConst or return register disagrees with its Checker receipt".into(),
                        );
                    }
                }
                (None, None, None) => {}
                _ => {
                    return Err(
                        "Values producer-caller scalar literal tail proof and bytecode sites are partial"
                            .into(),
                    );
                }
            }
            match (
                binding.len_instruction.as_ref(),
                binding.len_result.as_ref(),
                binding.len_binding_instruction.as_ref(),
                binding.len_binding.as_ref(),
                binding.len_pc,
                binding.len_rd,
                binding.len_ra,
                binding.len_receiver.as_ref(),
                binding.len_receiver_type.as_ref(),
                binding.len_receiver_clone_instruction.as_ref(),
                binding.len_receiver_drop_instruction.as_ref(),
                binding.len_receiver_clone_pc,
                binding.len_receiver_clone_rd,
                binding.len_receiver_clone_ra,
                binding.len_receiver_drop_pc,
                binding.len_receiver_drop_ra,
                binding.len_binding_move_pc,
                binding.len_binding_move_rd,
                binding.len_binding_move_rs,
                binding.caller_return_move_instruction.as_ref(),
                binding.caller_return_move_pc,
                binding.caller_return_move_rd,
                binding.caller_return_move_rs,
            ) {
                (
                    Some(_),
                    Some(_),
                    Some(_),
                    Some(_),
                    Some(len_pc),
                    Some(len_rd),
                    Some(len_ra),
                    Some(len_receiver),
                    Some(len_receiver_type),
                    Some(_),
                    Some(_),
                    Some(len_receiver_clone_pc),
                    Some(len_receiver_clone_rd),
                    Some(len_receiver_clone_ra),
                    Some(len_receiver_drop_pc),
                    Some(len_receiver_drop_ra),
                    Some(len_binding_move_pc),
                    Some(len_binding_move_rd),
                    Some(len_binding_move_rs),
                    Some(_),
                    Some(caller_return_move_pc),
                    Some(caller_return_move_rd),
                    Some(caller_return_move_rs),
                ) => {
                    let len_pc = usize::try_from(len_pc)
                        .map_err(|_| "Values caller List.len PC exceeds usize".to_string())?;
                    let len_binding_move_pc =
                        usize::try_from(len_binding_move_pc).map_err(|_| {
                            "Values caller len binding Move PC exceeds usize".to_string()
                        })?;
                    let caller_return_move_pc =
                        usize::try_from(caller_return_move_pc).map_err(|_| {
                            "Values caller scalar tail Move PC exceeds usize".to_string()
                        })?;
                    let len_receiver_clone_pc = usize::try_from(len_receiver_clone_pc)
                        .map_err(|_| "Values caller len receiver Clone PC exceeds usize")?;
                    let len_receiver_drop_pc = usize::try_from(len_receiver_drop_pc)
                        .map_err(|_| "Values caller len receiver Drop PC exceeds usize")?;
                    if len_receiver.as_str().trim().is_empty()
                        || !(caller_binding_move_pc < len_receiver_clone_pc
                            && len_receiver_clone_pc < len_pc
                            && len_pc < len_receiver_drop_pc
                            && len_receiver_drop_pc < len_binding_move_pc
                            && len_binding_move_pc < owner_drop_pc
                            && owner_drop_pc < caller_return_move_pc
                            && caller_return_move_pc < caller_return_pc)
                    {
                        return Err(
                            "Values producer-caller optional len sites are out of route order"
                                .into(),
                        );
                    }
                    let Some(Op::MirListLen {
                        rd,
                        ra,
                        contract: Some(contract),
                    }) = caller.code.get(len_pc)
                    else {
                        return Err(
                            "Values producer-caller len receipt has no canonical List.len opcode"
                                .into(),
                        );
                    };
                    let Some(ConstValue::ListOperation(shape)) =
                        caller.constants.get(*contract as usize)
                    else {
                        return Err(
                            "Values producer-caller len opcode has no ListOperation contract"
                                .into(),
                        );
                    };
                    if (*rd, *ra) != (len_rd, len_ra)
                        || len_receiver_type != &binding.owner_type
                        || len_ra != len_receiver_clone_rd
                        || len_receiver_clone_ra != binding.caller_binding_rd
                        || len_receiver_drop_ra != len_receiver_clone_rd
                        || !matches!(
                            caller.code.get(len_receiver_clone_pc),
                            Some(Op::MirOrdinaryMapOwnedAnyV2ValuesClone { rd, ra })
                                if (*rd, *ra) == (len_receiver_clone_rd, len_receiver_clone_ra)
                        )
                        || !matches!(
                            caller.code.get(len_receiver_drop_pc),
                            Some(Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { ra })
                                if *ra == len_receiver_drop_ra
                        )
                        || shape.operation != crate::core::mir::MirListOperation::Len
                        || shape.list_ty != binding.owner_type
                        || shape.result_ty != binding.caller_return_type
                        || shape.argument_ty.is_some()
                        || !matches!(
                            caller.code.get(len_binding_move_pc),
                            Some(Op::Mov { rd, rs })
                                if (*rd, *rs) == (len_binding_move_rd, len_binding_move_rs)
                        )
                        || caller_return_move_rd != binding.caller_return_ra
                        || caller_return_move_rs != len_binding_move_rd
                        || !matches!(
                            caller.code.get(caller_return_move_pc),
                            Some(Op::Mov { rd, rs })
                                if (*rd, *rs) == (caller_return_move_rd, caller_return_move_rs)
                        )
                    {
                        return Err(
                            "Values producer-caller optional len op, result type, or scalar Move disagrees with its typed receipt".into(),
                        );
                    }
                }
                (
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ) => {}
                _ => {
                    return Err(
                        "Values producer-caller optional len proof and bytecode sites are partial"
                            .into(),
                    );
                }
            }
            if producer
                .code
                .iter()
                .filter(|op| matches!(op, Op::Ret { .. }))
                .count()
                != 1
                || caller
                    .code
                    .iter()
                    .filter(|op| matches!(op, Op::Ret { .. }))
                    .count()
                    != 1
            {
                return Err(
                    "Values producer-caller route requires unique producer and scalar tail returns"
                        .into(),
                );
            }
        }
        if functions.iter().any(|function| {
            function
                .code
                .iter()
                .any(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }))
        }) {
            return Err(
                "Values producer-caller MVP does not authorize cross-function indexed unpack"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || values_producer_caller_bytecode_snapshot(entry, functions, &self.bindings)
                != self.expected_program
        {
            return Err(
                "Values producer-caller bytecode disagrees with its exact MIR route snapshot"
                    .into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.receipt.profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE.into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_manifest_for_test_only(&mut self) {
        self.receipt.mir_digest = "not-the-captured-mir-digest".into();
    }

    #[cfg(test)]
    pub(crate) fn corrupt_call_target_for_test_only(&mut self) {
        if let Some(binding) = self.bindings.first_mut() {
            binding.call_target = binding.caller_function;
        }
    }

    #[cfg(test)]
    pub(crate) fn corrupt_owner_drop_register_for_test_only(&mut self) {
        if let Some(binding) = self.bindings.first_mut() {
            binding.owner_drop_ra = Reg::MAX;
        }
    }

    #[cfg(test)]
    pub(crate) fn corrupt_len_tail_instruction_for_test_only(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> bool {
        let Some(binding) = self
            .bindings
            .iter_mut()
            .find(|binding| binding.len_instruction.is_some())
        else {
            return false;
        };
        let Ok(forged_instruction) =
            crate::core::mir::MirInstructionId::new("inst:clone:forged-values-tail")
        else {
            return false;
        };
        binding.caller_return_move_instruction = Some(forged_instruction);
        self.expected_program =
            values_producer_caller_bytecode_snapshot(entry, functions, &self.bindings);
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_literal_tail_value_for_test_only(
        &mut self,
        entry: FuncIdx,
        functions: &mut [FunctionProto],
    ) -> bool {
        let Some((caller_function, literal_pc, literal)) =
            self.bindings.iter().find_map(|binding| {
                Some((
                    binding.caller_function,
                    binding.caller_return_literal_pc?,
                    binding.scalar_return_literal.clone()?,
                ))
            })
        else {
            return false;
        };
        let Ok(literal_pc) = usize::try_from(literal_pc) else {
            return false;
        };
        let Some(caller) = functions.get_mut(caller_function as usize) else {
            return false;
        };

        match literal {
            crate::core::ir::ResolvedLiteral::Int(expected) => {
                let Some(Op::LoadConst { idx, .. }) = caller.code.get(literal_pc).copied() else {
                    return false;
                };
                let Some(ConstValue::Int(actual)) = caller.constants.get_mut(idx as usize) else {
                    return false;
                };
                if *actual != expected {
                    return false;
                }
                *actual = expected.wrapping_add(1);
            }
            crate::core::ir::ResolvedLiteral::FloatBits(expected) => {
                let Some(Op::LoadConst { idx, .. }) = caller.code.get(literal_pc).copied() else {
                    return false;
                };
                let Some(ConstValue::Float(actual)) = caller.constants.get_mut(idx as usize) else {
                    return false;
                };
                if actual.to_bits() != expected {
                    return false;
                }
                *actual = f64::from_bits(expected ^ 1);
            }
            crate::core::ir::ResolvedLiteral::Bool(true) => {
                let Some(Op::LoadTrue { rd }) = caller.code.get(literal_pc).copied() else {
                    return false;
                };
                caller.code[literal_pc] = Op::LoadFalse { rd };
            }
            crate::core::ir::ResolvedLiteral::Bool(false) => {
                let Some(Op::LoadFalse { rd }) = caller.code.get(literal_pc).copied() else {
                    return false;
                };
                caller.code[literal_pc] = Op::LoadTrue { rd };
            }
            crate::core::ir::ResolvedLiteral::String(_)
            | crate::core::ir::ResolvedLiteral::Unit => return false,
        }

        self.expected_program =
            values_producer_caller_bytecode_snapshot(entry, functions, &self.bindings);
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_producer_source_type_for_test_only(&mut self) {
        if let Some(proof) = self.proofs.first_mut() {
            proof.producer_return_source_type = proof.producer_surface_return_type.clone();
        }
    }

    #[cfg(test)]
    pub(crate) fn corrupt_producer_target_type_for_test_only(&mut self) {
        if let Some(proof) = self.proofs.first_mut() {
            proof.producer_return_target_type = proof.owner_type.clone();
        }
    }
}

impl CanonicalValuesCrosscallStringCallerRouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt>,
        bindings: Vec<CanonicalValuesCrosscallStringCallerBytecodeBinding>,
        payload_selection_digest: Option<&str>,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Values cross-call String-caller anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if proofs.len() != 1 || bindings.len() != 1 {
            return Err(
                "Values cross-call String-caller route must bind exactly one proof/site pair"
                    .into(),
            );
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Values cross-call String-caller manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "Values cross-call String-caller route receipt changed during manifest replay"
                    .into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Values cross-call String-caller route manifest render failed: {message}")
        })?;
        let expected_bindings = format!("{bindings:?}");
        let expected_program = ordinary_map_owned_any_v2_program_snapshot(entry, functions);
        let anchor = Self {
            expected_profile,
            receipt: receipt.clone(),
            proofs,
            bindings,
            expected_manifest,
            expected_bindings,
            expected_entry: entry,
            expected_program,
        };
        anchor.validate(entry, functions, payload_selection_digest)?;
        Ok(anchor)
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        payload_selection_digest: Option<&str>,
    ) -> Result<(), String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE;
        if self.expected_profile != expected_profile || self.receipt.profile != expected_profile {
            return Err(
                "Values cross-call String-caller route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!(
                "Values cross-call String-caller manifest replay failed at VM boundary: {message}"
            )
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "Values cross-call String-caller route receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if self.proofs.len() != 1 || self.bindings.len() != 1 {
            return Err(
                "Values cross-call String-caller private proof/site table is empty or ambiguous"
                    .into(),
            );
        }
        if format!("{:?}", self.bindings) != self.expected_bindings {
            return Err(
                "Values cross-call String-caller bytecode bindings disagree with their immutable site snapshot"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || ordinary_map_owned_any_v2_program_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "Values cross-call String-caller bytecode disagrees with its exact MIR route snapshot"
                    .into(),
            );
        }
        let proof = &self.proofs[0];
        let binding = &self.bindings[0];
        proof.validate().map_err(|message| {
            format!(
                "Values cross-call String-caller MIR proof is invalid at VM boundary: {message}"
            )
        })?;
        if proof.mir_digest != self.receipt.mir_digest
            || proof.type_desc_digest != self.receipt.type_desc_digest
            || payload_selection_digest != Some(proof.payload_selection_digest.as_str())
        {
            return Err(
                "Values cross-call String-caller proof disagrees with its route or payload-selection digest"
                    .into(),
            );
        }
        binding.validate(proof, functions, payload_selection_digest)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_proof_digest_for_test_only(&mut self) {
        if let Some(binding) = self.bindings.first_mut() {
            binding.proof_digest = "not-the-captured-proof-digest".into();
        }
    }

    #[cfg(test)]
    pub(crate) fn corrupt_string_drop_for_test_only(&mut self) {
        if let Some(binding) = self.bindings.first_mut() {
            if let Some(site) = binding.sites.get_mut(19) {
                site.op = Op::Drop { ra: Reg::MAX };
            }
        }
    }
}

impl CanonicalValuesCrosscallStringCallerBytecodeBinding {
    pub(crate) fn validate(
        &self,
        proof: &crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt,
        functions: &[FunctionProto],
        payload_selection_digest: Option<&str>,
    ) -> Result<(), String> {
        const SITE_COUNT: usize = 22;
        let proof_digest = blake3::hash(format!("{proof:?}").as_bytes())
            .to_hex()
            .to_string();
        if self.producer_owner != proof.producer_owner
            || self.caller_owner != proof.caller_owner
            || self.call_target != proof.call_target
            || self.producer_owner == self.caller_owner
            || self.call_target != self.producer_owner
            || self.proof_digest != proof_digest
            || !is_canonical_digest(&self.proof_digest)
            || self.mir_digest != proof.mir_digest
            || self.type_desc_digest != proof.type_desc_digest
            || self.payload_selection_digest != proof.payload_selection_digest
            || self.string_type_desc_digest != proof.string_type_desc_digest
            || self.payload_type_desc_digest != proof.payload_type_desc_digest
            || !is_canonical_digest(&self.mir_digest)
            || !is_canonical_digest(&self.type_desc_digest)
            || !is_canonical_digest(&self.payload_selection_digest)
            || !is_canonical_digest(&self.string_type_desc_digest)
            || !is_canonical_digest(&self.payload_type_desc_digest)
            || payload_selection_digest != Some(self.payload_selection_digest.as_str())
            || self
                .producer_payload_initializer_instruction
                .as_str()
                .trim()
                .is_empty()
            || !self
                .producer_payload_initializer_instruction
                .as_str()
                .starts_with("inst:const:")
            || self.sites.len() != SITE_COUNT
        {
            return Err(
                "Values cross-call String-caller bytecode binding disagrees with its producer/caller owner or digest proof"
                    .into(),
            );
        }
        let producer_key_const = crosscall_const_instruction(&proof.producer_key_literal)?;
        let caller_index_const = crate::core::mir::MirInstructionId::new(format!(
            "inst:const:{}",
            proof.checker_caller.index_expression.0
        ))
        .map_err(|error| format!("Values caller index Const identity is invalid: {error}"))?;
        let caller_expected_const = crosscall_const_instruction(&proof.caller_expected_literal)?;
        let expected_instructions = [
            proof.producer_map_new_instruction.clone(),
            proof.producer_map_set_instruction.clone(),
            self.producer_payload_initializer_instruction.clone(),
            proof.producer_payload_binding_instruction.clone(),
            producer_key_const,
            proof.producer_values_instruction.clone(),
            proof.producer_snapshot_binding_instruction.clone(),
            proof.producer_return_instruction.clone(),
            proof.producer_root_drop_instructions[0].clone(),
            proof.producer_root_drop_instructions[1].clone(),
            proof.producer_payload_drop_instruction.clone(),
            proof.caller_call_instruction.clone(),
            proof.caller_snapshot_binding_instruction.clone(),
            caller_index_const.clone(),
            proof.caller_unpack_instruction.clone(),
            proof.caller_unpack_binding_instruction.clone(),
            caller_expected_const.clone(),
            proof.caller_comparison_instruction.clone(),
            proof.caller_boolean_binding_instruction.clone(),
            proof.caller_string_drop_instruction.clone(),
            proof.caller_snapshot_drop_instruction.clone(),
            proof.caller_return_instruction.clone(),
        ];
        let producer_function = usize::try_from(self.producer_function)
            .map_err(|_| "Values producer function index exceeds usize".to_string())?;
        let caller_function = usize::try_from(self.caller_function)
            .map_err(|_| "Values caller function index exceeds usize".to_string())?;
        let producer = functions.get(producer_function).ok_or_else(|| {
            "Values cross-call producer function is outside bytecode table".to_string()
        })?;
        let caller = functions.get(caller_function).ok_or_else(|| {
            "Values cross-call caller function is outside bytecode table".to_string()
        })?;
        if producer.name != self.producer_owner.0
            || caller.name != self.caller_owner.0
            || self.producer_function == self.caller_function
        {
            return Err(
                "Values cross-call bytecode function indices do not name distinct proof owners"
                    .into(),
            );
        }
        let mut occupied_sites = std::collections::BTreeSet::new();
        for (index, (site, expected_instruction)) in self
            .sites
            .iter()
            .zip(expected_instructions.iter())
            .enumerate()
        {
            let (expected_owner, expected_function) = if index < 11 {
                (&self.producer_owner, self.producer_function)
            } else {
                (&self.caller_owner, self.caller_function)
            };
            let pc = usize::try_from(site.pc)
                .map_err(|_| "Values cross-call site PC exceeds usize".to_string())?;
            let function = functions.get(site.function as usize).ok_or_else(|| {
                "Values cross-call site function is outside bytecode table".to_string()
            })?;
            if &site.owner != expected_owner
                || site.function != expected_function
                || &site.instruction != expected_instruction
                || !occupied_sites.insert((site.function, site.pc))
                || function.code.get(pc) != Some(&site.op)
            {
                return Err(format!(
                    "Values cross-call site {index} is not the exact emitted instruction for its producer/caller owner"
                ));
            }
        }

        let op = |index: usize| &self.sites[index].op;
        let producer_new = match op(0) {
            Op::MirOrdinaryMapOwnedAnyV2New { rd } => *rd,
            _ => return Err("Values producer Map New bytecode site changed".into()),
        };
        let (producer_set, producer_set_root, producer_key, producer_set_payload) = match op(1) {
            Op::MirOrdinaryMapOwnedAnyV2Set { rd, ra, key, value } => (*rd, *ra, *key, *value),
            _ => return Err("Values producer Map Set bytecode site changed".into()),
        };
        let payload_source = match op(2) {
            Op::LoadConst { rd, idx } if matches!(producer.constants.get(*idx as usize), Some(ConstValue::Str(value)) if value == &proof.checker_caller.expected_string) => {
                *rd
            }
            _ => {
                return Err(
                    "Values producer payload initializer is not the Checker String literal".into(),
                )
            }
        };
        let producer_payload = match op(3) {
            Op::Move { rd, rs } if *rs == payload_source => *rd,
            _ => return Err("Values producer payload binding is not its exact owned Move".into()),
        };
        let exact_payload_clones = producer
            .code
            .iter()
            .filter(|candidate| {
                matches!(
                    candidate,
                    Op::Clone { rd, rs }
                        if *rd == producer_set_payload && *rs == producer_payload
                )
            })
            .count();
        let producer_key_reg = match op(4) {
            Op::LoadConst { rd, idx }
                if matches!(
                    producer.constants.get(*idx as usize),
                    Some(ConstValue::Str(_))
                ) =>
            {
                *rd
            }
            _ => return Err("Values producer Map Set key is not its literal String Const".into()),
        };
        let (values_result, values_root) = match op(5) {
            Op::MirOrdinaryMapOwnedAnyV2Values { rd, ra } => (*rd, *ra),
            _ => return Err("Values producer snapshot bytecode site changed".into()),
        };
        let (producer_snapshot, producer_values_source) = match op(6) {
            Op::Move { rd, rs } => (*rd, *rs),
            _ => return Err("Values producer snapshot binding is not its exact owned Move".into()),
        };
        let (producer_return, producer_return_source) = match op(7) {
            Op::Move { rd, rs } => (*rd, *rs),
            _ => return Err("Values producer tail return is not its exact owned Move".into()),
        };
        let drop_a = match op(8) {
            Op::MirOrdinaryMapOwnedAnyV2Drop { ra } => *ra,
            _ => return Err("Values producer first Map-root Drop site changed".into()),
        };
        let drop_b = match op(9) {
            Op::MirOrdinaryMapOwnedAnyV2Drop { ra } => *ra,
            _ => return Err("Values producer second Map-root Drop site changed".into()),
        };
        if ![drop_a, drop_b].contains(&producer_new)
            || ![drop_a, drop_b].contains(&producer_set)
            || drop_a == drop_b
            || !matches!(op(10), Op::Drop { ra } if *ra == producer_payload)
        {
            return Err(
                "Values producer root/payload Drop sites do not settle the exact owners".into(),
            );
        }
        if producer_set_root != producer_new
            || producer_set_payload == producer_payload
            || exact_payload_clones != 1
            || producer_key != producer_key_reg
            || values_root != producer_set
            || producer_values_source != values_result
            || producer_return_source != producer_snapshot
        {
            return Err(
                "Values producer Map/Values/return register chain or payload clone changed".into(),
            );
        }

        let (call_result, call_target) = match op(11) {
            Op::CallMove {
                rd, func, argc: 0, ..
            } => (*rd, *func),
            _ => {
                return Err(
                    "Values caller does not have the exact direct zero-argument CallMove".into(),
                )
            }
        };
        let (caller_snapshot, snapshot_source) = match op(12) {
            Op::Move { rd, rs } => (*rd, *rs),
            _ => return Err("Values caller snapshot binding is not its exact owned Move".into()),
        };
        let caller_index = match op(13) {
            Op::LoadConst { rd, idx }
                if matches!(
                    caller.constants.get(*idx as usize),
                    Some(ConstValue::Int(0))
                ) =>
            {
                *rd
            }
            _ => return Err("Values caller index is not the exact literal zero".into()),
        };
        let (unpack_result, unpack_snapshot, unpack_index) = match op(14) {
            Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { rd, ra, index } => (*rd, *ra, *index),
            _ => return Err("Values caller indexed String unpack bytecode site changed".into()),
        };
        let (caller_string, unpack_binding_source) = match op(15) {
            Op::Move { rd, rs } => (*rd, *rs),
            _ => return Err("Values caller String binding is not its exact owned Move".into()),
        };
        let expected_string = match op(16) {
            Op::LoadConst { rd, idx } if matches!(caller.constants.get(*idx as usize), Some(ConstValue::Str(value)) if value == &proof.checker_caller.expected_string) => {
                *rd
            }
            _ => {
                return Err(
                    "Values caller comparison peer is not the Checker String literal".into(),
                )
            }
        };
        let (comparison_result, comparison_left, comparison_right) = match op(17) {
            Op::Eq { rd, ra, rb } => (*rd, *ra, *rb),
            _ => return Err("Values caller String comparison is not exact equality".into()),
        };
        let (boolean_binding, boolean_source) = match op(18) {
            Op::Mov { rd, rs } => (*rd, *rs),
            _ => return Err("Values caller Bool binding is not its exact copy".into()),
        };
        let string_drop = match op(19) {
            Op::Drop { ra } => *ra,
            _ => return Err("Values caller selected String Drop site changed".into()),
        };
        let snapshot_drop = match op(20) {
            Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { ra } => *ra,
            _ => return Err("Values caller snapshot Drop site changed".into()),
        };
        let (caller_return, caller_return_source) = match op(21) {
            Op::Mov { rd, rs } => (*rd, *rs),
            _ => return Err("Values caller Boolean tail copy changed".into()),
        };
        let producer_return_pcs = producer
            .code
            .iter()
            .filter(|candidate| matches!(candidate, Op::Ret { ra } if *ra == producer_return))
            .count();
        let caller_return_pcs = caller
            .code
            .iter()
            .filter(|candidate| matches!(candidate, Op::Ret { ra } if *ra == caller_return))
            .count();
        if producer_return_pcs != 1
            || caller_return_pcs != 1
            || call_target != self.producer_function
            || snapshot_source != call_result
            || unpack_snapshot != caller_snapshot
            || unpack_index != caller_index
            || unpack_binding_source != unpack_result
            || comparison_left != caller_string
            || comparison_right != expected_string
            || boolean_source != comparison_result
            || string_drop != caller_string
            || snapshot_drop != caller_snapshot
            || caller_return_source != boolean_binding
        {
            return Err(
                "Values caller Call/unpack/compare/drop/return bytecode register chain changed"
                    .into(),
            );
        }
        Ok(())
    }
}

impl CanonicalValuesCrosscallStringCallerIndex1RouteBytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt>,
        bindings: Vec<CanonicalValuesCrosscallStringCallerIndex1BytecodeBinding>,
        payload_selection_digest: Option<&str>,
    ) -> Result<Self, String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE;
        if receipt.profile != expected_profile {
            return Err(format!(
                "Values cross-call String-caller Index1 anchor received route profile '{}'",
                receipt.profile
            ));
        }
        if proofs.len() != 1 || bindings.len() != 1 {
            return Err(
                "Values cross-call String-caller Index1 route must bind exactly one proof/site pair"
                    .into(),
            );
        }
        let replayed = receipt.manifest_round_trip().map_err(|message| {
            format!("Values cross-call String-caller Index1 manifest replay failed: {message}")
        })?;
        if replayed != *receipt {
            return Err(
                "Values cross-call String-caller Index1 route receipt changed during manifest replay"
                    .into(),
            );
        }
        let expected_manifest = receipt.manifest_text().map_err(|message| {
            format!("Values cross-call String-caller Index1 manifest render failed: {message}")
        })?;
        let expected_bindings = format!("{bindings:?}");
        let expected_program = ordinary_map_owned_any_v2_program_snapshot(entry, functions);
        let anchor = Self {
            expected_profile,
            receipt: receipt.clone(),
            proofs,
            bindings,
            expected_manifest,
            expected_bindings,
            expected_entry: entry,
            expected_program,
        };
        anchor.validate(entry, functions, payload_selection_digest)?;
        Ok(anchor)
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        payload_selection_digest: Option<&str>,
    ) -> Result<(), String> {
        let expected_profile =
            crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE;
        if self.expected_profile != expected_profile || self.receipt.profile != expected_profile {
            return Err(
                "Values cross-call String-caller Index1 route profile disagrees with its private anchor"
                    .into(),
            );
        }
        let replayed = self.receipt.manifest_round_trip().map_err(|message| {
            format!("Values cross-call String-caller Index1 manifest replay failed at VM boundary: {message}")
        })?;
        if replayed != self.receipt
            || self.receipt.manifest_text().ok().as_deref() != Some(self.expected_manifest.as_str())
        {
            return Err(
                "Values cross-call String-caller Index1 receipt disagrees with its immutable manifest anchor"
                    .into(),
            );
        }
        if self.proofs.len() != 1 || self.bindings.len() != 1 {
            return Err(
                "Values cross-call String-caller Index1 private proof/site table is empty or ambiguous"
                    .into(),
            );
        }
        if format!("{:?}", self.bindings) != self.expected_bindings {
            return Err(
                "Values cross-call String-caller Index1 bytecode bindings disagree with their immutable site snapshot"
                    .into(),
            );
        }
        if entry != self.expected_entry
            || ordinary_map_owned_any_v2_program_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "Values cross-call String-caller Index1 bytecode disagrees with its exact MIR route snapshot"
                    .into(),
            );
        }
        let proof = &self.proofs[0];
        let binding = &self.bindings[0];
        proof.validate().map_err(|message| {
            format!("Values cross-call String-caller Index1 MIR proof is invalid at VM boundary: {message}")
        })?;
        if proof.mir_digest != self.receipt.mir_digest
            || proof.type_desc_digest != self.receipt.type_desc_digest
            || payload_selection_digest != Some(proof.payload_selection_digest.as_str())
        {
            return Err(
                "Values cross-call String-caller Index1 proof disagrees with its route or payload-selection digest"
                    .into(),
            );
        }
        binding.validate(proof, functions, payload_selection_digest)
    }
}

impl CanonicalValuesCrosscallStringCallerIndex1BytecodeBinding {
    pub(crate) fn validate(
        &self,
        proof: &crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt,
        functions: &[FunctionProto],
        payload_selection_digest: Option<&str>,
    ) -> Result<(), String> {
        use crate::core::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion as Profile;

        const PRODUCER_SITE_COUNT: usize = 21;
        const SITE_COUNT: usize = 32;
        let proof_digest = blake3::hash(format!("{proof:?}").as_bytes())
            .to_hex()
            .to_string();
        if proof.schema
            != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_SCHEMA
            || proof.profile
                != crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE
            || proof.checker_caller.profile_version != Profile::V2
            || self.producer_owner != proof.producer_owner
            || self.caller_owner != proof.caller_owner
            || self.call_target != proof.call_target
            || self.producer_owner == self.caller_owner
            || self.call_target != self.producer_owner
            || self.proof_digest != proof_digest
            || !is_canonical_digest(&self.proof_digest)
            || self.mir_digest != proof.mir_digest
            || self.type_desc_digest != proof.type_desc_digest
            || self.payload_selection_digest != proof.payload_selection_digest
            || self.string_type_desc_digest != proof.string_type_desc_digest
            || self.payload_type_desc_digest != proof.payload_type_desc_digest
            || !is_canonical_digest(&self.mir_digest)
            || !is_canonical_digest(&self.type_desc_digest)
            || !is_canonical_digest(&self.payload_selection_digest)
            || !is_canonical_digest(&self.string_type_desc_digest)
            || !is_canonical_digest(&self.payload_type_desc_digest)
            || payload_selection_digest != Some(self.payload_selection_digest.as_str())
            || self.producer_payload_initializer_instructions.len() != 2
            || self.producer_payload_initializer_instructions.iter().any(|id| {
                id.as_str().trim().is_empty() || !id.as_str().starts_with("inst:const:")
            })
            || self.producer_set_temporary_drops.len() != 2
            || self.sites.len() != SITE_COUNT
        {
            return Err(
                "Values cross-call String-caller Index1 bytecode binding disagrees with its V2 owners, site count, or digest proof"
                    .into(),
            );
        }
        proof.validate()?;
        let producer_index = usize::try_from(self.producer_function)
            .map_err(|_| "Values Index1 producer function index exceeds usize")?;
        let caller_index = usize::try_from(self.caller_function)
            .map_err(|_| "Values Index1 caller function index exceeds usize")?;
        let producer = functions.get(producer_index).ok_or_else(|| {
            "Values cross-call Index1 producer function is outside bytecode table".to_string()
        })?;
        let caller = functions.get(caller_index).ok_or_else(|| {
            "Values cross-call Index1 caller function is outside bytecode table".to_string()
        })?;
        if producer.name != self.producer_owner.0
            || caller.name != self.caller_owner.0
            || self.producer_function == self.caller_function
        {
            return Err(
                "Values cross-call Index1 bytecode function indices do not name distinct proof owners"
                    .into(),
            );
        }

        let caller_index_const = crate::core::mir::MirInstructionId::new(format!(
            "inst:const:{}",
            proof.checker_caller.index_expression.0
        ))
        .map_err(|error| format!("Values Index1 caller Const identity is invalid: {error}"))?;
        let caller_expected_const = crosscall_const_instruction(&proof.caller_expected_literal)?;
        let mut producer_ids = std::collections::BTreeSet::new();
        let mut expected_ids = std::collections::BTreeSet::new();
        let mut insert_id = |owner: &crate::core::NodeId,
                             id: crate::core::mir::MirInstructionId|
         -> Result<(), String> {
            if id.as_str().trim().is_empty() || !expected_ids.insert((owner.clone(), id)) {
                return Err(
                    "Values cross-call Index1 receipt repeats or empties an exact MIR site".into(),
                );
            }
            Ok(())
        };
        insert_id(
            &proof.producer_owner,
            proof.producer_map_new_instruction.clone(),
        )?;
        producer_ids.insert(proof.producer_map_new_instruction.clone());
        for (entry, initializer) in proof
            .producer_set_entries
            .iter()
            .zip(&self.producer_payload_initializer_instructions)
        {
            let key_const = crosscall_const_instruction(&entry.key_value)?;
            for id in [
                entry.set_instruction.clone(),
                initializer.clone(),
                entry.payload_binding_instruction.clone(),
                key_const,
                entry.payload_drop_instruction.clone(),
            ] {
                insert_id(&proof.producer_owner, id.clone())?;
                producer_ids.insert(id);
            }
            let temporary_drops = self
                .producer_set_temporary_drops
                .iter()
                .filter(|drops| drops.set_instruction == entry.set_instruction)
                .collect::<Vec<_>>();
            let [temporary_drops] = temporary_drops.as_slice() else {
                return Err(
                    "Values Index1 Set is missing its exact key/payload temporary Drop pair".into(),
                );
            };
            if temporary_drops.key_value != entry.key_value
                || entry.key_temporary_drop_instruction.as_ref()
                    != Some(&temporary_drops.key_drop_instruction)
                || entry.payload_clone_value.as_ref() != Some(&temporary_drops.payload_value)
                || entry.payload_clone_drop_instruction.as_ref()
                    != Some(&temporary_drops.payload_drop_instruction)
                || !temporary_drops
                    .key_drop_instruction
                    .as_str()
                    .starts_with("inst:drop.ordinary-map-key:")
                || !temporary_drops
                    .payload_drop_instruction
                    .as_str()
                    .starts_with("inst:drop.ordinary-map-value:")
            {
                return Err(
                    "Values Index1 Set temporary Drops are not the exact key/value cleanup sites"
                        .into(),
                );
            }
            for id in [
                temporary_drops.key_drop_instruction.clone(),
                temporary_drops.payload_drop_instruction.clone(),
            ] {
                insert_id(&proof.producer_owner, id.clone())?;
                producer_ids.insert(id);
            }
        }
        for id in [
            proof.producer_values_instruction.clone(),
            proof.producer_snapshot_binding_instruction.clone(),
            proof.producer_return_instruction.clone(),
        ]
        .into_iter()
        .chain(proof.producer_root_drop_instructions.iter().cloned())
        {
            insert_id(&proof.producer_owner, id.clone())?;
            producer_ids.insert(id);
        }
        for id in [
            proof.caller_call_instruction.clone(),
            proof.caller_snapshot_binding_instruction.clone(),
            caller_index_const.clone(),
            proof.caller_unpack_instruction.clone(),
            proof.caller_unpack_binding_instruction.clone(),
            caller_expected_const.clone(),
            proof.caller_comparison_instruction.clone(),
            proof.caller_boolean_binding_instruction.clone(),
            proof.caller_string_drop_instruction.clone(),
            proof.caller_snapshot_drop_instruction.clone(),
            proof.caller_return_instruction.clone(),
        ] {
            insert_id(&proof.caller_owner, id)?;
        }
        if producer_ids.len() != PRODUCER_SITE_COUNT || expected_ids.len() != SITE_COUNT {
            return Err(
                "Values cross-call Index1 proof does not describe two Set/key/payload cleanup chains and three root Drops"
                    .into(),
            );
        }

        let mut seen = std::collections::BTreeSet::new();
        let mut sites_by_instruction = std::collections::BTreeMap::new();
        for site in &self.sites {
            let expected_function = if site.owner == self.producer_owner {
                self.producer_function
            } else if site.owner == self.caller_owner {
                self.caller_function
            } else {
                return Err(
                    "Values cross-call Index1 site belongs to an unrelated function owner".into(),
                );
            };
            let pc = usize::try_from(site.pc)
                .map_err(|_| "Values cross-call Index1 site PC exceeds usize")?;
            let function = functions.get(site.function as usize).ok_or_else(|| {
                "Values cross-call Index1 site function is outside bytecode table".to_string()
            })?;
            if site.function != expected_function
                || function.code.get(pc) != Some(&site.op)
                || !expected_ids.contains(&(site.owner.clone(), site.instruction.clone()))
                || !seen.insert((site.function, site.pc))
                || sites_by_instruction
                    .insert(site.instruction.clone(), site)
                    .is_some()
            {
                return Err(
                    "Values cross-call Index1 site is not a unique exact emitted instruction for its proof owner"
                        .into(),
                );
            }
        }
        if seen.len() != SITE_COUNT
            || sites_by_instruction.len() != SITE_COUNT
            || expected_ids.iter().any(|(owner, id)| {
                !self
                    .sites
                    .iter()
                    .any(|site| site.owner == *owner && site.instruction == *id)
            })
        {
            return Err(
                "Values cross-call Index1 site table does not cover its exact 32 producer/caller instructions"
                    .into(),
            );
        }
        let site = |id: &crate::core::mir::MirInstructionId| sites_by_instruction.get(id).copied();

        let producer_new_reg = match &site(&proof.producer_map_new_instruction)
            .ok_or_else(|| "Values Index1 producer New site is absent".to_string())?
            .op
        {
            Op::MirOrdinaryMapOwnedAnyV2New { rd } => *rd,
            _ => return Err("Values Index1 producer New opcode changed".into()),
        };
        if proof.producer_set_entries.len() != 2
            || proof.producer_root_drop_instructions.len() != 3
            || proof.producer_payload_drop_instructions.len() != 2
            || proof.producer_payload_drop_place_indices.len() != 2
            || proof.producer_root_drop_place_indices.len() != 3
            || self.producer_payload_initializer_instructions.len() != 2
            || proof
                .producer_set_entries
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>()
                != ["a", "b"]
        {
            return Err(
                "Values Index1 proof must bind two sorted Set payloads and three Map-root Drops"
                    .into(),
            );
        }

        let mut current_root = proof.producer_initial_root.clone();
        let mut current_root_reg = producer_new_reg;
        let mut root_regs = vec![producer_new_reg];
        for _ in 0..2 {
            let matching = proof
                .producer_set_entries
                .iter()
                .filter(|entry| entry.source_root == current_root)
                .collect::<Vec<_>>();
            let [entry] = matching.as_slice() else {
                return Err(
                    "Values Index1 Set entries do not form the exact three-root transfer chain"
                        .into(),
                );
            };
            let initializer = self
                .producer_payload_initializer_instructions
                .iter()
                .zip(&proof.producer_set_entries)
                .find_map(|(initializer, candidate)| (candidate == *entry).then_some(initializer))
                .ok_or_else(|| "Values Index1 payload initializer binding is absent".to_string())?;
            let init_site = site(initializer)
                .ok_or_else(|| "Values Index1 payload literal site is absent".to_string())?;
            let payload_source =
                match &init_site.op {
                    Op::LoadConst { rd, idx }
                        if matches!(
                            producer.constants.get(*idx as usize),
                            Some(ConstValue::Str(value)) if value == &entry.payload_literal
                        ) =>
                    {
                        *rd
                    }
                    _ => return Err(
                        "Values Index1 payload initializer is not its exact Checker String literal"
                            .into(),
                    ),
                };
            let payload_binding_site = site(&entry.payload_binding_instruction)
                .ok_or_else(|| "Values Index1 payload binding Move site is absent".to_string())?;
            let payload_reg = match &payload_binding_site.op {
                Op::Move { rd, rs } if *rs == payload_source => *rd,
                _ => return Err("Values Index1 payload binding is not the exact owned Move".into()),
            };
            let key_instruction = crosscall_const_instruction(&entry.key_value)?;
            let key_site = site(&key_instruction)
                .ok_or_else(|| "Values Index1 key literal site is absent".to_string())?;
            let key_reg = match &key_site.op {
                Op::LoadConst { rd, idx }
                    if matches!(
                        producer.constants.get(*idx as usize),
                        Some(ConstValue::Str(value)) if value == &entry.key
                    ) =>
                {
                    *rd
                }
                _ => {
                    return Err(
                        "Values Index1 Map Set key is not its exact sorted String literal".into(),
                    );
                }
            };
            let set_site = site(&entry.set_instruction)
                .ok_or_else(|| "Values Index1 Map Set site is absent".to_string())?;
            let (set_result, set_source, set_key, set_payload) = match &set_site.op {
                Op::MirOrdinaryMapOwnedAnyV2Set { rd, ra, key, value } => (*rd, *ra, *key, *value),
                _ => return Err("Values Index1 producer Set opcode changed".into()),
            };
            let clones = producer
                .code
                .iter()
                .filter(|op| matches!(op, Op::Clone { rd, rs } if *rs == payload_reg && *rd == set_payload))
                .count();
            if set_source != current_root_reg
                || set_key != key_reg
                || set_result == set_source
                || clones != 1
            {
                return Err(
                    "Values Index1 Set root, sorted key, or owned payload clone chain changed"
                        .into(),
                );
            }
            let payload_drop_site = site(&entry.payload_drop_instruction)
                .ok_or_else(|| "Values Index1 payload Drop site is absent".to_string())?;
            if !matches!(&payload_drop_site.op, Op::Drop { ra } if *ra == payload_reg) {
                return Err(
                    "Values Index1 producer payload Drop does not settle its exact String owner"
                        .into(),
                );
            }
            let temporary_drops = self
                .producer_set_temporary_drops
                .iter()
                .find(|drops| drops.set_instruction == entry.set_instruction)
                .ok_or_else(|| "Values Index1 Set temporary Drop binding is absent".to_string())?;
            let key_drop_site = site(&temporary_drops.key_drop_instruction)
                .ok_or_else(|| "Values Index1 Set key temporary Drop site is absent".to_string())?;
            let set_payload_drop_site = site(&temporary_drops.payload_drop_instruction)
                .ok_or_else(|| {
                    "Values Index1 Set payload temporary Drop site is absent".to_string()
                })?;
            let set_pc = site(&entry.set_instruction)
                .ok_or_else(|| "Values Index1 Set opcode site is absent".to_string())?
                .pc;
            if temporary_drops.key_value != entry.key_value
                || entry.key_temporary_drop_instruction.as_ref()
                    != Some(&temporary_drops.key_drop_instruction)
                || entry.payload_clone_value.as_ref() != Some(&temporary_drops.payload_value)
                || entry.payload_clone_drop_instruction.as_ref()
                    != Some(&temporary_drops.payload_drop_instruction)
                || !matches!(&key_drop_site.op, Op::Drop { ra } if *ra == key_reg)
                || !matches!(&set_payload_drop_site.op, Op::Drop { ra } if *ra == set_payload)
                || set_pc.checked_add(1) != Some(key_drop_site.pc)
                || set_pc.checked_add(2) != Some(set_payload_drop_site.pc)
            {
                return Err(
                    "Values Index1 Set does not immediately settle its exact key and cloned payload String temporaries"
                        .into(),
                );
            }
            current_root = entry.result_root.clone();
            current_root_reg = set_result;
            root_regs.push(set_result);
        }
        let producer_set_count = producer
            .code
            .iter()
            .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2Set { .. }))
            .count();
        let producer_root_drop_count = producer
            .code
            .iter()
            .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2Drop { .. }))
            .count();
        let producer_generic_drop_count = producer
            .code
            .iter()
            .filter(|op| matches!(op, Op::Drop { .. }))
            .count();
        let distinct_root_reg_count = root_regs
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        if current_root != proof.producer_final_root
            || root_regs.len() != 3
            || distinct_root_reg_count != 3
            || producer_set_count != 2
            || producer_root_drop_count != 3
            || producer_generic_drop_count != 6
        {
            return Err(format!(
                "Values Index1 producer Set/drop census changed (final_root={}, root_regs={}/{}, Sets={}, Map_Drops={}, String_Drops={})",
                current_root == proof.producer_final_root,
                root_regs.len(),
                distinct_root_reg_count,
                producer_set_count,
                producer_root_drop_count,
                producer_generic_drop_count,
            ));
        }
        let mut root_drop_regs = Vec::with_capacity(3);
        for instruction in &proof.producer_root_drop_instructions {
            let drop_site = site(instruction)
                .ok_or_else(|| "Values Index1 producer root Drop site is absent".to_string())?;
            let root_reg = match &drop_site.op {
                Op::MirOrdinaryMapOwnedAnyV2Drop { ra } => *ra,
                _ => return Err("Values Index1 producer root Drop opcode changed".into()),
            };
            root_drop_regs.push(root_reg);
        }
        if root_drop_regs
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            != root_regs.iter().copied().collect()
        {
            return Err(
                "Values Index1 root Drops do not settle exactly the three persistent root owners"
                    .into(),
            );
        }

        let (values_reg, values_root) = match &site(&proof.producer_values_instruction)
            .ok_or_else(|| "Values Index1 Values opcode site is absent".to_string())?
            .op
        {
            Op::MirOrdinaryMapOwnedAnyV2Values { rd, ra } => (*rd, *ra),
            _ => return Err("Values Index1 producer Values opcode changed".into()),
        };
        if values_root != current_root_reg
            || producer
                .code
                .iter()
                .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2Values { .. }))
                .count()
                != 1
        {
            return Err("Values Index1 snapshot does not transfer the final Map root".into());
        }
        let snapshot_reg = match &site(&proof.producer_snapshot_binding_instruction)
            .ok_or_else(|| "Values Index1 producer snapshot binding site is absent".to_string())?
            .op
        {
            Op::Move { rd, rs } if *rs == values_reg => *rd,
            _ => {
                return Err(
                    "Values Index1 producer snapshot binding is not its exact owned Move".into(),
                );
            }
        };
        let producer_return_reg = match &site(&proof.producer_return_instruction)
            .ok_or_else(|| "Values Index1 producer return Move site is absent".to_string())?
            .op
        {
            Op::Move { rd, rs } if *rs == snapshot_reg => *rd,
            _ => return Err("Values Index1 producer return does not transfer its snapshot".into()),
        };
        if producer
            .code
            .iter()
            .filter(|op| matches!(op, Op::Ret { ra } if *ra == producer_return_reg))
            .count()
            != 1
        {
            return Err(
                "Values Index1 producer tail Return does not consume its exact transfer".into(),
            );
        }

        let (call_result, call_target) = match &site(&proof.caller_call_instruction)
            .ok_or_else(|| "Values Index1 caller direct Call site is absent".to_string())?
            .op
        {
            Op::CallMove {
                rd, func, argc: 0, ..
            } => (*rd, *func),
            _ => {
                return Err(
                    "Values Index1 caller is not the exact direct zero-argument CallMove".into(),
                );
            }
        };
        let caller_snapshot = match &site(&proof.caller_snapshot_binding_instruction)
            .ok_or_else(|| "Values Index1 caller snapshot Move site is absent".to_string())?
            .op
        {
            Op::Move { rd, rs } if *rs == call_result => *rd,
            _ => {
                return Err(
                    "Values Index1 caller snapshot binding is not its exact owned Move".into(),
                );
            }
        };
        let index_reg = match &site(&caller_index_const)
            .ok_or_else(|| "Values Index1 literal-index Const site is absent".to_string())?
            .op
        {
            Op::LoadConst { rd, idx }
                if matches!(
                    caller.constants.get(*idx as usize),
                    Some(ConstValue::Int(1))
                ) =>
            {
                *rd
            }
            _ => return Err("Values Index1 caller index is not the exact literal one".into()),
        };
        let (unpack_result, unpack_snapshot, unpack_index) =
            match &site(&proof.caller_unpack_instruction)
                .ok_or_else(|| "Values Index1 caller unpack site is absent".to_string())?
                .op
            {
                Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { rd, ra, index } => (*rd, *ra, *index),
                _ => return Err("Values Index1 caller unpack opcode changed".into()),
            };
        let caller_string = match &site(&proof.caller_unpack_binding_instruction)
            .ok_or_else(|| "Values Index1 copied String binding site is absent".to_string())?
            .op
        {
            Op::Move { rd, rs } if *rs == unpack_result => *rd,
            _ => {
                return Err(
                    "Values Index1 selected String is not copied into its owned binding".into(),
                );
            }
        };
        let expected_string_reg = match &site(&caller_expected_const)
            .ok_or_else(|| "Values Index1 comparison String Const site is absent".to_string())?
            .op
        {
            Op::LoadConst { rd, idx }
                if matches!(
                    caller.constants.get(*idx as usize),
                    Some(ConstValue::Str(value)) if value == &proof.checker_caller.expected_string
                ) =>
            {
                *rd
            }
            _ => {
                return Err(
                    "Values Index1 comparison peer is not the Checker String literal".into(),
                );
            }
        };
        let (comparison, left, right) = match &site(&proof.caller_comparison_instruction)
            .ok_or_else(|| "Values Index1 comparison opcode site is absent".to_string())?
            .op
        {
            Op::Eq { rd, ra, rb } => (*rd, *ra, *rb),
            _ => return Err("Values Index1 String comparison is not exact equality".into()),
        };
        let boolean = match &site(&proof.caller_boolean_binding_instruction)
            .ok_or_else(|| "Values Index1 Bool binding site is absent".to_string())?
            .op
        {
            Op::Mov { rd, rs } if *rs == comparison => *rd,
            _ => return Err("Values Index1 Bool binding is not its exact copy".into()),
        };
        let string_drop = match &site(&proof.caller_string_drop_instruction)
            .ok_or_else(|| "Values Index1 selected String Drop site is absent".to_string())?
            .op
        {
            Op::Drop { ra } => *ra,
            _ => return Err("Values Index1 selected String Drop opcode changed".into()),
        };
        let snapshot_drop = match &site(&proof.caller_snapshot_drop_instruction)
            .ok_or_else(|| "Values Index1 snapshot Drop site is absent".to_string())?
            .op
        {
            Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { ra } => *ra,
            _ => return Err("Values Index1 snapshot Drop opcode changed".into()),
        };
        let caller_return = match &site(&proof.caller_return_instruction)
            .ok_or_else(|| "Values Index1 Boolean return copy site is absent".to_string())?
            .op
        {
            Op::Mov { rd, rs } if *rs == boolean => *rd,
            _ => return Err("Values Index1 caller Boolean tail copy changed".into()),
        };
        if call_target != self.producer_function
            || unpack_snapshot != caller_snapshot
            || unpack_index != index_reg
            || left != caller_string
            || right != expected_string_reg
            || string_drop != caller_string
            || snapshot_drop != caller_snapshot
            || caller
                .code
                .iter()
                .filter(|op| matches!(op, Op::Ret { ra } if *ra == caller_return))
                .count()
                != 1
            || caller
                .code
                .iter()
                .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }))
                .count()
                != 1
            || caller
                .code
                .iter()
                .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }))
                .count()
                != 1
        {
            return Err(
                "Values Index1 caller Call/unpack/compare/String-copy/Drop/return chain changed"
                    .into(),
            );
        }
        Ok(())
    }
}

fn crosscall_const_instruction(
    value: &crate::core::mir::MirValueId,
) -> Result<crate::core::mir::MirInstructionId, String> {
    let node = value
        .as_str()
        .strip_prefix("expr:")
        .filter(|node| !node.is_empty())
        .ok_or_else(|| {
            format!("Values cross-call Const value '{value}' has no expression identity")
        })?;
    crate::core::mir::MirInstructionId::new(format!("inst:const:{node}"))
        .map_err(|error| format!("Values cross-call Const identity is invalid: {error}"))
}

impl CanonicalMapAnyOwnerV1BytecodeReceipt {
    fn validate_exclusive_route_anchor(&self) -> Result<(), String> {
        if self.ordinary_map_owned_any_v2.as_ref().is_some_and(
            CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_values_producer_caller_bindings,
        ) != self.values_producer_caller_route_anchor.is_some()
        {
            return Err(
                "Values producer-caller result bindings require their unique dedicated route anchor".into(),
            );
        }
        if self.ordinary_map_owned_any_v2.as_ref().is_some_and(
            CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_values_crosscall_string_caller_anchor,
        ) && self.values_producer_caller_route_anchor.is_some()
        {
            return Err(
                "Values cross-call String-caller and legacy Values producer-caller anchors cannot coexist".into(),
            );
        }
        let has_values_crosscall_v1 = self.ordinary_map_owned_any_v2.as_ref().is_some_and(
            CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_values_crosscall_string_caller_anchor,
        );
        let has_values_crosscall_index1 = self.ordinary_map_owned_any_v2.as_ref().is_some_and(
            CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_values_crosscall_string_caller_index1_anchor,
        );
        if has_values_crosscall_v1 && has_values_crosscall_index1 {
            return Err(
                "Values cross-call String-caller V1 and Index1 anchors cannot coexist".into(),
            );
        }
        if self.ordinary_map_owned_any_v2.as_ref().is_some_and(
            CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_map_root_producer_caller_bindings,
        ) != self.map_root_producer_caller_route_anchor.is_some()
        {
            return Err(
                "Map-root producer-caller bindings require their unique dedicated route anchor"
                    .into(),
            );
        }
        if self.ordinary_map_owned_any_v2.as_ref().is_some_and(
            CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_map_root_get_any_caller_bindings,
        ) != self.map_root_get_any_caller_route_anchor.is_some()
        {
            return Err(
                "Map-root GetAny-caller bindings require their unique dedicated route anchor"
                    .into(),
            );
        }
        if self.ordinary_map_owned_any_v2.as_ref().is_some_and(
            CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_get_or_default_i32_bindings,
        ) != self
            .ordinary_map_owned_any_v2_get_or_default_i32_route_anchor
            .is_some()
        {
            return Err(
                "GetOrDefault<i32> bindings require their unique dedicated route anchor".into(),
            );
        }
        if self.ordinary_map_owned_any_v2.as_ref().is_some_and(
            CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_get_or_default_string_bindings,
        ) != self
            .ordinary_map_owned_any_v2_get_or_default_string_route_anchor
            .is_some()
        {
            return Err(
                "GetOrDefault<String> bindings require their unique dedicated route anchor".into(),
            );
        }
        let anchors = [
            self.new_result_anchor.is_some(),
            self.ordinary_map_owner_observer_v1.is_some(),
            self.empty_root_root_queries_v1_route_anchor.is_some(),
            self.values_field_move_route_anchor.is_some(),
            self.values_snapshot_replacement_route_anchor.is_some(),
            self.values_mixed_snapshot_route_anchor.is_some(),
            self.values_local_index_i32_route_anchor.is_some(),
            self.values_heterogeneous_string_index1_route_anchor.is_some(),
            self.keys_snapshot_route_anchor.is_some(),
            self.dynamic_keys_get_route_anchor.is_some(),
            self.values_mixed_snapshot_remove_route_anchor.is_some(),
            self.values_producer_caller_route_anchor.is_some(),
            self.ordinary_map_owned_any_v2.as_ref().is_some_and(
                CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_values_crosscall_string_caller_anchor,
            ),
            has_values_crosscall_index1,
            self.map_root_producer_caller_route_anchor.is_some(),
            self.map_root_get_any_caller_route_anchor.is_some(),
            self.computed_key_get_route_anchor.is_some(),
            self.computed_string_key_owner_chain_route_anchor.is_some(),
            self.ordinary_map_owned_any_v2_zero_miss_i32_route_anchor
                .is_some(),
            self.ordinary_map_owned_any_v2_get_or_default_i32_route_anchor
                .is_some(),
            self.ordinary_map_owned_any_v2_get_or_default_string_route_anchor
                .is_some(),
            self.ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor
                .is_some(),
            self.checked_get_route_anchor.is_some(),
            self.is_empty_route_anchor.is_some(),
            self.record_unit_route_anchor.is_some(),
            self.map_set_drop_control_route_anchor.is_some(),
            self.branch_local_set_diamond_route_anchor.is_some(),
            self.clone_isolation_route_anchor.is_some(),
            self.ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor
                .is_some(),
            self.direct_print_route_anchor.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if anchors > 1 {
            return Err(
                "bytecode carries conflicting narrow ordinary Map/Any owner-v2 route anchors"
                    .into(),
            );
        }
        Ok(())
    }

    pub(crate) fn capture(entry: FuncIdx, functions: &[FunctionProto]) -> Self {
        let expected_has_map_any_owner_v1 = functions.iter().any(function_has_map_any_owner_v1_ops);
        Self {
            expected_has_map_any_owner_v1,
            expected_program: if expected_has_map_any_owner_v1 {
                map_any_owner_v1_program_snapshot(entry, functions)
            } else {
                String::new()
            },
            ordinary_map_owner_lifecycle_i32_string_v1: None,
            ordinary_map_owned_get_i32_string_v1: None,
            ordinary_map_owned_any_v2: None,
            new_result_anchor: None,
            values_field_move_route_anchor: None,
            empty_root_root_queries_v1_route_anchor: None,
            values_snapshot_replacement_route_anchor: None,
            values_mixed_snapshot_route_anchor: None,
            values_local_index_i32_route_anchor: None,
            values_heterogeneous_string_index1_route_anchor: None,
            keys_snapshot_route_anchor: None,
            dynamic_keys_get_route_anchor: None,
            values_mixed_snapshot_remove_route_anchor: None,
            values_producer_caller_route_anchor: None,
            map_root_producer_caller_route_anchor: None,
            map_root_get_any_caller_route_anchor: None,
            computed_key_get_route_anchor: None,
            computed_string_key_owner_chain_route_anchor: None,
            ordinary_map_owned_any_v2_zero_miss_i32_route_anchor: None,
            ordinary_map_owned_any_v2_get_or_default_i32_route_anchor: None,
            ordinary_map_owned_any_v2_get_or_default_string_route_anchor: None,
            ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor: None,
            checked_get_route_anchor: None,
            is_empty_route_anchor: None,
            record_unit_route_anchor: None,
            map_set_drop_control_route_anchor: None,
            branch_local_set_diamond_route_anchor: None,
            clone_isolation_route_anchor: None,
            ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor: None,
            direct_print_route_anchor: None,
            ordinary_map_owner_observer_v1: None,
            managed_any_identity: None,
            managed_any_trap_cleanup: None,
            expected_managed_any_cfg_receipt: false,
            managed_any_cfg: None,
        }
    }

    pub(crate) fn capture_with_ordinary_map_owner_lifecycle_i32_string_v1(
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Self {
        let mut receipt = Self::capture(entry, functions);
        receipt.ordinary_map_owner_lifecycle_i32_string_v1 = Some(
            CanonicalOrdinaryMapOwnerLifecycleBytecodeReceipt::capture(entry, functions),
        );
        receipt.ordinary_map_owned_get_i32_string_v1 = Some(
            CanonicalOrdinaryMapOwnedGetBytecodeReceipt::capture(entry, functions),
        );
        receipt.ordinary_map_owned_any_v2 = Some(
            CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::capture(entry, functions),
        );
        receipt
    }

    pub(crate) fn bind_empty_root_root_queries_v1_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proof: crate::core::mir::MapEmptyRootRootQueriesV1MirReceipt,
    ) -> Result<(), String> {
        if receipt.profile != crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE
            || self.empty_root_root_queries_v1_route_anchor.is_some()
        {
            return Err("MapEmptyRootRootQueriesV1 requires a unique dedicated route".into());
        }
        let anchor = CanonicalEmptyRootRootQueriesV1RouteAnchor {
            proof,
            receipt: receipt.clone(),
            entry,
            program: format!("{functions:?}"),
            manifest: receipt.manifest_text()?,
        };
        self.empty_root_root_queries_v1_route_anchor = Some(anchor);
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn validate_empty_root_root_queries_v1_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        let Some(anchor) = &self.empty_root_root_queries_v1_route_anchor else {
            return Ok(false);
        };
        if anchor.receipt.profile != crate::core::mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE
            || anchor.receipt.manifest_round_trip()? != anchor.receipt
            || anchor.receipt.manifest_text()? != anchor.manifest
            || anchor.entry != entry
            || anchor.program != format!("{functions:?}")
        {
            return Err(
                "MapEmptyRootRootQueriesV1 bytecode differs from private source/route anchor"
                    .into(),
            );
        }
        self.ordinary_map_owned_any_v2
            .as_ref()
            .ok_or("missing independent V2 bytecode receipt")?
            .validate(entry, functions)?;
        Ok(true)
    }

    pub(crate) fn bind_values_field_move_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_snapshot_replacement_route_anchor.is_some()
            || self.values_mixed_snapshot_route_anchor.is_some()
            || self.values_mixed_snapshot_remove_route_anchor.is_some()
            || self.computed_key_get_route_anchor.is_some()
        {
            return Err(
                "bytecode cannot carry two independent Values production route anchors".into(),
            );
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Values field-move route has no emitted V2 bytecode".into());
        }
        self.values_field_move_route_anchor = Some(
            CanonicalValuesFieldMoveRouteBytecodeAnchor::capture(entry, functions, receipt)?,
        );
        Ok(())
    }

    pub(crate) fn bind_values_snapshot_replacement_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_field_move_route_anchor.is_some()
            || self.values_mixed_snapshot_route_anchor.is_some()
            || self.values_mixed_snapshot_remove_route_anchor.is_some()
            || self.computed_key_get_route_anchor.is_some()
        {
            return Err(
                "bytecode cannot carry two independent Values production route anchors".into(),
            );
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Values snapshot replacement route has no emitted V2 bytecode".into());
        }
        self.values_snapshot_replacement_route_anchor = Some(
            CanonicalValuesSnapshotReplacementRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        Ok(())
    }

    pub(crate) fn bind_values_mixed_snapshot_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_field_move_route_anchor.is_some()
            || self.values_snapshot_replacement_route_anchor.is_some()
            || self.values_mixed_snapshot_remove_route_anchor.is_some()
            || self.computed_key_get_route_anchor.is_some()
        {
            return Err(
                "bytecode cannot carry two independent Values production route anchors".into(),
            );
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Values mixed-snapshot route has no emitted V2 bytecode".into());
        }
        self.values_mixed_snapshot_route_anchor = Some(
            CanonicalValuesMixedSnapshotRouteBytecodeAnchor::capture(entry, functions, receipt)?,
        );
        Ok(())
    }

    pub(crate) fn bind_values_local_index_i32_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Values local-index i32 route has no emitted V2 bytecode".into());
        }
        self.values_local_index_i32_route_anchor = Some(
            CanonicalValuesLocalIndexI32RouteBytecodeAnchor::capture(entry, functions, receipt)?,
        );
        Ok(())
    }

    pub(crate) fn bind_values_heterogeneous_string_index1_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("heterogeneous Values route has no emitted V2 bytecode".into());
        }
        self.values_heterogeneous_string_index1_route_anchor = Some(
            CanonicalValuesHeterogeneousStringIndex1RouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        Ok(())
    }

    pub(crate) fn bind_keys_snapshot_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_keys_ops)
        {
            return Err("KeysSnapshot route has no emitted Keys bytecode".into());
        }
        self.keys_snapshot_route_anchor = Some(CanonicalKeysSnapshotRouteBytecodeAnchor::capture(
            entry, functions, receipt,
        )?);
        Ok(())
    }

    pub(crate) fn bind_dynamic_keys_get_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.dynamic_keys_get_route_anchor.is_some()
            || self.keys_snapshot_route_anchor.is_some()
            || !functions
                .iter()
                .any(function_has_ordinary_map_owned_any_v2_dynamic_arg0)
        {
            return Err("DynamicKeysGet requires one unique dedicated argv[0] route anchor".into());
        }
        self.dynamic_keys_get_route_anchor = Some(
            CanonicalDynamicKeysGetRouteBytecodeAnchor::capture(entry, functions, receipt)?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_values_mixed_snapshot_remove_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_field_move_route_anchor.is_some()
            || self.values_snapshot_replacement_route_anchor.is_some()
            || self.values_mixed_snapshot_route_anchor.is_some()
            || self.computed_key_get_route_anchor.is_some()
        {
            return Err(
                "bytecode cannot carry two independent Values production route anchors".into(),
            );
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Values mixed-snapshot-remove route has no emitted V2 bytecode".into());
        }
        self.values_mixed_snapshot_remove_route_anchor = Some(
            CanonicalValuesMixedSnapshotRemoveRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        Ok(())
    }

    pub(crate) fn bind_values_producer_caller_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesProducerCallerMirReceipt>,
        bindings: Vec<CanonicalValuesProducerCallerBytecodeBinding>,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Values producer-caller route has no emitted V2 bytecode".into());
        }
        let route_value_bindings = bindings.clone();
        let anchor = CanonicalValuesProducerCallerRouteBytecodeAnchor::capture(
            entry, functions, receipt, proofs, bindings,
        )?;
        self.ordinary_map_owned_any_v2
            .as_mut()
            .ok_or_else(|| {
                "Values producer-caller route has no independent V2 bytecode receipt".to_string()
            })?
            .bind_values_producer_caller_bindings(route_value_bindings)?;
        self.values_producer_caller_route_anchor = Some(anchor);
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_values_crosscall_string_caller_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt>,
        bindings: Vec<CanonicalValuesCrosscallStringCallerBytecodeBinding>,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_producer_caller_route_anchor.is_some()
            || self.ordinary_map_owned_any_v2.as_ref().is_some_and(
                CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_values_crosscall_string_caller_anchor,
            )
        {
            return Err(
                "Values cross-call String-caller route cannot share a producer-caller anchor"
                    .into(),
            );
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Values cross-call String-caller route has no emitted V2 bytecode".into());
        }
        self.ordinary_map_owned_any_v2
            .as_mut()
            .ok_or_else(|| {
                "Values cross-call String-caller route has no independent V2 bytecode receipt"
                    .to_string()
            })?
            .bind_values_crosscall_string_caller_anchor(
                entry, functions, receipt, proofs, bindings,
            )?;
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_values_crosscall_string_caller_index1_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt>,
        bindings: Vec<CanonicalValuesCrosscallStringCallerIndex1BytecodeBinding>,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_producer_caller_route_anchor.is_some()
            || self.ordinary_map_owned_any_v2.as_ref().is_some_and(
                CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt::has_values_crosscall_string_caller_anchor,
            )
        {
            return Err(
                "Values cross-call String-caller Index1 route cannot share another Values caller anchor"
                    .into(),
            );
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err(
                "Values cross-call String-caller Index1 route has no emitted V2 bytecode".into(),
            );
        }
        self.ordinary_map_owned_any_v2
            .as_mut()
            .ok_or_else(|| {
                "Values cross-call String-caller Index1 route has no independent V2 bytecode receipt"
                    .to_string()
            })?
            .bind_values_crosscall_string_caller_index1_anchor(
                entry, functions, receipt, proofs, bindings,
            )?;
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_map_root_producer_caller_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootProducerCallerMirReceipt>,
        bindings: Vec<CanonicalMapRootProducerCallerBytecodeBinding>,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Map-root producer-caller route has no emitted V2 bytecode".into());
        }
        let route_bindings = bindings.clone();
        let anchor = CanonicalMapRootProducerCallerRouteBytecodeAnchor::capture(
            entry, functions, receipt, proofs, bindings,
        )?;
        self.ordinary_map_owned_any_v2
            .as_mut()
            .ok_or_else(|| {
                "Map-root producer-caller route has no independent V2 bytecode receipt".to_string()
            })?
            .bind_map_root_producer_caller_bindings(route_bindings)?;
        self.map_root_producer_caller_route_anchor = Some(anchor);
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_map_root_get_any_caller_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerMirReceipt>,
        bindings: Vec<CanonicalMapRootGetAnyCallerBytecodeBinding>,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Map-root GetAny-caller route has no emitted V2 bytecode".into());
        }
        let route_bindings = bindings.clone();
        let anchor = CanonicalMapRootGetAnyCallerRouteBytecodeAnchor::capture(
            entry, functions, receipt, proofs, bindings,
        )?;
        self.ordinary_map_owned_any_v2
            .as_mut()
            .ok_or_else(|| {
                "Map-root GetAny-caller route has no independent V2 bytecode receipt".to_string()
            })?
            .bind_map_root_get_any_caller_bindings(route_bindings)?;
        self.map_root_get_any_caller_route_anchor = Some(anchor);
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_computed_key_get_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_field_move_route_anchor.is_some()
            || self.values_snapshot_replacement_route_anchor.is_some()
            || self.values_mixed_snapshot_route_anchor.is_some()
            || self.values_mixed_snapshot_remove_route_anchor.is_some()
        {
            return Err(
                "bytecode cannot carry two independent narrow V2 production route anchors".into(),
            );
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("computed-key Get route has no emitted V2 bytecode".into());
        }
        self.computed_key_get_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2ComputedKeyGetRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        Ok(())
    }

    pub(crate) fn bind_computed_string_key_owner_chain_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("ComputedStringKeyOwnerChain route has no emitted V2 bytecode".into());
        }
        self.computed_string_key_owner_chain_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_checked_get_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("CheckedGet route has no emitted V2 bytecode".into());
        }
        self.checked_get_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2CheckedGetRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        Ok(())
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_zero_miss_i32_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self
            .ordinary_map_owned_any_v2_zero_miss_i32_route_anchor
            .is_some()
        {
            return Err("ZeroMissI32 requires a unique dedicated route anchor".into());
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("ZeroMissI32 route has no emitted V2 bytecode".into());
        }
        self.ordinary_map_owned_any_v2_zero_miss_i32_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2ZeroMissI32RouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_get_or_default_i32_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        if self
            .ordinary_map_owned_any_v2_get_or_default_i32_route_anchor
            .is_some()
        {
            return Err("GetOrDefault<i32> requires a unique dedicated route anchor".into());
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("GetOrDefault<i32> route has no emitted V2 bytecode".into());
        }
        if ordinary_map_owned_any_v2_get_or_default_i32_opcode_count(functions) != 1 {
            return Err(
                "GetOrDefault<i32> route requires exactly one dedicated emitted operation".into(),
            );
        }
        self.ordinary_map_owned_any_v2_get_or_default_i32_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32RouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        // The exact MIR bindings are attached before their route anchor. Do
        // the exclusive-anchor check only after installing this anchor, so
        // the binding/anchor cardinality check observes the completed pair.
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_get_or_default_string_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        if self
            .ordinary_map_owned_any_v2_get_or_default_string_route_anchor
            .is_some()
        {
            return Err("GetOrDefault<String> requires a unique dedicated route anchor".into());
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("GetOrDefault<String> route has no emitted V2 bytecode".into());
        }
        if ordinary_map_owned_any_v2_get_or_default_string_opcode_count(functions) != 1
            || ordinary_map_owned_any_v2_get_or_default_i32_opcode_count(functions) != 0
        {
            return Err(
                "GetOrDefault<String> route requires exactly one String operation and no i32 operation".into(),
            );
        }
        self.ordinary_map_owned_any_v2_get_or_default_string_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self
            .ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor
            .is_some()
        {
            return Err("RemovedKeyCheckedMiss requires a unique dedicated route anchor".into());
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("RemovedKeyCheckedMiss route has no emitted V2 bytecode".into());
        }
        self.ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2RemovedKeyCheckedMissRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_is_empty_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.is_empty_route_anchor.is_some() {
            return Err("IsEmpty requires a unique dedicated route anchor".into());
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("IsEmpty route has no emitted V2 bytecode".into());
        }
        self.is_empty_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2IsEmptyRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_map_set_drop_control_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.map_set_drop_control_route_anchor.is_some() {
            return Err("MapSetDropControl requires a unique dedicated route anchor".into());
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("MapSetDropControl route has no emitted V2 bytecode".into());
        }
        self.map_set_drop_control_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2MapSetDropControlRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_record_unit_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.record_unit_route_anchor.is_some() {
            return Err("RecordUnit requires a unique dedicated route anchor".into());
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("RecordUnit route has no emitted V2 bytecode".into());
        }
        self.record_unit_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2RecordUnitRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    #[cfg(test)]
    pub(crate) fn clear_record_unit_route_anchor_for_test_only(&mut self) -> bool {
        self.record_unit_route_anchor.take().is_some()
    }

    pub(crate) fn bind_branch_local_set_diamond_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.branch_local_set_diamond_route_anchor.is_some() {
            return Err("branch-local Set diamond requires a unique dedicated route anchor".into());
        }
        self.branch_local_set_diamond_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2BranchLocalSetDiamondRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_clone_isolation_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self.clone_isolation_route_anchor.is_some() {
            return Err("CloneIsolation requires a unique dedicated route anchor".into());
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("CloneIsolation route has no emitted V2 bytecode".into());
        }
        self.clone_isolation_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2CloneIsolationRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if self
            .ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor
            .is_some()
        {
            return Err(
                "PersistentRootAliasReplacement requires a unique dedicated route anchor".into(),
            );
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("PersistentRootAliasReplacement route has no emitted V2 bytecode".into());
        }
        self.ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2PersistentRootAliasReplacementRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        self.validate_exclusive_route_anchor()
    }

    pub(crate) fn bind_direct_print_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        receipt: &crate::core::mir::CanonicalMirRouteReceipt,
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("DirectPrint route has no emitted V2 bytecode".into());
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_println_ops)
        {
            return Err("DirectPrint route has no emitted V2 println operation".into());
        }
        self.direct_print_route_anchor = Some(
            CanonicalOrdinaryMapOwnedAnyV2DirectPrintRouteBytecodeAnchor::capture(
                entry, functions, receipt,
            )?,
        );
        Ok(())
    }

    pub(crate) fn validate_values_field_move_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_field_move_route_anchor.is_some()
            && (self.values_snapshot_replacement_route_anchor.is_some()
                || self.values_mixed_snapshot_route_anchor.is_some()
                || self.values_mixed_snapshot_remove_route_anchor.is_some())
            || self.values_snapshot_replacement_route_anchor.is_some()
                && self.values_mixed_snapshot_route_anchor.is_some()
            || self.values_snapshot_replacement_route_anchor.is_some()
                && self.values_mixed_snapshot_remove_route_anchor.is_some()
            || self.values_mixed_snapshot_route_anchor.is_some()
                && self.values_mixed_snapshot_remove_route_anchor.is_some()
        {
            return Err("bytecode carries conflicting Values production route anchors".into());
        }
        match &self.values_field_move_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("Values field-move route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "Values field-move route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_values_snapshot_replacement_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_field_move_route_anchor.is_some()
            && (self.values_snapshot_replacement_route_anchor.is_some()
                || self.values_mixed_snapshot_route_anchor.is_some()
                || self.values_mixed_snapshot_remove_route_anchor.is_some())
            || self.values_snapshot_replacement_route_anchor.is_some()
                && self.values_mixed_snapshot_route_anchor.is_some()
            || self.values_snapshot_replacement_route_anchor.is_some()
                && self.values_mixed_snapshot_remove_route_anchor.is_some()
            || self.values_mixed_snapshot_route_anchor.is_some()
                && self.values_mixed_snapshot_remove_route_anchor.is_some()
        {
            return Err("bytecode carries conflicting Values production route anchors".into());
        }
        match &self.values_snapshot_replacement_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err(
                        "Values snapshot replacement route anchor has no V2 bytecode".into(),
                    );
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "Values snapshot replacement route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_values_mixed_snapshot_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_field_move_route_anchor.is_some()
            && (self.values_snapshot_replacement_route_anchor.is_some()
                || self.values_mixed_snapshot_route_anchor.is_some()
                || self.values_mixed_snapshot_remove_route_anchor.is_some())
            || self.values_snapshot_replacement_route_anchor.is_some()
                && self.values_mixed_snapshot_route_anchor.is_some()
            || self.values_snapshot_replacement_route_anchor.is_some()
                && self.values_mixed_snapshot_remove_route_anchor.is_some()
            || self.values_mixed_snapshot_route_anchor.is_some()
                && self.values_mixed_snapshot_remove_route_anchor.is_some()
        {
            return Err("bytecode carries conflicting Values production route anchors".into());
        }
        match &self.values_mixed_snapshot_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("Values mixed-snapshot route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "Values mixed-snapshot route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_values_local_index_i32_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.values_local_index_i32_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("Values local-index i32 route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "Values local-index i32 route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_values_heterogeneous_string_index1_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.values_heterogeneous_string_index1_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("heterogeneous Values route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "heterogeneous Values route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_keys_snapshot_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        let dynamic_keys_get = self.validate_dynamic_keys_get_route_anchor(entry, functions)?;
        match &self.keys_snapshot_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_keys_ops)
                {
                    return Err("KeysSnapshot route anchor has no Keys bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "KeysSnapshot route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None if functions
                .iter()
                .any(function_has_ordinary_map_owned_any_v2_keys_ops) =>
            {
                if dynamic_keys_get {
                    Ok(false)
                } else {
                    Err("ordinary Map/Any owner-v2 Keys bytecode has no exact KeysSnapshot or DynamicKeysGet route anchor".into())
                }
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_dynamic_keys_get_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.dynamic_keys_get_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_dynamic_arg0)
                {
                    return Err("DynamicKeysGet route anchor has no argv[0] bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "DynamicKeysGet route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None if functions
                .iter()
                .any(function_has_ordinary_map_owned_any_v2_dynamic_arg0) =>
            {
                Err(
                    "ordinary Map/Any DynamicKeysGet argv[0] opcode has no exact route anchor"
                        .into(),
                )
            }
            None => Ok(false),
        }
    }

    pub(crate) fn ordinary_map_owned_any_v2_dynamic_arg0_site_matches(
        &self,
        function: FuncIdx,
        pc: u32,
        rd: Reg,
    ) -> bool {
        self.dynamic_keys_get_route_anchor
            .as_ref()
            .is_some_and(|anchor| anchor.authorizes_arg0(function, pc, rd))
    }

    pub(crate) fn ordinary_map_owned_any_v2_keys_site_matches(
        &self,
        function: FuncIdx,
        pc: u32,
        rd: Reg,
        ra: Reg,
    ) -> bool {
        self.keys_snapshot_route_anchor
            .as_ref()
            .is_some_and(|anchor| anchor.authorizes_keys(function, pc, rd, ra))
            || self
                .dynamic_keys_get_route_anchor
                .as_ref()
                .is_some_and(|anchor| anchor.authorizes_keys(function, pc, rd, ra))
    }

    pub(crate) fn ordinary_map_owned_any_v2_keys_unpack_site_matches(
        &self,
        function: FuncIdx,
        pc: u32,
        rd: Reg,
        ra: Reg,
    ) -> bool {
        self.keys_snapshot_route_anchor
            .as_ref()
            .is_some_and(|anchor| anchor.authorizes_unpack(function, pc, rd, ra))
            || self
                .dynamic_keys_get_route_anchor
                .as_ref()
                .is_some_and(|anchor| anchor.authorizes_unpack(function, pc, rd, ra))
    }

    pub(crate) fn validate_values_mixed_snapshot_remove_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        if self.values_field_move_route_anchor.is_some()
            && (self.values_snapshot_replacement_route_anchor.is_some()
                || self.values_mixed_snapshot_route_anchor.is_some()
                || self.values_mixed_snapshot_remove_route_anchor.is_some())
            || self.values_snapshot_replacement_route_anchor.is_some()
                && (self.values_mixed_snapshot_route_anchor.is_some()
                    || self.values_mixed_snapshot_remove_route_anchor.is_some())
            || self.values_mixed_snapshot_route_anchor.is_some()
                && self.values_mixed_snapshot_remove_route_anchor.is_some()
        {
            return Err("bytecode carries conflicting Values production route anchors".into());
        }
        match &self.values_mixed_snapshot_remove_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err(
                        "Values mixed-snapshot-remove route anchor has no V2 bytecode".into(),
                    );
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "Values mixed-snapshot-remove route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_values_producer_caller_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.values_producer_caller_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("Values producer-caller route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "Values producer-caller route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_values_crosscall_string_caller_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        let Some(v2_receipt) = self.ordinary_map_owned_any_v2.as_ref() else {
            return Ok(false);
        };
        if !v2_receipt.has_values_crosscall_string_caller_anchor() {
            return Ok(false);
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err("Values cross-call String-caller route anchor has no V2 bytecode".into());
        }
        v2_receipt.validate(entry, functions)?;
        Ok(true)
    }

    pub(crate) fn validate_values_crosscall_string_caller_index1_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        let Some(v2_receipt) = self.ordinary_map_owned_any_v2.as_ref() else {
            return Ok(false);
        };
        if !v2_receipt.has_values_crosscall_string_caller_index1_anchor() {
            return Ok(false);
        }
        if !functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err(
                "Values cross-call String-caller Index1 route anchor has no V2 bytecode".into(),
            );
        }
        v2_receipt.validate(entry, functions)?;
        Ok(true)
    }

    pub(crate) fn validate_map_root_producer_caller_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match (
            &self.map_root_producer_caller_route_anchor,
            &self.map_root_get_any_caller_route_anchor,
        ) {
            (Some(anchor), None) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("Map-root producer-caller route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "Map-root producer-caller route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            (None, Some(anchor)) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("Map-root GetAny-caller route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "Map-root GetAny-caller route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            (None, None) => Ok(false),
            (Some(_), Some(_)) => {
                Err("bytecode carries both Map-root producer-caller route anchors".into())
            }
        }
    }

    /// Authorize only the two owner-v2 root transfers named by the private
    /// Map-root bridge anchor. Callers supply the active frame and the
    /// instruction's actual PC/registers; a route-ready boolean cannot widen
    /// this whitelist.
    pub(crate) fn authorizes_map_root_producer_caller_move(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        function_index: FuncIdx,
        pc: u32,
        rd: Reg,
        rs: Reg,
    ) -> Result<bool, String> {
        if !self.validate_map_root_producer_caller_route_anchor(entry, functions)? {
            return Ok(false);
        }
        if let Some(anchor) = self.map_root_producer_caller_route_anchor.as_ref() {
            let [binding] = anchor.bindings.as_slice() else {
                return Ok(false);
            };
            return Ok((function_index == binding.producer_function
                && pc == binding.producer_return_move_pc
                && rd == binding.producer_return_move_rd
                && rs == binding.producer_return_move_rs)
                || (function_index == binding.caller_function
                    && pc == binding.caller_binding_move_pc
                    && rd == binding.caller_binding_move_rd
                    && rs == binding.caller_binding_move_rs));
        }
        let Some(anchor) = self.map_root_get_any_caller_route_anchor.as_ref() else {
            return Ok(false);
        };
        let [binding] = anchor.bindings.as_slice() else {
            return Ok(false);
        };
        let Some(producer_move) = binding.sites.get(4) else {
            return Ok(false);
        };
        let Some(caller_move) = binding.sites.get(6) else {
            return Ok(false);
        };
        let producer_authorized = function_index == producer_move.function
            && pc == producer_move.pc
            && matches!(producer_move.op, Op::Move { rd: expected_rd, rs: expected_rs } if (rd, rs) == (expected_rd, expected_rs));
        let caller_authorized = function_index == caller_move.function
            && pc == caller_move.pc
            && matches!(caller_move.op, Op::Move { rd: expected_rd, rs: expected_rs } if (rd, rs) == (expected_rd, expected_rs));
        Ok(producer_authorized || caller_authorized)
    }

    /// The pending NewResult adapter permits the one root payload transfer
    /// emitted for its exact exhaustive `Result` match. Its Checker receipt
    /// and the complete bytecode snapshot are validated before this whitelist
    /// is consulted, so it cannot authorize moves in another program/profile.
    pub(crate) fn authorizes_ordinary_map_new_result_root_move(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        function_index: FuncIdx,
        pc: u32,
        rd: Reg,
        rs: Reg,
    ) -> Result<bool, String> {
        self.validate_ordinary_map_owned_any_v2(entry, functions)?;
        let Some(anchor) = self.new_result_anchor.as_ref() else {
            return Ok(false);
        };
        anchor.authorizes_root_move(entry, functions, function_index, pc, rd, rs)
    }

    pub(crate) fn validate_computed_key_get_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.computed_key_get_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("computed-key Get route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "computed-key Get route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_computed_string_key_owner_chain_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.computed_string_key_owner_chain_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err(
                        "ComputedStringKeyOwnerChain route anchor has no V2 bytecode".into(),
                    );
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "ComputedStringKeyOwnerChain anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Validate the recoverable ordinary Map NewResult route anchor before
    /// production execution. This narrow anchor authorizes only the exact
    /// single-function pending-L1 graph while broad V2 readiness stays closed.
    pub(crate) fn validate_ordinary_map_new_result_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.new_result_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("ordinary Map NewResult route anchor has no V2 bytecode".into());
                }
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_checked_get_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.checked_get_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("CheckedGet route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "CheckedGet route anchor has no independent V2 bytecode receipt".to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_ordinary_map_owned_any_v2_zero_miss_i32_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.ordinary_map_owned_any_v2_zero_miss_i32_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("ZeroMissI32 route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "ZeroMissI32 route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_ordinary_map_owned_any_v2_get_or_default_i32_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.ordinary_map_owned_any_v2_get_or_default_i32_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("GetOrDefault<i32> route anchor has no emitted V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "GetOrDefault<i32> route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_ordinary_map_owned_any_v2_get_or_default_string_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.ordinary_map_owned_any_v2_get_or_default_string_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("GetOrDefault<String> route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "GetOrDefault<String> route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("RemovedKeyCheckedMiss route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "RemovedKeyCheckedMiss route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_is_empty_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.is_empty_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("IsEmpty route anchor has no emitted V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "IsEmpty route anchor has no independent V2 bytecode receipt".to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_map_set_drop_control_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.map_set_drop_control_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("MapSetDropControl route anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "MapSetDropControl route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_record_unit_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.record_unit_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("RecordUnit route anchor has no emitted V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "RecordUnit route anchor has no independent V2 bytecode receipt".to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_branch_local_set_diamond_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.branch_local_set_diamond_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("branch-local Set diamond anchor has no V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "branch-local Set diamond anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_clone_isolation_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.clone_isolation_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err("CloneIsolation route anchor has no emitted V2 bytecode".into());
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "CloneIsolation route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_ops)
                {
                    return Err(
                        "PersistentRootAliasReplacement route anchor has no V2 bytecode".into(),
                    );
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "PersistentRootAliasReplacement route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn validate_direct_print_route_anchor(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<bool, String> {
        self.validate_exclusive_route_anchor()?;
        match &self.direct_print_route_anchor {
            Some(anchor) => {
                if !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_println_ops)
                {
                    return Err(
                        "DirectPrint route anchor has no emitted V2 println operation".into(),
                    );
                }
                self.ordinary_map_owned_any_v2
                    .as_ref()
                    .ok_or_else(|| {
                        "DirectPrint route anchor has no independent V2 bytecode receipt"
                            .to_string()
                    })?
                    .validate(entry, functions)?;
                anchor.validate(entry, functions)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_field_move_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_field_move_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_field_move_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_field_move_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_snapshot_replacement_route_profile_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.values_snapshot_replacement_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_snapshot_replacement_route_manifest_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.values_snapshot_replacement_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_values_snapshot_replacement_route_anchor_for_test_only(&mut self) -> bool {
        self.values_snapshot_replacement_route_anchor
            .take()
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_mixed_snapshot_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_mixed_snapshot_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_mixed_snapshot_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_mixed_snapshot_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_values_mixed_snapshot_route_anchor_for_test_only(&mut self) -> bool {
        self.values_mixed_snapshot_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_local_index_i32_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_local_index_i32_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_local_index_i32_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_local_index_i32_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_values_local_index_i32_route_anchor_for_test_only(&mut self) -> bool {
        self.values_local_index_i32_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_heterogeneous_string_index1_route_profile_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self
            .values_heterogeneous_string_index1_route_anchor
            .as_mut()
        else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_heterogeneous_string_index1_route_manifest_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self
            .values_heterogeneous_string_index1_route_anchor
            .as_mut()
        else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_values_heterogeneous_string_index1_route_anchor_for_test_only(
        &mut self,
    ) -> bool {
        self.values_heterogeneous_string_index1_route_anchor
            .take()
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_keys_snapshot_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.keys_snapshot_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_keys_snapshot_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.keys_snapshot_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_keys_snapshot_route_anchor_for_test_only(&mut self) -> bool {
        self.keys_snapshot_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_mixed_snapshot_remove_route_profile_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.values_mixed_snapshot_remove_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_mixed_snapshot_remove_route_manifest_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.values_mixed_snapshot_remove_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_values_mixed_snapshot_remove_route_anchor_for_test_only(&mut self) -> bool {
        self.values_mixed_snapshot_remove_route_anchor
            .take()
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_producer_caller_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_producer_caller_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_producer_caller_route_call_target_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.values_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_call_target_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_producer_caller_route_owner_drop_register_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.values_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_owner_drop_register_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_producer_caller_route_len_tail_instruction_for_test_only(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> bool {
        let Some(anchor) = self.values_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_len_tail_instruction_for_test_only(entry, functions)
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_producer_caller_route_literal_tail_value_for_test_only(
        &mut self,
        entry: FuncIdx,
        functions: &mut [FunctionProto],
    ) -> bool {
        let Some(anchor) = self.values_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_literal_tail_value_for_test_only(entry, functions)
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_producer_caller_source_type_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_producer_source_type_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_values_producer_caller_target_type_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.values_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_producer_target_type_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn validate_values_producer_caller_anchor_only_for_test_only(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        self.values_producer_caller_route_anchor
            .as_ref()
            .ok_or_else(|| "Values producer-caller route anchor is absent".to_string())?
            .validate(entry, functions)
    }

    #[cfg(test)]
    pub(crate) fn clear_values_producer_caller_route_anchor_for_test_only(&mut self) -> bool {
        self.values_producer_caller_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_map_root_producer_caller_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.map_root_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_map_root_producer_caller_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.map_root_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_map_root_producer_caller_route_call_target_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.map_root_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_call_target_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_map_root_producer_caller_route_drop_register_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.map_root_producer_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_root_drop_register_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn validate_map_root_producer_caller_anchor_only_for_test_only(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        self.map_root_producer_caller_route_anchor
            .as_ref()
            .ok_or_else(|| "Map-root producer-caller route anchor is absent".to_string())?
            .validate(entry, functions)
    }

    #[cfg(test)]
    pub(crate) fn map_root_producer_caller_move_sites_for_test_only(
        &self,
    ) -> Option<[(FuncIdx, u32, Reg, Reg); 2]> {
        let anchor = self.map_root_producer_caller_route_anchor.as_ref()?;
        let [binding] = anchor.bindings.as_slice() else {
            return None;
        };
        Some([
            (
                binding.producer_function,
                binding.producer_return_move_pc,
                binding.producer_return_move_rd,
                binding.producer_return_move_rs,
            ),
            (
                binding.caller_function,
                binding.caller_binding_move_pc,
                binding.caller_binding_move_rd,
                binding.caller_binding_move_rs,
            ),
        ])
    }

    #[cfg(test)]
    pub(crate) fn clear_map_root_producer_caller_route_anchor_for_test_only(&mut self) -> bool {
        self.map_root_producer_caller_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_map_root_get_any_caller_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.map_root_get_any_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_map_root_get_any_caller_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.map_root_get_any_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_map_root_get_any_caller_route_get_key_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.map_root_get_any_caller_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_get_any_key_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn validate_map_root_get_any_caller_anchor_only_for_test_only(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        self.map_root_get_any_caller_route_anchor
            .as_ref()
            .ok_or_else(|| "Map-root GetAny-caller route anchor is absent".to_string())?
            .validate(entry, functions)
    }

    #[cfg(test)]
    pub(crate) fn validate_map_root_get_any_caller_sites_only_for_test_only(
        &self,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        let anchor = self
            .map_root_get_any_caller_route_anchor
            .as_ref()
            .ok_or_else(|| "Map-root GetAny-caller route anchor is absent".to_string())?;
        let [binding] = anchor.bindings.as_slice() else {
            return Err("Map-root GetAny-caller route has no unique bytecode binding".into());
        };
        binding.validate_sites(functions)
    }

    #[cfg(test)]
    pub(crate) fn map_root_get_any_caller_move_sites_for_test_only(
        &self,
    ) -> Option<[(FuncIdx, u32, Reg, Reg); 2]> {
        let anchor = self.map_root_get_any_caller_route_anchor.as_ref()?;
        let [binding] = anchor.bindings.as_slice() else {
            return None;
        };
        let producer = binding.sites.get(4)?;
        let caller = binding.sites.get(6)?;
        let (
            Op::Move {
                rd: producer_rd,
                rs: producer_rs,
            },
            Op::Move {
                rd: caller_rd,
                rs: caller_rs,
            },
        ) = (producer.op, caller.op)
        else {
            return None;
        };
        Some([
            (producer.function, producer.pc, producer_rd, producer_rs),
            (caller.function, caller.pc, caller_rd, caller_rs),
        ])
    }

    #[cfg(test)]
    pub(crate) fn map_root_get_any_caller_sites_for_test_only(
        &self,
    ) -> Option<[(FuncIdx, u32, Op); 15]> {
        let anchor = self.map_root_get_any_caller_route_anchor.as_ref()?;
        let [binding] = anchor.bindings.as_slice() else {
            return None;
        };
        if binding.sites.len() != 15 {
            return None;
        }
        Some(std::array::from_fn(|index| {
            let site = &binding.sites[index];
            (site.function, site.pc, site.op)
        }))
    }

    #[cfg(test)]
    pub(crate) fn clear_map_root_get_any_caller_route_anchor_for_test_only(&mut self) -> bool {
        self.map_root_get_any_caller_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_computed_key_get_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.computed_key_get_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_computed_key_get_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.computed_key_get_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_computed_key_get_route_anchor_for_test_only(&mut self) -> bool {
        self.computed_key_get_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_computed_string_key_owner_chain_route_profile_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.computed_string_key_owner_chain_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_computed_string_key_owner_chain_route_manifest_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self.computed_string_key_owner_chain_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_computed_string_key_owner_chain_route_anchor_for_test_only(
        &mut self,
    ) -> bool {
        self.computed_string_key_owner_chain_route_anchor
            .take()
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_checked_get_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.checked_get_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_checked_get_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.checked_get_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_checked_get_route_anchor_for_test_only(&mut self) -> bool {
        self.checked_get_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_zero_miss_i32_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self
            .ordinary_map_owned_any_v2_zero_miss_i32_route_anchor
            .as_mut()
        else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_zero_miss_i32_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self
            .ordinary_map_owned_any_v2_zero_miss_i32_route_anchor
            .as_mut()
        else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_zero_miss_i32_route_anchor_for_test_only(&mut self) -> bool {
        self.ordinary_map_owned_any_v2_zero_miss_i32_route_anchor
            .take()
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_removed_key_checked_miss_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self
            .ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor
            .as_mut()
        else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_removed_key_checked_miss_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self
            .ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor
            .as_mut()
        else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_removed_key_checked_miss_route_anchor_for_test_only(&mut self) -> bool {
        self.ordinary_map_owned_any_v2_removed_key_checked_miss_route_anchor
            .take()
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_is_empty_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.is_empty_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_is_empty_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.is_empty_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_is_empty_route_anchor_for_test_only(&mut self) -> bool {
        self.is_empty_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_map_set_drop_control_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.map_set_drop_control_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_map_set_drop_control_route_anchor_for_test_only(&mut self) -> bool {
        self.map_set_drop_control_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_branch_local_set_diamond_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.branch_local_set_diamond_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_branch_local_set_diamond_route_anchor_for_test_only(&mut self) -> bool {
        self.branch_local_set_diamond_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_clone_isolation_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.clone_isolation_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_clone_isolation_route_manifest_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.clone_isolation_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_clone_isolation_route_anchor_for_test_only(&mut self) -> bool {
        self.clone_isolation_route_anchor.take().is_some()
    }

    #[cfg(test)]
    pub(crate) fn corrupt_persistent_root_alias_replacement_route_profile_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self
            .ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor
            .as_mut()
        else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_persistent_root_alias_replacement_route_digest_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self
            .ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor
            .as_mut()
        else {
            return false;
        };
        anchor.corrupt_digest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn corrupt_persistent_root_alias_replacement_route_manifest_for_test_only(
        &mut self,
    ) -> bool {
        let Some(anchor) = self
            .ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor
            .as_mut()
        else {
            return false;
        };
        anchor.corrupt_manifest_for_test_only();
        true
    }

    #[cfg(test)]
    pub(crate) fn clear_persistent_root_alias_replacement_route_anchor_for_test_only(
        &mut self,
    ) -> bool {
        self.ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor
            .take()
            .is_some()
    }

    pub(crate) fn set_managed_any_identity(
        &mut self,
        receipt: CanonicalManagedAnyIdentityBytecodeReceipt,
    ) {
        self.managed_any_identity = Some(receipt);
    }

    pub(crate) fn set_managed_any_trap_cleanup(
        &mut self,
        receipt: CanonicalManagedAnyTrapCleanupBytecodeReceipt,
    ) {
        self.managed_any_trap_cleanup = Some(receipt);
    }

    pub(crate) fn set_managed_any_cfg(&mut self, receipt: CanonicalManagedAnyCfgBytecodeReceipt) {
        self.expected_managed_any_cfg_receipt = true;
        self.managed_any_cfg = Some(receipt);
    }

    pub(crate) fn validate_managed_any_cfg(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        externally_required: bool,
    ) -> Result<(), String> {
        if externally_required != self.expected_managed_any_cfg_receipt {
            return Err(
                "Any/CFG bytecode required marker disagrees with the independent canonical MIR receipt".into(),
            );
        }
        match (
            externally_required && self.expected_managed_any_cfg_receipt,
            &self.managed_any_cfg,
        ) {
            (true, Some(receipt)) => receipt.validate(
                entry,
                functions,
                &self.existing_managed_any_receipts_snapshot(),
            ),
            (true, None) => Err(
                "Any-bearing canonical MIR bytecode is missing its independent CFG receipt".into(),
            ),
            (false, Some(_)) => Err(
                "Any/CFG bytecode receipt is present without its independent census marker".into(),
            ),
            (false, None) => Ok(()),
        }
    }

    /// Rebind only the complete bytecode-table snapshot for test fixtures
    /// that deliberately wrap a Checker-compiled program with bytecode-only
    /// Spawn/Actor shells. Source census, call bindings, and the independent
    /// receipt chain remain unchanged and are checked before this update.
    ///
    /// This is intentionally unavailable in production builds: runtime code
    /// must never be able to bless bytecode mutations after compilation.
    #[cfg(test)]
    pub(crate) fn reseal_managed_any_cfg_program_for_test(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        let previous_receipts = self.existing_managed_any_receipts_snapshot();
        let receipt = self
            .managed_any_cfg
            .as_mut()
            .ok_or_else(|| "test bytecode fixture has no Checker Any/CFG receipt".to_string())?;

        // A transformed shell must not rewrite source-bound Any owners or
        // owner-moving call sites. Those bindings are immutable inputs to the
        // Checker receipt; only bytecode-only helper functions and an
        // alternate wrapper entry may be introduced by this test seam.
        if !receipt.source_functions.is_empty() || !receipt.calls.is_empty() {
            return Err(
                "test bytecode reseal cannot alter source-bound Any functions or calls".into(),
            );
        }
        receipt.validate_source_and_call_bindings(entry, functions, &previous_receipts)?;
        receipt.expected_program = managed_any_cfg_program_snapshot(entry, functions);
        Ok(())
    }

    pub(crate) fn existing_managed_any_receipts_snapshot(&self) -> String {
        format!(
            "map-any-owner-v1={}:{}|ordinary-map-owner={:?}|ordinary-map-owner-observer-v1={:?}|ordinary-map-owned-get={:?}|ordinary-map-owned-any-v2={:?}|values-field-move-route={:?}|values-snapshot-replacement-route={:?}|values-mixed-snapshot-route={:?}|values-local-index-i32-route={:?}|values-heterogeneous-string-index1-route={:?}|keys-snapshot-route={:?}|values-mixed-snapshot-remove-route={:?}|clone-isolation-route={:?}|persistent-root-alias-replacement-route={:?}|managed-any-identity={:?}|managed-any-trap-cleanup={:?}",
            self.expected_has_map_any_owner_v1,
            self.expected_program,
            self.ordinary_map_owner_lifecycle_i32_string_v1,
            self.ordinary_map_owner_observer_v1,
            self.ordinary_map_owned_get_i32_string_v1,
            self.ordinary_map_owned_any_v2,
            self.values_field_move_route_anchor,
            self.values_snapshot_replacement_route_anchor,
            self.values_mixed_snapshot_route_anchor,
            self.values_local_index_i32_route_anchor,
            self.values_heterogeneous_string_index1_route_anchor,
            self.keys_snapshot_route_anchor,
            self.values_mixed_snapshot_remove_route_anchor,
            self.clone_isolation_route_anchor,
            self.ordinary_map_owned_any_v2_persistent_root_alias_replacement_route_anchor,
            self.managed_any_identity,
            self.managed_any_trap_cleanup,
        )
    }

    pub(crate) fn validate_managed_any_identity(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        match &self.managed_any_identity {
            Some(receipt) => receipt.validate(entry, functions),
            None if functions
                .iter()
                .any(function_is_managed_any_identity_candidate) =>
            {
                Err(
                    "ManagedAny generic identity bytecode has no private Checker instance receipt"
                        .into(),
                )
            }
            None => Ok(()),
        }
    }

    pub(crate) fn validate_managed_any_trap_cleanup(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        match &self.managed_any_trap_cleanup {
            Some(receipt) => receipt.validate(entry, functions),
            None if functions
                .iter()
                .any(function_is_managed_any_trap_cleanup_candidate) =>
            {
                Err(
                    "ManagedAny trap-cleanup bytecode has no private Checker instance receipt"
                        .into(),
                )
            }
            None => Ok(()),
        }
    }

    pub(crate) fn validate_ordinary_map_owner_lifecycle_i32_string_v1(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        match &self.ordinary_map_owner_lifecycle_i32_string_v1 {
            Some(receipt) => receipt.validate(entry, functions),
            None if functions
                .iter()
                .any(function_has_ordinary_map_owner_lifecycle_i32_string_v1_ops) =>
            {
                Err(
                    "ordinary Map owner lifecycle v1 bytecode has no independent Checker receipt"
                        .into(),
                )
            }
            None => Ok(()),
        }
    }

    pub(crate) fn validate_ordinary_map_owner_observer_v1(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        builtin_names: &[String],
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        match &self.ordinary_map_owner_observer_v1 {
            Some(anchor) => anchor.validate(entry, functions, builtin_names),
            None if functions
                .iter()
                .any(function_has_ordinary_map_owner_observer_v1_ops) =>
            {
                Err(
                    "ordinary Map owner observer v1 bytecode has no independent route receipt"
                        .into(),
                )
            }
            None => Ok(()),
        }
    }

    pub(crate) fn validate_ordinary_map_owned_get_i32_string_v1(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        match &self.ordinary_map_owned_get_i32_string_v1 {
            Some(receipt) => receipt.validate(entry, functions),
            None if functions
                .iter()
                .any(function_has_ordinary_map_owned_get_i32_string_v1_ops) =>
            {
                Err("ordinary Map owned-Get v1 bytecode has no independent Checker receipt".into())
            }
            None => Ok(()),
        }
    }

    pub(crate) fn validate_ordinary_map_owned_any_v2(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        self.validate_exclusive_route_anchor()?;
        let has_new_result = functions.iter().any(|function| {
            function
                .code
                .iter()
                .any(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2NewResult { .. }))
        });
        match (&self.new_result_anchor, has_new_result) {
            (Some(anchor), true) => anchor.validate(entry, functions)?,
            (None, false) => {}
            _ => return Err(
                "ordinary Map NewResult bytecode has no matching independent pending-L1 receipt"
                    .into(),
            ),
        }

        match &self.ordinary_map_owned_any_v2 {
            Some(receipt) => receipt.validate(entry, functions),
            None if functions
                .iter()
                .any(function_has_ordinary_map_owned_any_v2_ops) =>
            {
                Err("ordinary Map/Any owner-v2 bytecode has no independent Checker receipt".into())
            }
            None => Ok(()),
        }
    }

    pub(crate) fn bind_ordinary_map_new_result(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        core: &crate::core::mir::MirFunction,
        catalog: &crate::core::mir::types::MirTypeCatalog,
    ) -> Result<(), String> {
        crate::core::mir::validate_ordinary_map_new_result_types(core, catalog)?;
        if self.new_result_anchor.is_some() {
            return Err("ordinary Map NewResult bytecode anchor is already set".into());
        }
        let receipt = core.ownership.ordinary_map_new_result_receipt().unwrap();
        self.new_result_anchor = Some(CanonicalOrdinaryMapNewResultBytecodeAnchor {
            profile: crate::core::mir::ORDINARY_MAP_NEW_RESULT_PENDING_L1_PROFILE.into(),
            owner: core.owner.0.clone(),
            source_commitment: format!("{receipt:?}"),
            entry,
            root_move: ordinary_map_new_result_root_move_site(functions)?,
            expected_program: format!("{functions:?}"),
        });
        self.validate_ordinary_map_owned_any_v2(entry, functions)
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_payload_bindings_from_selection(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2PayloadBinding>,
        >,
        selection_digest: Option<String>,
    ) -> Result<(), String> {
        let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() else {
            if bindings.is_empty() && selection_digest.is_none() {
                return Ok(());
            }
            return Err(
                "ordinary Map/Any owner-v2 payload TypeDescs have no profile receipt".into(),
            );
        };
        receipt.bind_payload_bindings_from_selection(bindings, selection_digest)
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_get_any_bindings_from_selection(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2GetAnyBinding>,
        >,
    ) -> Result<(), String> {
        let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() else {
            if bindings.is_empty() {
                return Ok(());
            }
            return Err("ordinary Map/Any owner-v2 GetAny results have no profile receipt".into());
        };
        receipt.bind_get_any_bindings_from_selection(bindings);
        Ok(())
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_get_or_default_i32_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32Binding>,
        >,
    ) -> Result<(), String> {
        let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() else {
            if bindings.is_empty() {
                return Ok(());
            }
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<i32> sites have no profile receipt".into(),
            );
        };
        receipt.bind_get_or_default_i32_bindings_from_core_receipts(bindings)
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_get_or_default_string_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringBinding>,
        >,
    ) -> Result<(), String> {
        let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() else {
            if bindings.is_empty() {
                return Ok(());
            }
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<String> sites have no profile receipt"
                    .into(),
            );
        };
        receipt.bind_get_or_default_string_bindings_from_core_receipts(bindings)
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_unpack_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2UnpackBinding>,
        >,
    ) -> Result<(), String> {
        let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() else {
            if bindings.is_empty() {
                return Ok(());
            }
            return Err("ordinary Map/Any owner-v2 unpack sites have no profile receipt".into());
        };
        receipt.bind_unpack_bindings_from_core_receipts(bindings)
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_values_unpack_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackBinding>,
        >,
    ) -> Result<(), String> {
        let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() else {
            if bindings.is_empty() {
                return Ok(());
            }
            return Err(
                "ordinary Map/Any owner-v2 Values unpack sites have no profile receipt".into(),
            );
        };
        receipt.bind_values_unpack_bindings_from_core_receipts(bindings)
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_values_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesBinding>,
        >,
    ) -> Result<(), String> {
        let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() else {
            if bindings.is_empty() {
                return Ok(());
            }
            return Err("ordinary Map/Any owner-v2 Values results have no profile receipt".into());
        };
        receipt.bind_values_bindings_from_core_receipts(bindings)
    }

    pub(crate) fn bind_ordinary_map_owned_any_v2_println_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2PrintlnBinding>,
        >,
    ) -> Result<(), String> {
        let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() else {
            if bindings.is_empty()
                && !functions
                    .iter()
                    .any(function_has_ordinary_map_owned_any_v2_println_ops)
            {
                return Ok(());
            }
            return Err(
                "ordinary Map/Any owner-v2 Println has no independent V2 bytecode receipt".into(),
            );
        };
        if receipt.println_anchor.is_some() {
            return Err("ordinary Map/Any owner-v2 Println anchor is already bound".into());
        }
        receipt.bind_println_anchor(entry, functions, bindings)
    }

    pub(crate) fn bind_ordinary_map_owner_observer_v1_route_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        builtin_names: &[String],
        route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        checker_receipts: Vec<crate::core::mir::OrdinaryMapOwnerLifecycleObserverV1Receipt>,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnerObserverV1Binding>,
        >,
    ) -> Result<(), String> {
        if self.ordinary_map_owner_observer_v1.is_some() {
            return Err("ordinary Map owner observer v1 route anchor is already bound".into());
        }
        if self.ordinary_map_owner_lifecycle_i32_string_v1.is_none() {
            return Err(
                "ordinary Map owner observer v1 has no independent lifecycle receipt".into(),
            );
        }
        let anchor = CanonicalOrdinaryMapOwnerObserverV1BytecodeAnchor::capture(
            entry,
            functions,
            builtin_names,
            route_receipt,
            checker_receipts,
            bindings,
        )?;
        self.ordinary_map_owner_observer_v1 = Some(anchor);
        self.validate_exclusive_route_anchor()?;
        Ok(())
    }

    pub(crate) fn ordinary_map_owned_any_v2_payload_binding(
        &self,
        function: FuncIdx,
        set_ordinal: usize,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2PayloadBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .payload_binding(function, set_ordinal)
    }

    pub(crate) fn ordinary_map_owned_any_v2_selected_payload_binding(
        &self,
        type_id: &crate::core::ResolvedTypeId,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2PayloadBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .selected_payload_binding(type_id)
    }

    pub(crate) fn ordinary_map_owned_any_v2_get_any_binding(
        &self,
        function: FuncIdx,
        get_any_ordinal: usize,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2GetAnyBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .get_any_binding(function, get_any_ordinal)
    }

    pub(crate) fn ordinary_map_owned_any_v2_get_or_default_i32_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32Binding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .get_or_default_i32_binding_at_pc(function, pc)
    }

    pub(crate) fn ordinary_map_owned_any_v2_get_or_default_string_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .get_or_default_string_binding_at_pc(function, pc)
    }

    pub(crate) fn ordinary_map_owned_any_v2_println_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2PrintlnBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .println_binding_at_pc(function, pc)
    }

    pub(crate) fn ordinary_map_owner_observer_v1_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnerObserverV1Binding> {
        self.ordinary_map_owner_observer_v1
            .as_ref()?
            .bindings
            .get(&function)?
            .iter()
            .find(|binding| binding.pc == pc)
    }

    #[cfg(test)]
    pub(crate) fn corrupt_ordinary_map_owner_observer_v1_anchor_for_test_only(&mut self) {
        if let Some(anchor) = &mut self.ordinary_map_owner_observer_v1 {
            anchor.corrupt_profile_for_test_only();
        }
    }

    pub(crate) fn ordinary_map_owned_any_v2_unpack_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2UnpackBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .unpack_binding_at_pc(function, pc)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_unpack_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .values_unpack_binding_at_pc(function, pc)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_binding(
        &self,
        function: FuncIdx,
        values_ordinal: usize,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2ValuesBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .values_binding(function, values_ordinal)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2ValuesBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .values_binding_at_pc(function, pc)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_producer_caller_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalValuesProducerCallerBytecodeBinding> {
        let mut bindings = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_producer_caller_bindings
            .iter()
            .filter(|binding| {
                binding.producer_function == function && binding.producer_values_pc == pc
            });
        let binding = bindings.next()?;
        bindings.next().is_none().then_some(binding)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_crosscall_string_caller_site_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalValuesCrosscallStringCallerBytecodeSite> {
        if let Some(anchor) = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_crosscall_string_caller_anchor
            .as_ref()
        {
            let mut sites = anchor
                .bindings
                .iter()
                .flat_map(|binding| binding.sites.iter())
                .filter(|site| {
                    site.function == function
                        && site.pc == pc
                        && matches!(&site.op, Op::MirOrdinaryMapOwnedAnyV2Values { .. })
                });
            if let Some(site) = sites.next() {
                return sites.next().is_none().then_some(site);
            }
        }
        let anchor = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_crosscall_string_caller_index1_anchor
            .as_ref()?;
        let mut sites = anchor
            .bindings
            .iter()
            .flat_map(|binding| binding.sites.iter())
            .filter(|site| {
                site.function == function
                    && site.pc == pc
                    && matches!(&site.op, Op::MirOrdinaryMapOwnedAnyV2Values { .. })
            });
        let site = sites.next()?;
        sites.next().is_none().then_some(site)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_crosscall_string_caller_unpack_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<(
        &crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt,
        &CanonicalValuesCrosscallStringCallerBytecodeSite,
    )> {
        if let Some(anchor) = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_crosscall_string_caller_anchor
            .as_ref()
        {
            if let [proof] = anchor.proofs.as_slice() {
                let mut sites = anchor
                    .bindings
                    .iter()
                    .flat_map(|binding| binding.sites.iter())
                    .filter(|site| {
                        site.function == function
                            && site.pc == pc
                            && site.owner == proof.caller_owner
                            && site.instruction == proof.caller_unpack_instruction
                            && matches!(&site.op, Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. })
                    });
                if let Some(site) = sites.next() {
                    return sites.next().is_none().then_some((proof, site));
                }
            }
        }
        let anchor = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_crosscall_string_caller_index1_anchor
            .as_ref()?;
        let [proof] = anchor.proofs.as_slice() else {
            return None;
        };
        let mut sites = anchor
            .bindings
            .iter()
            .flat_map(|binding| binding.sites.iter())
            .filter(|site| {
                site.function == function
                    && site.pc == pc
                    && site.owner == proof.caller_owner
                    && site.instruction == proof.caller_unpack_instruction
                    && matches!(&site.op, Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. })
            });
        let site = sites.next()?;
        sites.next().is_none().then_some((proof, site))
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_crosscall_string_caller_snapshot_drop_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<(
        &crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt,
        &CanonicalValuesCrosscallStringCallerBytecodeSite,
    )> {
        if let Some(anchor) = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_crosscall_string_caller_anchor
            .as_ref()
        {
            if let [proof] = anchor.proofs.as_slice() {
                let mut sites = anchor
                    .bindings
                    .iter()
                    .flat_map(|binding| binding.sites.iter())
                    .filter(|site| {
                        site.function == function
                            && site.pc == pc
                            && site.owner == proof.caller_owner
                            && site.instruction == proof.caller_snapshot_drop_instruction
                            && matches!(&site.op, Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. })
                    });
                if let Some(site) = sites.next() {
                    return sites.next().is_none().then_some((proof, site));
                }
            }
        }
        let anchor = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_crosscall_string_caller_index1_anchor
            .as_ref()?;
        let [proof] = anchor.proofs.as_slice() else {
            return None;
        };
        let mut sites = anchor
            .bindings
            .iter()
            .flat_map(|binding| binding.sites.iter())
            .filter(|site| {
                site.function == function
                    && site.pc == pc
                    && site.owner == proof.caller_owner
                    && site.instruction == proof.caller_snapshot_drop_instruction
                    && matches!(&site.op, Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. })
            });
        let site = sites.next()?;
        sites.next().is_none().then_some((proof, site))
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_producer_caller_len_clone_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalValuesProducerCallerBytecodeBinding> {
        let mut bindings = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_producer_caller_bindings
            .iter()
            .filter(|binding| {
                binding.caller_function == function && binding.len_receiver_clone_pc == Some(pc)
            });
        let binding = bindings.next()?;
        bindings.next().is_none().then_some(binding)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_producer_caller_owner_drop_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalValuesProducerCallerBytecodeBinding> {
        let mut bindings = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_producer_caller_bindings
            .iter()
            .filter(|binding| binding.caller_function == function && binding.owner_drop_pc == pc);
        let binding = bindings.next()?;
        bindings.next().is_none().then_some(binding)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_producer_caller_len_drop_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalValuesProducerCallerBytecodeBinding> {
        let mut bindings = self
            .ordinary_map_owned_any_v2
            .as_ref()?
            .values_producer_caller_bindings
            .iter()
            .filter(|binding| {
                binding.caller_function == function && binding.len_receiver_drop_pc == Some(pc)
            });
        let binding = bindings.next()?;
        bindings.next().is_none().then_some(binding)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_drop_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2ValuesBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .values_drop_binding_at_pc(function, pc)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_clone_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<(
        &CanonicalOrdinaryMapOwnedAnyV2ValuesBinding,
        &CanonicalOrdinaryMapOwnedAnyV2ValuesCloneBinding,
    )> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .values_clone_binding_at_pc(function, pc)
    }

    pub(crate) fn ordinary_map_owned_any_v2_values_clone_drop_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<(
        &CanonicalOrdinaryMapOwnedAnyV2ValuesBinding,
        &CanonicalOrdinaryMapOwnedAnyV2ValuesCloneBinding,
    )> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .values_clone_drop_binding_at_pc(function, pc)
    }

    #[cfg(test)]
    pub(crate) fn replace_ordinary_map_owned_any_v2_values_bindings_for_test_only(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesBinding>,
        >,
    ) {
        if let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() {
            receipt.values_bindings = bindings;
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_ordinary_map_owned_any_v2_values_unpack_bindings_for_test_only(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackBinding>,
        >,
    ) {
        if let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() {
            receipt.values_unpack_bindings = bindings;
        }
    }

    #[cfg(test)]
    pub(crate) fn mutate_ordinary_map_owned_any_v2_println_anchor_for_test_only(
        &mut self,
        mutate: impl FnOnce(&mut Option<CanonicalOrdinaryMapOwnedAnyV2PrintlnAnchor>),
    ) {
        if let Some(receipt) = self.ordinary_map_owned_any_v2.as_mut() {
            mutate(&mut receipt.println_anchor);
        }
    }

    #[cfg(test)]
    pub(crate) fn corrupt_direct_print_route_profile_for_test_only(&mut self) -> bool {
        let Some(anchor) = self.direct_print_route_anchor.as_mut() else {
            return false;
        };
        anchor.corrupt_profile_for_test_only();
        true
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        let has_map_root = functions.iter().any(function_has_map_root_ops);
        let has_map_any_owner_v1 = functions.iter().any(function_has_map_any_owner_v1_ops);
        if !self.expected_has_map_any_owner_v1 && !has_map_any_owner_v1 {
            return Ok(());
        }
        if has_map_root {
            return Err("MapAnyOwner v1 receipt cannot authorize legacy MapRoot bytecode".into());
        }
        if functions
            .iter()
            .any(function_has_ordinary_map_owner_lifecycle_i32_string_v1_ops)
        {
            return Err(
                "MapAnyOwner v1 receipt cannot authorize ordinary Map owner lifecycle v1 bytecode"
                    .into(),
            );
        }
        if functions
            .iter()
            .any(function_has_ordinary_map_owned_get_i32_string_v1_ops)
        {
            return Err(
                "MapAnyOwner v1 receipt cannot authorize ordinary Map owned-Get v1 bytecode".into(),
            );
        }
        if functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err(
                "MapAnyOwner v1 receipt cannot authorize ordinary Map/Any owner-v2 bytecode".into(),
            );
        }
        if !self.expected_has_map_any_owner_v1 && has_map_any_owner_v1 {
            return Err(
                "canonical MapAnyOwner v1 bytecode has no independent Checker receipt".into(),
            );
        }
        if !has_map_any_owner_v1
            || map_any_owner_v1_program_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "canonical MapAnyOwner v1 bytecode disagrees with its independent Checker receipt snapshot".into(),
            );
        }
        Ok(())
    }
}

/// Independent full-program postcompile seal for source-receipted
/// Any-bearing CFGs and their direct call edges.  Unlike the narrower Map,
/// identity, and trap snapshots, this receipt always captures the complete
/// bytecode function table for canonical MIR programs, including signatures
/// represented by source bindings, register counts, constants, all
/// instruction operands, branches, and call targets.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalManagedAnyCfgBytecodeReceipt {
    profile_version: u16,
    source_census: String,
    /// Exact Checker source-plan snapshot from the validated MIR program.
    /// This remains bound even when the plan has no direct call bindings.
    interprocedural_source_snapshot: Option<String>,
    /// Independent source-plan presence bit; deleting only the Option child
    /// cannot silently turn a source-receipted program into a no-plan case.
    expected_has_interprocedural_source_snapshot: bool,
    source_functions: Vec<CanonicalManagedAnyCfgFunctionBinding>,
    calls: Vec<CanonicalManagedAnyCfgCallBinding>,
    previous_receipts: String,
    expected_program: String,
}

impl CanonicalManagedAnyCfgBytecodeReceipt {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        source_census: String,
        source_functions: Vec<CanonicalManagedAnyCfgFunctionBinding>,
        calls: Vec<CanonicalManagedAnyCfgCallBinding>,
        previous_receipts: String,
        interprocedural_source_snapshot: Option<String>,
        expected_has_interprocedural_source_snapshot: bool,
    ) -> Self {
        Self {
            profile_version: 1,
            source_census,
            interprocedural_source_snapshot,
            expected_has_interprocedural_source_snapshot,
            source_functions,
            calls,
            previous_receipts,
            expected_program: managed_any_cfg_program_snapshot(entry, functions),
        }
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        previous_receipts: &str,
    ) -> Result<(), String> {
        self.validate_source_and_call_bindings(entry, functions, previous_receipts)?;
        if managed_any_cfg_program_snapshot(entry, functions) != self.expected_program {
            return Err(
                "canonical Any/CFG bytecode disagrees with its whole-program Checker snapshot"
                    .into(),
            );
        }
        Ok(())
    }

    fn validate_source_and_call_bindings(
        &self,
        _entry: FuncIdx,
        functions: &[FunctionProto],
        previous_receipts: &str,
    ) -> Result<(), String> {
        if self.profile_version != 1 {
            return Err("Any/CFG bytecode receipt has an unsupported profile version".into());
        }
        if self.source_census.is_empty() {
            return Err("Any/CFG bytecode receipt has no Checker source census".into());
        }
        if self.expected_has_interprocedural_source_snapshot
            != self.interprocedural_source_snapshot.is_some()
        {
            return Err(
                "Any/CFG bytecode receipt has an inconsistent interprocedural source-plan presence marker".into(),
            );
        }
        let source_plan_marker = format!(
            "|interprocedural-source-snapshot={:?}|expected-presence={}",
            self.interprocedural_source_snapshot, self.expected_has_interprocedural_source_snapshot,
        );
        if !self.source_census.contains(&source_plan_marker) {
            return Err(
                "Any/CFG source census disagrees with its exact interprocedural source-plan snapshot".into(),
            );
        }
        if self.previous_receipts != previous_receipts {
            return Err(
                "Any/CFG bytecode receipt disagrees with the independent Map/identity/trap/Values-route snapshots".into(),
            );
        }
        let mut source_functions = std::collections::BTreeSet::new();
        for binding in &self.source_functions {
            if !source_functions.insert((binding.function_index, binding.owner.as_str())) {
                return Err("Any/CFG source function census contains a duplicate binding".into());
            }
            let function = functions
                .get(binding.function_index as usize)
                .ok_or("Any/CFG source function index is outside bytecode table")?;
            if function.name != binding.owner || binding.source_signature.is_empty() {
                return Err(
                    "Any/CFG source function binding disagrees with emitted bytecode".into(),
                );
            }
        }
        let mut call_sites = std::collections::BTreeSet::new();
        for binding in &self.calls {
            if !call_sites.insert((binding.caller_index, binding.pc)) {
                return Err("Any/CFG source call census contains a duplicate bytecode site".into());
            }
            let caller = functions
                .get(binding.caller_index as usize)
                .ok_or("Any/CFG caller index is outside bytecode table")?;
            let callee = functions
                .get(binding.callee_index as usize)
                .ok_or("Any/CFG callee index is outside bytecode table")?;
            if caller.name != binding.caller || callee.name != binding.callee {
                return Err("Any/CFG call binding disagrees with emitted function owners".into());
            }
            match caller.code.get(binding.pc as usize) {
                Some(Op::CallMove {
                    rd,
                    func,
                    args_base,
                    argc,
                }) if *rd == binding.rd
                    && *func == binding.callee_index
                    && *args_base == binding.args_base
                    && *argc == binding.argc => {}
                _ => {
                    return Err(
                        "Any/CFG source call no longer matches its emitted owner-moving call site"
                            .into(),
                    );
                }
            }
            if binding.source_instruction.is_empty()
                || binding.source_block.is_empty()
                || binding.source_kind.is_empty()
            {
                return Err("Any/CFG source call binding has incomplete MIR identity".into());
            }
        }
        Ok(())
    }
}

fn managed_any_cfg_program_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile=managed-any-cfg-v1;entry={entry};functions={}",
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

/// Independent compiler snapshot for the ordinary source-Map owner lifecycle
/// profile. Its namespace and version are intentionally distinct from both
/// MapRoot and MapAnyOwner v1.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalOrdinaryMapOwnerLifecycleBytecodeReceipt {
    profile_version: u16,
    expected_has_ordinary_map_owner_lifecycle_i32_string_v1: bool,
    expected_program: String,
}

impl CanonicalOrdinaryMapOwnerLifecycleBytecodeReceipt {
    pub(crate) fn capture(entry: FuncIdx, functions: &[FunctionProto]) -> Self {
        let expected_has_ordinary_map_owner_lifecycle_i32_string_v1 = functions
            .iter()
            .any(function_has_ordinary_map_owner_lifecycle_i32_string_v1_ops);
        Self {
            profile_version: 1,
            expected_has_ordinary_map_owner_lifecycle_i32_string_v1,
            expected_program: if expected_has_ordinary_map_owner_lifecycle_i32_string_v1 {
                ordinary_map_owner_lifecycle_i32_string_v1_program_snapshot(entry, functions)
            } else {
                String::new()
            },
        }
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.profile_version != 1 {
            return Err(
                "ordinary Map owner lifecycle receipt has an unsupported profile version".into(),
            );
        }
        let has_ordinary_map_owner_lifecycle_i32_string_v1 = functions
            .iter()
            .any(function_has_ordinary_map_owner_lifecycle_i32_string_v1_ops);
        if !self.expected_has_ordinary_map_owner_lifecycle_i32_string_v1
            && !has_ordinary_map_owner_lifecycle_i32_string_v1
        {
            return Ok(());
        }
        if !self.expected_has_ordinary_map_owner_lifecycle_i32_string_v1
            && has_ordinary_map_owner_lifecycle_i32_string_v1
        {
            return Err(
                "ordinary Map owner lifecycle v1 bytecode has no independent Checker receipt"
                    .into(),
            );
        }
        if functions.iter().any(function_has_map_root_ops) {
            return Err(
                "ordinary Map owner lifecycle v1 receipt cannot authorize MapRoot bytecode".into(),
            );
        }
        if functions.iter().any(function_has_map_any_owner_v1_ops) {
            return Err(
                "ordinary Map owner lifecycle v1 receipt cannot authorize MapAnyOwner v1 bytecode"
                    .into(),
            );
        }
        if functions
            .iter()
            .any(function_has_ordinary_map_owned_get_i32_string_v1_ops)
        {
            return Err(
                "ordinary Map owner lifecycle v1 receipt cannot authorize ordinary Map owned-Get v1 bytecode".into(),
            );
        }
        if functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err(
                "ordinary Map owner lifecycle v1 receipt cannot authorize owner-v2 bytecode".into(),
            );
        }
        if !has_ordinary_map_owner_lifecycle_i32_string_v1
            || ordinary_map_owner_lifecycle_i32_string_v1_program_snapshot(entry, functions)
                != self.expected_program
        {
            return Err(
                "ordinary Map owner lifecycle v1 bytecode disagrees with its independent Checker receipt snapshot".into(),
            );
        }
        Ok(())
    }
}

/// Independent snapshot for the ordinary Map typed owned-Get bytecode
/// profile. It binds the emitted Get result-shape constants together with
/// every register read/write in the program.
#[derive(Debug, Clone)]
pub(crate) struct CanonicalOrdinaryMapOwnedGetBytecodeReceipt {
    profile_version: u16,
    expected_has_ordinary_map_owned_get_i32_string_v1: bool,
    expected_program: String,
}

impl CanonicalOrdinaryMapOwnedGetBytecodeReceipt {
    pub(crate) fn capture(entry: FuncIdx, functions: &[FunctionProto]) -> Self {
        let expected_has_ordinary_map_owned_get_i32_string_v1 = functions
            .iter()
            .any(function_has_ordinary_map_owned_get_i32_string_v1_ops);
        Self {
            profile_version: 1,
            expected_has_ordinary_map_owned_get_i32_string_v1,
            expected_program: if expected_has_ordinary_map_owned_get_i32_string_v1 {
                ordinary_map_owned_get_i32_string_v1_program_snapshot(entry, functions)
            } else {
                String::new()
            },
        }
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.profile_version != 1 {
            return Err("ordinary Map owned-Get receipt has an unsupported profile version".into());
        }
        let has_owned_get = functions
            .iter()
            .any(function_has_ordinary_map_owned_get_i32_string_v1_ops);
        if !self.expected_has_ordinary_map_owned_get_i32_string_v1 && !has_owned_get {
            return Ok(());
        }
        if !self.expected_has_ordinary_map_owned_get_i32_string_v1 && has_owned_get {
            return Err(
                "ordinary Map owned-Get v1 bytecode has no independent Checker receipt".into(),
            );
        }
        if functions.iter().any(function_has_map_root_ops) {
            return Err(
                "ordinary Map owned-Get v1 receipt cannot authorize MapRoot bytecode".into(),
            );
        }
        if functions.iter().any(function_has_map_any_owner_v1_ops) {
            return Err(
                "ordinary Map owned-Get v1 receipt cannot authorize MapAnyOwner v1 bytecode".into(),
            );
        }
        if functions
            .iter()
            .any(function_has_ordinary_map_owner_lifecycle_i32_string_v1_ops)
        {
            return Err(
                "ordinary Map owned-Get v1 receipt cannot authorize ordinary Map owner lifecycle v1 bytecode".into(),
            );
        }
        if functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops)
        {
            return Err(
                "ordinary Map owned-Get v1 receipt cannot authorize owner-v2 bytecode".into(),
            );
        }
        if !has_owned_get
            || ordinary_map_owned_get_i32_string_v1_program_snapshot(entry, functions)
                != self.expected_program
        {
            return Err(
                "ordinary Map owned-Get v1 bytecode disagrees with its independent Checker receipt snapshot".into(),
            );
        }
        Ok(())
    }
}

pub(crate) fn function_has_map_root_ops(function: &FunctionProto) -> bool {
    function.code.iter().any(|op| {
        matches!(
            op,
            Op::MirMapRootNew { .. }
                | Op::MirMapRootSet { .. }
                | Op::MirMapRootRemove { .. }
                | Op::MirMapRootSize { .. }
                | Op::MirMapRootContains { .. }
                | Op::MirMapRootDrop { .. }
        )
    })
}

pub(crate) fn function_has_map_any_owner_v1_ops(function: &FunctionProto) -> bool {
    function.code.iter().any(|op| {
        matches!(
            op,
            Op::MirMapAnyOwnerV1New { .. }
                | Op::MirMapAnyOwnerV1Set { .. }
                | Op::MirMapAnyOwnerV1Remove { .. }
                | Op::MirMapAnyOwnerV1Size { .. }
                | Op::MirMapAnyOwnerV1Contains { .. }
                | Op::MirMapAnyOwnerV1Drop { .. }
        )
    })
}

pub(crate) fn function_has_ordinary_map_owner_lifecycle_i32_string_v1_ops(
    function: &FunctionProto,
) -> bool {
    function.code.iter().any(|op| {
        matches!(
            op,
            Op::MirOrdinaryMapOwnerLifecycleI32StringV1New { .. }
                | Op::MirOrdinaryMapOwnerLifecycleI32StringV1Clone { .. }
                | Op::MirOrdinaryMapOwnerLifecycleI32StringV1Set { .. }
                | Op::MirOrdinaryMapOwnerLifecycleI32StringV1Remove { .. }
                | Op::MirOrdinaryMapOwnerLifecycleI32StringV1Size { .. }
                | Op::MirOrdinaryMapOwnerLifecycleI32StringV1Contains { .. }
                | Op::MirOrdinaryMapOwnerLifecycleI32StringV1Drop { .. }
        )
    })
}

pub(crate) fn function_has_ordinary_map_owner_observer_v1_ops(function: &FunctionProto) -> bool {
    function.code.iter().any(|op| {
        matches!(
            op,
            Op::MirOrdinaryMapOwnerLifecycleObserverV1ToJson { .. }
                | Op::MirOrdinaryMapOwnerLifecycleObserverV1Println { .. }
        )
    })
}

pub(crate) fn function_has_ordinary_map_owned_get_i32_string_v1_ops(
    function: &FunctionProto,
) -> bool {
    function.code.iter().any(|op| {
        matches!(
            op,
            Op::MirOrdinaryMapOwnedGetI32StringV1New { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1Clone { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1Set { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1Remove { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1Size { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1Contains { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1GetI32 { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1GetString { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1GetAny { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1Values { .. }
                | Op::MirManagedAnyOp { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1Drop { .. }
        )
    })
}

pub(crate) fn function_has_ordinary_map_owned_any_v2_ops(function: &FunctionProto) -> bool {
    function.code.iter().any(|op| {
        matches!(
            op,
            Op::MirOrdinaryMapOwnedAnyV2New { .. }
                | Op::MirOrdinaryMapOwnedAnyV2NewResult { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Clone { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Set { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Remove { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Size { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Contains { .. }
                | Op::MirOrdinaryMapOwnedAnyV2GetAny { .. }
                | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }
                | Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultString { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Println { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Unpack { .. }
                | Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Values { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Keys { .. }
                | Op::MirOrdinaryMapOwnedAnyV2KeysUnpack { .. }
                | Op::MirOrdinaryMapOwnedAnyV2DynamicArg0 { .. }
                | Op::MirOrdinaryMapOwnedAnyV2IsEmpty { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Drop { .. }
                | Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }
                | Op::MirOrdinaryMapOwnedAnyV2ValuesClone { .. }
        )
    })
}

pub(crate) fn function_has_ordinary_map_owned_any_v2_keys_ops(function: &FunctionProto) -> bool {
    function.code.iter().any(|op| {
        matches!(
            op,
            Op::MirOrdinaryMapOwnedAnyV2Keys { .. } | Op::MirOrdinaryMapOwnedAnyV2KeysUnpack { .. }
        )
    })
}

pub(crate) fn function_has_ordinary_map_owned_any_v2_dynamic_arg0(
    function: &FunctionProto,
) -> bool {
    function
        .code
        .iter()
        .any(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2DynamicArg0 { .. }))
}

pub(crate) fn function_has_ordinary_map_owned_any_v2_println_ops(function: &FunctionProto) -> bool {
    function
        .code
        .iter()
        .any(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2Println { .. }))
}

/// Bytecode-level candidates that require the independent canonical MIR
/// Any/CFG receipt even when a narrower Map, identity, or trap receipt was
/// removed. Source-only Any signatures are covered by the canonical MIR
/// marker plus its required census receipt.
pub(crate) fn function_has_managed_any_cfg_candidate(function: &FunctionProto) -> bool {
    // Map owner profiles have their own receipts. They are not themselves
    // evidence that a ManagedAny CFG receipt is required: hand-built
    // owned-Get bytecode may contain only typed i32/String operations.
    function.code.iter().any(|op| {
        matches!(
            op,
            Op::MirOrdinaryMapOwnedGetI32StringV1GetAny { .. }
                | Op::MirOrdinaryMapOwnedGetI32StringV1Values { .. }
                | Op::MirManagedAnyOp { .. }
                | Op::MirManagedAnyStringCompare { .. }
        )
    }) || function_is_managed_any_identity_candidate(function)
        || function_is_managed_any_trap_cleanup_candidate(function)
        || function
            .constants
            .iter()
            .any(|constant| matches!(constant, ConstValue::ManagedAnyContract(_)))
}

fn map_root_program_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!("entry={entry};functions={}", functions.len());
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn map_any_owner_v1_program_snapshot(entry: FuncIdx, functions: &[FunctionProto]) -> String {
    let mut snapshot = format!(
        "profile=map-any-owner-v1;entry={entry};functions={}",
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn ordinary_map_owner_lifecycle_i32_string_v1_program_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile=ordinary-map-owner-lifecycle-i32-string-v1;version=1;entry={entry};functions={}",
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn ordinary_map_owner_observer_v1_program_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
    builtin_names: &[String],
) -> String {
    let mut snapshot = format!(
        "profile=ordinary-map-owner-lifecycle-observer-v1;version=1;entry={entry};functions={};builtins={builtin_names:?}",
        functions.len(),
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanonicalOrdinaryMapOwnerObserverV1Operation {
    ToJson,
    Println,
}

/// One emitted observer site joined to the exact Checker and MIR identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnerObserverV1Binding {
    pub(crate) receipt: crate::core::mir::OrdinaryMapOwnerLifecycleObserverV1Receipt,
    pub(crate) owner: crate::core::NodeId,
    pub(crate) callsite: crate::core::NodeId,
    pub(crate) operation: CanonicalOrdinaryMapOwnerObserverV1Operation,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) ra: Reg,
}

/// Independent route receipt for the narrow direct lifecycle-v1 Map observer
/// profile. It pins the canonical route receipt and Checker census alongside
/// the exact output/JSON op sites and complete bytecode program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnerObserverV1BytecodeAnchor {
    expected_profile: &'static str,
    route_receipt: crate::core::mir::CanonicalMirRouteReceipt,
    expected_route_manifest: String,
    expected_route_semantic_digest: String,
    checker_receipts: Vec<crate::core::mir::OrdinaryMapOwnerLifecycleObserverV1Receipt>,
    expected_checker_receipts: String,
    expected_entry: FuncIdx,
    expected_program: String,
    bindings: std::collections::BTreeMap<FuncIdx, Vec<CanonicalOrdinaryMapOwnerObserverV1Binding>>,
    expected_bindings: String,
}

impl CanonicalOrdinaryMapOwnerObserverV1BytecodeAnchor {
    pub(crate) fn capture(
        entry: FuncIdx,
        functions: &[FunctionProto],
        builtin_names: &[String],
        route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        checker_receipts: Vec<crate::core::mir::OrdinaryMapOwnerLifecycleObserverV1Receipt>,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnerObserverV1Binding>,
        >,
    ) -> Result<Self, String> {
        let expected_profile = "ordinary-map-owner-lifecycle-observer-v1";
        if route_receipt.profile != expected_profile {
            return Err(format!(
                "ordinary Map owner observer v1 anchor received route profile '{}'",
                route_receipt.profile
            ));
        }
        let replayed = route_receipt
            .manifest_round_trip()
            .map_err(|message| format!("observer route manifest replay failed: {message}"))?;
        if replayed != *route_receipt {
            return Err("observer route receipt changed during manifest replay".into());
        }
        if checker_receipts.is_empty() {
            return Err("ordinary Map owner observer v1 route has no Checker receipts".into());
        }
        let expected_route_manifest = route_receipt
            .manifest_text()
            .map_err(|message| format!("observer route manifest render failed: {message}"))?;
        let expected_route_semantic_digest = route_receipt.semantic_identity_digest();
        let anchor = Self {
            expected_profile,
            route_receipt: route_receipt.clone(),
            expected_route_manifest,
            expected_route_semantic_digest,
            expected_checker_receipts: format!("{checker_receipts:?}"),
            checker_receipts,
            expected_entry: entry,
            expected_program: ordinary_map_owner_observer_v1_program_snapshot(
                entry,
                functions,
                builtin_names,
            ),
            expected_bindings: format!("{bindings:?}"),
            bindings,
        };
        anchor.validate(entry, functions, builtin_names)?;
        Ok(anchor)
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        builtin_names: &[String],
    ) -> Result<(), String> {
        const PROFILE: &str = "ordinary-map-owner-lifecycle-observer-v1";
        if self.expected_profile != PROFILE || self.route_receipt.profile != self.expected_profile {
            return Err(
                "ordinary Map owner observer v1 route profile disagrees with its anchor".into(),
            );
        }
        let replayed = self
            .route_receipt
            .manifest_round_trip()
            .map_err(|message| {
                format!("observer route manifest replay failed at VM boundary: {message}")
            })?;
        if replayed != self.route_receipt
            || self.route_receipt.manifest_text().ok().as_deref()
                != Some(self.expected_route_manifest.as_str())
            || self.route_receipt.semantic_identity_digest() != self.expected_route_semantic_digest
        {
            return Err(
                "ordinary Map owner observer v1 route receipt disagrees with its immutable anchor"
                    .into(),
            );
        }
        if format!("{:?}", self.checker_receipts) != self.expected_checker_receipts {
            return Err(
                "ordinary Map owner observer v1 Checker receipts disagree with their immutable snapshot"
                    .into(),
            );
        }
        if self.expected_entry != entry
            || ordinary_map_owner_observer_v1_program_snapshot(entry, functions, builtin_names)
                != self.expected_program
        {
            return Err(
                "ordinary Map owner observer v1 anchor disagrees with its complete bytecode/entry snapshot"
                    .into(),
            );
        }
        if format!("{:?}", self.bindings) != self.expected_bindings {
            return Err(
                "ordinary Map owner observer v1 bindings disagree with their immutable Checker receipt snapshot"
                    .into(),
            );
        }

        let mut op_sites = std::collections::BTreeSet::new();
        for (function_index, function) in functions.iter().enumerate() {
            let function_index = FuncIdx::try_from(function_index)
                .map_err(|_| "ordinary Map owner observer v1 function index exceeds u32 ABI")?;
            for (pc, op) in function.code.iter().enumerate() {
                if matches!(
                    op,
                    Op::MirOrdinaryMapOwnerLifecycleObserverV1ToJson { .. }
                        | Op::MirOrdinaryMapOwnerLifecycleObserverV1Println { .. }
                ) {
                    let pc = u32::try_from(pc)
                        .map_err(|_| "ordinary Map owner observer v1 opcode PC exceeds u32 ABI")?;
                    op_sites.insert((function_index, pc));
                }
            }
        }
        let mut binding_sites = std::collections::BTreeSet::new();
        let binding_count: usize = self.bindings.values().map(Vec::len).sum();
        if self.checker_receipts.len() != binding_count {
            return Err(
                "ordinary Map owner observer v1 Checker/MIR receipt count differs from emitted sites"
                    .into(),
            );
        }
        for (function_index, bindings) in &self.bindings {
            let function = functions
                .get(*function_index as usize)
                .ok_or("ordinary Map owner observer v1 binding names an absent function")?;
            for binding in bindings {
                if &binding.owner != binding.receipt.owner()
                    || &binding.callsite != binding.receipt.callsite()
                    || self
                        .checker_receipts
                        .iter()
                        .filter(|receipt| *receipt == &binding.receipt)
                        .count()
                        != 1
                    || !binding_sites.insert((*function_index, binding.pc))
                    || binding.rd == binding.ra
                {
                    return Err(
                        "ordinary Map owner observer v1 binding disagrees with its exact Checker receipt or aliases its root"
                            .into(),
                    );
                }
                let exact_op = match binding.operation {
                    CanonicalOrdinaryMapOwnerObserverV1Operation::ToJson => matches!(
                        function.code.get(binding.pc as usize),
                        Some(Op::MirOrdinaryMapOwnerLifecycleObserverV1ToJson { rd, ra })
                            if (*rd, *ra) == (binding.rd, binding.ra)
                    ),
                    CanonicalOrdinaryMapOwnerObserverV1Operation::Println => matches!(
                        function.code.get(binding.pc as usize),
                        Some(Op::MirOrdinaryMapOwnerLifecycleObserverV1Println { rd, ra })
                            if (*rd, *ra) == (binding.rd, binding.ra)
                    ),
                };
                if !exact_op {
                    return Err(
                        "ordinary Map owner observer v1 binding disagrees with its exact opcode/register site"
                            .into(),
                    );
                }
            }
        }
        if binding_sites != op_sites
            || self.checker_receipts.iter().any(|receipt| {
                self.bindings
                    .values()
                    .flatten()
                    .filter(|binding| binding.receipt == *receipt)
                    .count()
                    != 1
            })
        {
            return Err("ordinary Map owner observer v1 receipt/opcode site census differs".into());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn corrupt_profile_for_test_only(&mut self) {
        self.route_receipt.profile = crate::core::mir::MIR_BYTECODE_DIRECT_ROUTE_PROFILE.into();
    }
}

fn ordinary_map_owned_get_i32_string_v1_program_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile=ordinary-map-owned-get-i32-string-v1;version=1;entry={entry};functions={}",
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

fn ordinary_map_owned_any_v2_program_snapshot(
    entry: FuncIdx,
    functions: &[FunctionProto],
) -> String {
    let mut snapshot = format!(
        "profile=ordinary-map-owned-any-v2;version=2;entry={entry};functions={}",
        functions.len()
    );
    for (index, function) in functions.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = write!(snapshot, "|function[{index}]={function:?}");
    }
    snapshot
}

/// Independent postcompile snapshot for the ordinary Map/Any owner-v2 profile.
/// The profile is separate from both v1 ordinary Map receipts so a mutable
/// bytecode table cannot borrow authority from a different payload ABI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt {
    profile_version: u16,
    expected_has_ordinary_map_owned_any_v2: bool,
    expected_program: String,
    /// Core-selected TypeDesc identities paired with each static Set site in
    /// emitted order. The adapter binds the Checker-derived selection rows
    /// to exact MIR instruction sites; runtime display names are never used
    /// to invent a TypeDesc identity.
    payload_bindings:
        std::collections::BTreeMap<FuncIdx, Vec<CanonicalOrdinaryMapOwnedAnyV2PayloadBinding>>,
    /// Unique Core-selected payload TypeDescs, used to validate the dynamic
    /// TypeId carried by a GetAny hit. The miss-only canonical i32 zero is
    /// checked as a separate result-contract exception.
    selected_payload_bindings: std::collections::BTreeMap<
        crate::core::ResolvedTypeId,
        CanonicalOrdinaryMapOwnedAnyV2PayloadBinding,
    >,
    /// Exact Core Set-selection receipt digest used to populate the selected
    /// payload table. Values snapshots bind this identity directly.
    payload_selection_digest: Option<String>,
    /// Checker-derived GetAny result settlements bound to exact emitted MIR
    /// instructions, values, registers, and bytecode PCs.
    get_any_bindings:
        std::collections::BTreeMap<FuncIdx, Vec<CanonicalOrdinaryMapOwnedAnyV2GetAnyBinding>>,
    /// Exact Checker/MIR proof for the dedicated GetOrDefault<i32> operation.
    /// This is separate from ordinary GetAny and Unpack receipts because its
    /// private pair is created, checked, and settled within one opcode.
    get_or_default_i32_bindings: std::collections::BTreeMap<
        FuncIdx,
        Vec<CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32Binding>,
    >,
    expected_get_or_default_i32_bindings: String,
    /// Exact Checker/MIR proof for the dedicated GetOrDefault<String>
    /// operation, including the i32 miss sentinel and String payload digest.
    get_or_default_string_bindings: std::collections::BTreeMap<
        FuncIdx,
        Vec<CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringBinding>,
    >,
    expected_get_or_default_string_bindings: String,
    /// Checker/MIR-derived exact target-type unpack sites.
    unpack_bindings:
        std::collections::BTreeMap<FuncIdx, Vec<CanonicalOrdinaryMapOwnedAnyV2UnpackBinding>>,
    expected_unpack_bindings: String,
    /// Checker/MIR-derived Values indexed-unpack sites, bound to exact
    /// snapshot/index/type-selection identities and emitted register/PC.
    values_unpack_bindings:
        std::collections::BTreeMap<FuncIdx, Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackBinding>>,
    expected_values_unpack_bindings: String,
    /// Checker-derived Values result owners bound to their exact emitted
    /// snapshot, result Move, and explicit Values Drop sites.
    values_bindings:
        std::collections::BTreeMap<FuncIdx, Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesBinding>>,
    expected_values_bindings: String,
    /// Exact Values results returned through the dedicated producer→caller
    /// route. These remain separate from local ExplicitDrop receipts; a
    /// producer return cannot impersonate the legacy same-function Drop.
    values_producer_caller_bindings: Vec<CanonicalValuesProducerCallerBytecodeBinding>,
    expected_values_producer_caller_bindings: String,
    /// Independent proof/binding for the cross-call Values String-copy route.
    /// It remains separate from local Values result/unpack ledgers and from
    /// the older producer-caller route.
    values_crosscall_string_caller_anchor:
        Option<CanonicalValuesCrosscallStringCallerRouteBytecodeAnchor>,
    /// Exact separate V2 route for the two-entry literal-index-one caller.
    values_crosscall_string_caller_index1_anchor:
        Option<CanonicalValuesCrosscallStringCallerIndex1RouteBytecodeAnchor>,
    /// Exact Map-root owner transfers returned through the dedicated
    /// producer→caller route, sealed independently from Values snapshots.
    map_root_producer_caller_bindings: Vec<CanonicalMapRootProducerCallerBytecodeBinding>,
    expected_map_root_producer_caller_bindings: String,
    /// Exact route-only GetAny/checked String-unpack bridge projection.
    map_root_get_any_caller_bindings: Vec<CanonicalMapRootGetAnyCallerBytecodeBinding>,
    expected_map_root_get_any_caller_bindings: String,
    /// Dedicated output authorization for borrowed Any-v2 Println sites.
    println_anchor: Option<CanonicalOrdinaryMapOwnedAnyV2PrintlnAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalOrdinaryMapOwnedAnyV2PayloadKind {
    Bool,
    I32,
    String,
    Record { nominal: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2PayloadBinding {
    pub(crate) type_id: crate::core::ResolvedTypeId,
    pub(crate) type_desc_digest: String,
    pub(crate) kind: CanonicalOrdinaryMapOwnedAnyV2PayloadKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2GetAnyBinding {
    pub(crate) owner: crate::core::NodeId,
    pub(crate) operation: crate::core::NodeId,
    pub(crate) instruction: crate::core::mir::MirInstructionId,
    pub(crate) source_node: crate::core::NodeId,
    pub(crate) source_local: crate::core::ir::ResolvedLocalId,
    pub(crate) root: crate::core::mir::MirValueId,
    pub(crate) checker_source_type: crate::core::ResolvedTypeId,
    pub(crate) key_node: crate::core::NodeId,
    pub(crate) key: crate::core::mir::MirValueId,
    pub(crate) checker_key_type: crate::core::ResolvedTypeId,
    pub(crate) checker_key_local: Option<crate::core::ir::ResolvedLocalId>,
    pub(crate) result: crate::core::mir::MirValueId,
    pub(crate) checker_result_type: crate::core::ResolvedTypeId,
    pub(crate) mir_result_type: crate::core::ResolvedTypeId,
    pub(crate) binding_local: crate::core::ir::ResolvedLocalId,
    pub(crate) binding: crate::core::mir::MirValueId,
    pub(crate) settlement: crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: crate::core::NodeId,
    pub(crate) settlement_instruction: crate::core::mir::MirInstructionId,
    pub(crate) temporary_root: Option<CanonicalOrdinaryMapOwnedAnyV2TemporaryRootBinding>,
    pub(crate) miss_payload_type: crate::core::ResolvedTypeId,
    pub(crate) miss_payload_type_desc_digest: String,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) ra: Reg,
    pub(crate) key_reg: Reg,
}

/// Typed MIR receipt plus its exact emitted bytecode site for one
/// `std::maps.get_or_default<i32>` call. The op internally creates and drops
/// a real V2 GetAny pair; the pair is never exposed as an unreceipted bytecode
/// value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32Binding {
    pub(crate) receipt: crate::core::mir::OrdinaryMapOwnedAnyV2GetOrDefaultI32MirReceipt,
    pub(crate) target_type_desc_digest: String,
    pub(crate) payload_selection_digest: Option<String>,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) ra: Reg,
    pub(crate) key_reg: Reg,
    pub(crate) default_reg: Reg,
}

/// Typed MIR receipt plus its exact emitted bytecode site for one
/// `std::maps.get_or_default<String>` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringBinding {
    pub(crate) receipt: crate::core::mir::OrdinaryMapOwnedAnyV2GetOrDefaultStringMirReceipt,
    pub(crate) target_type_desc_digest: String,
    pub(crate) payload_selection_digest: Option<String>,
    pub(crate) miss_type_desc_digest: String,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) ra: Reg,
    pub(crate) key_reg: Reg,
    pub(crate) default_reg: Reg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2PrintlnBinding {
    pub(crate) receipt: crate::core::mir::OrdinaryMapOwnedAnyV2PrintlnReceipt,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) pair: Reg,
}

/// Core/MIR binding for an exact target-type unpack of the DynamicAny field
/// from a GetAny pair. The source tuple remains the independently settled
/// owner; this receipt binds the borrowed projection and target TypeDesc to
/// one emitted opcode site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2UnpackBinding {
    pub(crate) owner: crate::core::NodeId,
    pub(crate) operation: crate::core::NodeId,
    pub(crate) instruction: crate::core::mir::MirInstructionId,
    pub(crate) projection: crate::core::NodeId,
    pub(crate) cast: crate::core::NodeId,
    pub(crate) conversion_kind: crate::core::ir::CheckedConversionKind,
    pub(crate) conversion_from: crate::core::ResolvedTypeId,
    pub(crate) conversion_to: crate::core::ResolvedTypeId,
    pub(crate) payload_source: crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource,
    pub(crate) pair: crate::core::mir::MirValueId,
    pub(crate) pair_local: crate::core::ir::ResolvedLocalId,
    pub(crate) result: crate::core::mir::MirValueId,
    pub(crate) target_type: crate::core::ResolvedTypeId,
    pub(crate) target_type_desc_digest: String,
    pub(crate) get_any_instruction: crate::core::mir::MirInstructionId,
    pub(crate) settlement: crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: crate::core::NodeId,
    pub(crate) settlement_instruction: crate::core::mir::MirInstructionId,
    pub(crate) miss_payload_type: crate::core::ResolvedTypeId,
    pub(crate) payload_selection_digest: String,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) ra: Reg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackBinding {
    pub(crate) owner: crate::core::NodeId,
    pub(crate) operation: crate::core::NodeId,
    pub(crate) instruction: crate::core::mir::MirInstructionId,
    pub(crate) values_binding_local: crate::core::ir::ResolvedLocalId,
    pub(crate) values: crate::core::mir::MirValueId,
    pub(crate) values_binding_point: crate::core::NodeId,
    pub(crate) projection: crate::core::NodeId,
    pub(crate) index_expression: crate::core::NodeId,
    pub(crate) index_type: crate::core::ResolvedTypeId,
    pub(crate) index: crate::core::mir::MirValueId,
    pub(crate) cast: crate::core::NodeId,
    pub(crate) conversion_kind: crate::core::ir::CheckedConversionKind,
    pub(crate) conversion_from: crate::core::ResolvedTypeId,
    pub(crate) conversion_to: crate::core::ResolvedTypeId,
    pub(crate) result: crate::core::mir::MirValueId,
    pub(crate) result_type: crate::core::ResolvedTypeId,
    pub(crate) result_binding_local: crate::core::ir::ResolvedLocalId,
    pub(crate) result_binding_point: crate::core::NodeId,
    pub(crate) result_binding: crate::core::mir::MirValueId,
    pub(crate) result_owner_settlement:
        crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind,
    pub(crate) result_owner_settlement_point: Option<crate::core::NodeId>,
    pub(crate) result_owner_settlement_index: Option<usize>,
    pub(crate) result_owner_settlement_instruction: Option<crate::core::mir::MirInstructionId>,
    pub(crate) result_owner_settlement_pc: Option<u32>,
    pub(crate) result_owner_settlement_reg: Option<Reg>,
    pub(crate) result_owner_settlement_arity: Option<u16>,
    /// Kept in the executable binding so VM preflight independently requires
    /// the Checker's zero-additional-use proof for whole-record owners.
    pub(crate) result_owner_whole_drop_unproved_uses:
        Option<std::collections::BTreeSet<crate::core::NodeId>>,
    pub(crate) result_owner_string_drop_source_uses:
        Option<std::collections::BTreeSet<crate::core::NodeId>>,
    pub(crate) result_owner_string_drop_statement_order: Option<usize>,
    pub(crate) result_owner_string_drop_reads:
        Option<Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackTextReadBinding>>,
    pub(crate) result_owner_field_move:
        Option<CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackFieldMoveBinding>,
    pub(crate) settlement: crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: crate::core::NodeId,
    pub(crate) settlement_instruction: crate::core::mir::MirInstructionId,
    pub(crate) payload_selection_digest: String,
    pub(crate) payload_type_desc_digest: String,
    /// Complete target descriptor copied from the independently validated
    /// MIR catalog. OOB sites may have no selected payload table entry, so
    /// their target identity must remain independently checkable here.
    pub(crate) target_type_desc: crate::core::mir::types::MirTypeDesc,
    /// Exact Checker/MIR disposition for the source index. A statically
    /// out-of-bounds access traps before inspecting any Values owner.
    pub(crate) index_disposition: crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) ra: Reg,
    pub(crate) index_reg: Reg,
    pub(crate) result_binding_pc: u32,
    pub(crate) result_binding_rd: Reg,
    pub(crate) result_binding_rs: Reg,
}

fn canonical_values_unpack_target_kind(
    descriptor: &crate::core::mir::types::MirTypeDesc,
) -> Option<CanonicalOrdinaryMapOwnedAnyV2PayloadKind> {
    use crate::core::mir::types::{
        MirAbiClass, MirGlueContract, MirGlueKind, MirLayout, MirOwnership, MirTypeKind,
    };

    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    if descriptor.id == i32_type
        && descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::I32)
        && descriptor.layout == MirLayout::Scalar
        && descriptor.ownership == MirOwnership::Copy
        && descriptor.abi
            == (MirAbiClass::Integer {
                bits: 32,
                signed: true,
            })
        && descriptor.session_protocol.is_none()
        && descriptor.native_record_abi.is_none()
        && !descriptor.needs_drop_glue
        && !descriptor.needs_clone_glue
        && descriptor.glue
            == (MirGlueContract {
                move_out: MirGlueKind::Noop,
                clone: MirGlueKind::Noop,
                drop: MirGlueKind::Noop,
            })
        && descriptor.drop_plan.is_none()
        && descriptor.variant_drop_plan.is_none()
    {
        return Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32);
    }
    if descriptor.id == string_type
        && descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::String)
        && descriptor.layout == MirLayout::Handle
        && descriptor.ownership == MirOwnership::Move
        && descriptor.abi == MirAbiClass::StringHandle
        && descriptor.session_protocol.is_none()
        && descriptor.native_record_abi.is_none()
        && descriptor.needs_drop_glue
        && descriptor.needs_clone_glue
        && descriptor.glue
            == (MirGlueContract {
                move_out: MirGlueKind::OwnedString,
                clone: MirGlueKind::OwnedString,
                drop: MirGlueKind::OwnedString,
            })
        && descriptor.drop_plan.is_none()
        && descriptor.variant_drop_plan.is_none()
    {
        return Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String);
    }
    if descriptor.kind != MirTypeKind::Nominal {
        return None;
    }
    let MirLayout::Record { nominal, fields } = &descriptor.layout else {
        return None;
    };
    if descriptor.ownership != MirOwnership::Move
        || descriptor.abi != MirAbiClass::Aggregate
        || descriptor.session_protocol.is_some()
        || descriptor.native_record_abi.is_some()
        || !descriptor.needs_drop_glue
        || !descriptor.needs_clone_glue
        || descriptor.glue
            != (MirGlueContract {
                move_out: MirGlueKind::Aggregate,
                clone: MirGlueKind::Aggregate,
                drop: MirGlueKind::Aggregate,
            })
        || descriptor.drop_plan.is_none()
        || descriptor.variant_drop_plan.is_some()
        || fields.len() != 1
        || fields[0].name != "text"
        || fields[0].ty != string_type
        || nominal.as_str().is_empty()
    {
        return None;
    }
    Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Record {
        nominal: nominal.as_str().to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackFieldMoveBinding {
    pub(crate) receipt: crate::core::mir::OrdinaryMapOwnedAnyV2ValuesUnpackFieldMoveResultReceipt,
    pub(crate) projection_pc: u32,
    pub(crate) projection_rd: Reg,
    pub(crate) projection_ra: Reg,
    pub(crate) text_binding_pc: u32,
    pub(crate) text_binding_rd: Reg,
    pub(crate) text_binding_rs: Reg,
    pub(crate) text_drop_pc: u32,
    pub(crate) text_drop_ra: Reg,
    pub(crate) text_read_uses: Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackTextReadBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackTextReadBinding {
    pub(crate) receipt: crate::core::mir::OrdinaryMapOwnedAnyV2ValuesUnpackTextReadResultReceipt,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) left_reg: Reg,
    pub(crate) right_reg: Reg,
    pub(crate) text_reg: Reg,
    pub(crate) peer_reg: Reg,
    pub(crate) peer_const_pc: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2TemporaryRootBinding {
    pub(crate) receipt: crate::core::mir::OrdinaryMapOwnedAnyV2TemporaryRootMirReceipt,
    pub(crate) new_pc: u32,
    pub(crate) get_any_pc: u32,
    pub(crate) drop_pc: u32,
    pub(crate) root_reg: Reg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2ValuesBinding {
    pub(crate) owner: crate::core::NodeId,
    pub(crate) operation: crate::core::NodeId,
    pub(crate) instruction: crate::core::mir::MirInstructionId,
    pub(crate) source_node: crate::core::NodeId,
    pub(crate) source_local: crate::core::ir::ResolvedLocalId,
    pub(crate) root: crate::core::mir::MirValueId,
    pub(crate) checker_source_type: crate::core::ResolvedTypeId,
    pub(crate) checker_result_type: crate::core::ResolvedTypeId,
    pub(crate) result: crate::core::mir::MirValueId,
    pub(crate) mir_result_type: crate::core::ResolvedTypeId,
    pub(crate) binding_local: crate::core::ir::ResolvedLocalId,
    pub(crate) binding: crate::core::mir::MirValueId,
    pub(crate) settlement: crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: crate::core::NodeId,
    pub(crate) settlement_instruction: crate::core::mir::MirInstructionId,
    pub(crate) payload_selection_digest: String,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) ra: Reg,
    pub(crate) binding_pc: u32,
    pub(crate) binding_rd: Reg,
    pub(crate) binding_rs: Reg,
    pub(crate) drop_pc: u32,
    pub(crate) drop_ra: Reg,
    pub(crate) borrowed_clones: Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesCloneBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOrdinaryMapOwnedAnyV2ValuesCloneBinding {
    pub(crate) instruction: crate::core::mir::MirInstructionId,
    pub(crate) source: crate::core::mir::MirValueId,
    pub(crate) result: crate::core::mir::MirValueId,
    pub(crate) clone_pc: u32,
    pub(crate) clone_rd: Reg,
    pub(crate) clone_ra: Reg,
    pub(crate) drop_pc: u32,
    pub(crate) drop_ra: Reg,
}

impl CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt {
    pub(crate) fn capture(entry: FuncIdx, functions: &[FunctionProto]) -> Self {
        let expected_has_ordinary_map_owned_any_v2 = functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops);
        Self {
            profile_version: 2,
            expected_has_ordinary_map_owned_any_v2,
            expected_program: if expected_has_ordinary_map_owned_any_v2 {
                ordinary_map_owned_any_v2_program_snapshot(entry, functions)
            } else {
                String::new()
            },
            payload_bindings: std::collections::BTreeMap::new(),
            selected_payload_bindings: std::collections::BTreeMap::new(),
            payload_selection_digest: None,
            get_any_bindings: std::collections::BTreeMap::new(),
            get_or_default_i32_bindings: std::collections::BTreeMap::new(),
            expected_get_or_default_i32_bindings: String::new(),
            get_or_default_string_bindings: std::collections::BTreeMap::new(),
            expected_get_or_default_string_bindings: String::new(),
            unpack_bindings: std::collections::BTreeMap::new(),
            expected_unpack_bindings: String::new(),
            values_unpack_bindings: std::collections::BTreeMap::new(),
            expected_values_unpack_bindings: String::new(),
            values_bindings: std::collections::BTreeMap::new(),
            expected_values_bindings: String::new(),
            values_producer_caller_bindings: Vec::new(),
            expected_values_producer_caller_bindings: String::new(),
            values_crosscall_string_caller_anchor: None,
            values_crosscall_string_caller_index1_anchor: None,
            map_root_producer_caller_bindings: Vec::new(),
            expected_map_root_producer_caller_bindings: String::new(),
            map_root_get_any_caller_bindings: Vec::new(),
            expected_map_root_get_any_caller_bindings: String::new(),
            println_anchor: None,
        }
    }

    pub(crate) fn bind_payload_bindings_from_selection(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2PayloadBinding>,
        >,
        selection_digest: Option<String>,
    ) -> Result<(), String> {
        let mut selected = std::collections::BTreeMap::new();
        for binding in bindings.values().flatten() {
            if let Some(previous) = selected.insert(binding.type_id.clone(), binding.clone()) {
                if previous.kind != binding.kind
                    || previous.type_desc_digest != binding.type_desc_digest
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 TypeDesc '{}' has conflicting selected payload kinds",
                        binding.type_id.as_str()
                    ));
                }
            }
        }
        self.payload_bindings = bindings;
        self.selected_payload_bindings = selected;
        self.payload_selection_digest = selection_digest;
        Ok(())
    }

    pub(crate) fn bind_get_any_bindings_from_selection(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2GetAnyBinding>,
        >,
    ) {
        self.get_any_bindings = bindings;
    }

    pub(crate) fn bind_get_or_default_i32_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32Binding>,
        >,
    ) -> Result<(), String> {
        if bindings
            .values()
            .flatten()
            .any(|binding| binding.payload_selection_digest != self.payload_selection_digest)
        {
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<i32> binding disagrees with the Core payload selection digest".into(),
            );
        }
        if !self.get_or_default_i32_bindings.is_empty() {
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<i32> bindings are already set".into(),
            );
        }
        self.get_or_default_i32_bindings = bindings;
        self.expected_get_or_default_i32_bindings = if self.get_or_default_i32_bindings.is_empty() {
            String::new()
        } else {
            format!("{:?}", self.get_or_default_i32_bindings)
        };
        Ok(())
    }

    pub(crate) fn bind_get_or_default_string_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringBinding>,
        >,
    ) -> Result<(), String> {
        if bindings
            .values()
            .flatten()
            .any(|binding| binding.payload_selection_digest != self.payload_selection_digest)
        {
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<String> binding disagrees with the Core payload selection digest".into(),
            );
        }
        if !self.get_or_default_string_bindings.is_empty() {
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<String> bindings are already set".into(),
            );
        }
        self.get_or_default_string_bindings = bindings;
        self.expected_get_or_default_string_bindings =
            if self.get_or_default_string_bindings.is_empty() {
                String::new()
            } else {
                format!("{:?}", self.get_or_default_string_bindings)
            };
        Ok(())
    }

    pub(crate) fn bind_unpack_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2UnpackBinding>,
        >,
    ) -> Result<(), String> {
        for binding in bindings.values().flatten() {
            if Some(binding.payload_selection_digest.as_str())
                != self.payload_selection_digest.as_deref()
            {
                return Err(
                    "ordinary Map/Any owner-v2 unpack binding disagrees with the Core payload selection digest".into(),
                );
            }
        }
        self.unpack_bindings = bindings;
        self.expected_unpack_bindings = if self.unpack_bindings.is_empty() {
            String::new()
        } else {
            format!("{:?}", self.unpack_bindings)
        };
        Ok(())
    }

    pub(crate) fn bind_values_unpack_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackBinding>,
        >,
    ) -> Result<(), String> {
        if bindings.values().flatten().any(|binding| {
            Some(binding.payload_selection_digest.as_str())
                != self.payload_selection_digest.as_deref()
        }) {
            return Err(
                "ordinary Map/Any owner-v2 Values unpack binding disagrees with the Core payload selection digest".into(),
            );
        }
        self.values_unpack_bindings = bindings;
        self.expected_values_unpack_bindings = if self.values_unpack_bindings.is_empty() {
            String::new()
        } else {
            format!("{:?}", self.values_unpack_bindings)
        };
        Ok(())
    }

    pub(crate) fn bind_values_bindings_from_core_receipts(
        &mut self,
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2ValuesBinding>,
        >,
    ) -> Result<(), String> {
        if bindings.values().flatten().any(|binding| {
            Some(binding.payload_selection_digest.as_str())
                != self.payload_selection_digest.as_deref()
        }) {
            return Err(
                "ordinary Map/Any owner-v2 Values binding disagrees with the Core payload selection digest".into(),
            );
        }
        self.values_bindings = bindings;
        // Keep an empty binding table as the canonical snapshot "{}" too.
        // Producer→caller routes emit a V2 Values opcode without a local
        // ExplicitDrop binding, so validation still compares this empty
        // table when those route-bound Values sites are present.
        self.expected_values_bindings = format!("{:?}", self.values_bindings);
        Ok(())
    }

    pub(crate) fn bind_values_producer_caller_bindings(
        &mut self,
        bindings: Vec<CanonicalValuesProducerCallerBytecodeBinding>,
    ) -> Result<(), String> {
        if !self.values_producer_caller_bindings.is_empty() {
            return Err(
                "ordinary Map/Any owner-v2 Values producer-caller bindings are already set".into(),
            );
        }
        self.values_producer_caller_bindings = bindings;
        self.expected_values_producer_caller_bindings =
            if self.values_producer_caller_bindings.is_empty() {
                String::new()
            } else {
                format!("{:?}", self.values_producer_caller_bindings)
            };
        Ok(())
    }

    pub(crate) fn bind_values_crosscall_string_caller_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt>,
        bindings: Vec<CanonicalValuesCrosscallStringCallerBytecodeBinding>,
    ) -> Result<(), String> {
        if self.values_crosscall_string_caller_anchor.is_some() {
            return Err("Values cross-call String-caller route anchor is already bound".into());
        }
        let anchor = CanonicalValuesCrosscallStringCallerRouteBytecodeAnchor::capture(
            entry,
            functions,
            route_receipt,
            proofs,
            bindings,
            self.payload_selection_digest.as_deref(),
        )?;
        self.expected_values_unpack_bindings = format!("{:?}", self.values_unpack_bindings);
        self.values_crosscall_string_caller_anchor = Some(anchor);
        Ok(())
    }

    pub(crate) fn bind_values_crosscall_string_caller_index1_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        route_receipt: &crate::core::mir::CanonicalMirRouteReceipt,
        proofs: Vec<crate::core::mir::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerMirReceipt>,
        bindings: Vec<CanonicalValuesCrosscallStringCallerIndex1BytecodeBinding>,
    ) -> Result<(), String> {
        if self.values_crosscall_string_caller_index1_anchor.is_some() {
            return Err(
                "Values cross-call String-caller Index1 route anchor is already bound".into(),
            );
        }
        if self.values_crosscall_string_caller_anchor.is_some() {
            return Err(
                "Values cross-call String-caller V1 and Index1 anchors cannot coexist".into(),
            );
        }
        let anchor = CanonicalValuesCrosscallStringCallerIndex1RouteBytecodeAnchor::capture(
            entry,
            functions,
            route_receipt,
            proofs,
            bindings,
            self.payload_selection_digest.as_deref(),
        )?;
        self.expected_values_unpack_bindings = format!("{:?}", self.values_unpack_bindings);
        self.values_crosscall_string_caller_index1_anchor = Some(anchor);
        Ok(())
    }

    pub(crate) fn bind_map_root_producer_caller_bindings(
        &mut self,
        bindings: Vec<CanonicalMapRootProducerCallerBytecodeBinding>,
    ) -> Result<(), String> {
        if !self.map_root_producer_caller_bindings.is_empty() {
            return Err(
                "ordinary Map/Any owner-v2 Map-root producer-caller bindings are already set"
                    .into(),
            );
        }
        self.map_root_producer_caller_bindings = bindings;
        self.expected_map_root_producer_caller_bindings =
            if self.map_root_producer_caller_bindings.is_empty() {
                String::new()
            } else {
                format!("{:?}", self.map_root_producer_caller_bindings)
            };
        Ok(())
    }

    pub(crate) fn bind_map_root_get_any_caller_bindings(
        &mut self,
        bindings: Vec<CanonicalMapRootGetAnyCallerBytecodeBinding>,
    ) -> Result<(), String> {
        if !self.map_root_get_any_caller_bindings.is_empty() || bindings.len() != 1 {
            return Err(
                "ordinary Map/Any owner-v2 Map-root GetAny-caller bindings must be set once with one bridge".into(),
            );
        }
        self.map_root_get_any_caller_bindings = bindings;
        self.expected_map_root_get_any_caller_bindings =
            format!("{:?}", self.map_root_get_any_caller_bindings);
        Ok(())
    }

    fn has_values_producer_caller_bindings(&self) -> bool {
        !self.values_producer_caller_bindings.is_empty()
    }

    fn has_get_or_default_i32_bindings(&self) -> bool {
        !self.get_or_default_i32_bindings.is_empty()
    }

    fn has_get_or_default_string_bindings(&self) -> bool {
        !self.get_or_default_string_bindings.is_empty()
    }

    fn has_values_crosscall_string_caller_anchor(&self) -> bool {
        self.values_crosscall_string_caller_anchor.is_some()
    }

    fn has_values_crosscall_string_caller_index1_anchor(&self) -> bool {
        self.values_crosscall_string_caller_index1_anchor.is_some()
    }

    fn has_map_root_producer_caller_bindings(&self) -> bool {
        !self.map_root_producer_caller_bindings.is_empty()
    }

    fn has_map_root_get_any_caller_bindings(&self) -> bool {
        !self.map_root_get_any_caller_bindings.is_empty()
    }

    fn bind_println_anchor(
        &mut self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        bindings: std::collections::BTreeMap<
            FuncIdx,
            Vec<CanonicalOrdinaryMapOwnedAnyV2PrintlnBinding>,
        >,
    ) -> Result<(), String> {
        let has_println_sites = functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_println_ops);
        if !has_println_sites && bindings.is_empty() {
            return Ok(());
        }
        let payload_selection_digest = self.payload_selection_digest.clone().ok_or_else(|| {
            "ordinary Map/Any owner-v2 Println has no payload selection digest".to_string()
        })?;
        let anchor = CanonicalOrdinaryMapOwnedAnyV2PrintlnAnchor {
            profile: "ordinary-map-owned-any-v2-println-v1",
            expected_entry: entry,
            expected_program: ordinary_map_owned_any_v2_program_snapshot(entry, functions),
            payload_selection_digest,
            expected_bindings: format!("{bindings:?}"),
            bindings,
        };
        anchor.validate(entry, functions, self)?;
        self.println_anchor = Some(anchor);
        Ok(())
    }

    pub(crate) fn payload_binding(
        &self,
        function: FuncIdx,
        set_ordinal: usize,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2PayloadBinding> {
        self.payload_bindings.get(&function)?.get(set_ordinal)
    }

    pub(crate) fn selected_payload_binding(
        &self,
        type_id: &crate::core::ResolvedTypeId,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2PayloadBinding> {
        self.selected_payload_bindings.get(type_id)
    }

    pub(crate) fn get_any_binding(
        &self,
        function: FuncIdx,
        get_any_ordinal: usize,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2GetAnyBinding> {
        self.get_any_bindings.get(&function)?.get(get_any_ordinal)
    }

    pub(crate) fn get_or_default_i32_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultI32Binding> {
        self.get_or_default_i32_bindings
            .get(&function)?
            .iter()
            .find(|binding| binding.pc == pc)
    }

    pub(crate) fn get_or_default_string_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2GetOrDefaultStringBinding> {
        self.get_or_default_string_bindings
            .get(&function)?
            .iter()
            .find(|binding| binding.pc == pc)
    }

    pub(crate) fn println_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2PrintlnBinding> {
        self.println_anchor
            .as_ref()?
            .bindings
            .get(&function)?
            .iter()
            .find(|binding| binding.pc == pc)
    }

    pub(crate) fn unpack_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2UnpackBinding> {
        self.unpack_bindings
            .get(&function)?
            .iter()
            .find(|binding| binding.pc == pc)
    }

    pub(crate) fn values_unpack_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2ValuesUnpackBinding> {
        self.values_unpack_bindings
            .get(&function)?
            .iter()
            .find(|binding| binding.pc == pc)
    }

    pub(crate) fn values_binding(
        &self,
        function: FuncIdx,
        values_ordinal: usize,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2ValuesBinding> {
        self.values_bindings.get(&function)?.get(values_ordinal)
    }

    pub(crate) fn values_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2ValuesBinding> {
        self.values_bindings
            .get(&function)?
            .iter()
            .find(|binding| binding.pc == pc)
    }

    pub(crate) fn values_drop_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2ValuesBinding> {
        self.values_bindings
            .get(&function)?
            .iter()
            .find(|binding| binding.drop_pc == pc)
    }

    pub(crate) fn values_clone_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<(
        &CanonicalOrdinaryMapOwnedAnyV2ValuesBinding,
        &CanonicalOrdinaryMapOwnedAnyV2ValuesCloneBinding,
    )> {
        self.values_bindings
            .get(&function)?
            .iter()
            .find_map(|binding| {
                binding
                    .borrowed_clones
                    .iter()
                    .find(|clone| clone.clone_pc == pc)
                    .map(|clone| (binding, clone))
            })
    }

    pub(crate) fn values_clone_drop_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<(
        &CanonicalOrdinaryMapOwnedAnyV2ValuesBinding,
        &CanonicalOrdinaryMapOwnedAnyV2ValuesCloneBinding,
    )> {
        self.values_bindings
            .get(&function)?
            .iter()
            .find_map(|binding| {
                binding
                    .borrowed_clones
                    .iter()
                    .find(|clone| clone.drop_pc == pc)
                    .map(|clone| (binding, clone))
            })
    }

    pub(crate) fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
    ) -> Result<(), String> {
        if self.profile_version != 2 {
            return Err(
                "ordinary Map/Any owner-v2 receipt has an unsupported profile version".into(),
            );
        }
        let has_v2 = functions
            .iter()
            .any(function_has_ordinary_map_owned_any_v2_ops);
        if !self.expected_has_ordinary_map_owned_any_v2 && !has_v2 {
            return if self.payload_bindings.is_empty()
                && self.selected_payload_bindings.is_empty()
                && self.payload_selection_digest.is_none()
                && self.get_any_bindings.is_empty()
                && self.get_or_default_i32_bindings.is_empty()
                && self.get_or_default_string_bindings.is_empty()
                && self.unpack_bindings.is_empty()
                && self.values_unpack_bindings.is_empty()
                && self.values_bindings.is_empty()
                && self.values_producer_caller_bindings.is_empty()
                && self.values_crosscall_string_caller_anchor.is_none()
                && self.values_crosscall_string_caller_index1_anchor.is_none()
                && self.map_root_producer_caller_bindings.is_empty()
                && self.map_root_get_any_caller_bindings.is_empty()
                && self.println_anchor.is_none()
            {
                Ok(())
            } else {
                Err("ordinary Map/Any owner-v2 payload bindings exist without V2 bytecode".into())
            };
        }
        if !self.expected_has_ordinary_map_owned_any_v2 && has_v2 {
            return Err(
                "ordinary Map/Any owner-v2 bytecode has no independent Checker receipt".into(),
            );
        }
        if functions.iter().any(function_has_map_root_ops)
            || functions.iter().any(function_has_map_any_owner_v1_ops)
            || functions
                .iter()
                .any(function_has_ordinary_map_owner_lifecycle_i32_string_v1_ops)
            || functions
                .iter()
                .any(function_has_ordinary_map_owned_get_i32_string_v1_ops)
        {
            return Err(
                "ordinary Map/Any owner-v2 receipt cannot authorize another Map profile".into(),
            );
        }
        if !has_v2
            || ordinary_map_owned_any_v2_program_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "ordinary Map/Any owner-v2 bytecode disagrees with its independent Checker receipt snapshot".into(),
            );
        }
        match &self.println_anchor {
            Some(anchor) => anchor.validate(entry, functions, self)?,
            None if functions
                .iter()
                .any(function_has_ordinary_map_owned_any_v2_println_ops) =>
            {
                return Err(
                    "ordinary Map/Any owner-v2 Println bytecode has no dedicated V2 anchor".into(),
                );
            }
            None => {}
        }
        match &self.values_crosscall_string_caller_anchor {
            Some(anchor) => {
                anchor.validate(entry, functions, self.payload_selection_digest.as_deref())?
            }
            None => {}
        }
        match &self.values_crosscall_string_caller_index1_anchor {
            Some(anchor) => {
                anchor.validate(entry, functions, self.payload_selection_digest.as_deref())?
            }
            None => {}
        }
        let has_values_sites = functions.iter().any(|function| {
            function.code.iter().any(|op| {
                matches!(
                    op,
                    Op::MirOrdinaryMapOwnedAnyV2Values { .. }
                        | Op::MirOrdinaryMapOwnedAnyV2ValuesClone { .. }
                        | Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }
                )
            })
        });
        if (has_values_sites
            || !self.values_bindings.is_empty()
            || !self.values_producer_caller_bindings.is_empty()
            || self.values_crosscall_string_caller_anchor.is_some())
            && format!("{:?}", self.values_bindings) != self.expected_values_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 Values receipts disagree with their immutable Core-derived binding snapshot".into(),
            );
        }
        let values_producer_caller_bindings_snapshot =
            if self.values_producer_caller_bindings.is_empty() {
                String::new()
            } else {
                format!("{:?}", self.values_producer_caller_bindings)
            };
        if values_producer_caller_bindings_snapshot != self.expected_values_producer_caller_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 Values producer-caller bindings disagree with their immutable route snapshot".into(),
            );
        }
        let map_root_producer_caller_bindings_snapshot =
            if self.map_root_producer_caller_bindings.is_empty() {
                String::new()
            } else {
                format!("{:?}", self.map_root_producer_caller_bindings)
            };
        if map_root_producer_caller_bindings_snapshot
            != self.expected_map_root_producer_caller_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 Map-root producer-caller bindings disagree with their immutable route snapshot".into(),
            );
        }
        let map_root_get_any_caller_bindings_snapshot =
            if self.map_root_get_any_caller_bindings.is_empty() {
                String::new()
            } else {
                format!("{:?}", self.map_root_get_any_caller_bindings)
            };
        if map_root_get_any_caller_bindings_snapshot
            != self.expected_map_root_get_any_caller_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 Map-root GetAny-caller bindings disagree with their immutable route snapshot".into(),
            );
        }
        let has_unpack_sites = functions.iter().any(|function| {
            function
                .code
                .iter()
                .any(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2Unpack { .. }))
        });
        if (has_unpack_sites || !self.unpack_bindings.is_empty())
            && format!("{:?}", self.unpack_bindings) != self.expected_unpack_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 unpack bindings disagree with their immutable Core/MIR snapshot".into(),
            );
        }

        let has_get_or_default_i32_sites = functions.iter().any(|function| {
            function
                .code
                .iter()
                .any(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 { .. }))
        });
        if (has_get_or_default_i32_sites || !self.get_or_default_i32_bindings.is_empty())
            && format!("{:?}", self.get_or_default_i32_bindings)
                != self.expected_get_or_default_i32_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<i32> bindings disagree with their immutable Core/MIR snapshot".into(),
            );
        }
        let has_get_or_default_string_sites = functions.iter().any(|function| {
            function
                .code
                .iter()
                .any(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultString { .. }))
        });
        if (has_get_or_default_string_sites || !self.get_or_default_string_bindings.is_empty())
            && format!("{:?}", self.get_or_default_string_bindings)
                != self.expected_get_or_default_string_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<String> bindings disagree with their immutable Core/MIR snapshot".into(),
            );
        }
        let has_values_unpack_sites = functions.iter().any(|function| {
            function
                .code
                .iter()
                .any(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }))
        });
        if (has_values_unpack_sites
            || !self.values_unpack_bindings.is_empty()
            || self.values_crosscall_string_caller_anchor.is_some()
            || self.values_crosscall_string_caller_index1_anchor.is_some())
            && format!("{:?}", self.values_unpack_bindings) != self.expected_values_unpack_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 Values unpack bindings disagree with their immutable Core/MIR snapshot".into(),
            );
        }

        let mut expected_binding_functions = std::collections::BTreeSet::new();
        let mut expected_get_any_functions = std::collections::BTreeSet::new();
        let mut expected_get_or_default_i32_functions = std::collections::BTreeSet::new();
        let mut expected_get_or_default_string_functions = std::collections::BTreeSet::new();
        let mut expected_unpack_functions = std::collections::BTreeSet::new();
        let mut expected_values_unpack_functions = std::collections::BTreeSet::new();
        let mut expected_values_functions = std::collections::BTreeSet::new();
        for (function_index, function) in functions.iter().enumerate() {
            let set_count = function
                .code
                .iter()
                .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2Set { .. }))
                .count();
            let function_index = FuncIdx::try_from(function_index)
                .map_err(|_| "ordinary Map/Any owner-v2 function index exceeds u32 ABI")?;
            if set_count > 0 {
                expected_binding_functions.insert(function_index);
                let bindings = self.payload_bindings.get(&function_index).ok_or_else(|| {
                    format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {set_count} Set site(s) but no MIR payload TypeDesc bindings"
                    )
                })?;
                if bindings.len() != set_count {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {set_count} Set site(s) but {} payload TypeDesc binding(s)",
                        bindings.len()
                    ));
                }
                for binding in bindings {
                    let type_id = binding.type_id.as_str();
                    if type_id.len() != 35
                        || !type_id.starts_with("rt:")
                        || !type_id[3..]
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 payload TypeDesc identity is not canonical: {type_id}"
                        ));
                    }
                    if !is_canonical_digest(&binding.type_desc_digest) {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 payload TypeDesc '{}' has a noncanonical digest",
                            binding.type_id.as_str()
                        ));
                    }
                    match &binding.kind {
                        CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Bool
                            if binding.type_id
                                != crate::core::ir::primitive_type_id(
                                    crate::core::PrimitiveType::Bool,
                                ) =>
                        {
                            return Err(
                                "ordinary Map/Any owner-v2 bool binding has a non-bool TypeDesc identity"
                                    .into(),
                            );
                        }
                        CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32
                            if binding.type_id
                                != crate::core::ir::primitive_type_id(
                                    crate::core::PrimitiveType::I32,
                                ) =>
                        {
                            return Err(
                            "ordinary Map/Any owner-v2 i32 binding has a non-i32 TypeDesc identity"
                                .into(),
                        );
                        }
                        CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String
                            if binding.type_id
                                != crate::core::ir::primitive_type_id(
                                    crate::core::PrimitiveType::String,
                                ) =>
                        {
                            return Err(
                                "ordinary Map/Any owner-v2 String binding has a non-String TypeDesc identity"
                                    .into(),
                            );
                        }
                        CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Record { nominal }
                            if nominal.is_empty() =>
                        {
                            return Err(
                            "ordinary Map/Any owner-v2 record binding has an empty nominal identity"
                                .into(),
                        );
                        }
                        _ => {}
                    }
                }
            }

            let get_any_sites = function
                .code
                .iter()
                .enumerate()
                .filter_map(|(pc, op)| match op {
                    Op::MirOrdinaryMapOwnedAnyV2GetAny { rd, ra, key } => {
                        Some((pc, *rd, *ra, *key))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !get_any_sites.is_empty() {
                expected_get_any_functions.insert(function_index);
                let get_any_bindings = self.get_any_bindings.get(&function_index).ok_or_else(|| {
                    format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} GetAny site(s) but no Checker result-settlement bindings",
                        get_any_sites.len()
                    )
                })?;
                if get_any_bindings.len() != get_any_sites.len() {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} GetAny site(s) but {} Checker result-settlement binding(s)",
                        get_any_sites.len(),
                        get_any_bindings.len()
                    ));
                }
                for ((pc, rd, ra, key_reg), binding) in get_any_sites.iter().zip(get_any_bindings) {
                    let temporary_root_valid = match &binding.temporary_root {
                        None => !binding
                            .source_local
                            .0
                             .0
                            .starts_with("ordinary-map-owned-any-v2:temporary-root:"),
                        Some(temporary_root) => {
                            let receipt = &temporary_root.receipt;
                            receipt.local == binding.source_local
                                && receipt.root == binding.root
                                && receipt.get_any == binding.operation
                                && receipt.get_any_instruction == binding.instruction
                                && receipt.source_node == binding.source_node
                                && receipt.source_type == binding.checker_source_type
                                && receipt.construction == receipt.source_node
                                && receipt.root_type == receipt.source_type
                                && receipt.settlement_point == receipt.get_any
                                && receipt.settlement
                                    == crate::core::OrdinaryMapOwnedAnyV2RootSettlementKind::ImmediateGetAnyDrop
                                && temporary_root.get_any_pc == binding.pc
                                && temporary_root.root_reg == binding.ra
                                && temporary_root.new_pc < binding.pc
                                && binding.pc.checked_add(1) == Some(temporary_root.drop_pc)
                                && matches!(
                                    function.code.get(temporary_root.new_pc as usize),
                                    Some(Op::MirOrdinaryMapOwnedAnyV2New { rd: new_root })
                                        if *new_root == temporary_root.root_reg
                                )
                                && matches!(
                                    function.code.get(temporary_root.drop_pc as usize),
                                    Some(Op::MirOrdinaryMapOwnedAnyV2Drop { ra: dropped_root })
                                        if *dropped_root == temporary_root.root_reg
                                )
                        }
                    };
                    if binding.pc
                        != u32::try_from(*pc).map_err(|_| {
                            "ordinary Map/Any owner-v2 GetAny PC exceeds u32 ABI".to_string()
                        })?
                        || binding.rd != *rd
                        || binding.ra != *ra
                        || binding.key_reg != *key_reg
                        || binding.mir_result_type
                            != crate::core::mir::types::ordinary_map_owned_any_v2_get_result_type_id(
                            )
                        || binding.miss_payload_type
                            != crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32)
                        || binding.settlement
                            != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                        || binding.owner.0.is_empty()
                        || binding.instruction.as_str().is_empty()
                        || binding.source_node.0.is_empty()
                        || binding.source_local.0 .0.is_empty()
                        || binding.root.as_str().is_empty()
                        || binding.checker_source_type.as_str().is_empty()
                        || binding.key_node.0.is_empty()
                        || binding.key.as_str().is_empty()
                        || binding.checker_key_type.as_str().is_empty()
                        || binding.checker_key_local.as_ref().is_some_and(|local| {
                            local.0 .0.is_empty()
                                || binding.key.as_str() != format!("local:{}", local.0 .0)
                        })
                        || (binding.checker_key_local.is_none()
                            && binding.key.as_str() != format!("expr:{}", binding.key_node.0))
                        || binding.result.as_str().is_empty()
                        || binding.checker_result_type.as_str().is_empty()
                        || binding.binding_local.0 .0.is_empty()
                        || binding.binding.as_str().is_empty()
                        || binding.settlement_point.0.is_empty()
                        || binding.settlement_instruction.as_str().is_empty()
                        || !is_canonical_digest(&binding.miss_payload_type_desc_digest)
                        || !temporary_root_valid
                    {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 GetAny binding at function {function_index} PC {} disagrees with the exact Checker site/type/settlement",
                            binding.pc
                        ));
                    }
                }
            }

            let get_or_default_i32_sites = function
                .code
                .iter()
                .enumerate()
                .filter_map(|(pc, op)| match op {
                    Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultI32 {
                        rd,
                        ra,
                        key,
                        default,
                    } => Some((pc, *rd, *ra, *key, *default)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !get_or_default_i32_sites.is_empty() {
                expected_get_or_default_i32_functions.insert(function_index);
                let bindings = self
                    .get_or_default_i32_bindings
                    .get(&function_index)
                    .ok_or_else(|| {
                        format!(
                            "ordinary Map/Any owner-v2 function {function_index} has {} GetOrDefault<i32> site(s) but no typed Core/MIR binding",
                            get_or_default_i32_sites.len()
                        )
                    })?;
                if bindings.len() != get_or_default_i32_sites.len() {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} GetOrDefault<i32> site(s) but {} typed Core/MIR binding(s)",
                        get_or_default_i32_sites.len(),
                        bindings.len()
                    ));
                }
                let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
                let string_type =
                    crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
                let map_type = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
                for ((pc, rd, ra, key_reg, default_reg), binding) in
                    get_or_default_i32_sites.iter().zip(bindings)
                {
                    let receipt = &binding.receipt;
                    receipt.validate()?;
                    let selected_i32_matches = self
                        .selected_payload_bindings
                        .get(&i32_type)
                        .is_none_or(|selected| {
                            selected.type_id == i32_type
                                && selected.kind == CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32
                                && selected.type_desc_digest == binding.target_type_desc_digest
                        });
                    let exact_registers = (*rd, *ra, *key_reg, *default_reg)
                        == (binding.rd, binding.ra, binding.key_reg, binding.default_reg);
                    if binding.pc
                        != u32::try_from(*pc).map_err(|_| {
                            "ordinary Map/Any owner-v2 GetOrDefault<i32> PC exceeds u32 ABI"
                                .to_string()
                        })?
                        || !exact_registers
                        || [binding.rd, binding.ra, binding.key_reg, binding.default_reg]
                            .iter()
                            .any(|reg| *reg >= function.register_count)
                        || binding.rd == binding.ra
                        || binding.rd == binding.key_reg
                        || binding.rd == binding.default_reg
                        || receipt.owner.0 != function.name
                        || receipt.root_type != map_type
                        || receipt.key_type != string_type
                        || receipt.type_argument != i32_type
                        || receipt.default_type != i32_type
                        || receipt.result_type != i32_type
                        || !is_canonical_digest(&binding.target_type_desc_digest)
                        || binding.payload_selection_digest != self.payload_selection_digest
                        || !selected_i32_matches
                    {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 GetOrDefault<i32> binding at function {function_index} PC {} disagrees with its typed MIR receipt, i32 TypeDesc, payload selection, or exact opcode/register site",
                            binding.pc
                        ));
                    }
                }
            }

            let get_or_default_string_sites = function
                .code
                .iter()
                .enumerate()
                .filter_map(|(pc, op)| match op {
                    Op::MirOrdinaryMapOwnedAnyV2GetOrDefaultString {
                        rd,
                        ra,
                        key,
                        default,
                    } => Some((pc, *rd, *ra, *key, *default)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !get_or_default_string_sites.is_empty() {
                expected_get_or_default_string_functions.insert(function_index);
                let bindings = self
                    .get_or_default_string_bindings
                    .get(&function_index)
                    .ok_or_else(|| {
                        format!(
                            "ordinary Map/Any owner-v2 function {function_index} has {} GetOrDefault<String> site(s) but no typed Core/MIR binding",
                            get_or_default_string_sites.len()
                        )
                    })?;
                if bindings.len() != get_or_default_string_sites.len() {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} GetOrDefault<String> site(s) but {} typed Core/MIR binding(s)",
                        get_or_default_string_sites.len(),
                        bindings.len()
                    ));
                }
                let string_type =
                    crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
                let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
                let map_type = crate::core::mir::types::ordinary_map_owned_any_v2_type_id();
                for ((pc, rd, ra, key_reg, default_reg), binding) in
                    get_or_default_string_sites.iter().zip(bindings)
                {
                    let receipt = &binding.receipt;
                    receipt.validate()?;
                    let selected_string_matches = self
                        .selected_payload_bindings
                        .get(&string_type)
                        .is_some_and(|selected| {
                            selected.type_id == string_type
                                && selected.kind
                                    == CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String
                                && selected.type_desc_digest == binding.target_type_desc_digest
                        });
                    let exact_registers = (*rd, *ra, *key_reg, *default_reg)
                        == (binding.rd, binding.ra, binding.key_reg, binding.default_reg);
                    if binding.pc
                        != u32::try_from(*pc).map_err(|_| {
                            "ordinary Map/Any owner-v2 GetOrDefault<String> PC exceeds u32 ABI"
                                .to_string()
                        })?
                        || !exact_registers
                        || [binding.rd, binding.ra, binding.key_reg, binding.default_reg]
                            .iter()
                            .any(|reg| *reg >= function.register_count)
                        || binding.rd == binding.ra
                        || binding.rd == binding.key_reg
                        || binding.rd == binding.default_reg
                        || receipt.owner.0 != function.name
                        || receipt.root_type != map_type
                        || receipt.key_type != string_type
                        || receipt.type_argument != string_type
                        || receipt.default_type != string_type
                        || receipt.result_type != string_type
                        || receipt.selected_payload_type_desc != string_type
                        || !is_canonical_digest(&binding.target_type_desc_digest)
                        || !is_canonical_digest(&binding.miss_type_desc_digest)
                        || receipt.miss_payload_type_desc != i32_type
                        || receipt.miss_payload_type_desc_digest != binding.miss_type_desc_digest
                        || binding.payload_selection_digest != self.payload_selection_digest
                        || !selected_string_matches
                    {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 GetOrDefault<String> binding at function {function_index} PC {} disagrees with its typed MIR receipt, String/i32 sentinel TypeDesc identities, payload selection, or exact opcode/register site",
                            binding.pc
                        ));
                    }
                }
            }

            let unpack_sites = function
                .code
                .iter()
                .enumerate()
                .filter_map(|(pc, op)| match op {
                    Op::MirOrdinaryMapOwnedAnyV2Unpack { rd, ra } => Some((pc, *rd, *ra)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !unpack_sites.is_empty() {
                expected_unpack_functions.insert(function_index);
                let unpack_bindings = self.unpack_bindings.get(&function_index).ok_or_else(|| {
                    format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} unpack site(s) but no Core/MIR binding",
                        unpack_sites.len()
                    )
                })?;
                if unpack_bindings.len() != unpack_sites.len() {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} unpack site(s) but {} Core/MIR binding(s)",
                        unpack_sites.len(),
                        unpack_bindings.len()
                    ));
                }
                let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
                let string_type =
                    crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
                for ((pc, rd, ra), binding) in unpack_sites.iter().zip(unpack_bindings) {
                    let get_any_binding = self
                        .get_any_bindings
                        .get(&function_index)
                        .into_iter()
                        .flatten()
                        .find(|get_any| {
                            get_any.owner == binding.owner
                                && get_any.operation == binding.operation
                                && get_any.instruction == binding.get_any_instruction
                                && get_any.binding == binding.pair
                                && get_any.binding_local == binding.pair_local
                                && get_any.settlement == binding.settlement
                                && get_any.settlement_point == binding.settlement_point
                                && get_any.settlement_instruction == binding.settlement_instruction
                                && get_any.miss_payload_type == binding.miss_payload_type
                        });
                    let selected = self.selected_payload_bindings.get(&binding.target_type);
                    let selected_target_matches = if binding.target_type == i32_type {
                        selected.is_some_and(|selected| {
                            selected.type_id == binding.target_type
                                && selected.type_desc_digest == binding.target_type_desc_digest
                                && selected.kind == CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32
                        })
                    } else if binding.target_type
                        == crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool)
                    {
                        selected.is_some_and(|selected| {
                            selected.type_id == binding.target_type
                                && selected.type_desc_digest == binding.target_type_desc_digest
                                && selected.kind == CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Bool
                        })
                    } else if binding.target_type == string_type {
                        selected.is_some_and(|selected| {
                            selected.type_id == binding.target_type
                                && selected.type_desc_digest == binding.target_type_desc_digest
                                && selected.kind
                                    == CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String
                        })
                    } else {
                        selected.is_some_and(|selected| {
                            selected.type_id == binding.target_type
                                && selected.type_desc_digest == binding.target_type_desc_digest
                                && matches!(
                                    &selected.kind,
                                    CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Record { nominal }
                                        if !nominal.is_empty()
                                )
                        })
                    };
                    let source_matches = match &binding.payload_source {
                        crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::SelectedSetPayload => {
                            selected_target_matches
                        }
                        crate::core::mir::OrdinaryMapOwnedAnyV2UnpackPayloadSource::CanonicalI32Miss => {
                            binding.target_type == i32_type
                                && get_any_binding.is_some_and(|get_any| {
                                    get_any.miss_payload_type == binding.target_type
                                        && get_any.miss_payload_type_desc_digest
                                            == binding.target_type_desc_digest
                                })
                        }
                    };
                    let get_any_binding_matches = get_any_binding.is_some_and(|get_any| {
                        binding.target_type != binding.miss_payload_type
                            || get_any.miss_payload_type_desc_digest
                                == binding.target_type_desc_digest
                    });
                    if binding.pc
                        != u32::try_from(*pc).map_err(|_| {
                            "ordinary Map/Any owner-v2 unpack PC exceeds u32 ABI".to_string()
                        })?
                        || binding.rd != *rd
                        || binding.ra != *ra
                        || binding.owner.0.is_empty()
                        || binding.owner.0 != function.name
                        || binding.operation.0.is_empty()
                        || binding.instruction.as_str().is_empty()
                        || binding.projection.0.is_empty()
                        || binding.pair.as_str().is_empty()
                        || binding.pair_local.0 .0.is_empty()
                        || binding.result.as_str().is_empty()
                        || !is_canonical_digest(&binding.target_type_desc_digest)
                        || binding.cast.0.is_empty()
                        || binding.conversion_kind
                            != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                        || binding.conversion_to != binding.target_type
                        || binding.conversion_from.as_str().is_empty()
                        || binding.get_any_instruction.as_str().is_empty()
                        || binding.settlement
                            != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                        || binding.settlement_point.0.is_empty()
                        || binding.settlement_instruction.as_str().is_empty()
                        || binding.miss_payload_type != i32_type
                        || Some(binding.payload_selection_digest.as_str())
                            != self.payload_selection_digest.as_deref()
                        || !get_any_binding_matches
                        || !source_matches
                    {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 unpack binding at function {function_index} PC {} disagrees with its Checker GetAny, MIR projection, exact target TypeDesc, or payload selection",
                            binding.pc
                        ));
                    }
                }
            }

            let values_sites = function
                .code
                .iter()
                .enumerate()
                .filter_map(|(pc, op)| match op {
                    Op::MirOrdinaryMapOwnedAnyV2Values { rd, ra } => Some((pc, *rd, *ra)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let values_clone_sites = function
                .code
                .iter()
                .enumerate()
                .filter_map(|(pc, op)| match op {
                    Op::MirOrdinaryMapOwnedAnyV2ValuesClone { rd, ra } => Some((pc, *rd, *ra)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let values_drop_sites = function
                .code
                .iter()
                .enumerate()
                .filter_map(|(pc, op)| match op {
                    Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { ra } => Some((pc, *ra)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let route_producer_bindings = self
                .values_producer_caller_bindings
                .iter()
                .filter(|binding| binding.producer_function == function_index)
                .collect::<Vec<_>>();
            let route_caller_bindings = self
                .values_producer_caller_bindings
                .iter()
                .filter(|binding| binding.caller_function == function_index)
                .collect::<Vec<_>>();
            let crosscall_anchor = self.values_crosscall_string_caller_anchor.as_ref();
            let crosscall_producer_bindings = crosscall_anchor
                .into_iter()
                .flat_map(|anchor| anchor.bindings.iter())
                .filter(|binding| binding.producer_function == function_index)
                .collect::<Vec<_>>();
            let crosscall_caller_bindings = crosscall_anchor
                .into_iter()
                .flat_map(|anchor| anchor.bindings.iter())
                .filter(|binding| binding.caller_function == function_index)
                .collect::<Vec<_>>();
            let crosscall_index1_anchor =
                self.values_crosscall_string_caller_index1_anchor.as_ref();
            let crosscall_index1_producer_bindings = crosscall_index1_anchor
                .into_iter()
                .flat_map(|anchor| anchor.bindings.iter())
                .filter(|binding| binding.producer_function == function_index)
                .collect::<Vec<_>>();
            let crosscall_index1_caller_bindings = crosscall_index1_anchor
                .into_iter()
                .flat_map(|anchor| anchor.bindings.iter())
                .filter(|binding| binding.caller_function == function_index)
                .collect::<Vec<_>>();
            let crosscall_index1_proof =
                crosscall_index1_anchor.and_then(|anchor| anchor.proofs.first());
            let mut route_values_pcs = std::collections::BTreeSet::new();
            for binding in &route_producer_bindings {
                let values_pc = usize::try_from(binding.producer_values_pc)
                    .map_err(|_| "Values producer opcode PC exceeds usize")?;
                let binding_move_pc = usize::try_from(binding.producer_binding_move_pc)
                    .map_err(|_| "Values producer binding Move PC exceeds usize")?;
                let return_move_pc = usize::try_from(binding.producer_return_move_pc)
                    .map_err(|_| "Values producer return Move PC exceeds usize")?;
                let return_pc = usize::try_from(binding.producer_return_pc)
                    .map_err(|_| "Values producer tail Return PC exceeds usize")?;
                if binding.producer_owner.0 != function.name
                    || binding.producer_result_type
                        != crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
                    || !(values_pc < binding_move_pc
                        && binding_move_pc < return_move_pc
                        && return_move_pc < return_pc)
                    || !route_values_pcs.insert(binding.producer_values_pc)
                    || !matches!(
                        function.code.get(values_pc),
                        Some(Op::MirOrdinaryMapOwnedAnyV2Values { rd, ra })
                            if (*rd, *ra) == (binding.producer_values_rd, binding.producer_values_ra)
                    )
                    || !matches!(
                        function.code.get(binding_move_pc),
                        Some(Op::Move { rd, rs })
                            if (*rd, *rs) == (binding.producer_binding_move_rd, binding.producer_binding_move_rs)
                    )
                    || !matches!(
                        function.code.get(return_move_pc),
                        Some(Op::Move { rd, rs })
                            if (*rd, *rs) == (binding.producer_return_move_rd, binding.producer_return_move_rs)
                    )
                    || !matches!(
                        function.code.get(return_pc),
                        Some(Op::Ret { ra }) if *ra == binding.producer_return_ra
                    )
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 producer Values at function {function_index} PC {} is not the exact route-bound Values→Move→Move→Ret chain",
                        binding.producer_values_pc
                    ));
                }
            }
            for binding in &crosscall_producer_bindings {
                let values_pc = binding.sites[5].pc;
                if binding.producer_owner.0 != function.name
                    || binding.call_target != binding.producer_owner
                    || !route_values_pcs.insert(values_pc)
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 cross-call producer Values at function {function_index} is not a unique source-owner site"
                    ));
                }
            }
            // The separate Index1 anchor has already validated its exact
            // producer Set cleanup IDs and all six generic String Drops.
            // Only its Values snapshot opcode belongs in this Values-opcode
            // census; its String Clone/Drop sites remain owned by that route.
            for binding in &crosscall_index1_producer_bindings {
                let proof = crosscall_index1_proof.ok_or_else(|| {
                    "Values cross-call String-caller Index1 producer binding has no proof"
                        .to_string()
                })?;
                let values_site = binding
                    .sites
                    .iter()
                    .find(|site| site.instruction == proof.producer_values_instruction)
                    .ok_or_else(|| {
                        "Values cross-call String-caller Index1 producer has no exact Values site"
                            .to_string()
                    })?;
                let values_pc = usize::try_from(values_site.pc)
                    .map_err(|_| "Values Index1 producer Values PC exceeds usize")?;
                if binding.producer_owner.0 != function.name
                    || binding.call_target != binding.producer_owner
                    || values_site.owner != binding.producer_owner
                    || values_site.function != function_index
                    || function.code.get(values_pc) != Some(&values_site.op)
                    || !matches!(&values_site.op, Op::MirOrdinaryMapOwnedAnyV2Values { .. })
                    || !route_values_pcs.insert(values_site.pc)
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 Index1 producer Values at function {function_index} is not the exact route-bound source-owner site"
                    ));
                }
            }
            let mut expected_clone_pcs = std::collections::BTreeSet::new();
            let mut expected_drop_pcs = std::collections::BTreeSet::new();
            for binding in &route_caller_bindings {
                let owner_drop_pc = usize::try_from(binding.owner_drop_pc)
                    .map_err(|_| "Values producer-caller Drop PC exceeds usize")?;
                if binding.caller_owner.0 != function.name
                    || !expected_drop_pcs.insert(binding.owner_drop_pc)
                    || !matches!(
                        function.code.get(owner_drop_pc),
                        Some(Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { ra })
                            if *ra == binding.owner_drop_ra
                    )
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 caller Drop at function {function_index} PC {} is not the exact route-bound owner settlement",
                        binding.owner_drop_pc
                    ));
                }
                match (
                    binding.len_receiver.as_ref(),
                    binding.len_receiver_type.as_ref(),
                    binding.len_receiver_clone_pc,
                    binding.len_receiver_clone_rd,
                    binding.len_receiver_clone_ra,
                    binding.len_receiver_drop_pc,
                    binding.len_receiver_drop_ra,
                ) {
                    (
                        Some(_),
                        Some(receiver_type),
                        Some(clone_pc),
                        Some(clone_rd),
                        Some(clone_ra),
                        Some(drop_pc),
                        Some(drop_ra),
                    ) => {
                        let clone_site = usize::try_from(clone_pc)
                            .map_err(|_| "Values caller len receiver Clone PC exceeds usize")?;
                        let drop_site = usize::try_from(drop_pc)
                            .map_err(|_| "Values caller len receiver Drop PC exceeds usize")?;
                        if receiver_type != &binding.owner_type
                            || clone_ra != binding.caller_binding_rd
                            || drop_ra != clone_rd
                            || !expected_clone_pcs.insert(clone_pc)
                            || !expected_drop_pcs.insert(drop_pc)
                            || !matches!(
                                function.code.get(clone_site),
                                Some(Op::MirOrdinaryMapOwnedAnyV2ValuesClone { rd, ra })
                                    if (*rd, *ra) == (clone_rd, clone_ra)
                            )
                            || !matches!(
                                function.code.get(drop_site),
                                Some(Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { ra })
                                    if *ra == drop_ra
                            )
                        {
                            return Err(format!(
                                "ordinary Map/Any owner-v2 len receiver for caller function {function_index} is not the exact V2 Clone/Drop pair"
                            ));
                        }
                    }
                    (None, None, None, None, None, None, None) => {}
                    _ => {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 caller function {function_index} has a partial Len receiver binding"
                        ));
                    }
                }
            }
            for binding in &crosscall_caller_bindings {
                let snapshot_drop = &binding.sites[20];
                if binding.caller_owner.0 != function.name
                    || snapshot_drop.function != function_index
                    || !expected_drop_pcs.insert(snapshot_drop.pc)
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 cross-call caller snapshot Drop at function {function_index} is not a unique caller-owner settlement"
                    ));
                }
            }
            for binding in &crosscall_index1_caller_bindings {
                let proof = crosscall_index1_proof.ok_or_else(|| {
                    "Values cross-call String-caller Index1 caller binding has no proof".to_string()
                })?;
                let snapshot_drop = binding
                    .sites
                    .iter()
                    .find(|site| site.instruction == proof.caller_snapshot_drop_instruction)
                    .ok_or_else(|| {
                        "Values cross-call String-caller Index1 caller has no exact snapshot Drop site"
                            .to_string()
                    })?;
                let snapshot_drop_pc = usize::try_from(snapshot_drop.pc)
                    .map_err(|_| "Values Index1 caller snapshot Drop PC exceeds usize")?;
                if binding.caller_owner.0 != function.name
                    || snapshot_drop.owner != binding.caller_owner
                    || snapshot_drop.function != function_index
                    || function.code.get(snapshot_drop_pc) != Some(&snapshot_drop.op)
                    || !matches!(
                        &snapshot_drop.op,
                        Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }
                    )
                    || !expected_drop_pcs.insert(snapshot_drop.pc)
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 Index1 caller snapshot Drop at function {function_index} is not the exact route-bound owner settlement"
                    ));
                }
            }
            if values_sites.is_empty() {
                let actual_drop_pcs = values_drop_sites
                    .iter()
                    .map(|(pc, _)| u32::try_from(*pc))
                    .collect::<Result<std::collections::BTreeSet<_>, _>>()
                    .map_err(|_| "ordinary Map/Any owner-v2 Values Drop PC exceeds u32 ABI")?;
                let actual_clone_pcs = values_clone_sites
                    .iter()
                    .map(|(pc, _, _)| u32::try_from(*pc))
                    .collect::<Result<std::collections::BTreeSet<_>, _>>()
                    .map_err(|_| "ordinary Map/Any owner-v2 Values Clone PC exceeds u32 ABI")?;
                if actual_clone_pcs != expected_clone_pcs || actual_drop_pcs != expected_drop_pcs {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has Values Clone/Drop sites without an exact local receipt or producer-caller route"
                    ));
                }
            }
            if !values_sites.is_empty() {
                expected_values_functions.insert(function_index);
                let explicit_values_sites = values_sites
                    .iter()
                    .copied()
                    .filter(|(pc, _, _)| {
                        u32::try_from(*pc)
                            .ok()
                            .is_none_or(|pc| !route_values_pcs.contains(&pc))
                    })
                    .collect::<Vec<_>>();
                let values_bindings = self
                    .values_bindings
                    .get(&function_index)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if values_bindings.len() != explicit_values_sites.len()
                    || values_sites.len() != explicit_values_sites.len() + route_values_pcs.len()
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} local Values site(s), {} explicit-Drop binding(s), and {} exact producer-return binding(s)",
                        explicit_values_sites.len(),
                        values_bindings.len(),
                        route_values_pcs.len()
                    ));
                }
                for ((pc, rd, ra), binding) in explicit_values_sites.iter().zip(values_bindings) {
                    let expected_pc = u32::try_from(*pc)
                        .map_err(|_| "ordinary Map/Any owner-v2 Values PC exceeds u32 ABI")?;
                    let move_matches = matches!(
                        function.code.get(binding.binding_pc as usize),
                        Some(Op::Move { rd, rs })
                            if *rd == binding.binding_rd && *rs == binding.binding_rs
                    );
                    let drop_matches = matches!(
                        function.code.get(binding.drop_pc as usize),
                        Some(Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { ra })
                            if *ra == binding.drop_ra
                    );
                    if binding.pc != expected_pc
                        || binding.rd != *rd
                        || binding.ra != *ra
                        || binding.mir_result_type
                            != crate::core::mir::types::ordinary_map_owned_any_v2_values_type_id()
                        || binding.settlement
                            != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                        || binding.owner.0.is_empty()
                        || binding.owner.0 != function.name
                        || binding.operation.0.is_empty()
                        || binding.instruction.as_str().is_empty()
                        || binding.source_node.0.is_empty()
                        || binding.source_local.0 .0.is_empty()
                        || binding.root.as_str().is_empty()
                        || binding.checker_source_type.as_str().is_empty()
                        || binding.checker_result_type.as_str().is_empty()
                        || binding.result.as_str().is_empty()
                        || binding.binding_local.0 .0.is_empty()
                        || binding.binding.as_str().is_empty()
                        || binding.settlement_point.0.is_empty()
                        || binding.settlement_instruction.as_str().is_empty()
                        || !is_canonical_digest(&binding.payload_selection_digest)
                        || Some(binding.payload_selection_digest.as_str())
                            != self.payload_selection_digest.as_deref()
                        || !move_matches
                        || !drop_matches
                    {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 Values binding at function {function_index} PC {} disagrees with the exact Checker source/result/Move/Drop/payload-selection sites",
                            binding.pc
                        ));
                    }
                    if !expected_drop_pcs.insert(binding.drop_pc) {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 function {function_index} repeats a Values result Drop PC"
                        ));
                    }
                    for clone in &binding.borrowed_clones {
                        let clone_pc = usize::try_from(clone.clone_pc).map_err(|_| {
                            "ordinary Map/Any owner-v2 Values Clone PC exceeds usize"
                        })?;
                        let drop_pc = usize::try_from(clone.drop_pc).map_err(|_| {
                            "ordinary Map/Any owner-v2 Values Clone Drop PC exceeds usize"
                        })?;
                        if clone.instruction.as_str().is_empty()
                            || clone.source != binding.binding
                            || clone.result == binding.binding
                            || clone.clone_rd == clone.clone_ra
                            || clone.clone_ra != binding.binding_rd
                            || clone.drop_ra != clone.clone_rd
                            || clone.clone_pc <= binding.binding_pc
                            || clone.clone_pc >= clone.drop_pc
                            || clone.drop_pc >= binding.drop_pc
                            || !expected_clone_pcs.insert(clone.clone_pc)
                            || !expected_drop_pcs.insert(clone.drop_pc)
                            || !matches!(
                                function.code.get(clone_pc),
                                Some(Op::MirOrdinaryMapOwnedAnyV2ValuesClone { rd, ra })
                                    if *rd == clone.clone_rd && *ra == clone.clone_ra
                            )
                            || !matches!(
                                function.code.get(drop_pc),
                                Some(Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { ra })
                                    if *ra == clone.drop_ra
                            )
                        {
                            return Err(format!(
                                "ordinary Map/Any owner-v2 Values Clone binding at function {function_index} PC {} disagrees with its exact source owner, destination, or temporary Drop",
                                clone.clone_pc
                            ));
                        }
                    }
                }
                let actual_clone_pcs = values_clone_sites
                    .iter()
                    .map(|(pc, _, _)| u32::try_from(*pc))
                    .collect::<Result<std::collections::BTreeSet<_>, _>>()
                    .map_err(|_| "ordinary Map/Any owner-v2 Values Clone PC exceeds u32 ABI")?;
                let actual_drop_pcs = values_drop_sites
                    .iter()
                    .map(|(pc, _)| u32::try_from(*pc))
                    .collect::<Result<std::collections::BTreeSet<_>, _>>()
                    .map_err(|_| "ordinary Map/Any owner-v2 Values Drop PC exceeds u32 ABI")?;
                if actual_clone_pcs != expected_clone_pcs || actual_drop_pcs != expected_drop_pcs {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has unbound Values Clone/Drop sites"
                    ));
                }
            }

            let values_unpack_sites = function
                .code
                .iter()
                .enumerate()
                .filter_map(|(pc, op)| match op {
                    Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { rd, ra, index } => {
                        Some((pc, *rd, *ra, *index))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            let mut crosscall_unpack_pcs = crosscall_caller_bindings
                .iter()
                .map(|binding| binding.sites[14].pc)
                .collect::<std::collections::BTreeSet<_>>();
            for binding in &crosscall_index1_caller_bindings {
                let proof = crosscall_index1_proof.ok_or_else(|| {
                    "Values cross-call String-caller Index1 caller binding has no proof".to_string()
                })?;
                let unpack_site = binding
                    .sites
                    .iter()
                    .find(|site| site.instruction == proof.caller_unpack_instruction)
                    .ok_or_else(|| {
                        "Values cross-call String-caller Index1 caller has no exact unpack site"
                            .to_string()
                    })?;
                let unpack_pc = usize::try_from(unpack_site.pc)
                    .map_err(|_| "Values Index1 caller unpack PC exceeds usize")?;
                if binding.caller_owner.0 != function.name
                    || unpack_site.owner != binding.caller_owner
                    || unpack_site.function != function_index
                    || function.code.get(unpack_pc) != Some(&unpack_site.op)
                    || !matches!(
                        &unpack_site.op,
                        Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                    )
                    || !crosscall_unpack_pcs.insert(unpack_site.pc)
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 Index1 caller unpack at function {function_index} is not the exact route-bound site"
                    ));
                }
            }
            let local_values_unpack_sites = values_unpack_sites
                .iter()
                .copied()
                .filter(|(pc, _, _, _)| {
                    u32::try_from(*pc)
                        .ok()
                        .is_none_or(|pc| !crosscall_unpack_pcs.contains(&pc))
                })
                .collect::<Vec<_>>();
            if !values_unpack_sites.is_empty() || !crosscall_unpack_pcs.is_empty() {
                expected_values_unpack_functions.insert(function_index);
                let bindings = self
                    .values_unpack_bindings
                    .get(&function_index)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if bindings.len() != local_values_unpack_sites.len()
                    || values_unpack_sites.len()
                        != local_values_unpack_sites.len() + crosscall_unpack_pcs.len()
                {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} local Values unpack site(s), {} local Core/MIR binding(s), {} total emitted Values unpack site(s), and {} cross-call String-caller site(s)",
                        local_values_unpack_sites.len(),
                        bindings.len(),
                        values_unpack_sites.len(),
                        crosscall_unpack_pcs.len()
                    ));
                }
                let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
                let i64_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I64);
                for ((pc, rd, ra, index_reg), binding) in
                    local_values_unpack_sites.iter().zip(bindings)
                {
                    let selected = self.selected_payload_bindings.get(&binding.conversion_to);
                    let selected_payload_matches = selected.is_some_and(|selected| {
                        selected.type_id == binding.conversion_to
                            && selected.type_desc_digest == binding.payload_type_desc_digest
                            && matches!(
                                &selected.kind,
                                CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32
                                    | CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String
                                    | CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Record { .. }
                            )
                    });
                    let out_of_bounds_target_kind =
                        canonical_values_unpack_target_kind(&binding.target_type_desc);
                    let target_kind = match binding.index_disposition {
                        crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::SelectedPayload => {
                            selected
                                .filter(|_| selected_payload_matches)
                                .map(|selected| selected.kind.clone())
                        }
                        crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::AlwaysOutOfBounds {
                            ..
                        } => {
                            let selected_agrees_with_target = selected.map_or(true, |selected| {
                                selected_payload_matches
                                    && out_of_bounds_target_kind.as_ref() == Some(&selected.kind)
                            });
                            (binding.index_disposition.is_canonical()
                                && selected_agrees_with_target)
                                .then_some(out_of_bounds_target_kind)
                                .flatten()
                        }
                    };
                    let target_type_desc_matches = binding.target_type_desc.id
                        == binding.conversion_to
                        && blake3::hash(binding.target_type_desc.canonical_text().as_bytes())
                            .to_hex()
                            .to_string()
                            == binding.payload_type_desc_digest;
                    let result_binding_matches = match target_kind.as_ref() {
                        Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32) => matches!(
                            function.code.get(binding.result_binding_pc as usize),
                            Some(Op::Mov { rd, rs })
                                if *rd == binding.result_binding_rd
                                    && *rs == binding.result_binding_rs
                        ),
                        Some(
                            CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String
                            | CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Record { .. },
                        ) => matches!(
                            function.code.get(binding.result_binding_pc as usize),
                            Some(Op::Move { rd, rs })
                                if *rd == binding.result_binding_rd
                                    && *rs == binding.result_binding_rs
                        ),
                        _ => false,
                    };
                    let disposition_matches = match binding.index_disposition {
                        crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::SelectedPayload => {
                            selected_payload_matches && target_kind.is_some()
                        }
                        crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::AlwaysOutOfBounds {
                            ..
                        } => binding.index_disposition.is_canonical() && target_kind.is_some(),
                    };
                    let result_owner_settlement_matches = match binding.result_owner_settlement {
                        crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::NoOwnedPayload => {
                            binding.conversion_to == crate::core::ir::primitive_type_id(
                                crate::core::PrimitiveType::I32,
                            ) && binding.result_owner_settlement_point.is_none()
                                && binding.result_owner_settlement_index.is_none()
                                && binding.result_owner_settlement_instruction.is_none()
                                && binding.result_owner_settlement_pc.is_none()
                                && binding.result_owner_settlement_reg.is_none()
                                && binding.result_owner_settlement_arity.is_none()
                                && binding.result_owner_whole_drop_unproved_uses.is_none()
                                && binding.result_owner_field_move.is_none()
                                && matches!(target_kind.as_ref(), Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32))
                        }
                        crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::CompilerOwnedStringDrop => {
                            let Some(pc) = binding.result_owner_settlement_pc else {
                                return Err(format!(
                                    "ordinary Map/Any owner-v2 Values unpack binding at function {function_index} PC {} lacks its String owner-Drop PC",
                                    binding.pc
                                ));
                            };
                            binding.conversion_to
                                == crate::core::ir::primitive_type_id(
                                    crate::core::PrimitiveType::String,
                                )
                                && binding.result_binding_rs == binding.rd
                                && binding.result_binding_pc > binding.pc
                                && pc > binding.result_binding_pc
                                && binding.result_owner_settlement_point.as_ref().is_some_and(|point| {
                                    binding.result_owner_settlement_instruction.as_ref().is_some_and(|instruction| {
                                        instruction
                                            .as_str()
                                            .strip_prefix("inst:drop.")
                                            .and_then(|suffix| suffix.split_once(':'))
                                            .is_some_and(|(role, anchor)| {
                                                role.parse::<usize>().is_ok_and(|index| role == index.to_string())
                                                    && anchor == point.0.as_str()
                                            })
                                    })
                                })
                                && binding.result_owner_settlement_index.is_none()
                                && binding
                                    .result_owner_settlement_instruction
                                    .as_ref()
                                    .is_some_and(|instruction| {
                                        !instruction.as_str().trim().is_empty()
                                    })
                                && binding.result_owner_settlement_reg
                                    == Some(binding.result_binding_rd)
                                && binding.result_owner_settlement_arity.is_none()
                                && binding.result_owner_whole_drop_unproved_uses.is_none()
                                && binding.result_owner_field_move.is_none()
                                && matches!(
                                    function.code.get(pc as usize),
                                    Some(Op::Drop { ra })
                                        if Some(*ra) == binding.result_owner_settlement_reg
                                )
                                && matches!(target_kind.as_ref(), Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String))
                        }
                        crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::ExplicitStringDrop => {
                            let Some(pc) = binding.result_owner_settlement_pc else {
                                return Err(format!(
                                    "ordinary Map/Any owner-v2 Values unpack binding at function {function_index} PC {} lacks its explicit String Drop PC",
                                    binding.pc
                                ));
                            };
                            let Some(reads) = binding.result_owner_string_drop_reads.as_ref() else {
                                return Err(format!(
                                    "ordinary Map/Any owner-v2 Values unpack binding at function {function_index} PC {} lacks its direct String read bindings",
                                    binding.pc
                                ));
                            };
                            let Some(source_uses) = binding.result_owner_string_drop_source_uses.as_ref() else {
                                return Err(format!(
                                    "ordinary Map/Any owner-v2 Values unpack binding at function {function_index} PC {} lacks its Checker String use set",
                                    binding.pc
                                ));
                            };
                            let Some(drop_order) = binding.result_owner_string_drop_statement_order else {
                                return Err(format!(
                                    "ordinary Map/Any owner-v2 Values unpack binding at function {function_index} PC {} lacks its source Drop order",
                                    binding.pc
                                ));
                            };
                            let mut exact_uses = std::collections::BTreeSet::new();
                            let mut previous_pc = binding.result_binding_pc;
                            let text_reads_match = reads.iter().all(|read| {
                                    let source = &read.receipt.source;
                                    let expected_opcode = if source.not_equal {
                                        matches!(
                                            function.code.get(read.pc as usize),
                                            Some(Op::Ne { rd, ra, rb })
                                                if *rd == read.rd
                                                    && *ra == read.left_reg
                                                    && *rb == read.right_reg
                                        )
                                    } else {
                                        matches!(
                                            function.code.get(read.pc as usize),
                                            Some(Op::Eq { rd, ra, rb })
                                                if *rd == read.rd
                                                    && *ra == read.left_reg
                                                    && *rb == read.right_reg
                                        )
                                    };
                                    let source_is_left = read.receipt.left.as_str()
                                        == format!("local:{}", binding.result_binding_local.0 .0);
                                    let source_is_right = read.receipt.right.as_str()
                                        == format!("local:{}", binding.result_binding_local.0 .0);
                                    let peer = crate::core::mir::MirValueId::new(format!(
                                        "expr:{}",
                                        source.peer.0
                                    ));
                                    let peer_matches = peer.as_ref().is_ok_and(|peer| {
                                        if source_is_left {
                                            &read.receipt.right == peer
                                                && read.left_reg == binding.result_binding_rd
                                                && read.right_reg == read.peer_reg
                                        } else if source_is_right {
                                            &read.receipt.left == peer
                                                && read.right_reg == binding.result_binding_rd
                                                && read.left_reg == read.peer_reg
                                        } else {
                                            false
                                        }
                                    });
                                    let peer_constant_matches = matches!(
                                        function.code.get(read.peer_const_pc as usize),
                                        Some(Op::LoadConst { rd, idx })
                                            if *rd == read.peer_reg
                                                && matches!(
                                                    function.constants.get(*idx as usize),
                                                    Some(ConstValue::Str(value))
                                                        if value == &source.peer_literal
                                                )
                                    );
                                    let source_is_exact = source.statement_order < drop_order
                                        && !source.load.0.trim().is_empty()
                                        && !source.operation.0.trim().is_empty()
                                        && !source.statement.0.trim().is_empty()
                                        && !source.peer.0.trim().is_empty()
                                        && exact_uses.insert(source.load.clone());
                                    let read_is_ordered = read.pc > previous_pc
                                        && read.pc < pc
                                        && read.receipt.instruction.as_str()
                                            == format!("inst:binary:{}", source.operation.0)
                                        && read.receipt.result.as_str()
                                            == format!("expr:{}", source.operation.0)
                                        && expected_opcode
                                        && peer_matches
                                        && peer_constant_matches;
                                    previous_pc = read.pc;
                                    source_is_exact && read_is_ordered
                                });
                            if let (Some(point), Some(place_index), Some(instruction)) = (
                                binding.result_owner_settlement_point.as_ref(),
                                binding.result_owner_settlement_index,
                                binding.result_owner_settlement_instruction.as_ref(),
                            ) {
                                exact_uses.insert(point.clone());
                                let expected_instruction = format!(
                                    "inst:drop.{place_index}:{}",
                                    point.0
                                );
                                binding.conversion_to
                                    == crate::core::ir::primitive_type_id(
                                        crate::core::PrimitiveType::String,
                                    )
                                    && binding.result_binding_rs == binding.rd
                                    && binding.result_binding_pc > binding.pc
                                    && pc > binding.result_binding_pc
                                    && instruction.as_str() == expected_instruction
                                    && source_uses == &exact_uses
                                    && binding.result_owner_settlement_reg
                                        == Some(binding.result_binding_rd)
                                    && binding.result_owner_settlement_arity.is_none()
                                    && binding.result_owner_whole_drop_unproved_uses.is_none()
                                    && binding.result_owner_field_move.is_none()
                                    && binding.result_owner_string_drop_reads.is_some()
                                    && text_reads_match
                                    && matches!(
                                        function.code.get(pc as usize),
                                        Some(Op::Drop { ra }) if Some(*ra) == binding.result_owner_settlement_reg
                                    )
                                    && matches!(target_kind.as_ref(), Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String))
                            } else {
                                false
                            }
                        }
                        crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::ExplicitWholeDrop => {
                            let Some(pc) = binding.result_owner_settlement_pc else {
                                return Err(format!(
                                    "ordinary Map/Any owner-v2 Values unpack binding at function {function_index} PC {} lacks its Label owner-Drop PC",
                                    binding.pc
                                ));
                            };
                            binding.result_owner_settlement_point.as_ref().is_some_and(|point| {
                                binding.result_owner_settlement_index.is_some_and(|place_index| {
                                    binding.result_owner_settlement_instruction.as_ref().is_some_and(|instruction| {
                                        instruction
                                            .as_str()
                                            .strip_prefix("inst:drop.")
                                            .and_then(|suffix| suffix.split_once(':'))
                                            .is_some_and(|(role, anchor)| {
                                                role.parse::<usize>().is_ok_and(|index| {
                                                    index == place_index
                                                        && role == index.to_string()
                                                        && anchor == point.0.as_str()
                                                })
                                            })
                                    })
                                })
                            })
                                && binding.result_binding_rs == binding.rd
                                && binding.result_binding_pc > binding.pc
                                && pc > binding.result_binding_pc
                                && binding
                                    .result_owner_settlement_instruction
                                    .as_ref()
                                    .is_some_and(|instruction| {
                                        !instruction.as_str().trim().is_empty()
                                    })
                                && binding.result_owner_settlement_reg
                                    == Some(binding.result_binding_rd)
                                && binding.result_owner_settlement_arity == Some(1)
                                && binding.result_owner_whole_drop_unproved_uses.as_ref().is_some_and(|uses| uses.is_empty())
                                && binding.result_owner_field_move.is_none()
                                && matches!(
                                    function.code.get(pc as usize),
                                    Some(Op::DropAggregate { ra, arity })
                                        if Some(*ra) == binding.result_owner_settlement_reg
                                            && Some(*arity) == binding.result_owner_settlement_arity
                                )
                                && matches!(target_kind.as_ref(), Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Record { .. }))
                        }
                        crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackOwnerSettlementKind::DirectStringFieldMove => {
                            let Some(field_move) = binding.result_owner_field_move.as_ref() else {
                                return Err(format!(
                                    "ordinary Map/Any owner-v2 Values unpack binding at function {function_index} PC {} has no exact field-move receipt",
                                    binding.pc
                                ));
                            };
                            let source = &field_move.receipt.source;
                            let expected_project = format!(
                                "inst:move_project:{}",
                                source.field_projection.0
                            );
                            let expected_bind = format!(
                                "inst:bind:{}",
                                source.text_binding_point.0
                            );
                            let expected_drop = format!(
                                "inst:drop.{}:{}",
                                source.text_settlement_index,
                                source.text_settlement_point.0
                            );
                            let text_value = crate::core::mir::MirValueId::new(format!(
                                "local:{}",
                                source.text_binding.0 .0
                            ));
                            let text_reads_match = field_move.receipt.text_read_uses.len()
                                == source.text_read_uses.len()
                                && field_move.text_read_uses.len() == source.text_read_uses.len()
                                && field_move
                                    .text_read_uses
                                    .iter()
                                    .zip(&field_move.receipt.text_read_uses)
                                    .zip(&source.text_read_uses)
                                    .enumerate()
                                    .all(|(index, ((read, mir_read), source_read))| {
                                        let expected_opcode = if source_read.not_equal {
                                            matches!(
                                                function.code.get(read.pc as usize),
                                                Some(Op::Ne { rd, ra, rb })
                                                    if *rd == read.rd
                                                        && *ra == read.left_reg
                                                        && *rb == read.right_reg
                                            )
                                        } else {
                                            matches!(
                                                function.code.get(read.pc as usize),
                                                Some(Op::Eq { rd, ra, rb })
                                                    if *rd == read.rd
                                                        && *ra == read.left_reg
                                                        && *rb == read.right_reg
                                            )
                                        };
                                        let source_on_left = text_value
                                            .as_ref()
                                            .is_ok_and(|value| &mir_read.left == value);
                                        let source_on_right = text_value
                                            .as_ref()
                                            .is_ok_and(|value| &mir_read.right == value);
                                        let peer_constant_matches = matches!(
                                            function.code.get(read.peer_const_pc as usize),
                                            Some(Op::LoadConst { rd, idx })
                                                if *rd == read.peer_reg
                                                    && matches!(
                                                        function.constants.get(*idx as usize),
                                                        Some(ConstValue::Str(value))
                                                            if value == &source_read.peer_literal
                                                    )
                                        );
                                        read.receipt == *mir_read
                                            && read.receipt.source == *source_read
                                            && read.pc > field_move.text_binding_pc
                                            && read.pc < field_move.text_drop_pc
                                            && (index == 0
                                                || field_move.text_read_uses[index - 1].pc < read.pc)
                                            && expected_opcode
                                            && peer_constant_matches
                                            && read.text_reg == field_move.text_binding_rd
                                            && if source_on_left {
                                                read.left_reg == read.text_reg
                                                    && read.right_reg == read.peer_reg
                                            } else if source_on_right {
                                                read.right_reg == read.text_reg
                                                    && read.left_reg == read.peer_reg
                                            } else {
                                                false
                                            }
                                    });
                            let projection_pc = field_move.projection_pc as usize;
                            let text_binding_pc = field_move.text_binding_pc as usize;
                            let text_drop_pc = field_move.text_drop_pc as usize;
                            source.field_name == "text"
                                && source.has_exact_source_use_sets()
                                && text_reads_match
                                && field_move.receipt.projection_instruction.as_str()
                                    == expected_project
                                && field_move.receipt.text_binding_instruction.as_str()
                                    == expected_bind
                                && field_move.receipt.text_settlement_instruction.as_str()
                                    == expected_drop
                                && binding.result_owner_settlement_instruction.as_ref()
                                    == Some(&field_move.receipt.text_settlement_instruction)
                                && binding.result_owner_settlement_point.as_ref()
                                    == Some(&source.text_settlement_point)
                                && binding.result_owner_settlement_index
                                    == Some(source.text_settlement_index)
                                && binding.result_owner_settlement_pc
                                    == Some(field_move.text_drop_pc)
                                && binding.result_owner_settlement_reg
                                    == Some(field_move.text_binding_rd)
                                && binding.result_owner_settlement_arity.is_none()
                                && binding.result_owner_whole_drop_unproved_uses.is_none()
                                && binding.result_binding_rs == binding.rd
                                && binding.result_binding_pc > binding.pc
                                && field_move.projection_pc > binding.result_binding_pc
                                && field_move.projection_ra == binding.result_binding_rd
                                && field_move.text_binding_pc > field_move.projection_pc
                                && field_move.text_binding_rs == field_move.projection_rd
                                && field_move.text_drop_pc > field_move.text_binding_pc
                                && field_move.text_drop_ra == field_move.text_binding_rd
                                && matches!(
                                    function.code.get(projection_pc),
                                    Some(Op::RecordMoveGet { rd, ra, field, contract: Some(contract) })
                                        if *rd == field_move.projection_rd
                                            && *ra == field_move.projection_ra
                                            && matches!(
                                                function.constants.get(*field as usize),
                                                Some(ConstValue::Str(name)) if name == "text"
                                            )
                                            && matches!(
                                                function.constants.get(*contract as usize),
                                                Some(ConstValue::RecordProjection(shape))
                                                    if shape.field == source.field
                                                        && shape.name == "text"
                                                        && shape.index == 0
                                                        && shape.arity == 1
                                            )
                                )
                                && matches!(
                                    function.code.get(text_binding_pc),
                                    Some(Op::Move { rd, rs })
                                        if *rd == field_move.text_binding_rd
                                            && *rs == field_move.text_binding_rs
                                )
                                && matches!(
                                    function.code.get(text_drop_pc),
                                    Some(Op::Drop { ra }) if *ra == field_move.text_drop_ra
                                )
                                && matches!(target_kind.as_ref(), Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Record { .. }))
                        }
                    };
                    let values_receipt_matches = self
                        .values_bindings
                        .get(&function_index)
                        .into_iter()
                        .flatten()
                        .any(|values| {
                            values.binding == binding.values
                                && values.binding_local == binding.values_binding_local
                                && values.operation == binding.operation
                                && values.settlement
                                    == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                                && values.settlement_point == binding.settlement_point
                                && values.settlement_instruction
                                    == binding.settlement_instruction
                                && values.payload_selection_digest
                                    == binding.payload_selection_digest
                        });
                    if binding.pc
                        != u32::try_from(*pc).map_err(|_| {
                            "ordinary Map/Any owner-v2 Values unpack PC exceeds u32 ABI"
                        })?
                        || binding.rd != *rd
                        || binding.ra != *ra
                        || binding.index_reg != *index_reg
                        || !result_binding_matches
                        || binding.result_binding_rs != binding.rd
                        || binding.result_binding_pc <= binding.pc
                        || binding.owner.0.is_empty()
                        || binding.owner.0 != function.name
                        || binding.operation.0.is_empty()
                        || binding.instruction.as_str().is_empty()
                        || binding.values_binding_local.0 .0.is_empty()
                        || binding.values.as_str().is_empty()
                        || binding.values_binding_point.0.is_empty()
                        || binding.projection.0.is_empty()
                        || binding.index_expression.0.is_empty()
                        || binding.index.as_str().is_empty()
                        || (binding.index_type != i32_type && binding.index_type != i64_type)
                        || binding.cast.0.is_empty()
                        || binding.conversion_kind
                            != crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                        || binding.conversion_from.as_str().is_empty()
                        || binding.conversion_to != binding.result_type
                        || binding.result.as_str().is_empty()
                        || binding.result_binding_local.0 .0.is_empty()
                        || binding.result_binding_point.0.is_empty()
                        || binding.result_binding.as_str().is_empty()
                        || !result_owner_settlement_matches
                        || !target_type_desc_matches
                        || !binding.index_disposition.is_canonical()
                        || binding.settlement
                            != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                        || binding.settlement_point.0.is_empty()
                        || binding.settlement_instruction.as_str().is_empty()
                        || !is_canonical_digest(&binding.payload_selection_digest)
                        || !is_canonical_digest(&binding.payload_type_desc_digest)
                        || Some(binding.payload_selection_digest.as_str())
                            != self.payload_selection_digest.as_deref()
                        || !disposition_matches
                        || !values_receipt_matches
                    {
                        return Err(format!(
                            "ordinary Map/Any owner-v2 Values unpack binding at function {function_index} PC {} disagrees with the exact snapshot, index, target TypeDesc, or payload selection",
                            binding.pc
                        ));
                    }
                }
            }
        }
        if self
            .payload_bindings
            .keys()
            .any(|function| !expected_binding_functions.contains(function))
        {
            return Err(
                "ordinary Map/Any owner-v2 receipt has payload bindings without a matching Set site".into(),
            );
        }
        if self
            .get_any_bindings
            .keys()
            .any(|function| !expected_get_any_functions.contains(function))
        {
            return Err(
                "ordinary Map/Any owner-v2 receipt has GetAny bindings without a matching site"
                    .into(),
            );
        }
        if self
            .get_or_default_i32_bindings
            .keys()
            .any(|function| !expected_get_or_default_i32_functions.contains(function))
        {
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<i32> bindings exist without a matching opcode site"
                    .into(),
            );
        }
        if self
            .get_or_default_string_bindings
            .keys()
            .any(|function| !expected_get_or_default_string_functions.contains(function))
        {
            return Err(
                "ordinary Map/Any owner-v2 GetOrDefault<String> bindings exist without a matching opcode site"
                    .into(),
            );
        }
        if self
            .unpack_bindings
            .keys()
            .any(|function| !expected_unpack_functions.contains(function))
        {
            return Err(
                "ordinary Map/Any owner-v2 receipt has unpack bindings without a matching site"
                    .into(),
            );
        }
        if self
            .values_bindings
            .keys()
            .any(|function| !expected_values_functions.contains(function))
        {
            return Err(
                "ordinary Map/Any owner-v2 receipt has Values bindings without a matching site"
                    .into(),
            );
        }
        if self
            .values_unpack_bindings
            .keys()
            .any(|function| !expected_values_unpack_functions.contains(function))
        {
            return Err(
                "ordinary Map/Any owner-v2 receipt has Values unpack bindings without a matching site".into(),
            );
        }
        let selected_from_sets = self
            .payload_bindings
            .values()
            .flatten()
            .map(|binding| {
                (
                    binding.type_id.clone(),
                    (binding.type_desc_digest.clone(), binding.kind.clone()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        if selected_from_sets.len() != self.selected_payload_bindings.len()
            || self
                .selected_payload_bindings
                .iter()
                .any(|(type_id, binding)| {
                    type_id != &binding.type_id
                        || selected_from_sets.get(type_id)
                            != Some(&(binding.type_desc_digest.clone(), binding.kind.clone()))
                })
        {
            return Err(
                "ordinary Map/Any owner-v2 selected TypeDesc table disagrees with Set bindings"
                    .into(),
            );
        }
        let has_receipted_read_selection = !self.get_any_bindings.is_empty()
            || !self.get_or_default_i32_bindings.is_empty()
            || !self.unpack_bindings.is_empty()
            || !self.values_bindings.is_empty()
            || !self.values_producer_caller_bindings.is_empty()
            || !self.values_unpack_bindings.is_empty()
            || self.values_crosscall_string_caller_anchor.is_some()
            || self.values_crosscall_string_caller_index1_anchor.is_some();
        if (!self.payload_bindings.is_empty() && self.payload_selection_digest.is_none())
            || (self.payload_bindings.is_empty()
                && self.payload_selection_digest.is_some()
                && !has_receipted_read_selection)
        {
            return Err(
                "ordinary Map/Any owner-v2 payload selection digest lacks Set or receipted read bindings"
                    .into(),
            );
        }
        if self
            .payload_selection_digest
            .as_deref()
            .is_some_and(|digest| !is_canonical_digest(digest))
        {
            return Err(
                "ordinary Map/Any owner-v2 payload selection digest is not canonical".into(),
            );
        }
        if !self.values_bindings.values().flatten().all(|binding| {
            Some(binding.payload_selection_digest.as_str())
                == self.payload_selection_digest.as_deref()
        }) {
            return Err(
                "ordinary Map/Any owner-v2 Values receipt disagrees with the exact payload selection digest".into(),
            );
        }
        Ok(())
    }
}

impl CanonicalOrdinaryMapOwnedAnyV2PrintlnAnchor {
    fn validate(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        owner_receipt: &CanonicalOrdinaryMapOwnedAnyV2BytecodeReceipt,
    ) -> Result<(), String> {
        const PROFILE: &str = "ordinary-map-owned-any-v2-println-v1";
        if self.profile != PROFILE {
            return Err(
                "ordinary Map/Any owner-v2 Println anchor has an unsupported profile".into(),
            );
        }
        if self.expected_entry != entry
            || ordinary_map_owned_any_v2_program_snapshot(entry, functions) != self.expected_program
        {
            return Err(
                "ordinary Map/Any owner-v2 Println anchor disagrees with its complete bytecode/entry snapshot".into(),
            );
        }
        if format!("{:?}", self.bindings) != self.expected_bindings {
            return Err(
                "ordinary Map/Any owner-v2 Println bindings disagree with their immutable Core receipt snapshot".into(),
            );
        }
        if Some(self.payload_selection_digest.as_str())
            != owner_receipt.payload_selection_digest.as_deref()
        {
            return Err(
                "ordinary Map/Any owner-v2 Println anchor disagrees with the V2 payload-selection digest".into(),
            );
        }

        let mut op_sites = std::collections::BTreeSet::new();
        for (function_index, function) in functions.iter().enumerate() {
            let function_index = FuncIdx::try_from(function_index)
                .map_err(|_| "ordinary Map/Any owner-v2 function index exceeds u32 ABI")?;
            for (pc, op) in function.code.iter().enumerate() {
                if matches!(op, Op::MirOrdinaryMapOwnedAnyV2Println { .. }) {
                    op_sites.insert((function_index, pc as u32));
                }
            }
        }
        let mut binding_sites = std::collections::BTreeSet::new();
        for (function_index, bindings) in &self.bindings {
            let function = functions
                .get(*function_index as usize)
                .ok_or("ordinary Map/Any owner-v2 Println binding names an absent function")?;
            for binding in bindings {
                binding.receipt.validate()?;
                if binding.receipt.payload_selection_digest != self.payload_selection_digest
                    || !binding_sites.insert((*function_index, binding.pc))
                {
                    return Err(
                        "ordinary Map/Any owner-v2 Println binding has a conflicting payload digest or duplicate site".into(),
                    );
                }
                let exact_op = matches!(
                    function.code.get(binding.pc as usize),
                    Some(Op::MirOrdinaryMapOwnedAnyV2Println { rd, pair })
                        if *rd == binding.rd && *pair == binding.pair
                );
                if !exact_op || binding.rd == binding.pair {
                    return Err(
                        "ordinary Map/Any owner-v2 Println binding disagrees with its exact opcode/register site".into(),
                    );
                }

                let get_any = &binding.receipt.get_any;
                let matching_get_any = owner_receipt
                    .get_any_bindings
                    .get(function_index)
                    .into_iter()
                    .flatten()
                    .filter(|get_binding| {
                        get_binding.owner == binding.receipt.owner
                            && get_binding.operation == get_any.operation
                            && get_binding.instruction == get_any.instruction
                            && get_binding.source_node == get_any.source_node
                            && get_binding.source_local == get_any.source_local
                            && get_binding.root == get_any.root
                            && get_binding.checker_source_type == get_any.checker_source_type
                            && get_binding.key_node == get_any.key_node
                            && get_binding.key == get_any.key
                            && get_binding.checker_key_type == get_any.checker_key_type
                            && get_binding.checker_key_local == get_any.checker_key_local
                            && get_binding.result == get_any.result
                            && get_binding.checker_result_type == get_any.checker_result_type
                            && get_binding.mir_result_type == get_any.mir_result_type
                            && get_binding.binding_local == get_any.binding_local
                            && get_binding.binding == binding.receipt.pair
                            && get_binding.settlement == get_any.settlement
                            && get_binding.settlement_point == get_any.settlement_point
                            && get_binding.settlement_instruction == get_any.settlement_instruction
                            && get_binding.miss_payload_type == get_any.miss_payload_type
                            && get_binding.miss_payload_type_desc_digest
                                == get_any.miss_payload_type_desc_digest
                    })
                    .collect::<Vec<_>>();
                if matching_get_any.len() != 1 {
                    return Err(
                        "ordinary Map/Any owner-v2 Println pair has no unique exact bytecode GetAny binding".into(),
                    );
                }

                let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
                let string_type =
                    crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
                let miss = &binding.receipt.get_any;
                let mut seen_types = std::collections::BTreeSet::new();
                for selected in &binding.receipt.selected_types {
                    if !seen_types.insert(selected.type_id.clone())
                        || selected.type_desc_digest.len() != 64
                        || !selected
                            .type_desc_digest
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    {
                        return Err(
                            "ordinary Map/Any owner-v2 Println selected TypeDesc identity is malformed".into(),
                        );
                    }
                    if let Some(selected_payload) =
                        owner_receipt.selected_payload_binding(&selected.type_id)
                    {
                        let payload_kind_matches = matches!(
                            (&selected_payload.kind, &selected.type_id),
                            (CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32, id) if id == &i32_type
                        ) || matches!(
                            (&selected_payload.kind, &selected.type_id),
                            (CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String, id)
                                if id == &string_type
                        );
                        if selected_payload.type_desc_digest != selected.type_desc_digest
                            || !payload_kind_matches
                        {
                            return Err(
                                "ordinary Map/Any owner-v2 Println selected TypeDesc disagrees with the V2 payload table".into(),
                            );
                        }
                    } else if selected.type_id != miss.miss_payload_type
                        || selected.type_desc_digest != miss.miss_payload_type_desc_digest
                        || selected.type_id != i32_type
                    {
                        return Err(
                            "ordinary Map/Any owner-v2 Println selection contains an unbound payload TypeDesc".into(),
                        );
                    }
                }
                if !binding
                    .receipt
                    .selected_types
                    .iter()
                    .any(|selected| selected.type_id == i32_type)
                {
                    return Err(
                        "ordinary Map/Any owner-v2 Println selection omits canonical i32 miss"
                            .into(),
                    );
                }
            }
        }
        if op_sites.is_empty() || op_sites != binding_sites {
            return Err(
                "ordinary Map/Any owner-v2 Println opcode and dedicated anchor site censuses differ".into(),
            );
        }
        Ok(())
    }
}

pub(crate) fn is_canonical_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Immutable compiler-side provenance for one canonical FFI call site.
///
/// `BytecodeProgram` intentionally keeps the executable tables public for
/// compatibility and inspection.  This private-to-the-crate snapshot gives
/// the VM a source of truth that those tables cannot rewrite from outside the
/// bytecode module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalFfiBinding {
    pub(crate) function: FuncIdx,
    pub(crate) pc: u32,
    pub(crate) extern_idx: u16,
    pub(crate) instruction: ConstIdx,
    /// The complete register-window operands emitted for this call site.
    /// Keeping them here prevents a mutable public instruction table from
    /// changing the value flow of a checker-owned FFI call after compilation.
    pub(crate) rd: Reg,
    pub(crate) args_base: Reg,
    pub(crate) argc: u16,
    pub(crate) param_count: u16,
    pub(crate) register_count: u16,
    pub(crate) mut_param_indices: Vec<u16>,
    pub(crate) has_requires: bool,
    pub(crate) has_ensures: bool,
    pub(crate) requires_funcs: Vec<FuncIdx>,
    pub(crate) ensures_funcs: Vec<FuncIdx>,
    pub(crate) instruction_text: String,
    pub(crate) descriptor: CanonicalFfiDescriptor,
    /// The route receipt used to admit this bytecode snapshot. Canonical
    /// callers pass it through the binding so a VM can verify that every
    /// call-site snapshot still belongs to one immutable route identity
    /// before loading any FFI symbol. Compatibility bytecode leaves this
    /// unset because it has no canonical route receipt.
    pub(crate) route_receipt: Option<crate::core::mir::CanonicalMirRouteReceipt>,
}

/// The deliberately narrow scalar ABI admitted by the Canonical MIR FFI
/// bytecode consumer.  This is a physical execution view of a MIR TypeDesc,
/// not a type-name lookup; the adapter constructs it only after validating the
/// checker-owned layout and ABI contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalFfiScalarType {
    I32,
    I64,
    Bool,
    F32,
    F64,
    Unit,
}

impl CanonicalFfiScalarType {
    pub(crate) fn from_mir_kind(kind: crate::core::mir::types::MirFfiScalarKind) -> Self {
        match kind {
            crate::core::mir::types::MirFfiScalarKind::I32 => Self::I32,
            crate::core::mir::types::MirFfiScalarKind::I64 => Self::I64,
            crate::core::mir::types::MirFfiScalarKind::Bool => Self::Bool,
            crate::core::mir::types::MirFfiScalarKind::F32 => Self::F32,
            crate::core::mir::types::MirFfiScalarKind::F64 => Self::F64,
            crate::core::mir::types::MirFfiScalarKind::Unit => Self::Unit,
        }
    }

    pub(crate) fn mir_kind(&self) -> crate::core::mir::types::MirFfiScalarKind {
        match self {
            Self::I32 => crate::core::mir::types::MirFfiScalarKind::I32,
            Self::I64 => crate::core::mir::types::MirFfiScalarKind::I64,
            Self::Bool => crate::core::mir::types::MirFfiScalarKind::Bool,
            Self::F32 => crate::core::mir::types::MirFfiScalarKind::F32,
            Self::F64 => crate::core::mir::types::MirFfiScalarKind::F64,
            Self::Unit => crate::core::mir::types::MirFfiScalarKind::Unit,
        }
    }

    pub(crate) fn abi_class(&self) -> crate::core::mir::types::MirAbiClass {
        self.mir_kind().abi_class()
    }
}

/// One checker-owned scalar FFI call descriptor for AST-free bytecode.
/// Descriptors are per MIR call receipt, so an Op index cannot silently reuse
/// a symbol with a different argument or result ABI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalFfiDescriptor {
    pub caller: String,
    pub instruction: String,
    pub callee: String,
    pub symbol: String,
    pub abi: String,
    /// Number of declaration parameters before the C ellipsis. This is not
    /// the bytecode call's actual operand count: `arguments` and
    /// `parameter_conversions` contain one entry per actual operand.
    pub fixed_argument_count: usize,
    /// Whether the declaration has a C variadic tail. The receipt remains
    /// explicit even when this particular call supplies no tail operands.
    pub variadic: bool,
    /// Physical ABI type for each actual call operand. The fixed prefix uses
    /// declaration parameter types; the tail uses checker-promoted types.
    pub arguments: Vec<CanonicalFfiScalarType>,
    /// Checker-owned conversion receipt for each actual operand, from its
    /// source MIR value ABI to the physical ABI type above.
    pub parameter_conversions: Vec<crate::core::mir::MirFfiAbiConversion>,
    /// Ordered checker-owned promotion receipts for the actual C variadic
    /// operands. Each index is absolute in `arguments` and therefore binds a
    /// source identity, source type, and physical promoted type to one slot.
    pub variadic_arguments: Vec<CanonicalFfiVariadicArgument>,
    pub result: CanonicalFfiScalarType,
    /// Checker-owned conversion receipt from declaration ABI to the MIR result
    /// ABI. Calls without a MIR result identity carry `None`; unit-return calls
    /// with a stable Unit result carry an explicit Unit→Unit conversion.
    pub result_conversion: Option<crate::core::mir::MirFfiAbiConversion>,
    /// MIR identities bind predicate leaves to this call's evaluated arguments.
    pub argument_ids: Vec<crate::core::mir::MirValueId>,
    /// Canonical predicate only: no AST expression or source-name environment.
    pub requires: Option<crate::core::mir::MirContractExpr>,
    /// MIR identity of the returned scalar, when the call returns a value.
    /// This binds an `ensures: result ...` predicate to the actual call output.
    pub result_id: Option<crate::core::mir::MirValueId>,
    /// Canonical postcondition only: no AST expression or source-name
    /// environment crosses into the bytecode VM.
    pub ensures: Option<crate::core::mir::MirContractExpr>,
}

/// One C default-promotion receipt carried into the AST-free bytecode VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalFfiVariadicArgument {
    pub argument_index: usize,
    pub source_node: crate::core::NodeId,
    /// Source ABI class, distinct from the physical promoted type because
    /// C's default promotions include i8/i16/u8/u16 source values.
    pub source_type: crate::core::mir::types::MirAbiClass,
    pub promoted_type: CanonicalFfiScalarType,
    pub promotion: crate::core::mir::MirFfiDefaultPromotion,
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

#[cfg(test)]
mod tests {
    use super::{
        function_has_managed_any_cfg_candidate, CanonicalManagedAnyCfgBytecodeReceipt,
        CanonicalMapAnyOwnerV1BytecodeReceipt, CanonicalMapRootBytecodeReceipt, ConstValue,
        FunctionProto, Op,
    };

    #[test]
    fn checked_constant_pool_indices_preserve_dedup_and_reject_u32_overflow() {
        let mut proto = FunctionProto::new("function:test".into(), 0);
        assert_eq!(
            proto.try_add_const(ConstValue::Int(7)),
            Ok(0),
            "checked insertion must return the first pool slot"
        );
        assert_eq!(
            proto.try_add_const(ConstValue::Int(7)),
            Ok(0),
            "checked insertion must preserve legacy integer deduplication"
        );
        assert_eq!(
            proto.try_add_const_raw(ConstValue::Int(7)),
            Ok(1),
            "raw insertion must remain non-deduplicating"
        );
        let first_unrepresentable = (u32::MAX as usize)
            .checked_add(1)
            .expect("test host must represent one value above u32::MAX");
        assert_eq!(
            FunctionProto::checked_const_idx(first_unrepresentable),
            Err("constant pool index exceeds u32 ABI")
        );
    }

    #[test]
    fn managed_any_opcode_is_a_cfg_candidate_without_its_contract_constant() {
        let mut proto = FunctionProto::new("function:main".into(), 1);
        proto.emit(Op::MirManagedAnyOp {
            rd: None,
            ra: 0,
            operation: crate::core::mir::MirManagedAnyOperation::Drop,
            contract: 0,
        });
        assert!(proto.constants.is_empty());
        assert!(function_has_managed_any_cfg_candidate(&proto));
    }

    #[test]
    fn managed_any_string_compare_is_a_cfg_candidate_without_a_contract_constant() {
        let mut proto = FunctionProto::new("function:main".into(), 3);
        proto.emit(Op::MirManagedAnyStringCompare {
            rd: 2,
            any: 0,
            string: 1,
            equal: true,
        });
        assert!(proto.constants.is_empty());
        assert!(function_has_managed_any_cfg_candidate(&proto));
        assert!(proto.code[0].reads_reg(0));
        assert!(proto.code[0].reads_reg(1));
        assert!(!proto.code[0].reads_reg(2));
        assert_eq!(
            proto.code[0].dest_reg(),
            None,
            "the checked comparison may trap"
        );
    }

    #[test]
    fn managed_any_println_opcode_is_a_cfg_candidate_without_a_contract_constant() {
        let mut proto = FunctionProto::new("function:main".into(), 2);
        proto.emit(Op::MirManagedAnyOp {
            rd: Some(1),
            ra: 0,
            operation: crate::core::mir::MirManagedAnyOperation::Println,
            contract: u32::MAX,
        });
        assert!(proto.constants.is_empty());
        assert!(function_has_managed_any_cfg_candidate(&proto));
    }

    #[test]
    fn cfg_child_removal_does_not_use_a_valid_map_receipt_as_authority() {
        let mut proto = FunctionProto::new("function:main".into(), 0);
        let rd = proto.alloc_reg();
        proto.emit(Op::MirMapAnyOwnerV1New { rd });
        let functions = vec![proto];
        let mut root = CanonicalMapRootBytecodeReceipt::capture(0, &functions);
        root.require_managed_any_cfg_receipt();

        let mut carrier = CanonicalMapAnyOwnerV1BytecodeReceipt::capture(0, &functions);
        let previous_receipts = carrier.existing_managed_any_receipts_snapshot();
        carrier.set_managed_any_cfg(CanonicalManagedAnyCfgBytecodeReceipt::capture(
            0,
            &functions,
            "checked source census|interprocedural-source-snapshot=None|expected-presence=false"
                .into(),
            Vec::new(),
            Vec::new(),
            previous_receipts,
            None,
            false,
        ));
        assert!(carrier.validate(0, &functions).is_ok());
        assert!(root.expects_managed_any_cfg_receipt());

        carrier.managed_any_cfg = None;
        assert!(carrier.validate(0, &functions).is_ok());
        assert!(carrier
            .validate_managed_any_cfg(0, &functions, true)
            .unwrap_err()
            .contains("missing its independent CFG receipt"));
    }

    #[test]
    fn full_cfg_snapshot_rejects_function_deletion_and_added_unreceipted_call() {
        let mut entry = FunctionProto::new("function:main".into(), 0);
        let rd = entry.alloc_reg();
        entry.emit(Op::LoadUnit { rd });
        let helper = FunctionProto::new("function:ordinary_helper".into(), 1);
        let functions = vec![entry, helper];
        let receipt = CanonicalManagedAnyCfgBytecodeReceipt::capture(
            0,
            &functions,
            "checked source census with no such call edge|interprocedural-source-snapshot=None|expected-presence=false"
                .into(),
            Vec::new(),
            Vec::new(),
            "prior receipts".into(),
            None,
            false,
        );
        assert!(receipt.validate(0, &functions, "prior receipts").is_ok());

        let mut removed_function = functions.clone();
        removed_function.pop();
        assert!(receipt
            .validate(0, &removed_function, "prior receipts")
            .unwrap_err()
            .contains("whole-program Checker snapshot"));

        let mut added_call = functions.clone();
        added_call[0].emit(Op::CallMove {
            rd: 0,
            func: 1,
            args_base: 0,
            argc: 1,
        });
        assert!(receipt
            .validate(0, &added_call, "prior receipts")
            .unwrap_err()
            .contains("whole-program Checker snapshot"));
    }

    #[test]
    fn cfg_source_plan_snapshot_binds_presence_and_exact_bytes_without_call_edges() {
        let functions = vec![FunctionProto::new("function:main".into(), 0)];
        let source_snapshot = Some("v2-source-plan;bindings=[];generation=7".to_owned());
        let marker =
            format!("|interprocedural-source-snapshot={source_snapshot:?}|expected-presence=true");
        let receipt = CanonicalManagedAnyCfgBytecodeReceipt::capture(
            0,
            &functions,
            format!("checked source census{marker}"),
            Vec::new(),
            Vec::new(),
            "prior receipt set".into(),
            source_snapshot,
            true,
        );
        assert!(receipt.validate(0, &functions, "prior receipt set").is_ok());

        let mut changed_source = receipt.clone();
        changed_source.interprocedural_source_snapshot =
            Some("v2-source-plan;bindings=[];generation=8".into());
        assert!(changed_source
            .validate(0, &functions, "prior receipt set")
            .unwrap_err()
            .contains("source-plan snapshot"));

        let mut missing_child = receipt;
        missing_child.interprocedural_source_snapshot = None;
        missing_child.expected_has_interprocedural_source_snapshot = false;
        assert!(missing_child
            .validate(0, &functions, "prior receipt set")
            .unwrap_err()
            .contains("source-plan snapshot"));
    }
}

#[derive(Debug, Clone)]
struct CanonicalOrdinaryMapNewResultBytecodeAnchor {
    profile: String,
    owner: String,
    source_commitment: String,
    entry: FuncIdx,
    root_move: (FuncIdx, u32, Reg, Reg),
    expected_program: String,
}

fn ordinary_map_new_result_root_move_site(
    functions: &[FunctionProto],
) -> Result<(FuncIdx, u32, Reg, Reg), String> {
    let mut sites = Vec::new();
    for (function_index, function) in functions.iter().enumerate() {
        let function_index = FuncIdx::try_from(function_index)
            .map_err(|_| "ordinary Map NewResult function index exceeds bytecode ABI")?;
        for (pc, op) in function.code.iter().enumerate() {
            if let Op::Move { rd, rs } = op {
                let pc = u32::try_from(pc)
                    .map_err(|_| "ordinary Map NewResult Move PC exceeds bytecode ABI")?;
                sites.push((function_index, pc, *rd, *rs));
            }
        }
    }
    match sites.as_slice() {
        [site] => Ok(*site),
        _ => Err("ordinary Map NewResult must lower to exactly one root payload Move site".into()),
    }
}

impl CanonicalOrdinaryMapNewResultBytecodeAnchor {
    fn validate(&self, entry: FuncIdx, functions: &[FunctionProto]) -> Result<(), String> {
        let root_move = ordinary_map_new_result_root_move_site(functions)?;
        if self.profile != crate::core::mir::ORDINARY_MAP_NEW_RESULT_PENDING_L1_PROFILE
            || self.entry != entry
            || entry != 0
            || functions.len() != 1
            || functions[0].name != self.owner
            || self.source_commitment.is_empty()
            || self.expected_program != format!("{functions:?}")
            || functions[0]
                .code
                .iter()
                .filter(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2NewResult { .. }))
                .count()
                != 1
            || self.root_move != root_move
            || functions[0].code.iter().any(|op| {
                matches!(
                    op,
                    Op::MirOrdinaryMapOwnedAnyV2New { .. }
                        | Op::MirOrdinaryMapOwnedAnyV2Clone { .. }
                        | Op::MirOrdinaryMapOwnedAnyV2Set { .. }
                        | Op::MirOrdinaryMapOwnedAnyV2Unpack { .. }
                )
            })
        {
            return Err(
                "ordinary Map NewResult bytecode disagrees with its independent pending-L1 receipt"
                    .into(),
            );
        }
        Ok(())
    }

    fn authorizes_root_move(
        &self,
        entry: FuncIdx,
        functions: &[FunctionProto],
        function_index: FuncIdx,
        pc: u32,
        rd: Reg,
        rs: Reg,
    ) -> Result<bool, String> {
        self.validate(entry, functions)?;
        Ok(self.root_move == (function_index, pc, rd, rs))
    }
}
