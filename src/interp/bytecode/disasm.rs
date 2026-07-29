//! Bytecode disassembler.
//!
//! Provides human-readable output of compiled bytecode for debugging.
//! Uses the opcode metadata table for formatting.

use super::instr::*;

/// Operand format for disassembly display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandFormat {
    /// rd, ra, rb (three registers)
    RdRaRb,
    /// rd, ra (two registers)
    RdRa,
    /// rd only
    Rd,
    /// rd + constant index
    RdConst,
    /// jump offset
    Jump,
    /// jump offset + register
    JumpReg,
    /// call: rd, func/builtin, args_base, argc
    Call,
    /// data: rd + base + count
    Data,
    /// field: rd, ra, field_idx
    Field,
    /// no operands
    None,
    /// special (handled per-opcode)
    Special,
}

/// Opcode metadata for disassembly.
pub struct OpDesc {
    pub name: &'static str,
    pub format: OperandFormat,
}

/// Get the display name for an opcode.
pub fn op_name(op: &Op) -> &'static str {
    match op {
        Op::LoadConst { .. } => "LOAD_CONST",
        Op::LoadUnit { .. } => "LOAD_UNIT",
        Op::LoadTrue { .. } => "LOAD_TRUE",
        Op::LoadFalse { .. } => "LOAD_FALSE",
        Op::Mov { .. } => "MOV",
        Op::AddInt { .. } => "ADD_INT",
        Op::SubInt { .. } => "SUB_INT",
        Op::MulInt { .. } => "MUL_INT",
        Op::DivInt { .. } => "DIV_INT",
        Op::ModInt { .. } => "MOD_INT",
        Op::NegInt { .. } => "NEG_INT",
        Op::AddFloat { .. } => "ADD_FLOAT",
        Op::SubFloat { .. } => "SUB_FLOAT",
        Op::MulFloat { .. } => "MUL_FLOAT",
        Op::DivFloat { .. } => "DIV_FLOAT",
        Op::NegFloat { .. } => "NEG_FLOAT",
        Op::IntToFloat { .. } => "INT_TO_FLOAT",
        Op::EqInt { .. } => "EQ_INT",
        Op::NeInt { .. } => "NE_INT",
        Op::LtInt { .. } => "LT_INT",
        Op::GtInt { .. } => "GT_INT",
        Op::LeInt { .. } => "LE_INT",
        Op::GeInt { .. } => "GE_INT",
        Op::EqFloat { .. } => "EQ_FLOAT",
        Op::LtFloat { .. } => "LT_FLOAT",
        Op::GtFloat { .. } => "GT_FLOAT",
        Op::LeFloat { .. } => "LE_FLOAT",
        Op::GeFloat { .. } => "GE_FLOAT",
        Op::Eq { .. } => "EQ",
        Op::Ne { .. } => "NE",
        Op::BitAnd { .. } => "BIT_AND",
        Op::BitOr { .. } => "BIT_OR",
        Op::BitXor { .. } => "BIT_XOR",
        Op::Shl { .. } => "SHL",
        Op::Shr { .. } => "SHR",
        Op::BitNot { .. } => "BIT_NOT",
        Op::Not { .. } => "NOT",
        Op::And { .. } => "AND",
        Op::Or { .. } => "OR",
        Op::ConcatStr { .. } => "CONCAT_STR",
        Op::Jmp { .. } => "JMP",
        Op::JmpIf { .. } => "JMP_IF",
        Op::JmpIfNot { .. } => "JMP_IF_NOT",
        Op::Call { .. } => "CALL",
        Op::CallBuiltin { .. } => "CALL_BUILTIN",
        Op::CallIndirect { .. } => "CALL_INDIRECT",
        Op::Ret { .. } => "RET",
        Op::RetUnit => "RET_UNIT",
        Op::NewList { .. } => "NEW_LIST",
        Op::ListPush { .. } => "LIST_PUSH",
        Op::ListGet { .. } => "LIST_GET",
        Op::ListSet { .. } => "LIST_SET",
        Op::Len { .. } => "LEN",
        Op::NewTuple { .. } => "NEW_TUPLE",
        Op::TupleGet { .. } => "TUPLE_GET",
        Op::NewRecord { .. } => "NEW_RECORD",
        Op::RecordGet { .. } => "RECORD_GET",
        Op::RecordSet { .. } => "RECORD_SET",
        Op::NewMap { .. } => "NEW_MAP",
        Op::NewSet { .. } => "NEW_SET",
        Op::NewVariant { .. } => "NEW_VARIANT",
        Op::VariantTag { .. } => "VARIANT_TAG",
        Op::VariantPayload { .. } => "VARIANT_PAYLOAD",
        Op::IsVariant { .. } => "IS_VARIANT",
        Op::VariantGet { .. } => "VARIANT_GET",
        Op::Some { .. } => "SOME",
        Op::None { .. } => "NONE",
        Op::Ok { .. } => "OK",
        Op::Err { .. } => "ERR",
        Op::IsSome { .. } => "IS_SOME",
        Op::Unwrap { .. } => "UNWRAP",
        Op::NewClosure { .. } => "NEW_CLOSURE",
        Op::Spawn { .. } => "SPAWN",
        Op::Await { .. } => "AWAIT",
        Op::Cast { .. } => "CAST",
        Op::ToString { .. } => "TO_STRING",
        Op::TypeOf { .. } => "TYPE_OF",
        Op::Trap { .. } => "TRAP",
        Op::Nop => "NOP",
    }
}

