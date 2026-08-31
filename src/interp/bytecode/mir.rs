//! Direct consumer of canonical MIR for the bytecode VM.
//!
//! This adapter is intentionally narrow.  It proves the architectural seam:
//! once a `MirProgram` exists, bytecode emission no longer sees the AST,
//! resolver, or checker.  Unsupported MIR shapes are reported explicitly
//! instead of falling back to the legacy compiler.  The supported slice is
//! scalar values, calls, branches, and loop-shaped CFG edges.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::core::ir::{ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedUnaryOp};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{MirAbiClass, MirGlueKind, MirLayout, MirOwnership, MirTypeDesc};
use crate::core::mir::{
    MirAggregateKind, MirFunction, MirInstructionKind, MirOwnershipEventKind, MirProjection,
    MirTerminator, MirValueId,
};
use crate::core::NodeId;

use super::instr::{BytecodeProgram, ConstValue, FuncIdx, FunctionProto, Op, Reg};

/// A fail-closed error from the canonical-MIR → bytecode adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirBytecodeError {
    pub function: NodeId,
    pub message: String,
}

impl fmt::Display for MirBytecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "MIR bytecode '{}': {}",
            self.function.0, self.message
        )
    }
}

impl std::error::Error for MirBytecodeError {}

/// Compile an already validated canonical MIR program into a bytecode program.
///
/// The returned program is deliberately free of an AST (`ast: None`).  This is
/// an important migration invariant: this consumer can only execute facts
/// carried by MIR and its type catalog.
pub fn compile_mir_program(
    program: &MirProgram,
) -> Result<Arc<BytecodeProgram>, Vec<MirBytecodeError>> {
    let ordered: Vec<(&NodeId, &MirFunction)> = program.functions().iter().collect();
    if ordered.is_empty() {
        return Err(vec![MirBytecodeError {
            function: NodeId("mir-program".into()),
            message: "program contains no functions".into(),
        }]);
    }

    let mut indices = BTreeMap::new();
    for (index, (owner, _)) in ordered.iter().enumerate() {
        indices.insert((*owner).clone(), index as FuncIdx);
    }

    let mut functions = Vec::with_capacity(ordered.len());
    let mut errors = Vec::new();
    for (_, function) in &ordered {
        match compile_function(function, program, &indices) {
            Ok(proto) => functions.push(proto),
            Err(mut function_errors) => errors.append(&mut function_errors),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let entry = ordered
        .iter()
        .find(|(owner, _)| owner.0 == "function:main")
        .or_else(|| {
            ordered
                .iter()
                .find(|(owner, _)| owner.0.ends_with("::main"))
        })
        .map(|(owner, _)| indices[owner])
        .ok_or_else(|| {
            vec![MirBytecodeError {
                function: NodeId("mir-program".into()),
                message: "program has no canonical main callable".into(),
            }]
        })?;

    Ok(Arc::new(BytecodeProgram {
        functions,
        entry,
        builtin_names: Vec::new(),
        extern_names: Vec::new(),
        actor_defs: std::collections::HashMap::new(),
        flow_defs: std::collections::HashMap::new(),
        flow_transition_funcs: std::collections::HashMap::new(),
        flow_fails_transitions: std::collections::HashSet::new(),
        actor_method_funcs: std::collections::HashMap::new(),
        max_children: None,
        flow_persistent: std::collections::HashMap::new(),
        flow_fault_type: std::collections::HashMap::new(),
        type_defs: std::collections::HashMap::new(),
        ast: None,
        record_fields: std::collections::HashMap::new(),
    }))
}

struct FunctionEmitter<'a> {
    function: &'a MirFunction,
    program: &'a MirProgram,
    indices: &'a BTreeMap<NodeId, FuncIdx>,
    proto: FunctionProto,
    registers: BTreeMap<MirValueId, Reg>,
    block_starts: BTreeMap<crate::core::mir::MirBlockId, usize>,
    pending_jumps: Vec<(usize, crate::core::mir::MirBlockId)>,
    errors: Vec<MirBytecodeError>,
}

fn compile_function(
    function: &MirFunction,
    program: &MirProgram,
    indices: &BTreeMap<NodeId, FuncIdx>,
) -> Result<FunctionProto, Vec<MirBytecodeError>> {
    if function.parameters.len() > u16::MAX as usize {
        return Err(vec![MirBytecodeError {
            function: function.owner.clone(),
            message: "parameter count exceeds bytecode register ABI".into(),
        }]);
    }
    let mut emitter = FunctionEmitter {
        function,
        program,
        indices,
        proto: FunctionProto::new(function.owner.0.clone(), function.parameters.len() as u16),
        registers: BTreeMap::new(),
        block_starts: BTreeMap::new(),
        pending_jumps: Vec::new(),
        errors: Vec::new(),
    };
    emitter.assign_registers();
    emitter.validate_signature();
    emitter.validate_ownership();
    emitter.emit_blocks();
    emitter.patch_jumps();
    if emitter.errors.is_empty() {
        Ok(emitter.proto)
    } else {
        Err(emitter.errors)
    }
}

impl<'a> FunctionEmitter<'a> {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(MirBytecodeError {
            function: self.function.owner.clone(),
            message: message.into(),
        });
    }

    fn assign_registers(&mut self) {
        if self.function.values.len() > u16::MAX as usize {
            self.error("MIR value catalog exceeds bytecode register ABI");
            return;
        }
        // The VM calling convention puts parameters in registers [0..argc).
        for (index, parameter) in self.function.parameters.iter().enumerate() {
            self.registers.insert(parameter.clone(), index as Reg);
        }
        for value in self.function.values.keys() {
            if !self.registers.contains_key(value) {
                let reg = self.proto.alloc_reg();
                self.registers.insert(value.clone(), reg);
            }
        }
    }

    fn validate_signature(&mut self) {
        if self.function.parameters.len() > u16::MAX as usize {
            self.error("parameter count exceeds bytecode register ABI");
            return;
        }
        let parameter_ids = self.function.parameters.clone();
        for parameter in parameter_ids {
            if let Some(value) = self.function.values.get(&parameter) {
                if self.supported_type(&value.ty).is_err() {
                    self.error(format!(
                        "parameter '{}' is not in the canonical bytecode slice",
                        parameter
                    ));
                }
            }
        }
        if self.supported_type(&self.function.result).is_err() {
            self.error("function result is not in the canonical bytecode slice");
        }
    }

    /// Prove that every checker-owned ownership fact has a runtime
    /// representation in this adapter.  The scalar emitter has no drop,
    /// borrow, session, or actor glue, so accepting those facts would make the
    /// backend appear executable while silently discarding an ownership
    /// obligation.  Fail closed before emitting any bytecode instead.
    fn validate_ownership(&mut self) {
        let events = self.function.ownership.events.clone();
        for event in events {
            let Some(value) = event.value.as_ref() else {
                if matches!(
                    event.kind,
                    MirOwnershipEventKind::Move
                        | MirOwnershipEventKind::Drop
                        | MirOwnershipEventKind::Return
                        | MirOwnershipEventKind::TransferSession
                        | MirOwnershipEventKind::TransferChild
                        | MirOwnershipEventKind::BorrowShared
                        | MirOwnershipEventKind::BorrowMut
                        | MirOwnershipEventKind::BorrowEnd
                ) {
                    self.error(format!(
                        "ownership event '{}' at '{}' has no MIR value identity",
                        event.kind.as_str(),
                        event.point.0
                    ));
                }
                continue;
            };
            let Some(value_info) = self.function.values.get(value) else {
                // MirFunction::validate normally catches this; retain a local
                // guard so this adapter remains safe if called on a manually
                // assembled function in a downstream tool.
                self.error(format!(
                    "ownership event '{}' references absent value '{}'",
                    event.kind.as_str(),
                    value
                ));
                continue;
            };
            let Some(desc) = self.program.type_catalog().get(&value_info.ty) else {
                self.error(format!(
                    "ownership event '{}' value '{}' has no type descriptor",
                    event.kind.as_str(),
                    value
                ));
                continue;
            };
            match event.kind {
                MirOwnershipEventKind::Read
                | MirOwnershipEventKind::Write
                | MirOwnershipEventKind::Introduce => {}
                MirOwnershipEventKind::Move | MirOwnershipEventKind::Return => {
                    if desc.ownership != MirOwnership::Copy
                        && desc.glue.move_out == MirGlueKind::Unsupported
                    {
                        self.error(format!(
                            "ownership event '{}' for '{}' needs canonical move glue",
                            event.kind.as_str(),
                            value
                        ));
                    }
                }
                MirOwnershipEventKind::Drop => {
                    if desc.ownership != MirOwnership::Copy
                        && desc.glue.drop == MirGlueKind::Unsupported
                    {
                        self.error(format!(
                            "ownership event '{}' for '{}' needs canonical drop glue",
                            event.kind.as_str(),
                            value
                        ));
                    }
                }
                MirOwnershipEventKind::TransferSession
                | MirOwnershipEventKind::TransferChild
                | MirOwnershipEventKind::BorrowShared
                | MirOwnershipEventKind::BorrowMut
                | MirOwnershipEventKind::BorrowEnd => self.error(format!(
                    "ownership event '{}' for '{}' is outside the scalar bytecode glue slice",
                    event.kind.as_str(),
                    value
                )),
            }
        }
    }

    fn reg(&mut self, value: &MirValueId) -> Option<Reg> {
        match self.registers.get(value).copied() {
            Some(reg) => Some(reg),
            None => {
                self.error(format!("value '{}' has no bytecode register", value));
                None
            }
        }
    }

    fn type_of(&self, value: &MirValueId) -> Option<&MirTypeDesc> {
        let ty = self.function.values.get(value)?.ty.clone();
        self.program.type_catalog().get(&ty)
    }

    fn emit_blocks(&mut self) {
        let blocks: Vec<_> = self.function.blocks.values().cloned().collect();
        for block in &blocks {
            self.block_starts
                .insert(block.id.clone(), self.proto.code.len());
            for instruction in &block.instructions {
                self.emit_instruction(&instruction.kind);
            }
            self.emit_terminator(&block.terminator);
        }
    }

    fn emit_instruction(&mut self, instruction: &MirInstructionKind) {
        match instruction {
            MirInstructionKind::Const { result, literal } => {
                if let Err(message) = self.supported_type_for_value(result) {
                    self.error(format!("constant '{}' is unsupported: {message}", result));
                    return;
                }
                let Some(rd) = self.reg(result) else { return };
                let op = match literal {
                    ResolvedLiteral::Int(value) => {
                        let idx = self.proto.add_const(ConstValue::Int(*value));
                        Op::LoadConst { rd, idx }
                    }
                    ResolvedLiteral::FloatBits(bits) => {
                        let idx = self
                            .proto
                            .add_const(ConstValue::Float(f64::from_bits(*bits)));
                        Op::LoadConst { rd, idx }
                    }
                    ResolvedLiteral::String(value) => {
                        let idx = self.proto.add_const(ConstValue::Str(value.clone()));
                        Op::LoadConst { rd, idx }
                    }
                    ResolvedLiteral::Bool(true) => Op::LoadTrue { rd },
                    ResolvedLiteral::Bool(false) => Op::LoadFalse { rd },
                    ResolvedLiteral::Unit => Op::LoadUnit { rd },
                };
                self.proto.emit(op);
            }
            MirInstructionKind::Load { result, place } => {
                self.emit_load(result, place);
            }
            MirInstructionKind::Copy { result, source } => {
                let Some(rd) = self.reg(result) else { return };
                let Some(rs) = self.reg(source) else { return };
                let Some(desc) = self.type_of(source) else {
                    self.error(format!("value '{}' has no type descriptor", source));
                    return;
                };
                if self.supported_type(&desc.id).is_err() {
                    self.error(format!(
                        "value '{}' is not in the canonical bytecode slice",
                        source
                    ));
                    return;
                }
                self.proto.emit(Op::Mov { rd, rs });
            }
            MirInstructionKind::Move { result, source } => {
                let Some(rd) = self.reg(result) else { return };
                let Some(rs) = self.reg(source) else { return };
                let Some(desc) = self.type_of(source) else {
                    self.error(format!("value '{}' has no type descriptor", source));
                    return;
                };
                if self.supported_type(&desc.id).is_err() {
                    self.error(format!(
                        "value '{}' is not in the canonical bytecode slice",
                        source
                    ));
                    return;
                }
                if desc.ownership == MirOwnership::Copy {
                    self.proto.emit(Op::Mov { rd, rs });
                } else if desc.glue.move_out == MirGlueKind::OwnedString {
                    self.proto.emit(Op::Move { rd, rs });
                } else {
                    self.error(format!(
                        "move of {:?} value '{}' has no canonical move glue",
                        desc.ownership, source
                    ));
                }
            }
            MirInstructionKind::Clone { result, source } => {
                let Some(rd) = self.reg(result) else { return };
                let Some(rs) = self.reg(source) else { return };
                let Some(desc) = self.type_of(source) else {
                    self.error(format!("value '{}' has no type descriptor", source));
                    return;
                };
                if self.supported_type(&desc.id).is_err() {
                    self.error(format!(
                        "value '{}' is not in the canonical bytecode slice",
                        source
                    ));
                    return;
                }
                if desc.ownership == MirOwnership::Copy {
                    self.proto.emit(Op::Mov { rd, rs });
                } else if desc.glue.clone == MirGlueKind::OwnedString {
                    self.proto.emit(Op::Clone { rd, rs });
                } else {
                    self.error(format!(
                        "clone of {:?} value '{}' has no canonical clone glue",
                        desc.ownership, source
                    ));
                }
            }
            MirInstructionKind::Drop { value } => {
                let Some(desc) = self.type_of(value) else {
                    self.error(format!("drop value '{}' has no type descriptor", value));
                    return;
                };
                if self.supported_type(&desc.id).is_err() {
                    self.error(format!(
                        "value '{}' is not in the canonical bytecode slice",
                        value
                    ));
                } else if desc.ownership != MirOwnership::Copy
                    && desc.glue.drop == MirGlueKind::OwnedString
                {
                    let Some(ra) = self.reg(value) else { return };
                    self.proto.emit(Op::Drop { ra });
                } else if desc.ownership != MirOwnership::Copy {
                    self.error(format!(
                        "drop of {:?} value '{}' has no canonical drop glue",
                        desc.ownership, value
                    ));
                }
            }
            MirInstructionKind::Borrow { .. } | MirInstructionKind::EndBorrow { .. } => {
                self.error("borrow lifetime instructions are not emitted by the scalar adapter");
            }
            MirInstructionKind::Project {
                result,
                base,
                projection,
            } => {
                self.emit_project(result, base, projection);
            }
            MirInstructionKind::Construct {
                result,
                kind: MirAggregateKind::Tuple,
                fields,
            } => self.emit_tuple_construct(result, fields),
            MirInstructionKind::Construct {
                result,
                kind: MirAggregateKind::Record { nominal, fields },
                fields: values,
            } => self.emit_record_construct(result, nominal, fields, values),
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => self.emit_variant_construct(result, nominal, variant, fields),
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind: MirAggregateKind::Record { nominal, fields },
                fields: values,
            } => self.emit_record_update(result, base, nominal, fields, values),
            MirInstructionKind::UpdateRecord { .. } => {
                self.error("record update instruction requires a record aggregate kind")
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => self.emit_binary(result, *op, left, right),
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => self.emit_unary(result, *op, operand),
            MirInstructionKind::Call {
                result,
                callee,
                arguments,
            } => self.emit_call(result.as_ref(), callee, arguments),
            MirInstructionKind::Convert { result, source } => self.emit_convert(result, source),
            MirInstructionKind::Nop => {}
        }
    }

    fn emit_load(&mut self, result: &MirValueId, place: &crate::core::ResolvedPlace) {
        let Some(rd) = self.reg(result) else { return };
        let source_id = match MirValueId::new(format!("local:{}", place.base.0 .0)) {
            Ok(id) => id,
            Err(error) => {
                self.error(error.to_string());
                return;
            }
        };
        let Some(mut current_ty) = self
            .function
            .values
            .get(&source_id)
            .map(|value| value.ty.clone())
        else {
            self.error(format!("load base '{}' has no MIR value", source_id));
            return;
        };
        if let Err(message) = self.supported_type(&current_ty) {
            self.error(format!(
                "load base '{}' is unsupported: {message}",
                source_id
            ));
            return;
        }
        if let Err(message) = self.supported_type_for_value(result) {
            self.error(format!(
                "load result '{}' is unsupported: {message}",
                result
            ));
            return;
        }
        let Some(mut current_reg) = self.reg(&source_id) else {
            return;
        };
        if place.projections.is_empty() {
            let Some(current_ty_desc) = self.program.type_catalog().get(&current_ty) else {
                self.error(format!(
                    "load base type '{}' is absent from TypeDesc",
                    current_ty.as_str()
                ));
                return;
            };
            if current_ty_desc.ownership == MirOwnership::Copy {
                self.proto.emit(Op::Mov {
                    rd,
                    rs: current_reg,
                });
            } else if current_ty_desc.glue.clone == MirGlueKind::OwnedString {
                self.proto.emit(Op::Clone {
                    rd,
                    rs: current_reg,
                });
            } else {
                self.error(format!(
                    "load of '{}' has no canonical clone glue",
                    source_id
                ));
            }
            return;
        }
        for (index, projection) in place.projections.iter().enumerate() {
            let destination = if index + 1 == place.projections.len() {
                rd
            } else {
                self.proto.alloc_reg()
            };
            match projection {
                crate::core::ir::ResolvedProjection::Tuple {
                    index: tuple_index,
                    ty: projected_ty,
                } => {
                    let Some(desc) = self.program.type_catalog().get(&current_ty) else {
                        self.error(format!(
                            "projected load base type '{}' is absent",
                            current_ty.as_str()
                        ));
                        return;
                    };
                    let MirLayout::Tuple(elements) = &desc.layout else {
                        self.error(format!(
                            "projected load base type '{}' is not a tuple",
                            current_ty.as_str()
                        ));
                        return;
                    };
                    let Some(expected_ty) = elements.get(*tuple_index) else {
                        self.error(format!(
                            "tuple projected load index {} is out of bounds",
                            tuple_index
                        ));
                        return;
                    };
                    if expected_ty != projected_ty {
                        self.error(format!(
                            "tuple projected load type '{}' disagrees with layout type '{}'",
                            projected_ty.as_str(),
                            expected_ty.as_str()
                        ));
                        return;
                    }
                    if *tuple_index > u16::MAX as usize {
                        self.error("tuple projected load index exceeds bytecode field ABI");
                        return;
                    }
                    self.proto.emit(Op::TupleGet {
                        rd: destination,
                        ra: current_reg,
                        idx: *tuple_index as u16,
                    });
                    current_ty = projected_ty.clone();
                }
                crate::core::ir::ResolvedProjection::Field {
                    field,
                    ty: projected_ty,
                    ..
                } => {
                    let Some(desc) = self.program.type_catalog().get(&current_ty) else {
                        self.error(format!(
                            "projected load base type '{}' is absent",
                            current_ty.as_str()
                        ));
                        return;
                    };
                    let MirLayout::Record { fields, .. } = &desc.layout else {
                        self.error(format!(
                            "projected load base type '{}' is not a record",
                            current_ty.as_str()
                        ));
                        return;
                    };
                    let Some(field_desc) = fields.iter().find(|candidate| candidate.id == *field)
                    else {
                        self.error(format!(
                            "projected load field '{}' is absent from TypeDesc",
                            field.0
                        ));
                        return;
                    };
                    if &field_desc.ty != projected_ty {
                        self.error(format!(
                            "record projected load type '{}' disagrees with layout type '{}'",
                            projected_ty.as_str(),
                            field_desc.ty.as_str()
                        ));
                        return;
                    }
                    let field_idx = self
                        .proto
                        .add_const(ConstValue::Str(field_desc.name.clone()));
                    self.proto.emit(Op::RecordGet {
                        rd: destination,
                        ra: current_reg,
                        field: field_idx,
                    });
                    current_ty = projected_ty.clone();
                }
                crate::core::ir::ResolvedProjection::Index { .. } => {
                    self.error("indexed projected loads have no canonical MIR layout contract");
                    return;
                }
                crate::core::ir::ResolvedProjection::Deref { .. } => {
                    self.error("dereference projected loads have no canonical MIR layout contract");
                    return;
                }
            }
            current_reg = destination;
        }
    }

    fn emit_binary(
        &mut self,
        result: &MirValueId,
        op: ResolvedBinaryOp,
        left: &MirValueId,
        right: &MirValueId,
    ) {
        for value in [result, left, right] {
            if let Err(message) = self.supported_type_for_value(value) {
                self.error(format!(
                    "binary value '{}' is unsupported: {message}",
                    value
                ));
                return;
            }
        }
        let (Some(rd), Some(ra), Some(rb)) = (self.reg(result), self.reg(left), self.reg(right))
        else {
            return;
        };
        let Some(operand_desc) = self.type_of(left) else {
            self.error(format!("binary operand '{}' has no type descriptor", left));
            return;
        };
        let opcode = match operand_desc.abi {
            MirAbiClass::Integer { bits, signed: true } if bits <= 64 => match op {
                ResolvedBinaryOp::Add => Op::AddInt { rd, ra, rb },
                ResolvedBinaryOp::Subtract => Op::SubInt { rd, ra, rb },
                ResolvedBinaryOp::Multiply => Op::MulInt { rd, ra, rb },
                ResolvedBinaryOp::Divide => Op::DivInt { rd, ra, rb },
                ResolvedBinaryOp::Remainder => Op::ModInt { rd, ra, rb },
                ResolvedBinaryOp::Power => Op::PowInt { rd, ra, rb },
                ResolvedBinaryOp::Equal => Op::EqInt { rd, ra, rb },
                ResolvedBinaryOp::NotEqual => Op::NeInt { rd, ra, rb },
                ResolvedBinaryOp::Less => Op::LtInt { rd, ra, rb },
                ResolvedBinaryOp::Greater => Op::GtInt { rd, ra, rb },
                ResolvedBinaryOp::LessEqual => Op::LeInt { rd, ra, rb },
                ResolvedBinaryOp::GreaterEqual => Op::GeInt { rd, ra, rb },
                ResolvedBinaryOp::BitAnd => Op::BitAnd { rd, ra, rb },
                ResolvedBinaryOp::BitOr => Op::BitOr { rd, ra, rb },
                ResolvedBinaryOp::BitXor => Op::BitXor { rd, ra, rb },
                ResolvedBinaryOp::ShiftLeft => Op::Shl { rd, ra, rb },
                ResolvedBinaryOp::ShiftRight => Op::Shr { rd, ra, rb },
                _ => {
                    self.error(format!("operator {op:?} is invalid for integer result"));
                    return;
                }
            },
            MirAbiClass::Float { bits: 32 | 64 } => match op {
                ResolvedBinaryOp::Add => Op::AddFloat { rd, ra, rb },
                ResolvedBinaryOp::Subtract => Op::SubFloat { rd, ra, rb },
                ResolvedBinaryOp::Multiply => Op::MulFloat { rd, ra, rb },
                ResolvedBinaryOp::Divide => Op::DivFloat { rd, ra, rb },
                ResolvedBinaryOp::Power => Op::PowFloat { rd, ra, rb },
                ResolvedBinaryOp::Equal => Op::EqFloat { rd, ra, rb },
                ResolvedBinaryOp::NotEqual => Op::Ne { rd, ra, rb },
                ResolvedBinaryOp::Less => Op::LtFloat { rd, ra, rb },
                ResolvedBinaryOp::Greater => Op::GtFloat { rd, ra, rb },
                ResolvedBinaryOp::LessEqual => Op::LeFloat { rd, ra, rb },
                ResolvedBinaryOp::GreaterEqual => Op::GeFloat { rd, ra, rb },
                _ => {
                    self.error(format!("operator {op:?} is invalid for float result"));
                    return;
                }
            },
            MirAbiClass::Bool => match op {
                ResolvedBinaryOp::LogicalAnd => Op::And { rd, ra, rb },
                ResolvedBinaryOp::LogicalOr => Op::Or { rd, ra, rb },
                ResolvedBinaryOp::Equal => Op::Eq { rd, ra, rb },
                ResolvedBinaryOp::NotEqual => Op::Ne { rd, ra, rb },
                _ => {
                    self.error(format!("operator {op:?} is invalid for bool result"));
                    return;
                }
            },
            MirAbiClass::StringHandle => match op {
                ResolvedBinaryOp::Add => Op::ConcatStr { rd, ra, rb },
                ResolvedBinaryOp::Equal => Op::Eq { rd, ra, rb },
                ResolvedBinaryOp::NotEqual => Op::Ne { rd, ra, rb },
                _ => {
                    self.error(format!("operator {op:?} is invalid for string result"));
                    return;
                }
            },
            _ => {
                self.error(format!("binary result '{}' is not scalar", result));
                return;
            }
        };
        let i32_width = matches!(
            operand_desc.abi,
            MirAbiClass::Integer {
                bits: 32,
                signed: true
            }
        );
        if i32_width && matches!(op, ResolvedBinaryOp::Divide | ResolvedBinaryOp::Remainder) {
            self.proto.emit(Op::CheckI32DivRem { ra, rb });
        }
        if i32_width
            && matches!(
                op,
                ResolvedBinaryOp::ShiftLeft | ResolvedBinaryOp::ShiftRight
            )
        {
            self.proto.emit(Op::MaskShiftAmt { rb, mask: 31 });
        }
        self.proto.emit(opcode);
        if i32_width {
            match op {
                ResolvedBinaryOp::Add => {
                    self.proto.emit(Op::CheckI32 { rd, kind: 0 });
                }
                ResolvedBinaryOp::Subtract => {
                    self.proto.emit(Op::CheckI32 { rd, kind: 1 });
                }
                ResolvedBinaryOp::Multiply => {
                    self.proto.emit(Op::CheckI32 { rd, kind: 2 });
                }
                ResolvedBinaryOp::Power | ResolvedBinaryOp::ShiftLeft => {
                    self.proto.emit(Op::WrapI32 { rd });
                }
                _ => {}
            }
        }
    }

    fn emit_unary(&mut self, result: &MirValueId, op: ResolvedUnaryOp, operand: &MirValueId) {
        for value in [result, operand] {
            if let Err(message) = self.supported_type_for_value(value) {
                self.error(format!("unary value '{}' is unsupported: {message}", value));
                return;
            }
        }
        let (Some(rd), Some(ra)) = (self.reg(result), self.reg(operand)) else {
            return;
        };
        let Some(desc) = self.type_of(result) else {
            self.error(format!("unary result '{}' has no type descriptor", result));
            return;
        };
        let opcode = match (desc.abi, op) {
            (MirAbiClass::Integer { bits, signed: true }, ResolvedUnaryOp::Negate)
                if bits <= 64 =>
            {
                Op::NegInt { rd, ra }
            }
            (MirAbiClass::Integer { bits, signed: true }, ResolvedUnaryOp::Not) if bits <= 64 => {
                Op::BitNot { rd, ra }
            }
            (MirAbiClass::Float { bits: 32 | 64 }, ResolvedUnaryOp::Negate) => {
                Op::NegFloat { rd, ra }
            }
            (MirAbiClass::Bool, ResolvedUnaryOp::Not) => Op::Not { rd, ra },
            (MirAbiClass::StringHandle | MirAbiClass::Unit, ResolvedUnaryOp::Dereference) => {
                Op::Mov { rd, rs: ra }
            }
            _ => {
                self.error(format!(
                    "unary operator {op:?} is outside scalar bytecode slice"
                ));
                return;
            }
        };
        self.proto.emit(opcode);
    }

    fn emit_call(
        &mut self,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        arguments: &[MirValueId],
    ) {
        let ResolvedCallee::Function(owner) = callee else {
            self.error(format!("callee {callee:?} is not a canonical function"));
            return;
        };
        let Some(&func) = self.indices.get(owner) else {
            self.error(format!("callee '{}' is absent from MIR program", owner.0));
            return;
        };
        for argument in arguments {
            if let Err(message) = self.supported_type_for_value(argument) {
                self.error(format!(
                    "call argument '{}' is unsupported: {message}",
                    argument
                ));
                return;
            }
        }
        if let Some(result) = result {
            if let Err(message) = self.supported_type_for_value(result) {
                self.error(format!(
                    "call result '{}' is unsupported: {message}",
                    result
                ));
                return;
            }
        }
        let rd = result
            .and_then(|value| self.reg(value))
            .unwrap_or_else(|| self.proto.alloc_reg());
        // Calls require a consecutive argument range.  Materialize a parallel
        // copy so a source register can also be a destination of another copy.
        let args_base = self.proto.alloc_reg();
        let mut arg_regs = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let scratch = if arg_regs.is_empty() {
                args_base
            } else {
                self.proto.alloc_reg()
            };
            let Some(source) = self.reg(argument) else {
                return;
            };
            let Some(desc) = self.type_of(argument) else {
                self.error(format!(
                    "call argument '{}' has no type descriptor",
                    argument
                ));
                return;
            };
            if desc.ownership == MirOwnership::Copy {
                self.proto.emit(Op::Mov {
                    rd: scratch,
                    rs: source,
                });
            } else if desc.glue.move_out == MirGlueKind::OwnedString {
                self.proto.emit(Op::Move {
                    rd: scratch,
                    rs: source,
                });
            } else {
                self.error(format!(
                    "call argument '{}' has no canonical move glue",
                    argument
                ));
                return;
            }
            arg_regs.push(scratch);
        }
        debug_assert!(
            arguments.is_empty() || arg_regs.windows(2).all(|pair| pair[1] == pair[0] + 1)
        );
        self.proto.emit(Op::CallMove {
            rd,
            func,
            args_base,
            argc: arguments.len() as u16,
        });
    }

    fn emit_convert(&mut self, result: &MirValueId, source: &MirValueId) {
        for value in [result, source] {
            if let Err(message) = self.supported_type_for_value(value) {
                self.error(format!(
                    "conversion value '{}' is unsupported: {message}",
                    value
                ));
                return;
            }
        }
        let (Some(rd), Some(ra)) = (self.reg(result), self.reg(source)) else {
            return;
        };
        let (Some(from), Some(to)) = (self.type_of(source), self.type_of(result)) else {
            self.error("conversion operand lacks a type descriptor");
            return;
        };
        let opcode = match (from.abi, to.abi) {
            (MirAbiClass::Integer { bits, signed: true }, MirAbiClass::Float { bits: 32 | 64 })
                if bits <= 64 =>
            {
                Op::IntToFloat { rd, ra }
            }
            (from, to) if from == to => Op::Mov { rd, rs: ra },
            _ => {
                self.error(format!(
                    "conversion {:?} -> {:?} is outside bytecode slice",
                    from.abi, to.abi
                ));
                return;
            }
        };
        self.proto.emit(opcode);
    }

    fn emit_project(&mut self, result: &MirValueId, base: &MirValueId, projection: &MirProjection) {
        let (Some(rd), Some(ra)) = (self.reg(result), self.reg(base)) else {
            return;
        };
        for value in [result, base] {
            if let Err(message) = self.supported_type_for_value(value) {
                self.error(format!(
                    "projection value '{}' is unsupported: {message}",
                    value
                ));
                return;
            }
        }
        let Some(base_desc) = self.type_of(base).cloned() else {
            self.error(format!("projection base '{}' has no type descriptor", base));
            return;
        };
        let Some(result_desc) = self.type_of(result).cloned() else {
            self.error(format!(
                "projection result '{}' has no type descriptor",
                result
            ));
            return;
        };
        match (&base_desc.layout, projection) {
            (MirLayout::Tuple(elements), MirProjection::Tuple(index)) => {
                let Some(element_ty) = elements.get(*index) else {
                    self.error(format!("tuple projection index {} is out of bounds", index));
                    return;
                };
                if element_ty != &result_desc.id {
                    self.error(format!(
                        "tuple projection result '{}' has type '{}' but layout selects '{}'",
                        result,
                        result_desc.id.as_str(),
                        element_ty.as_str()
                    ));
                    return;
                }
                if *index > u16::MAX as usize {
                    self.error("tuple projection index exceeds bytecode field ABI");
                    return;
                }
                self.proto.emit(Op::TupleGet {
                    rd,
                    ra,
                    idx: *index as u16,
                });
            }
            (MirLayout::Record { fields, .. }, MirProjection::Field(field)) => {
                let Some(field_desc) = fields.iter().find(|candidate| candidate.id == *field)
                else {
                    self.error(format!(
                        "record projection field '{}' is absent from TypeDesc",
                        field.0
                    ));
                    return;
                };
                if field_desc.ty != result_desc.id {
                    self.error(format!(
                        "record projection result '{}' has type '{}' but field selects '{}'",
                        result,
                        result_desc.id.as_str(),
                        field_desc.ty.as_str()
                    ));
                    return;
                }
                let field_idx = self
                    .proto
                    .add_const(ConstValue::Str(field_desc.name.clone()));
                self.proto.emit(Op::RecordGet {
                    rd,
                    ra,
                    field: field_idx,
                });
            }
            (_, MirProjection::Index(_)) => {
                self.error("indexed projection has no canonical MIR layout contract");
            }
            (_, MirProjection::Dereference) => {
                self.error("dereference projection has no canonical MIR layout contract");
            }
            (layout, projection) => self.error(format!(
                "projection {:?} does not match base layout {:?}",
                projection, layout
            )),
        }
    }

    fn emit_tuple_construct(&mut self, result: &MirValueId, fields: &[MirValueId]) {
        let Some(rd) = self.reg(result) else { return };
        let Some(result_desc) = self.type_of(result) else {
            self.error(format!("tuple result '{}' has no type descriptor", result));
            return;
        };
        let MirLayout::Tuple(elements) = &result_desc.layout else {
            self.error(format!("tuple result '{}' has no tuple layout", result));
            return;
        };
        if let Err(message) = self.supported_type(&result_desc.id) {
            self.error(format!(
                "tuple result '{}' is unsupported: {message}",
                result
            ));
            return;
        }
        if elements.len() != fields.len() {
            self.error(format!(
                "tuple construction has {} fields but layout expects {}",
                fields.len(),
                elements.len()
            ));
            return;
        }
        for (index, (field, expected_ty)) in fields.iter().zip(elements).enumerate() {
            let Some(field_info) = self.function.values.get(field) else {
                self.error(format!("tuple field '{}' is absent", field));
                return;
            };
            if &field_info.ty != expected_ty {
                self.error(format!(
                    "tuple field {} type '{}' disagrees with layout type '{}'",
                    index,
                    field_info.ty.as_str(),
                    expected_ty.as_str()
                ));
                return;
            }
            if let Err(message) = self.supported_type(expected_ty) {
                self.error(format!("tuple field {} is unsupported: {message}", index));
                return;
            }
        }
        if fields.len() > u16::MAX as usize {
            self.error("tuple arity exceeds bytecode aggregate ABI");
            return;
        }
        let base = self.proto.alloc_reg();
        for (index, field) in fields.iter().enumerate() {
            let Some(source) = self.reg(field) else {
                return;
            };
            let destination = if index == 0 {
                base
            } else {
                self.proto.alloc_reg()
            };
            self.proto.emit(Op::Mov {
                rd: destination,
                rs: source,
            });
        }
        self.proto.emit(Op::NewTuple {
            rd,
            base,
            arity: fields.len() as u16,
        });
    }

    fn emit_record_construct(
        &mut self,
        result: &MirValueId,
        nominal: &crate::core::ir::NominalTypeId,
        field_ids: &[crate::core::NodeId],
        values: &[MirValueId],
    ) {
        let Some(rd) = self.reg(result) else { return };
        let Some(result_desc) = self.type_of(result).cloned() else {
            self.error(format!("record result '{}' has no type descriptor", result));
            return;
        };
        let MirLayout::Record {
            nominal: expected_nominal,
            fields: layout_fields,
        } = result_desc.layout.clone()
        else {
            self.error(format!("record result '{}' has no record layout", result));
            return;
        };
        if nominal != &expected_nominal {
            self.error(format!(
                "record nominal '{}' disagrees with TypeDesc nominal '{}'",
                nominal.as_str(),
                expected_nominal.as_str()
            ));
            return;
        }
        if field_ids.len() != values.len() || field_ids.len() != layout_fields.len() {
            self.error("record construction field/value arity disagrees with TypeDesc");
            return;
        }
        if let Err(message) = self.supported_type(&result_desc.id) {
            self.error(format!(
                "record result '{}' is unsupported: {message}",
                result
            ));
            return;
        }
        let mut supplied = BTreeMap::new();
        for (field, value) in field_ids.iter().zip(values) {
            let Some(field_desc) = layout_fields
                .iter()
                .find(|candidate| candidate.id == *field)
            else {
                self.error(format!(
                    "record construction field '{}' is absent from TypeDesc",
                    field.0
                ));
                return;
            };
            let Some(value_info) = self.function.values.get(value) else {
                self.error(format!("record field '{}' is absent", value));
                return;
            };
            if value_info.ty != field_desc.ty {
                self.error(format!(
                    "record field '{}' type '{}' disagrees with layout type '{}'",
                    field.0,
                    value_info.ty.as_str(),
                    field_desc.ty.as_str()
                ));
                return;
            }
            if supplied.insert(field.clone(), value.clone()).is_some() {
                self.error(format!(
                    "record construction field '{}' is repeated",
                    field.0
                ));
                return;
            }
        }
        let base = self.proto.alloc_reg();
        for (index, field_desc) in layout_fields.iter().enumerate() {
            let Some(source_value) = supplied.get(&field_desc.id) else {
                self.error(format!(
                    "record construction omits field '{}'",
                    field_desc.name
                ));
                return;
            };
            let Some(source) = self.reg(source_value) else {
                return;
            };
            let destination = if index == 0 {
                base
            } else {
                self.proto.alloc_reg()
            };
            self.proto.emit(Op::Mov {
                rd: destination,
                rs: source,
            });
        }
        let type_name = self
            .proto
            .add_const_raw(ConstValue::Str(expected_nominal.as_str().to_string()));
        for field in &layout_fields {
            self.proto
                .add_const_raw(ConstValue::Str(field.name.clone()));
        }
        self.proto.emit(Op::NewRecord {
            rd,
            type_name,
            base,
            count: layout_fields.len() as u16,
        });
    }

    fn emit_variant_construct(
        &mut self,
        result: &MirValueId,
        nominal: &crate::core::ir::NominalTypeId,
        variant: &crate::core::NodeId,
        fields: &[(crate::core::NodeId, MirValueId)],
    ) {
        let Some(rd) = self.reg(result) else { return };
        let Some(result_desc) = self.type_of(result).cloned() else {
            self.error(format!(
                "variant result '{}' has no type descriptor",
                result
            ));
            return;
        };
        let Some((expected_nominal, variants)) =
            self.program.type_catalog().variant_layout(&result_desc.id)
        else {
            self.error(format!(
                "variant result '{}' has no canonical variant layout",
                result
            ));
            return;
        };
        if nominal.as_str() != expected_nominal {
            self.error(format!(
                "variant nominal '{}' disagrees with TypeDesc nominal '{}'",
                nominal.as_str(),
                expected_nominal
            ));
            return;
        }
        let Some(variant_desc) = variants.iter().find(|candidate| candidate.id == *variant) else {
            self.error(format!("variant '{}' is absent from TypeDesc", variant.0));
            return;
        };
        if let Err(message) = self.supported_type(&result_desc.id) {
            self.error(format!(
                "variant result '{}' is unsupported: {message}",
                result
            ));
            return;
        }
        if fields.len() != variant_desc.fields.len() {
            self.error(format!(
                "variant '{}' has {} fields but construction carries {}",
                variant_desc.name,
                variant_desc.fields.len(),
                fields.len()
            ));
            return;
        }
        let mut supplied = BTreeMap::new();
        for (field, value) in fields {
            let Some(field_desc) = variant_desc
                .fields
                .iter()
                .find(|candidate| candidate.id == *field)
            else {
                self.error(format!(
                    "variant field '{}' is absent from TypeDesc",
                    field.0
                ));
                return;
            };
            let Some(value_info) = self.function.values.get(value) else {
                self.error(format!(
                    "variant field value '{}' is absent from MIR",
                    value
                ));
                return;
            };
            if value_info.ty != field_desc.ty {
                self.error(format!(
                    "variant field '{}' type '{}' disagrees with layout type '{}'",
                    field.0,
                    value_info.ty.as_str(),
                    field_desc.ty.as_str()
                ));
                return;
            }
            if supplied.insert(field.clone(), value.clone()).is_some() {
                self.error(format!("variant field '{}' is repeated", field.0));
                return;
            }
        }
        if supplied.len() != variant_desc.fields.len() {
            self.error(format!(
                "variant '{}' construction omits a payload field",
                variant_desc.name
            ));
            return;
        }
        if variant_desc.fields.len() > u16::MAX as usize {
            self.error("variant payload arity exceeds bytecode aggregate ABI");
            return;
        }
        let base = self.proto.alloc_reg();
        for (index, field_desc) in variant_desc.fields.iter().enumerate() {
            let Some(value) = supplied.get(&field_desc.id) else {
                self.error("variant payload field disappeared during emission");
                return;
            };
            let Some(source) = self.reg(value) else {
                return;
            };
            let destination = if index == 0 {
                base
            } else {
                self.proto.alloc_reg()
            };
            self.proto.emit(Op::Mov {
                rd: destination,
                rs: source,
            });
        }
        let type_name = self
            .proto
            .add_const(ConstValue::Str(variant_desc.name.clone()));
        self.proto.emit(Op::NewVariant {
            rd,
            type_name,
            variant: variant_desc.discriminant,
            base,
            arity: variant_desc.fields.len() as u16,
        });
    }

    fn emit_record_update(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        nominal: &crate::core::ir::NominalTypeId,
        field_ids: &[crate::core::NodeId],
        values: &[MirValueId],
    ) {
        let (Some(rd), Some(ra)) = (self.reg(result), self.reg(base)) else {
            return;
        };
        let Some(result_desc) = self.type_of(result).cloned() else {
            self.error(format!(
                "record update result '{}' has no type descriptor",
                result
            ));
            return;
        };
        let Some(base_desc) = self.type_of(base).cloned() else {
            self.error(format!(
                "record update base '{}' has no type descriptor",
                base
            ));
            return;
        };
        let MirLayout::Record {
            nominal: expected_nominal,
            fields: layout_fields,
        } = result_desc.layout.clone()
        else {
            self.error("record update result has no record layout");
            return;
        };
        let MirLayout::Record {
            nominal: base_nominal,
            fields: base_fields,
        } = base_desc.layout
        else {
            self.error("record update base has no record layout");
            return;
        };
        if nominal != &expected_nominal
            || base_nominal != expected_nominal
            || base_fields != layout_fields
        {
            self.error("record update nominal/layout disagrees with TypeDesc");
            return;
        }
        if field_ids.len() != values.len() || field_ids.len() > u16::MAX as usize {
            self.error("record update field/value arity exceeds the bytecode ABI");
            return;
        }
        if let Err(message) = self.supported_type(&result_desc.id) {
            self.error(format!("record update result is unsupported: {message}"));
            return;
        }
        let mut supplied = BTreeMap::new();
        for (field, value) in field_ids.iter().zip(values) {
            let Some(field_desc) = layout_fields
                .iter()
                .find(|candidate| candidate.id == *field)
            else {
                self.error(format!(
                    "record update field '{}' is absent from TypeDesc",
                    field.0
                ));
                return;
            };
            let Some(value_info) = self.function.values.get(value) else {
                self.error(format!("record update value '{}' is absent", value));
                return;
            };
            if value_info.ty != field_desc.ty {
                self.error(format!(
                    "record update field '{}' type '{}' disagrees with layout type '{}'",
                    field.0,
                    value_info.ty.as_str(),
                    field_desc.ty.as_str()
                ));
                return;
            }
            if supplied.insert(field.clone(), value.clone()).is_some() {
                self.error(format!("record update field '{}' is repeated", field.0));
                return;
            }
        }
        let update_base = self.proto.alloc_reg();
        for (index, field_desc) in layout_fields
            .iter()
            .filter(|field| supplied.contains_key(&field.id))
            .enumerate()
        {
            let Some(source_value) = supplied.get(&field_desc.id) else {
                return;
            };
            let Some(source) = self.reg(source_value) else {
                return;
            };
            let destination = if index == 0 {
                update_base
            } else {
                self.proto.alloc_reg()
            };
            self.proto.emit(Op::Mov {
                rd: destination,
                rs: source,
            });
        }
        let type_name = self
            .proto
            .add_const_raw(ConstValue::Str(expected_nominal.as_str().to_string()));
        for field in layout_fields
            .iter()
            .filter(|field| supplied.contains_key(&field.id))
        {
            self.proto
                .add_const_raw(ConstValue::Str(field.name.clone()));
        }
        self.proto.emit(Op::UpdateRecord {
            rd,
            type_name,
            ra,
            base: update_base,
            count: supplied.len() as u16,
        });
    }

    fn supported_type_for_value(&self, value: &MirValueId) -> Result<(), String> {
        let info = self
            .function
            .values
            .get(value)
            .ok_or_else(|| format!("value '{}' is absent from MIR value catalog", value))?;
        self.supported_type(&info.ty)
    }

    fn supported_type(&self, ty: &crate::core::ResolvedTypeId) -> Result<(), String> {
        let desc = self
            .program
            .type_catalog()
            .get(ty)
            .ok_or_else(|| format!("type '{}' is absent from MIR type catalog", ty.as_str()))?;
        match desc.abi {
            MirAbiClass::Integer { bits, signed } if signed && bits <= 64 => Ok(()),
            MirAbiClass::Float { bits } if bits == 32 || bits == 64 => Ok(()),
            MirAbiClass::Bool | MirAbiClass::Unit if desc.ownership == MirOwnership::Copy => Ok(()),
            MirAbiClass::StringHandle
                if desc.ownership == MirOwnership::Move
                    && desc.glue.move_out == MirGlueKind::OwnedString
                    && desc.glue.clone == MirGlueKind::OwnedString
                    && desc.glue.drop == MirGlueKind::OwnedString =>
            {
                Ok(())
            }
            MirAbiClass::Aggregate => match &desc.layout {
                MirLayout::Tuple(elements) => {
                    if desc.ownership != MirOwnership::Copy {
                        return Err(format!(
                            "type '{}' has ownership {:?} and needs runtime glue",
                            ty.as_str(),
                            desc.ownership
                        ));
                    }
                    for element in elements {
                        self.supported_type(element)?;
                    }
                    Ok(())
                }
                MirLayout::Record { fields, .. } => {
                    if desc.ownership != MirOwnership::Copy {
                        return Err(format!(
                            "type '{}' has ownership {:?} and needs runtime glue",
                            ty.as_str(),
                            desc.ownership
                        ));
                    }
                    for field in fields {
                        self.supported_type(&field.ty)?;
                    }
                    Ok(())
                }
                MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => {
                    if desc.ownership != MirOwnership::Copy {
                        return Err(format!(
                            "type '{}' has ownership {:?} and needs runtime glue",
                            ty.as_str(),
                            desc.ownership
                        ));
                    }
                    for variant in variants {
                        for field in &variant.fields {
                            self.supported_type(&field.ty)?;
                        }
                    }
                    Ok(())
                }
                layout => Err(format!(
                    "aggregate layout {:?} is not in the canonical bytecode slice",
                    layout
                )),
            },
            _ => Err(format!(
                "type '{}' has unsupported ABI {:?}",
                ty.as_str(),
                desc.abi
            )),
        }
    }

    fn emit_terminator(&mut self, terminator: &MirTerminator) {
        match terminator {
            MirTerminator::Goto {
                target, arguments, ..
            } => {
                self.emit_edge_arguments(target, arguments);
                let jump = self.proto.emit(Op::Jmp { offset: 0 });
                self.pending_jumps.push((jump, target.clone()));
            }
            MirTerminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                let Some(condition) = self.reg(condition) else {
                    return;
                };
                let conditional = self.proto.emit(Op::JmpIfNot {
                    offset: 0,
                    ra: condition,
                });
                self.emit_edge_arguments(then_target, then_arguments);
                let then_jump = self.proto.emit(Op::Jmp { offset: 0 });
                self.pending_jumps.push((then_jump, then_target.clone()));
                let else_start = self.proto.code.len();
                self.proto.patch_jump_to(conditional, else_start);
                self.emit_edge_arguments(else_target, else_arguments);
                let else_jump = self.proto.emit(Op::Jmp { offset: 0 });
                self.pending_jumps.push((else_jump, else_target.clone()));
            }
            MirTerminator::Return { value } => match value {
                Some(value) => {
                    if let Some(reg) = self.reg(value) {
                        self.proto.emit(Op::Ret { ra: reg });
                    }
                }
                None => {
                    self.proto.emit(Op::RetUnit);
                }
            },
            MirTerminator::Switch { scrutinee, arms } => self.emit_switch(scrutinee, arms),
            MirTerminator::Trap { code } => {
                self.error(format!("trap terminator '{code}' is not lowered"))
            }
            MirTerminator::Fault { .. } => {
                self.error("fault terminators are deferred to flow lowering")
            }
            MirTerminator::Unreachable => {
                self.error("unreachable terminator is not executable bytecode")
            }
        }
    }

    fn emit_switch(&mut self, scrutinee: &MirValueId, arms: &[crate::core::mir::MirSwitchArm]) {
        let Some(scrutinee_reg) = self.reg(scrutinee) else {
            return;
        };
        let Some(scrutinee_info) = self.function.values.get(scrutinee) else {
            self.error(format!(
                "switch scrutinee '{}' is absent from MIR",
                scrutinee
            ));
            return;
        };
        let Some((nominal, variants)) = self
            .program
            .type_catalog()
            .variant_layout(&scrutinee_info.ty)
        else {
            self.error("switch has no canonical variant layout");
            return;
        };
        if let Err(message) = self.supported_type(&scrutinee_info.ty) {
            self.error(format!("switch scrutinee is unsupported: {message}"));
            return;
        }
        let mut has_default = false;
        for arm in arms {
            match &arm.case {
                crate::core::mir::MirSwitchCase::Variant(variant) => {
                    let Some(variant_desc) =
                        variants.iter().find(|candidate| candidate.id == *variant)
                    else {
                        self.error(format!(
                            "switch variant '{}' is absent from TypeDesc",
                            variant.0
                        ));
                        return;
                    };
                    let condition = self.proto.alloc_reg();
                    let tag = self
                        .proto
                        .add_const(ConstValue::Str(variant_desc.name.clone()));
                    self.proto.emit(Op::IsVariant {
                        rd: condition,
                        ra: scrutinee_reg,
                        tag,
                    });
                    let next_arm = self.proto.emit(Op::JmpIfNot {
                        offset: 0,
                        ra: condition,
                    });
                    self.emit_variant_edge_arguments(
                        arm.target.clone(),
                        &arm.arguments,
                        &arm.bindings,
                        scrutinee_reg,
                        variant_desc,
                    );
                    let jump = self.proto.emit(Op::Jmp { offset: 0 });
                    self.pending_jumps.push((jump, arm.target.clone()));
                    self.proto.patch_jump_to(next_arm, self.proto.code.len());
                }
                crate::core::mir::MirSwitchCase::Default => {
                    has_default = true;
                    self.emit_edge_arguments(&arm.target, &arm.arguments);
                    let jump = self.proto.emit(Op::Jmp { offset: 0 });
                    self.pending_jumps.push((jump, arm.target.clone()));
                }
                crate::core::mir::MirSwitchCase::Literal(_) => {
                    self.error(format!(
                        "literal switch case is invalid for canonical variant nominal '{}'",
                        nominal
                    ));
                    return;
                }
            }
        }
        if !has_default && arms.is_empty() {
            self.error("variant switch has no arms");
        }
    }

    fn emit_variant_edge_arguments(
        &mut self,
        target: crate::core::mir::MirBlockId,
        arguments: &[MirValueId],
        bindings: &[crate::core::mir::MirSwitchBinding],
        scrutinee: Reg,
        variant: &crate::core::mir::types::MirVariantDesc,
    ) {
        let Some(block) = self.function.blocks.get(&target) else {
            self.error(format!("edge target '{}' is absent", target));
            return;
        };
        if block.parameters.len() != arguments.len() + bindings.len() {
            self.error(format!("edge to '{}' has wrong argument arity", target));
            return;
        }
        let mut sources = Vec::with_capacity(arguments.len() + bindings.len());
        for argument in arguments {
            let Some(source) = self.reg(argument) else {
                return;
            };
            let scratch = self.proto.alloc_reg();
            let Some(argument_info) = self.function.values.get(argument) else {
                self.error(format!("edge argument '{}' is absent", argument));
                return;
            };
            if !self.emit_value_transfer(scratch, source, &argument_info.ty) {
                return;
            }
            sources.push(scratch);
        }
        for binding in bindings {
            let Some(index) = variant
                .fields
                .iter()
                .position(|field| field.id == binding.field)
            else {
                self.error(format!(
                    "switch binding field '{}' is absent",
                    binding.field.0
                ));
                return;
            };
            if index > u16::MAX as usize {
                self.error("variant payload index exceeds bytecode field ABI");
                return;
            }
            let scratch = self.proto.alloc_reg();
            self.proto.emit(Op::VariantGet {
                rd: scratch,
                ra: scrutinee,
                idx: index as u16,
            });
            sources.push(scratch);
        }
        for (source, parameter) in sources.into_iter().zip(&block.parameters) {
            let Some(destination) = self.reg(&parameter.value) else {
                return;
            };
            let Some(parameter_info) = self.function.values.get(&parameter.value) else {
                self.error(format!("edge parameter '{}' is absent", parameter.value));
                return;
            };
            if !self.emit_value_transfer(destination, source, &parameter_info.ty) {
                return;
            }
        }
    }

    fn emit_edge_arguments(
        &mut self,
        target: &crate::core::mir::MirBlockId,
        arguments: &[MirValueId],
    ) {
        let Some(block) = self.function.blocks.get(target) else {
            self.error(format!("edge target '{}' is absent", target));
            return;
        };
        if block.parameters.len() != arguments.len() {
            self.error(format!("edge to '{}' has wrong argument arity", target));
            return;
        }
        let mut scratch = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let Some(source) = self.reg(argument) else {
                return;
            };
            let temp = self.proto.alloc_reg();
            let Some(argument_info) = self.function.values.get(argument) else {
                self.error(format!("edge argument '{}' is absent", argument));
                return;
            };
            if !self.emit_value_transfer(temp, source, &argument_info.ty) {
                return;
            }
            scratch.push(temp);
        }
        for (temp, parameter) in scratch.into_iter().zip(&block.parameters) {
            let Some(destination) = self.reg(&parameter.value) else {
                return;
            };
            let Some(parameter_info) = self.function.values.get(&parameter.value) else {
                self.error(format!("edge parameter '{}' is absent", parameter.value));
                return;
            };
            if !self.emit_value_transfer(destination, temp, &parameter_info.ty) {
                return;
            }
        }
    }

    fn emit_value_transfer(&mut self, rd: Reg, rs: Reg, ty: &crate::core::ResolvedTypeId) -> bool {
        let Some(desc) = self.program.type_catalog().get(ty) else {
            self.error(format!("transfer type '{}' has no TypeDesc", ty.as_str()));
            return false;
        };
        if let Err(message) = self.supported_type(ty) {
            self.error(format!(
                "transfer type '{}' is unsupported: {message}",
                ty.as_str()
            ));
            return false;
        }
        if desc.ownership == MirOwnership::Copy {
            self.proto.emit(Op::Mov { rd, rs });
            true
        } else if desc.glue.move_out == MirGlueKind::OwnedString {
            self.proto.emit(Op::Move { rd, rs });
            true
        } else {
            self.error(format!(
                "transfer type '{}' has no canonical move glue",
                ty.as_str()
            ));
            false
        }
    }

    fn patch_jumps(&mut self) {
        let pending = std::mem::take(&mut self.pending_jumps);
        for (jump, target) in pending {
            if let Some(&pc) = self.block_starts.get(&target) {
                self.proto.patch_jump_to(jump, pc);
            } else {
                self.error(format!("jump target '{}' has no bytecode address", target));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::compile_mir_program;
    use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
    use crate::core::mir::{MirOwnershipEvent, MirOwnershipEventKind};
    use crate::interp::bytecode::BytecodeVM;
    use crate::interp::bytecode::Op;
    use crate::interp::value::Value;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn compile(source: &str) -> std::sync::Arc<crate::interp::bytecode::BytecodeProgram> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        compile_mir_program(&mir).expect("MIR bytecode")
    }

    #[test]
    fn executes_scalar_call_through_canonical_mir() {
        let program =
            compile("func add_one(x: i32) -> i32 { x + 1 }\nfunc main() -> i32 { add_one(41) }");
        assert!(
            program.ast.is_none(),
            "canonical MIR consumer must not retain AST"
        );
        let value = BytecodeVM::new(program).run_value().expect("VM execution");
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn executes_if_cfg_through_canonical_mir() {
        let program = compile("func main() -> i32 { if true { 7 } else { 9 } }");
        let value = BytecodeVM::new(program).run_value().expect("VM execution");
        assert!(matches!(value, Value::Int(7)));
    }

    #[test]
    fn executes_tuple_construction_and_projection_through_canonical_mir() {
        let program = compile("func main() -> i32 { let pair = (40, 2); pair.0 + pair.1 }");
        let value = BytecodeVM::new(program).run_value().expect("VM execution");
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn executes_record_projection_and_update_through_canonical_mir() {
        let source = "type Point { x: i32, y: bool }\nfunc main() -> i32 { let p = Point { y: true, x: 40 }; let q = Point { y: false, ..p }; q.x }";
        let program = compile(source);
        let value = BytecodeVM::new(program).run_value().expect("VM execution");
        assert!(matches!(value, Value::Int(40)));
    }

    #[test]
    fn returns_tuple_through_canonical_mir() {
        let program = compile("func main() -> (i32, i32) { (40, 2) }");
        let value = BytecodeVM::new(program).run_value().expect("VM execution");
        assert!(matches!(
            value,
            Value::Tuple(items)
                if items.as_slice() == [Value::Int(40), Value::Int(2)]
        ));
    }

    #[test]
    fn emits_integer_comparisons_using_operand_type_not_bool_result_type() {
        let program = compile("func main() -> bool { 2 > 1 }");
        let value = BytecodeVM::new(program).run_value().expect("VM execution");
        assert!(matches!(value, Value::Bool(true)));
    }

    #[test]
    fn bytecode_and_reference_agree_on_the_same_canonical_mir() {
        let source = "func main() -> i32 { if 3 > 1 { 40 + 2 } else { 0 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = mir
            .functions()
            .keys()
            .find(|owner| owner.0 == "function:main")
            .cloned()
            .expect("main owner");
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = BytecodeVM::new(compile_mir_program(&mir).expect("MIR bytecode"))
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));
    }

    #[test]
    fn bytecode_and_reference_agree_on_record_projection_and_update() {
        let source = "type Point { x: i32, y: bool }\nfunc main() -> i32 { let p = Point { y: true, x: 40 }; let q = Point { y: false, ..p }; Point { x: q.x, y: true }.x }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = BytecodeVM::new(compile_mir_program(&mir).expect("MIR bytecode"))
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(40));
        assert!(matches!(bytecode, Value::Int(40)));
    }

    #[test]
    fn executes_copy_option_and_result_variants_through_canonical_mir() {
        for (source, expected) in [
            (
                "func main() -> i32 { let value: Option<i32> = Some(41); match value { Some(v) => v, None => 0 } }",
                41,
            ),
            (
                "func main() -> i32 { let value: Result<i32, i32> = Err(7); match value { Ok(v) => v, Err(e) => e } }",
                7,
            ),
            (
                "func main() -> i32 { let value: Option<i32> = None; match value { Some(v) => v, None => 0 } }",
                0,
            ),
            (
                "func main() -> i32 { let value: Result<i32, i32> = Ok(41); match value { Ok(v) => v, Err(e) => e } }",
                41,
            ),
        ] {
            let program = compile(source);
            let value = BytecodeVM::new(program).run_value().expect("VM execution");
            assert!(matches!(value, Value::Int(actual) if actual == expected));
        }
    }

    #[test]
    fn preserves_variant_return_shape_between_reference_and_bytecode() {
        let source = "func main() -> Option<i32> { Some(41) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = BytecodeVM::new(compile_mir_program(&mir).expect("MIR bytecode"))
            .run_value()
            .expect("bytecode execution");
        assert!(matches!(
            reference,
            MirRuntimeValue::Variant { variant, payload, .. }
                if variant.0 == "builtin:variant:Option::Some"
                    && payload == vec![MirRuntimeValue::Int(41)]
        ));
        assert!(matches!(
            bytecode,
            Value::Variant(tag, payload)
                if tag == "Some" && payload == vec![Value::Int(41)]
        ));
    }

    #[test]
    fn executes_owned_string_move_through_canonical_mir() {
        let source = "func main() -> string { let value = \"owned\"; value }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = BytecodeVM::new(compile_mir_program(&mir).expect("MIR bytecode"))
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::String("owned".into()));
        assert!(matches!(bytecode, Value::String(value) if value.as_str() == "owned"));
    }

    #[test]
    fn executes_owned_string_clone_and_drop_glue_through_canonical_mir() {
        let source = "func main() -> string { let value = \"owned\"; let copy = value; drop(copy); \"done\" }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let main = &bytecode.functions[bytecode.entry as usize];
        assert!(main.code.iter().any(|op| matches!(op, Op::Move { .. })));
        assert!(main.code.iter().any(|op| matches!(op, Op::Clone { .. })));
        assert!(main.code.iter().any(|op| matches!(op, Op::Drop { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::String("done".into()));
        assert!(matches!(value, Value::String(value) if value.as_str() == "done"));
    }

    #[test]
    fn transfers_owned_string_call_arguments_through_canonical_mir() {
        let source = "func identity(value: string) -> string { value }\nfunc main() -> string { identity(\"owned\") }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let main = &bytecode.functions[bytecode.entry as usize];
        assert!(main.code.iter().any(|op| matches!(op, Op::Move { .. })));
        assert!(main.code.iter().any(|op| matches!(op, Op::CallMove { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::String("owned".into()));
        assert!(matches!(value, Value::String(value) if value.as_str() == "owned"));
    }

    #[test]
    fn bytecode_and_reference_agree_on_copy_variant_match() {
        let source =
            "func main() -> i32 { let value: Option<i32> = Some(41); match value { Some(v) => v + 1, None => 0 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = BytecodeVM::new(compile_mir_program(&mir).expect("MIR bytecode"))
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(bytecode, Value::Int(42)));
    }

    #[test]
    fn rejects_move_variant_payload_before_any_backend() {
        let source =
            "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(v) => v, None => \"fallback\" } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("move payload needs explicit aggregate glue");
        assert!(format!("{error:?}").contains("no canonical"));
    }

    #[test]
    fn rejects_copy_of_owned_string_at_canonical_mir_boundary() {
        let source = "func main() -> string { let value = \"owned\"; value }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let mut function = mir.functions().get(&owner).cloned().expect("main");
        let instruction = function
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::Move { .. }
                )
            })
            .expect("string bind move");
        let crate::core::mir::MirInstructionKind::Move { result, source } =
            instruction.kind.clone()
        else {
            unreachable!();
        };
        instruction.kind = crate::core::mir::MirInstructionKind::Copy { result, source };
        let error = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            mir.type_catalog().clone(),
        )
        .expect_err("owned string cannot be copied");
        assert!(error
            .iter()
            .any(|error| error.message.contains("copy instruction is invalid")));
    }

    #[test]
    fn rejects_use_after_drop_before_any_backend() {
        let source = "func main() -> string { let value = \"owned\"; drop(value); \"done\" }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let mut function = mir.functions().get(&owner).cloned().expect("main");
        let block = function
            .blocks
            .values_mut()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        crate::core::mir::MirInstructionKind::Drop { .. }
                    )
                })
            })
            .expect("drop block");
        let value = block
            .instructions
            .iter()
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Drop { value } => Some(value.clone()),
                _ => None,
            })
            .expect("drop value");
        block.instructions.push(crate::core::mir::MirInstruction {
            id: crate::core::mir::MirInstructionId::new("synthetic/second-drop")
                .expect("instruction id"),
            kind: crate::core::mir::MirInstructionKind::Drop { value },
        });
        let error = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            mir.type_catalog().clone(),
        )
        .expect_err("a non-Copy value cannot be dropped twice");
        assert!(error
            .iter()
            .any(|error| error.message.contains("use after consuming non-Copy value")));
    }

    #[test]
    fn rejects_ownership_events_without_runtime_glue() {
        let source = "func main() -> i32 { 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let mut function = mir.functions().get(&owner).cloned().expect("main");
        let value = function
            .values
            .keys()
            .find(|value| {
                value
                    .to_string()
                    .starts_with("expr:function:main/node:expr.literal")
            })
            .cloned()
            .unwrap_or_else(|| function.values.keys().next().cloned().expect("value"));
        function.ownership.events.push(MirOwnershipEvent {
            kind: MirOwnershipEventKind::BorrowShared,
            resource: "synthetic/resource".into(),
            value: Some(value.clone()),
            source: Some("x".into()),
            target: None,
            point: crate::core::NodeId("synthetic/point".into()),
        });
        let patched = crate::core::mir::reference::MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            mir.type_catalog().clone(),
        )
        .expect("patched MIR remains structurally valid");
        let errors = compile_mir_program(&patched).expect_err("glue must be explicit");
        assert!(errors
            .iter()
            .any(|error| error.message.contains("borrow_shared")));
    }
}
