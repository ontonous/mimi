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
            | MirOrdinaryMapOwnedAnyV2Unpack { rd, .. }
            | MirOrdinaryMapOwnedAnyV2ValuesUnpack { rd, .. }
            | MirOrdinaryMapOwnedAnyV2Values { rd, .. }
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
            | Op::MirOrdinaryMapOwnedAnyV2Unpack { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { rd, .. }
            | Op::MirOrdinaryMapOwnedAnyV2Values { rd, .. }
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
            MirOrdinaryMapOwnedAnyV2Unpack { ra, .. } => *ra == reg,
            MirOrdinaryMapOwnedAnyV2ValuesUnpack { ra, index, .. } => *ra == reg || *index == reg,
            MirOrdinaryMapOwnedAnyV2Remove { ra, key, .. }
            | MirOrdinaryMapOwnedAnyV2Contains { ra, key, .. }
            | MirOrdinaryMapOwnedAnyV2GetAny { ra, key, .. } => *ra == reg || *key == reg,
            MirOrdinaryMapOwnedAnyV2Values { ra, .. }
            | MirOrdinaryMapOwnedAnyV2IsEmpty { ra, .. } => *ra == reg,
            NegInt { ra, .. }
            | NegFloat { ra, .. }
            | BitNot { ra, .. }
            | Not { ra, .. }
            | IntToFloat { ra, .. }
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
            | MirOrdinaryMapOwnedGetI32StringV1Clone { ra, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Size { ra, .. }
            | MirOrdinaryMapOwnedGetI32StringV1Drop { ra }
            | MirOrdinaryMapOwnedAnyV2Clone { ra, .. }
            | MirOrdinaryMapOwnedAnyV2Size { ra, .. }
            | MirOrdinaryMapOwnedAnyV2Drop { ra }
            | MirOrdinaryMapOwnedAnyV2ValuesDrop { ra }
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
    managed_any_identity: Option<CanonicalManagedAnyIdentityBytecodeReceipt>,
    managed_any_trap_cleanup: Option<CanonicalManagedAnyTrapCleanupBytecodeReceipt>,
    /// Independent whole-program Any/CFG bytecode seal.  The expected bit is
    /// deliberately separate from this optional child so deleting the CFG
    /// receipt cannot be masked by a still-valid Map snapshot.
    expected_managed_any_cfg_receipt: bool,
    managed_any_cfg: Option<CanonicalManagedAnyCfgBytecodeReceipt>,
}

impl CanonicalMapAnyOwnerV1BytecodeReceipt {
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

    pub(crate) fn existing_managed_any_receipts_snapshot(&self) -> String {
        format!(
            "map-any-owner-v1={}:{}|ordinary-map-owner={:?}|ordinary-map-owned-get={:?}|ordinary-map-owned-any-v2={:?}|managed-any-identity={:?}|managed-any-trap-cleanup={:?}",
            self.expected_has_map_any_owner_v1,
            self.expected_program,
            self.ordinary_map_owner_lifecycle_i32_string_v1,
            self.ordinary_map_owned_get_i32_string_v1,
            self.ordinary_map_owned_any_v2,
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

    pub(crate) fn ordinary_map_owned_any_v2_values_drop_binding_at_pc(
        &self,
        function: FuncIdx,
        pc: u32,
    ) -> Option<&CanonicalOrdinaryMapOwnedAnyV2ValuesBinding> {
        self.ordinary_map_owned_any_v2
            .as_ref()?
            .values_drop_binding_at_pc(function, pc)
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
                "Any/CFG bytecode receipt disagrees with the independent Map/identity/trap snapshots".into(),
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
        if managed_any_cfg_program_snapshot(entry, functions) != self.expected_program {
            return Err(
                "canonical Any/CFG bytecode disagrees with its whole-program Checker snapshot"
                    .into(),
            );
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
                | Op::MirOrdinaryMapOwnedAnyV2Clone { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Set { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Remove { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Size { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Contains { .. }
                | Op::MirOrdinaryMapOwnedAnyV2GetAny { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Unpack { .. }
                | Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Values { .. }
                | Op::MirOrdinaryMapOwnedAnyV2IsEmpty { .. }
                | Op::MirOrdinaryMapOwnedAnyV2Drop { .. }
                | Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }
        )
    })
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalOrdinaryMapOwnedAnyV2PayloadKind {
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
    pub(crate) settlement: crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind,
    pub(crate) settlement_point: crate::core::NodeId,
    pub(crate) settlement_instruction: crate::core::mir::MirInstructionId,
    pub(crate) payload_selection_digest: String,
    pub(crate) payload_type_desc_digest: String,
    pub(crate) pc: u32,
    pub(crate) rd: Reg,
    pub(crate) ra: Reg,
    pub(crate) index_reg: Reg,
    pub(crate) result_binding_pc: u32,
    pub(crate) result_binding_rd: Reg,
    pub(crate) result_binding_rs: Reg,
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
            unpack_bindings: std::collections::BTreeMap::new(),
            expected_unpack_bindings: String::new(),
            values_unpack_bindings: std::collections::BTreeMap::new(),
            expected_values_unpack_bindings: String::new(),
            values_bindings: std::collections::BTreeMap::new(),
            expected_values_bindings: String::new(),
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
        self.expected_values_bindings = if self.values_bindings.is_empty() {
            String::new()
        } else {
            format!("{:?}", self.values_bindings)
        };
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
                && self.unpack_bindings.is_empty()
                && self.values_unpack_bindings.is_empty()
                && self.values_bindings.is_empty()
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
        let has_values_sites = functions.iter().any(|function| {
            function.code.iter().any(|op| {
                matches!(
                    op,
                    Op::MirOrdinaryMapOwnedAnyV2Values { .. }
                        | Op::MirOrdinaryMapOwnedAnyV2ValuesDrop { .. }
                )
            })
        });
        if (has_values_sites || !self.values_bindings.is_empty())
            && format!("{:?}", self.values_bindings) != self.expected_values_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 Values receipts disagree with their immutable Core-derived binding snapshot".into(),
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
        let has_values_unpack_sites = functions.iter().any(|function| {
            function
                .code
                .iter()
                .any(|op| matches!(op, Op::MirOrdinaryMapOwnedAnyV2ValuesUnpack { .. }))
        });
        if (has_values_unpack_sites || !self.values_unpack_bindings.is_empty())
            && format!("{:?}", self.values_unpack_bindings) != self.expected_values_unpack_bindings
        {
            return Err(
                "ordinary Map/Any owner-v2 Values unpack bindings disagree with their immutable Core/MIR snapshot".into(),
            );
        }

        let mut expected_binding_functions = std::collections::BTreeSet::new();
        let mut expected_get_any_functions = std::collections::BTreeSet::new();
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
            if !values_sites.is_empty() {
                expected_values_functions.insert(function_index);
                let values_bindings = self.values_bindings.get(&function_index).ok_or_else(|| {
                    format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} Values site(s) but no Checker result-settlement bindings",
                        values_sites.len()
                    )
                })?;
                if values_bindings.len() != values_sites.len() {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} Values site(s) but {} Checker result-settlement binding(s)",
                        values_sites.len(),
                        values_bindings.len()
                    ));
                }
                for ((pc, rd, ra), binding) in values_sites.iter().zip(values_bindings) {
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
            if !values_unpack_sites.is_empty() {
                expected_values_unpack_functions.insert(function_index);
                let bindings = self
                    .values_unpack_bindings
                    .get(&function_index)
                    .ok_or_else(|| {
                        format!(
                            "ordinary Map/Any owner-v2 function {function_index} has {} Values unpack site(s) but no Core/MIR bindings",
                            values_unpack_sites.len()
                        )
                    })?;
                if bindings.len() != values_unpack_sites.len() {
                    return Err(format!(
                        "ordinary Map/Any owner-v2 function {function_index} has {} Values unpack site(s) but {} Core/MIR bindings",
                        values_unpack_sites.len(),
                        bindings.len()
                    ));
                }
                let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
                let i64_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I64);
                for ((pc, rd, ra, index_reg), binding) in values_unpack_sites.iter().zip(bindings) {
                    let selected = self.selected_payload_bindings.get(&binding.conversion_to);
                    let result_binding_matches = match selected.map(|selected| &selected.kind) {
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
                        None => false,
                    };
                    let selected_target_matches = selected.is_some_and(|selected| {
                        selected.type_id == binding.conversion_to
                            && selected.type_desc_digest == binding.payload_type_desc_digest
                            && matches!(
                                &selected.kind,
                                CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32
                                    | CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String
                                    | CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Record { .. }
                            )
                    });
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
                                && matches!(
                                    selected.map(|selected| &selected.kind),
                                    Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::I32)
                                )
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
                                && matches!(
                                    function.code.get(pc as usize),
                                    Some(Op::Drop { ra })
                                        if Some(*ra) == binding.result_owner_settlement_reg
                                )
                                && matches!(
                                    selected.map(|selected| &selected.kind),
                                    Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::String)
                                )
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
                                && matches!(
                                    function.code.get(pc as usize),
                                    Some(Op::DropAggregate { ra, arity })
                                        if Some(*ra) == binding.result_owner_settlement_reg
                                            && Some(*arity) == binding.result_owner_settlement_arity
                                )
                                && matches!(
                                    selected.map(|selected| &selected.kind),
                                    Some(CanonicalOrdinaryMapOwnedAnyV2PayloadKind::Record { .. })
                                )
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
                        || binding.settlement
                            != crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                        || binding.settlement_point.0.is_empty()
                        || binding.settlement_instruction.as_str().is_empty()
                        || !is_canonical_digest(&binding.payload_selection_digest)
                        || !is_canonical_digest(&binding.payload_type_desc_digest)
                        || Some(binding.payload_selection_digest.as_str())
                            != self.payload_selection_digest.as_deref()
                        || !selected_target_matches
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
        if self.payload_bindings.is_empty() != self.payload_selection_digest.is_none() {
            return Err(
                "ordinary Map/Any owner-v2 payload selection digest disagrees with Set bindings"
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

fn is_canonical_digest(value: &str) -> bool {
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