/// Format a single instruction as a string.
pub fn format_op(op: &Op, proto: &FunctionProto, pc: usize) -> String {
    let name = op_name(op);
    match op {
        Op::LoadConst { rd, idx } => {
            let val = &proto.constants[*idx as usize];
            let display = match val {
                ConstValue::Int(v) => format!("{}", v),
                ConstValue::Float(v) => format!("{}", v),
                ConstValue::Bool(v) => format!("{}", v),
                ConstValue::Str(v) => format!("{:?}", v),
                ConstValue::Unit => "unit".to_string(),
            };
            format!("{:04}  {:<16} r{} = {}", pc, name, rd, display)
        }
        Op::LoadUnit { rd } => format!("{:04}  {:<16} r{} = unit", pc, name, rd),
        Op::LoadTrue { rd } => format!("{:04}  {:<16} r{} = true", pc, name, rd),
        Op::LoadFalse { rd } => format!("{:04}  {:<16} r{} = false", pc, name, rd),
        Op::Mov { rd, rs } => format!("{:04}  {:<16} r{} = r{}", pc, name, rd, rs),
        Op::AddInt { rd, ra, rb } | Op::SubInt { rd, ra, rb } | Op::MulInt { rd, ra, rb }
        | Op::DivInt { rd, ra, rb } | Op::ModInt { rd, ra, rb }
        | Op::AddFloat { rd, ra, rb } | Op::SubFloat { rd, ra, rb }
        | Op::MulFloat { rd, ra, rb } | Op::DivFloat { rd, ra, rb }
        | Op::EqInt { rd, ra, rb } | Op::NeInt { rd, ra, rb }
        | Op::LtInt { rd, ra, rb } | Op::GtInt { rd, ra, rb }
        | Op::LeInt { rd, ra, rb } | Op::GeInt { rd, ra, rb }
        | Op::EqFloat { rd, ra, rb } | Op::LtFloat { rd, ra, rb }
        | Op::GtFloat { rd, ra, rb } | Op::LeFloat { rd, ra, rb }
        | Op::GeFloat { rd, ra, rb } | Op::Eq { rd, ra, rb } | Op::Ne { rd, ra, rb }
        | Op::BitAnd { rd, ra, rb } | Op::BitOr { rd, ra, rb }
        | Op::BitXor { rd, ra, rb } | Op::Shl { rd, ra, rb } | Op::Shr { rd, ra, rb }
        | Op::And { rd, ra, rb } | Op::Or { rd, ra, rb }
        | Op::ConcatStr { rd, ra, rb } => {
            format!("{:04}  {:<16} r{} = r{} op r{}", pc, name, rd, ra, rb)
        }
        Op::NegInt { rd, ra } | Op::NegFloat { rd, ra } | Op::IntToFloat { rd, ra }
        | Op::BitNot { rd, ra } | Op::Not { rd, ra } | Op::ToString { rd, ra }
        | Op::TypeOf { rd, ra } => {
            format!("{:04}  {:<16} r{} = op r{}", pc, name, rd, ra)
        }
        Op::Jmp { offset } => {
            let target = pc as i32 + 1 + offset;
            format!("{:04}  {:<16} -> {}", pc, name, target)
        }
        Op::JmpIf { offset, ra } | Op::JmpIfNot { offset, ra } => {
            let target = pc as i32 + 1 + offset;
            format!("{:04}  {:<16} r{} -> {}", pc, name, ra, target)
        }
        Op::Call { rd, func, args_base, argc } => {
            let fname = proto.constants.get(*func as usize)
                .map(|c| match c { ConstValue::Str(s) => s.as_str(), _ => "?" })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{} = func[{}](r{}..r{})", pc, name, rd, func, args_base, *args_base as u16 + argc - 1)
        }
        Op::CallBuiltin { rd, builtin, args_base, argc } => {
            format!("{:04}  {:<16} r{} = builtin[{}](r{}..r{})", pc, name, rd, builtin, args_base, *args_base as u16 + argc - 1)
        }
        Op::CallIndirect { rd, callee, args_base, argc } => {
            format!("{:04}  {:<16} r{} = r{}(r{}..r{})", pc, name, rd, callee, args_base, *args_base as u16 + argc - 1)
        }
        Op::Ret { ra } => format!("{:04}  {:<16} return r{}", pc, name, ra),
        Op::RetUnit => format!("{:04}  {:<16} return unit", pc, name),
        Op::NewList { rd, capacity } => format!("{:04}  {:<16} r{} = list(cap={})", pc, name, rd, capacity),
        Op::ListPush { ra, rb } => format!("{:04}  {:<16} r{}.push(r{})", pc, name, ra, rb),
        Op::ListGet { rd, ra, rb } => format!("{:04}  {:<16} r{} = r{}[r{}]", pc, name, rd, ra, rb),
        Op::ListSet { ra, rb, rc } => format!("{:04}  {:<16} r{}[r{}] = r{}", pc, name, ra, rb, rc),
        Op::Len { rd, ra } => format!("{:04}  {:<16} r{} = len(r{})", pc, name, rd, ra),
        Op::NewTuple { rd, base, arity } => format!("{:04}  {:<16} r{} = tuple(r{}..r{})", pc, name, rd, base, *base as u16 + arity - 1),
        Op::TupleGet { rd, ra, idx } => format!("{:04}  {:<16} r{} = r{}.{}", pc, name, rd, ra, idx),
        Op::NewRecord { rd, type_name, base, count } => {
            let tname = proto.constants.get(*type_name as usize)
                .map(|c| match c { ConstValue::Str(s) => s.as_str(), _ => "?" })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{} = {}(r{}..r{})", pc, name, rd, tname, base, *base as u16 + count - 1)
        }
        Op::RecordGet { rd, ra, field } => {
            let fname = proto.constants.get(*field as usize)
                .map(|c| match c { ConstValue::Str(s) => s.as_str(), _ => "?" })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{} = r{}.{}", pc, name, rd, ra, fname)
        }
        Op::RecordSet { ra, field, rb } => {
            let fname = proto.constants.get(*field as usize)
                .map(|c| match c { ConstValue::Str(s) => s.as_str(), _ => "?" })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{}.{} = r{}", pc, name, ra, fname, rb)
        }
        Op::NewMap { rd } => format!("{:04}  {:<16} r{} = map()", pc, name, rd),
        Op::NewSet { rd } => format!("{:04}  {:<16} r{} = set()", pc, name, rd),
        Op::NewVariant { rd, type_name, variant, base, arity } => {
            let tname = proto.constants.get(*type_name as usize)
                .map(|c| match c { ConstValue::Str(s) => s.as_str(), _ => "?" })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{} = {}::v{}(r{}..r{})", pc, name, rd, tname, variant, base, *base as u16 + arity - 1)
        }
        Op::VariantTag { rd, ra } => format!("{:04}  {:<16} r{} = tag(r{})", pc, name, rd, ra),
        Op::VariantPayload { rd, ra, idx } => format!("{:04}  {:<16} r{} = payload(r{}, {})", pc, name, rd, ra, idx),
        Op::IsVariant { rd, ra, tag } => {
            let tname = proto.constants.get(*tag as usize)
                .map(|c| match c { ConstValue::Str(s) => s.as_str(), _ => "?" })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{} = is(r{}, {})", pc, name, rd, ra, tname)
        }
        Op::VariantGet { rd, ra, idx } => format!("{:04}  {:<16} r{} = r{}[{}]", pc, name, rd, ra, idx),
        Op::Some { rd, ra } => format!("{:04}  {:<16} r{} = Some(r{})", pc, name, rd, ra),
        Op::None { rd } => format!("{:04}  {:<16} r{} = None", pc, name, rd),
        Op::Ok { rd, ra } => format!("{:04}  {:<16} r{} = Ok(r{})", pc, name, rd, ra),
        Op::Err { rd, ra } => format!("{:04}  {:<16} r{} = Err(r{})", pc, name, rd, ra),
        Op::IsSome { rd, ra } => format!("{:04}  {:<16} r{} = is_some(r{})", pc, name, rd, ra),
        Op::Unwrap { rd, ra } => format!("{:04}  {:<16} r{} = unwrap(r{})", pc, name, rd, ra),
        Op::NewClosure { rd, proto: pidx, captures_base, capture_count } => {
            format!("{:04}  {:<16} r{} = closure(proto={}, cap=r{}..r{})", pc, name, rd, pidx, captures_base, *captures_base as u16 + capture_count - 1)
        }
        Op::Spawn { rd, func, args_base, argc } => {
            format!("{:04}  {:<16} r{} = spawn(func[{}], r{}..r{})", pc, name, rd, func, args_base, *args_base as u16 + argc - 1)
        }
        Op::Await { rd, ra } => format!("{:04}  {:<16} r{} = await(r{})", pc, name, rd, ra),
        Op::Cast { rd, ra, target } => format!("{:04}  {:<16} r{} = cast(r{}, ty={})", pc, name, rd, ra, target),
        Op::Trap { msg } => {
            let m = proto.constants.get(*msg as usize)
                .map(|c| match c { ConstValue::Str(s) => s.as_str(), _ => "?" })
                .unwrap_or("?");
            format!("{:04}  {:<16} {:?}", pc, name, m)
        }
        Op::Nop => format!("{:04}  {:<16}", pc, name),
    }
}

