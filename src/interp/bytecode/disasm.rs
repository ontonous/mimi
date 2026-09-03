//! Bytecode disassembler.
//!
//! Provides human-readable output of compiled bytecode for debugging.
//! Uses the opcode metadata table for formatting.

use super::instr::*;

/// Get the display name for an opcode.
pub fn op_name(op: &Op) -> &'static str {
    match op {
        Op::LoadConst { .. } => "LOAD_CONST",
        Op::LoadUnit { .. } => "LOAD_UNIT",
        Op::LoadTrue { .. } => "LOAD_TRUE",
        Op::LoadFalse { .. } => "LOAD_FALSE",
        Op::Mov { .. } => "MOV",
        Op::Move { .. } => "MOVE",
        Op::Clone { .. } => "CLONE",
        Op::Drop { .. } => "DROP",
        Op::DropAggregate { .. } => "DROP_AGGREGATE",
        Op::DropVariant { .. } => "DROP_VARIANT",
        Op::DerefValue { .. } => "DEREF_VALUE",
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
        Op::CheckI32 { .. } => "CHECK_I32",
        Op::CheckI32DivRem { .. } => "CHECK_I32_DIVREM",
        Op::WrapI32 { .. } => "WRAP_I32",
        Op::MaskShiftAmt { .. } => "MASK_SHIFT_AMT",
        Op::PowInt { .. } => "POW_INT",
        Op::PowFloat { .. } => "POW_FLOAT",
        Op::BitNot { .. } => "BIT_NOT",
        Op::Not { .. } => "NOT",
        Op::And { .. } => "AND",
        Op::Or { .. } => "OR",
        Op::ConcatStr { .. } => "CONCAT_STR",
        Op::StrAppend { .. } => "STR_APPEND",
        Op::Jmp { .. } => "JMP",
        Op::JmpIf { .. } => "JMP_IF",
        Op::JmpIfNot { .. } => "JMP_IF_NOT",
        Op::Call { .. } => "CALL",
        Op::CallMove { .. } => "CALL_MOVE",
        Op::MutateSetup { .. } => "MUTATE_SETUP",
        Op::MutateSetupField { .. } => "MUTATE_SETUP_FIELD",
        Op::CallBuiltin { .. } => "CALL_BUILTIN",
        Op::CallIndirect { .. } => "CALL_INDIRECT",
        Op::Ret { .. } => "RET",
        Op::RetUnit => "RET_UNIT",
        Op::RetEarly { .. } => "RET_EARLY",
        Op::NewList { .. } => "NEW_LIST",
        Op::ListPush { .. } => "LIST_PUSH",
        Op::ListPop { .. } => "LIST_POP",
        Op::ListGet { .. } => "LIST_GET",
        Op::ListSet { .. } => "LIST_SET",
        Op::Len { .. } => "LEN",
        Op::NewTuple { .. } => "NEW_TUPLE",
        Op::NewTupleMove { .. } => "NEW_TUPLE_MOVE",
        Op::TupleGet { .. } => "TUPLE_GET",
        Op::NewRecord { .. } => "NEW_RECORD",
        Op::NewRecordMove { .. } => "NEW_RECORD_MOVE",
        Op::UpdateRecord { .. } => "UPDATE_RECORD",
        Op::RecordGet { .. } => "RECORD_GET",
        Op::RecordMoveGet { .. } => "RECORD_MOVE_GET",
        Op::RecordSet { .. } => "RECORD_SET",
        Op::TupleSet { .. } => "TUPLE_SET",
        Op::NewMap { .. } => "NEW_MAP",
        Op::NewSet { .. } => "NEW_SET",
        Op::MapGet { .. } => "MAP_GET",
        Op::MapSet { .. } => "MAP_SET",
        Op::MapContains { .. } => "MAP_CONTAINS",
        Op::SetAdd { .. } => "SET_ADD",
        Op::SetContains { .. } => "SET_CONTAINS",
        Op::MirSetNew { .. } => "MIR_SET_NEW",
        Op::MirSetSize { .. } => "MIR_SET_SIZE",
        Op::MirSetIsEmpty { .. } => "MIR_SET_IS_EMPTY",
        Op::MirSetContains { .. } => "MIR_SET_CONTAINS",
        Op::MirSetInsert { .. } => "MIR_SET_INSERT",
        Op::MirSetRemove { .. } => "MIR_SET_REMOVE",
        Op::MirSetToList { .. } => "MIR_SET_TO_LIST",
        Op::MirListLen { .. } => "MIR_LIST_LEN",
        Op::MirListReverse { .. } => "MIR_LIST_REVERSE",
        Op::MirListConcat { .. } => "MIR_LIST_CONCAT",
        Op::MirVariantPredicate { .. } => "MIR_VARIANT_PREDICATE",
        Op::NewVariant { .. } => "NEW_VARIANT",
        Op::NewVariantMove { .. } => "NEW_VARIANT_MOVE",
        Op::DestructureVariantMove { .. } => "DESTRUCTURE_VARIANT_MOVE",
        Op::VariantTag { .. } => "VARIANT_TAG",
        Op::VariantPayload { .. } => "VARIANT_PAYLOAD",
        Op::IsVariant { .. } => "IS_VARIANT",
        Op::VariantGet { .. } => "VARIANT_GET",
        Op::VariantMoveGet { .. } => "VARIANT_MOVE_GET",
        Op::PatternField { .. } => "PATTERN_FIELD",
        Op::Some { .. } => "SOME",
        Op::None { .. } => "NONE",
        Op::NewCap { .. } => "NEW_CAP",
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
        Op::NonExhaustiveMatch => "NON_EXHAUSTIVE_MATCH",
        Op::Nop => "NOP",
        Op::IeeeEnter => "IEEE_ENTER",
        Op::IeeeExit => "IEEE_EXIT",
        Op::SetFaultPc { .. } => "SET_FAULT_PC",
        Op::ClearFaultPc => "CLEAR_FAULT_PC",
        Op::FaultRetEarly => "FAULT_RET_EARLY",
        Op::ActorSpawn { .. } => "ACTOR_SPAWN",
        Op::ActorSpawnDetached { .. } => "ACTOR_SPAWN_DETACHED",
        Op::FlowTransition { .. } => "FLOW_TRANSITION",
        Op::DynMethodCall { .. } => "DYN_METHOD_CALL",
        Op::SharedNew { .. } => "SHARED_NEW",
        Op::SharedSet { .. } => "SHARED_SET",
        Op::WeakNew { .. } => "WEAK_NEW",
        Op::CallExtern { .. } => "CALL_EXTERN",
        Op::QuotePushLit { .. } => "QUOTE_PUSH_LIT",
        Op::QuotePushIdent { .. } => "QUOTE_PUSH_IDENT",
        Op::QuoteInterpPush { .. } => "QUOTE_INTERP_PUSH",
        Op::QuoteAstPush { .. } => "QUOTE_AST_PUSH",
        Op::QuoteCapture { .. } => "QUOTE_CAPTURE",
        Op::QuoteBlock { .. } => "QUOTE_BLOCK",
        Op::QuoteList { .. } => "QUOTE_LIST",
        Op::QuoteTuple { .. } => "QUOTE_TUPLE",
        Op::QuoteBinary { .. } => "QUOTE_BINARY",
        Op::QuoteUnary { .. } => "QUOTE_UNARY",
        Op::QuoteCall { .. } => "QUOTE_CALL",
        Op::QuoteField { .. } => "QUOTE_FIELD",
        Op::QuoteIndex => "QUOTE_INDEX",
        Op::QuoteIf { .. } => "QUOTE_IF",
        Op::QuoteLet { .. } => "QUOTE_LET",
        Op::QuoteCast { .. } => "QUOTE_CAST",
        Op::QuoteExprStmt => "QUOTE_EXPR_STMT",
        Op::QuoteReturn { .. } => "QUOTE_RETURN",
        Op::QuoteWhile => "QUOTE_WHILE",
        Op::QuoteWhileLet { .. } => "QUOTE_WHILE_LET",
        Op::QuoteBreak { .. } => "QUOTE_BREAK",
        Op::QuoteContinue => "QUOTE_CONTINUE",
        Op::QuoteLambda { .. } => "QUOTE_LAMBDA",
        Op::QuoteFor { .. } => "QUOTE_FOR",
        Op::QuoteAssign => "QUOTE_ASSIGN",
        Op::QuoteLoop => "QUOTE_LOOP",
        Op::QuoteRecord { .. } => "QUOTE_RECORD",
        Op::QuoteTry => "QUOTE_TRY",
        Op::QuoteResult { .. } => "QUOTE_RESULT",
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
                ConstValue::Type(t) => format!("type {:?}", t),
                ConstValue::QuoteAst(q) => format!("quote {:?}", q),
                ConstValue::LambdaSpec { .. } => "lambda_spec".to_string(),
                ConstValue::Pattern(p) => format!("pattern {:?}", p),
                ConstValue::StrVec(v) => format!("strvec {:?}", v),
                ConstValue::VariantShapes(v) => format!("variant_shapes {:?}", v),
                ConstValue::RecordProjection(v) => format!("record_projection {:?}", v),
                ConstValue::TupleProjection(v) => format!("tuple_projection {:?}", v),
                ConstValue::ListProjection(v) => format!("list_projection {:?}", v),
                ConstValue::ListOperation(v) => format!("list_operation {:?}", v),
                ConstValue::VariantPredicate(v) => format!("variant_predicate {:?}", v),
            };
            format!("{:04}  {:<16} r{} = {}", pc, name, rd, display)
        }
        Op::LoadUnit { rd } => format!("{:04}  {:<16} r{} = unit", pc, name, rd),
        Op::LoadTrue { rd } => format!("{:04}  {:<16} r{} = true", pc, name, rd),
        Op::LoadFalse { rd } => format!("{:04}  {:<16} r{} = false", pc, name, rd),
        Op::Mov { rd, rs } => format!("{:04}  {:<16} r{} = r{}", pc, name, rd, rs),
        Op::Move { rd, rs } => format!("{:04}  {:<16} r{} = move(r{})", pc, name, rd, rs),
        Op::Clone { rd, rs } => format!("{:04}  {:<16} r{} = clone(r{})", pc, name, rd, rs),
        Op::Drop { ra } => format!("{:04}  {:<16} drop r{}", pc, name, ra),
        Op::DerefValue { rd, ra } => format!("{:04}  {:<16} r{} = *r{}", pc, name, rd, ra),
        Op::CheckI32 { rd, kind } => {
            format!("{:04}  {:<16} check_i32 r{} (kind {})", pc, name, rd, kind)
        }
        Op::CheckI32DivRem { ra, rb } => {
            format!("{:04}  {:<16} check_i32_divrem r{}, r{}", pc, name, ra, rb)
        }
        Op::WrapI32 { rd } => format!("{:04}  {:<16} r{} = wrap_i32 r{}", pc, name, rd, rd),
        Op::MaskShiftAmt { rb, mask } => {
            format!("{:04}  {:<16} r{} &= {}", pc, name, rb, mask)
        }
        Op::AddInt { rd, ra, rb }
        | Op::SubInt { rd, ra, rb }
        | Op::MulInt { rd, ra, rb }
        | Op::DivInt { rd, ra, rb }
        | Op::ModInt { rd, ra, rb }
        | Op::AddFloat { rd, ra, rb }
        | Op::SubFloat { rd, ra, rb }
        | Op::MulFloat { rd, ra, rb }
        | Op::DivFloat { rd, ra, rb }
        | Op::EqInt { rd, ra, rb }
        | Op::NeInt { rd, ra, rb }
        | Op::LtInt { rd, ra, rb }
        | Op::GtInt { rd, ra, rb }
        | Op::LeInt { rd, ra, rb }
        | Op::GeInt { rd, ra, rb }
        | Op::EqFloat { rd, ra, rb }
        | Op::LtFloat { rd, ra, rb }
        | Op::GtFloat { rd, ra, rb }
        | Op::LeFloat { rd, ra, rb }
        | Op::GeFloat { rd, ra, rb }
        | Op::Eq { rd, ra, rb }
        | Op::Ne { rd, ra, rb }
        | Op::BitAnd { rd, ra, rb }
        | Op::BitOr { rd, ra, rb }
        | Op::BitXor { rd, ra, rb }
        | Op::Shl { rd, ra, rb }
        | Op::Shr { rd, ra, rb }
        | Op::PowInt { rd, ra, rb }
        | Op::PowFloat { rd, ra, rb }
        | Op::And { rd, ra, rb }
        | Op::Or { rd, ra, rb }
        | Op::ConcatStr { rd, ra, rb } => {
            format!("{:04}  {:<16} r{} = r{} op r{}", pc, name, rd, ra, rb)
        }
        Op::StrAppend { ra, rb } => {
            format!("{:04}  {:<16} r{} += r{}", pc, name, ra, rb)
        }
        Op::NegInt { rd, ra }
        | Op::NegFloat { rd, ra }
        | Op::IntToFloat { rd, ra }
        | Op::BitNot { rd, ra }
        | Op::Not { rd, ra }
        | Op::ToString { rd, ra }
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
        Op::Call {
            rd,
            func,
            args_base,
            argc,
        }
        | Op::CallMove {
            rd,
            func,
            args_base,
            argc,
        } => {
            let _fname = proto
                .constants
                .get(*func as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = func[{}](r{}..r{})",
                pc,
                name,
                rd,
                func,
                args_base,
                *args_base as u16 + argc.saturating_sub(1)
            )
        }
        Op::MutateSetup { regs_base, count } => format!(
            "{:04}  {:<16} r{}..r{} = mutate writeback targets",
            pc,
            name,
            regs_base,
            *regs_base as u16 + count.saturating_sub(1)
        ),
        Op::MutateSetupField { regs_base, count } => format!(
            "{:04}  {:<16} r{}..r{} = mutate FIELD writeback targets (obj, field)",
            pc,
            name,
            regs_base,
            *regs_base as u16 + (count.saturating_mul(2)).saturating_sub(1)
        ),
        Op::CallBuiltin {
            rd,
            builtin,
            args_base,
            argc,
        } => {
            format!(
                "{:04}  {:<16} r{} = builtin[{}](r{}..r{})",
                pc,
                name,
                rd,
                builtin,
                args_base,
                *args_base as u16 + argc.saturating_sub(1)
            )
        }
        Op::CallExtern {
            rd,
            extern_idx,
            args_base,
            argc,
        } => {
            format!(
                "{:04}  {:<16} r{} = extern[{}](r{}..r{})",
                pc,
                name,
                rd,
                extern_idx,
                args_base,
                *args_base as u16 + argc.saturating_sub(1)
            )
        }
        Op::CallIndirect {
            rd,
            callee,
            args_base,
            argc,
        } => {
            format!(
                "{:04}  {:<16} r{} = r{}(r{}..r{})",
                pc,
                name,
                rd,
                callee,
                args_base,
                *args_base as u16 + argc.saturating_sub(1)
            )
        }
        Op::Ret { ra } => format!("{:04}  {:<16} return r{}", pc, name, ra),
        Op::RetEarly { ra } => format!("{:04}  {:<16} ret_early r{}", pc, name, ra),
        Op::RetUnit => format!("{:04}  {:<16} return unit", pc, name),
        Op::QuotePushLit { const_idx } => {
            let val = &proto.constants[*const_idx as usize];
            let display = match val {
                ConstValue::Int(v) => format!("{}", v),
                ConstValue::Float(v) => format!("{}", v),
                ConstValue::Bool(v) => format!("{}", v),
                ConstValue::Str(v) => format!("{:?}", v),
                ConstValue::Unit => "unit".to_string(),
                ConstValue::Type(t) => format!("type {:?}", t),
                ConstValue::QuoteAst(q) => format!("quote {:?}", q),
                ConstValue::LambdaSpec { .. } => "lambda_spec".to_string(),
                ConstValue::Pattern(p) => format!("pattern {:?}", p),
                ConstValue::StrVec(v) => format!("strvec {:?}", v),
                ConstValue::VariantShapes(v) => format!("variant_shapes {:?}", v),
                ConstValue::RecordProjection(v) => format!("record_projection {:?}", v),
                ConstValue::TupleProjection(v) => format!("tuple_projection {:?}", v),
                ConstValue::ListProjection(v) => format!("list_projection {:?}", v),
                ConstValue::ListOperation(v) => format!("list_operation {:?}", v),
                ConstValue::VariantPredicate(v) => format!("variant_predicate {:?}", v),
            };
            format!("{:04}  {:<16} push {:?} ({})", pc, name, val, display)
        }
        Op::QuotePushIdent { str_idx } | Op::QuoteField { str_idx } | Op::QuoteLet { str_idx } => {
            let s = proto
                .constants
                .get(*str_idx as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.clone(),
                    _ => "?".to_string(),
                })
                .unwrap_or_else(|| "?".to_string());
            format!("{:04}  {:<16} {:?}", pc, name, s)
        }
        Op::QuoteInterpPush { rs } | Op::QuoteAstPush { rs } => {
            format!("{:04}  {:<16} r{}", pc, name, rs)
        }
        Op::QuoteCapture { str_idx, reg } => {
            let s = proto
                .constants
                .get(*str_idx as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.clone(),
                    _ => "?".to_string(),
                })
                .unwrap_or_else(|| "?".to_string());
            format!("{:04}  {:<16} {} = r{}", pc, name, s, reg)
        }
        Op::QuoteBlock { n }
        | Op::QuoteList { n }
        | Op::QuoteTuple { n }
        | Op::QuoteCall { argc: n } => {
            format!("{:04}  {:<16} n={}", pc, name, n)
        }
        Op::QuoteReturn { has_value } => {
            format!("{:04}  {:<16} has_value={}", pc, name, has_value)
        }
        Op::QuoteBinary { op } => format!("{:04}  {:<16} {:?}", pc, name, op),
        Op::QuoteUnary { op } => format!("{:04}  {:<16} {:?}", pc, name, op),
        Op::QuoteIndex
        | Op::QuoteExprStmt
        | Op::QuoteWhile
        | Op::QuoteTry
        | Op::QuoteContinue
        | Op::QuoteAssign
        | Op::QuoteLoop => {
            format!("{:04}  {:<16}", pc, name)
        }
        Op::QuoteWhileLet { pat_idx } => {
            format!("{:04}  {:<16} pat_idx={}", pc, name, pat_idx)
        }
        Op::QuoteBreak { has_value } => {
            format!("{:04}  {:<16} has_value={}", pc, name, has_value)
        }
        Op::QuoteLambda { spec_idx } => {
            format!("{:04}  {:<16} spec_idx={}", pc, name, spec_idx)
        }
        Op::QuoteFor { var_idx } => {
            format!("{:04}  {:<16} var_idx={}", pc, name, var_idx)
        }
        Op::QuoteRecord {
            n,
            names_idx,
            ty_idx,
        } => {
            format!(
                "{:04}  {:<16} n={} names_idx={} ty_idx={}",
                pc, name, n, names_idx, ty_idx
            )
        }
        Op::QuoteIf { has_else } => format!("{:04}  {:<16} has_else={}", pc, name, has_else),
        Op::QuoteCast { type_idx } => {
            let s = proto
                .constants
                .get(*type_idx as usize)
                .map(|c| match c {
                    ConstValue::Type(t) => format!("{:?}", t),
                    _ => "?".to_string(),
                })
                .unwrap_or_else(|| "?".to_string());
            format!("{:04}  {:<16} {}", pc, name, s)
        }
        Op::QuoteResult { rd } => format!("{:04}  {:<16} r{}", pc, name, rd),
        Op::NewList { rd, capacity } => {
            format!("{:04}  {:<16} r{} = list(cap={})", pc, name, rd, capacity)
        }
        Op::ListPush { ra, rb } => format!("{:04}  {:<16} r{}.push(r{})", pc, name, ra, rb),
        Op::ListPop { rd, ra } => format!("{:04}  {:<16} r{} = r{}.pop()", pc, name, rd, ra),
        Op::ListGet {
            rd,
            ra,
            rb,
            contract,
        } => format!(
            "{:04}  {:<16} r{} = r{}[r{}]{}",
            pc,
            name,
            rd,
            ra,
            rb,
            contract
                .map(|contract| format!(" contract={contract}"))
                .unwrap_or_default()
        ),
        Op::ListSet { ra, rb, rc } => format!("{:04}  {:<16} r{}[r{}] = r{}", pc, name, ra, rb, rc),
        Op::Len { rd, ra } => format!("{:04}  {:<16} r{} = len(r{})", pc, name, rd, ra),
        Op::NewTuple { rd, base, arity } => format!(
            "{:04}  {:<16} r{} = tuple(r{}..r{})",
            pc,
            name,
            rd,
            base,
            *base as u16 + arity.saturating_sub(1)
        ),
        Op::NewTupleMove { rd, base, arity } => format!(
            "{:04}  {:<16} r{} = tuple_move(r{}..r{})",
            pc,
            name,
            rd,
            base,
            *base as u16 + arity.saturating_sub(1)
        ),
        Op::DropAggregate { ra, arity } => format!(
            "{:04}  {:<16} drop_aggregate r{} (arity={})",
            pc, name, ra, arity
        ),
        Op::DropVariant { ra, shapes } => format!(
            "{:04}  {:<16} drop_variant r{} [shapes const {}]",
            pc, name, ra, shapes
        ),
        Op::TupleGet {
            rd,
            ra,
            idx,
            contract,
        } => {
            let receipt = contract
                .map(|contract| format!(" [contract {}]", contract))
                .unwrap_or_default();
            format!(
                "{:04}  {:<16} r{} = r{}.{}{}",
                pc, name, rd, ra, idx, receipt
            )
        }
        Op::NewRecord {
            rd,
            type_name,
            base,
            count,
        } => {
            let tname = proto
                .constants
                .get(*type_name as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = {}(r{}..r{})",
                pc,
                name,
                rd,
                tname,
                base,
                *base as u16 + count.saturating_sub(1)
            )
        }
        Op::NewRecordMove {
            rd,
            type_name,
            base,
            count,
        } => {
            let tname = proto
                .constants
                .get(*type_name as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = {}_move(r{}..r{})",
                pc,
                name,
                rd,
                tname,
                base,
                *base as u16 + count.saturating_sub(1)
            )
        }
        Op::UpdateRecord {
            rd,
            type_name,
            ra,
            base,
            count,
        } => {
            let tname = proto
                .constants
                .get(*type_name as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = {}(r{}, r{}..r{})",
                pc,
                name,
                rd,
                tname,
                ra,
                base,
                *base as u16 + count.saturating_sub(1)
            )
        }
        Op::RecordGet {
            rd,
            ra,
            field,
            contract,
        } => {
            let fname = proto
                .constants
                .get(*field as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = r{}.{}{}",
                pc,
                name,
                rd,
                ra,
                fname,
                contract
                    .map(|idx| format!(" [contract {}]", idx))
                    .unwrap_or_default()
            )
        }
        Op::RecordMoveGet {
            rd,
            ra,
            field,
            contract,
        } => {
            let fname = proto
                .constants
                .get(*field as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = move r{}.{}{}",
                pc,
                name,
                rd,
                ra,
                fname,
                contract
                    .map(|idx| format!(" [contract {}]", idx))
                    .unwrap_or_default()
            )
        }
        Op::RecordSet { ra, field, rb } => {
            let fname = proto
                .constants
                .get(*field as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{}.{} = r{}", pc, name, ra, fname, rb)
        }
        Op::TupleSet { ra, idx, rb } => {
            let iname = proto
                .constants
                .get(*idx as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{}.{} = r{}", pc, name, ra, iname, rb)
        }
        Op::NewMap { rd } => format!("{:04}  {:<16} r{} = map()", pc, name, rd),
        Op::NewSet { rd } => format!("{:04}  {:<16} r{} = set()", pc, name, rd),
        Op::MapGet { rd, ra, rb } => format!("{:04}  {:<16} r{} = r{}[r{}]", pc, name, rd, ra, rb),
        Op::MapSet { ra, rb, rc } => format!("{:04}  {:<16} r{}[r{}] = r{}", pc, name, ra, rb, rc),
        Op::MapContains { rd, ra, rb } => format!(
            "{:04}  {:<16} r{} = contains(r{}, r{})",
            pc, name, rd, ra, rb
        ),
        Op::SetAdd { ra, rb } => format!("{:04}  {:<16} r{}.add(r{})", pc, name, ra, rb),
        Op::SetContains { rd, ra, rb } => format!(
            "{:04}  {:<16} r{} = contains(r{}, r{})",
            pc, name, rd, ra, rb
        ),
        Op::MirSetNew { rd } => format!("{:04}  {:<16} r{} = mir_set()", pc, name, rd),
        Op::MirSetSize { rd, ra } => {
            format!("{:04}  {:<16} r{} = mir_set_size(r{})", pc, name, rd, ra)
        }
        Op::MirSetIsEmpty { rd, ra } => {
            format!(
                "{:04}  {:<16} r{} = mir_set_is_empty(r{})",
                pc, name, rd, ra
            )
        }
        Op::MirSetContains { rd, ra, rb } => format!(
            "{:04}  {:<16} r{} = mir_set_contains(r{}, r{})",
            pc, name, rd, ra, rb
        ),
        Op::MirSetInsert { rd, ra, rb } => format!(
            "{:04}  {:<16} r{} = mir_set_insert_move(r{}, r{})",
            pc, name, rd, ra, rb
        ),
        Op::MirSetRemove { rd, ra, rb } => format!(
            "{:04}  {:<16} r{} = mir_set_remove_move(r{}, r{})",
            pc, name, rd, ra, rb
        ),
        Op::MirSetToList { rd, ra } => {
            format!("{:04}  {:<16} r{} = mir_set_to_list(r{})", pc, name, rd, ra)
        }
        Op::MirListLen { rd, ra, contract } => {
            format!(
                "{:04}  {:<16} r{} = mir_list_len(r{}){}",
                pc,
                name,
                rd,
                ra,
                contract
                    .map(|contract| format!(" contract={contract}"))
                    .unwrap_or_default()
            )
        }
        Op::MirListReverse { rd, ra, contract } => {
            format!(
                "{:04}  {:<16} r{} = mir_list_reverse(r{}){}",
                pc,
                name,
                rd,
                ra,
                contract
                    .map(|contract| format!(" contract={contract}"))
                    .unwrap_or_default()
            )
        }
        Op::MirListConcat {
            rd,
            ra,
            rb,
            contract,
        } => {
            format!(
                "{:04}  {:<16} r{} = mir_list_concat(r{}, r{}){}",
                pc,
                name,
                rd,
                ra,
                rb,
                contract
                    .map(|contract| format!(" contract={contract}"))
                    .unwrap_or_default()
            )
        }
        Op::MirVariantPredicate {
            rd,
            ra,
            predicate,
            contract,
        } => format!(
            "{:04}  {:<16} r{} = mir_variant_predicate::{:?}(r{}){}",
            pc,
            name,
            rd,
            predicate,
            ra,
            contract
                .map(|contract| format!(" contract={contract}"))
                .unwrap_or_default()
        ),
        Op::NewVariant {
            rd,
            type_name,
            variant,
            base,
            arity,
            ..
        } => {
            let tname = proto
                .constants
                .get(*type_name as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = {}::v{}(r{}..r{})",
                pc,
                name,
                rd,
                tname,
                variant,
                base,
                *base as u16 + arity.saturating_sub(1)
            )
        }
        Op::NewVariantMove {
            rd,
            type_name,
            variant,
            base,
            arity,
            ..
        } => {
            let tname = proto
                .constants
                .get(*type_name as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = {}::v{}_move(r{}..r{})",
                pc,
                name,
                rd,
                tname,
                variant,
                base,
                *base as u16 + arity.saturating_sub(1)
            )
        }
        Op::DestructureVariantMove {
            ra,
            base,
            arity,
            variant_tag,
            shapes,
        } => {
            let tag = proto
                .constants
                .get(*variant_tag as usize)
                .map(|constant| match constant {
                    ConstValue::Str(tag) => tag.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} destructure_variant_move<{}> r{} -> r{}..r{} [shapes const {}]",
                pc,
                name,
                tag,
                ra,
                base,
                *base as u16 + arity.saturating_sub(1),
                shapes
            )
        }
        Op::VariantTag { rd, ra } => format!("{:04}  {:<16} r{} = tag(r{})", pc, name, rd, ra),
        Op::VariantPayload { rd, ra, idx } => format!(
            "{:04}  {:<16} r{} = payload(r{}, {})",
            pc, name, rd, ra, idx
        ),
        Op::IsVariant { rd, ra, tag } => {
            let tname = proto
                .constants
                .get(*tag as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{} = is(r{}, {})", pc, name, rd, ra, tname)
        }
        Op::VariantGet {
            rd,
            ra,
            idx,
            variant_tag,
            shapes,
        } => {
            format!(
                "{:04}  {:<16} r{} = r{}[{}] [tag const {}, shapes const {}]",
                pc, name, rd, ra, idx, variant_tag, shapes
            )
        }
        Op::VariantMoveGet {
            rd,
            ra,
            idx,
            variant_tag,
            shapes,
        } => {
            format!(
                "{:04}  {:<16} r{} = move r{}[{}] [tag const {}, shapes const {}]",
                pc, name, rd, ra, idx, variant_tag, shapes
            )
        }
        Op::PatternField { rd, ra, field } => {
            format!("{:04}  {:<16} r{} = r{}.field[{}]", pc, name, rd, ra, field)
        }
        Op::Some { rd, ra } => format!("{:04}  {:<16} r{} = Some(r{})", pc, name, rd, ra),
        Op::None { rd } => format!("{:04}  {:<16} r{} = None", pc, name, rd),
        Op::NewCap { rd, name: cap_idx } => {
            format!("{:04}  {:<16} r{} = Cap(const[{}])", pc, name, rd, cap_idx)
        }
        Op::Ok { rd, ra } => format!("{:04}  {:<16} r{} = Ok(r{})", pc, name, rd, ra),
        Op::Err { rd, ra } => format!("{:04}  {:<16} r{} = Err(r{})", pc, name, rd, ra),
        Op::IsSome { rd, ra } => format!("{:04}  {:<16} r{} = is_some(r{})", pc, name, rd, ra),
        Op::Unwrap { rd, ra } => format!("{:04}  {:<16} r{} = unwrap(r{})", pc, name, rd, ra),
        Op::NewClosure {
            rd,
            proto: pidx,
            captures_base,
            capture_count,
        } => {
            format!(
                "{:04}  {:<16} r{} = closure(proto={}, cap=r{}..r{})",
                pc,
                name,
                rd,
                pidx,
                captures_base,
                *captures_base as u16 + capture_count.saturating_sub(1)
            )
        }
        Op::Spawn {
            rd,
            func,
            args_base,
            argc,
        } => {
            format!(
                "{:04}  {:<16} r{} = spawn(func[{}], r{}..r{})",
                pc,
                name,
                rd,
                func,
                args_base,
                *args_base as u16 + argc.saturating_sub(1)
            )
        }
        Op::Await { rd, ra } => format!("{:04}  {:<16} r{} = await(r{})", pc, name, rd, ra),
        Op::Cast { rd, ra, target } => format!(
            "{:04}  {:<16} r{} = cast(r{}, ty={})",
            pc, name, rd, ra, target
        ),
        Op::Trap { msg } => {
            let m = proto
                .constants
                .get(*msg as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!("{:04}  {:<16} {:?}", pc, name, m)
        }
        Op::NonExhaustiveMatch => {
            format!("{:04}  {:<16} panic:E0805", pc, name)
        }
        Op::Nop | Op::IeeeEnter | Op::IeeeExit => {
            format!("{:04}  {:<16}", pc, name)
        }
        Op::ActorSpawn { rd, actor } | Op::ActorSpawnDetached { rd, actor } => {
            let a = proto
                .constants
                .get(*actor as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!("{:04}  {:<16} r{} = actor_spawn({})", pc, name, rd, a)
        }
        Op::FlowTransition {
            rd,
            flow,
            method,
            args_base,
            argc,
        } => {
            let f = proto
                .constants
                .get(*flow as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            let m = proto
                .constants
                .get(*method as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = {}::{}(r{}..r{})",
                pc,
                name,
                rd,
                f,
                m,
                args_base,
                *args_base as u16 + argc.saturating_sub(1)
            )
        }
        Op::DynMethodCall {
            rd,
            method,
            args_base,
            argc,
        } => {
            let m = proto
                .constants
                .get(*method as usize)
                .map(|c| match c {
                    ConstValue::Str(s) => s.as_str(),
                    _ => "?",
                })
                .unwrap_or("?");
            format!(
                "{:04}  {:<16} r{} = dyn_call(r{}, method={}, argc={})",
                pc, name, rd, args_base, m, argc
            )
        }
        Op::SharedNew { rd, ra } => format!("{:04}  {:<16} r{} = shared(r{})", pc, name, rd, ra),
        Op::SharedSet { ra, rb } => format!("{:04}  {:<16} *r{} = r{}", pc, name, ra, rb),
        Op::WeakNew { rd, ra } => format!("{:04}  {:<16} r{} = weak(r{})", pc, name, rd, ra),
        Op::SetFaultPc { handler_pc } => {
            format!("{:04}  {:<16} handler_pc={}", pc, name, handler_pc)
        }
        Op::ClearFaultPc => format!("{:04}  {:<16}", pc, name),
        Op::FaultRetEarly => format!(
            "{:04}  {:<16}  ; re-emit early return after compensations",
            pc, name
        ),
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
            ConstValue::Type(t) => format!("type {:?}", t),
            ConstValue::QuoteAst(q) => format!("quote {:?}", q),
            ConstValue::LambdaSpec { .. } => "lambda_spec".to_string(),
            ConstValue::Pattern(p) => format!("pattern {:?}", p),
            ConstValue::StrVec(v) => format!("strvec {:?}", v),
            ConstValue::VariantShapes(v) => format!("variant_shapes {:?}", v),
            ConstValue::RecordProjection(v) => format!("record_projection {:?}", v),
            ConstValue::TupleProjection(v) => format!("tuple_projection {:?}", v),
            ConstValue::ListProjection(v) => format!("list_projection {:?}", v),
            ConstValue::ListOperation(v) => format!("list_operation {:?}", v),
            ConstValue::VariantPredicate(v) => format!("variant_predicate {:?}", v),
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