/// Disassemble a function prototype into a human-readable string.
pub fn disassemble(proto: &FunctionProto) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "; {} (params={}, regs={}, consts={})\n",
        proto.name,
        proto.param_count,
        proto.register_count,
        proto.constants.len()
    ));

    // Print constant pool.
    for (i, c) in proto.constants.iter().enumerate() {
        let display = match c {
            ConstValue::Int(v) => format!("{}", v),
            ConstValue::Float(v) => format!("{}", v),
            ConstValue::Bool(v) => format!("{}", v),
            ConstValue::Str(v) => format!("{:?}", v),
            ConstValue::Unit => "unit".to_string(),
        };
        out.push_str(&format!(";   const[{}] = {}\n", i, display));
    }

    // Print instructions.
    for (pc, op) in proto.code.iter().enumerate() {
        out.push_str(&format_op(op, proto, pc));
        out.push('\n');
    }

    out
}

/// Disassemble an entire program.
pub fn disassemble_program(program: &BytecodeProgram) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "; BytecodeProgram: {} functions, entry={}\n\n",
        program.functions.len(),
        program.entry
    ));
    for (i, proto) in program.functions.iter().enumerate() {
        out.push_str(&format!("; ── func[{}] ──────────────────────────\n", i));
        out.push_str(&disassemble(proto));
        out.push('\n');
    }
    out
}
