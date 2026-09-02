//! Direct consumer of canonical MIR for the bytecode VM.
//!
//! This adapter is intentionally narrow.  It proves the architectural seam:
//! once a `MirProgram` exists, bytecode emission no longer sees the AST,
//! resolver, or checker.  Unsupported MIR shapes are reported explicitly
//! instead of falling back to the legacy compiler.  The supported slice is
//! scalar values, calls, branches, loop-shaped CFG edges, and recursively
//! glued tuple/record products, and concrete Copy-scalar Lists.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::core::ir::{ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedUnaryOp};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirAbiClass, MirConversionContract, MirConversionKind, MirGlueKind, MirLayout, MirOwnership,
    MirTypeDesc, MirTypeKind,
};
use crate::core::mir::{
    MirAggregateKind, MirFunction, MirInstructionKind, MirListOperation, MirOwnershipEventKind,
    MirProjection, MirSetOperation, MirTerminator, MirValueId,
};
use crate::core::NodeId;

use super::instr::{
    BytecodeProgram, ConstIdx, ConstValue, FuncIdx, FunctionProto, Op, Reg, VariantShape,
};

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

    let builtin_names = super::registry::create_registry().names();
    Ok(Arc::new(BytecodeProgram {
        functions,
        entry,
        builtin_names,
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
    /// representation in this adapter.  The admitted immutable Copy-scalar
    /// borrow is value-shaped and therefore needs no separate bytecode glue;
    /// mutable, session, and actor effects remain fail-closed so an
    /// unsupported fact cannot be silently discarded.
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
                | MirOwnershipEventKind::BorrowMut => self.error(format!(
                    "ownership event '{}' for '{}' is outside the scalar bytecode glue slice",
                    event.kind.as_str(),
                    value
                )),
                MirOwnershipEventKind::BorrowShared | MirOwnershipEventKind::BorrowEnd => {}
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
                } else if matches!(
                    desc.glue.move_out,
                    MirGlueKind::OwnedString
                        | MirGlueKind::List
                        | MirGlueKind::Set
                        | MirGlueKind::Aggregate
                ) {
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
                } else if matches!(
                    desc.glue.clone,
                    MirGlueKind::OwnedString
                        | MirGlueKind::List
                        | MirGlueKind::Set
                        | MirGlueKind::Aggregate
                ) {
                    self.proto.emit(Op::Clone { rd, rs });
                } else {
                    self.error(format!(
                        "clone of {:?} value '{}' has no canonical clone glue",
                        desc.ownership, source
                    ));
                }
            }
            MirInstructionKind::Drop { value } => {
                let Some(desc) = self.type_of(value).cloned() else {
                    self.error(format!("drop value '{}' has no type descriptor", value));
                    return;
                };
                if self.supported_type(&desc.id).is_err() {
                    self.error(format!(
                        "value '{}' is not in the canonical bytecode slice",
                        value
                    ));
                } else if desc.ownership != MirOwnership::Copy
                    && matches!(
                        desc.glue.drop,
                        MirGlueKind::OwnedString | MirGlueKind::List | MirGlueKind::Set
                    )
                {
                    let Some(ra) = self.reg(value) else { return };
                    self.proto.emit(Op::Drop { ra });
                } else if desc.ownership != MirOwnership::Copy
                    && desc.glue.drop == MirGlueKind::Aggregate
                {
                    let Some(ra) = self.reg(value) else { return };
                    let arity = match &desc.layout {
                        MirLayout::Tuple(elements) => elements.len(),
                        MirLayout::Record { fields, .. } => fields.len(),
                        MirLayout::Option { .. } | MirLayout::Result { .. } => {
                            let Some(ra) = self.reg(value) else { return };
                            self.emit_drop_variant(ra, &desc.id);
                            return;
                        }
                        layout => {
                            self.error(format!(
                                "aggregate drop value '{}' has no product layout: {:?}",
                                value, layout
                            ));
                            return;
                        }
                    };
                    if arity > u16::MAX as usize {
                        self.error("aggregate drop arity exceeds bytecode ABI");
                        return;
                    }
                    self.proto.emit(Op::DropAggregate {
                        ra,
                        arity: arity as u16,
                    });
                } else if desc.ownership != MirOwnership::Copy {
                    self.error(format!(
                        "drop of {:?} value '{}' has no canonical drop glue",
                        desc.ownership, value
                    ));
                }
            }
            MirInstructionKind::Borrow {
                result,
                source,
                mutable,
            } => self.emit_borrow(result, source, *mutable),
            MirInstructionKind::EndBorrow { borrow } => self.emit_end_borrow(borrow),
            MirInstructionKind::Project {
                result,
                base,
                projection,
            } => {
                self.emit_project(result, base, projection);
            }
            MirInstructionKind::MoveProject {
                result,
                base,
                projection,
            } => {
                self.emit_move_project(result, base, projection);
            }
            MirInstructionKind::Construct {
                result,
                kind: MirAggregateKind::Tuple,
                fields,
            } => self.emit_tuple_construct(result, fields),
            MirInstructionKind::ConstructList { result, elements } => {
                self.emit_list_construct(result, elements)
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
            } => self.emit_list_op(result, *operation, list),
            MirInstructionKind::ConstructSet { result, elements } => {
                self.emit_set_construct(result, elements)
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => self.emit_set_op(result, *operation, set, argument.as_ref()),
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
            } => self.emit_variant_construct(result, nominal, variant, fields, false),
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => self.emit_variant_construct(result, nominal, variant, fields, true),
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
                ..
            } => self.emit_call(result.as_ref(), callee, arguments),
            MirInstructionKind::FlowTransition {
                result,
                transition,
                arguments,
            } => self.emit_flow_transition(result, transition, arguments),
            MirInstructionKind::BuiltinCall {
                result,
                kind,
                arguments,
            } => self.emit_builtin_call(result, *kind, arguments),
            MirInstructionKind::Convert { result, source } => self.emit_convert(result, source),
            MirInstructionKind::Nop => {}
        }
    }

    fn emit_builtin_call(
        &mut self,
        result: &MirValueId,
        kind: crate::core::mir::types::MirBuiltinKind,
        arguments: &[MirValueId],
    ) {
        let contract = crate::core::mir::types::MirBuiltinContract::for_kind(kind);
        if arguments.len() != contract.arity {
            self.error(format!(
                "builtin '{}' has {} arguments but its MIR contract requires {}",
                contract.name,
                arguments.len(),
                contract.arity
            ));
            return;
        }
        for value in arguments.iter().chain(std::iter::once(result)) {
            if let Err(message) = self.supported_type_for_value(value) {
                self.error(format!(
                    "builtin '{}' value '{}' is unsupported: {message}",
                    contract.name, value
                ));
                return;
            }
        }
        let mut first_type = None;
        for (index, argument) in arguments.iter().enumerate() {
            let Some(argument_desc) = self.type_of(argument) else {
                self.error(format!(
                    "builtin '{}' argument {index} '{}' has no TypeDesc",
                    contract.name, argument
                ));
                return;
            };
            if contract.requires_same_input_type {
                if let Some(first_type) = &first_type {
                    let Some(argument_info) = self.function.values.get(argument) else {
                        return;
                    };
                    if first_type != &argument_info.ty {
                        self.error(format!(
                            "builtin '{}' arguments do not share one canonical ResolvedTypeId",
                            contract.name
                        ));
                        return;
                    }
                } else if let Some(argument_info) = self.function.values.get(argument) {
                    first_type = Some(argument_info.ty.clone());
                }
            } else if first_type.is_none() {
                first_type = self
                    .function
                    .values
                    .get(argument)
                    .map(|value| value.ty.clone());
            }
            if !contract.accepts_abi(argument_desc.abi)
                || !contract.accepts_layout(&argument_desc.layout)
                || (contract.requires_copy && argument_desc.ownership != MirOwnership::Copy)
            {
                self.error(format!(
                    "builtin '{}' argument {index} does not satisfy its canonical TypeDesc/ABI contract (accepted ABI: {})",
                    contract.name,
                    contract.accepted_abi_description()
                ));
                return;
            }
        }
        let Some(first_type) = first_type else {
            return;
        };
        let Some(result_info) = self.function.values.get(result) else {
            return;
        };
        if contract.preserves_type && result_info.ty != first_type {
            self.error(format!(
                "builtin '{}' result does not preserve the canonical argument type",
                contract.name
            ));
            return;
        }
        if contract.result_must_be_unit {
            let valid_unit = self
                .program
                .type_catalog()
                .get(&result_info.ty)
                .is_some_and(|descriptor| {
                    descriptor.layout == MirLayout::Unit
                        && descriptor.abi == MirAbiClass::Unit
                        && descriptor.ownership == MirOwnership::Copy
                        && descriptor.glue
                            == (crate::core::mir::types::MirGlueContract {
                                move_out: MirGlueKind::Noop,
                                clone: MirGlueKind::Noop,
                                drop: MirGlueKind::Noop,
                            })
                });
            if !valid_unit {
                self.error(format!(
                    "builtin '{}' result must be the canonical Copy unit TypeDesc",
                    contract.name
                ));
                return;
            }
        }
        let Some(rd) = self.reg(result) else { return };
        // Builtin operands use the same consecutive range ABI as calls.  The
        // copies are explicit so MIR value register allocation remains an
        // implementation detail and never becomes semantic argument order.
        let args_base = self.proto.alloc_reg();
        for (index, argument) in arguments.iter().enumerate() {
            let destination = if index == 0 {
                args_base
            } else {
                self.proto.alloc_reg()
            };
            let Some(source) = self.reg(argument) else {
                return;
            };
            self.proto.emit(Op::Mov {
                rd: destination,
                rs: source,
            });
        }
        let registry = super::registry::create_registry();
        let Some(builtin) = registry.lookup(contract.name) else {
            self.error(format!(
                "builtin '{}' has no bytecode registry implementation",
                contract.name
            ));
            return;
        };
        self.proto.emit(Op::CallBuiltin {
            rd,
            builtin,
            args_base,
            argc: contract.arity as u16,
        });
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
            } else if matches!(
                current_ty_desc.glue.clone,
                MirGlueKind::OwnedString
                    | MirGlueKind::List
                    | MirGlueKind::Set
                    | MirGlueKind::Aggregate
            ) {
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
        self.emit_call_target(result, func, arguments);
    }

    fn emit_flow_transition(
        &mut self,
        result: &MirValueId,
        transition: &NodeId,
        arguments: &[MirValueId],
    ) {
        let Some(contract) = self.program.transitions().get(transition) else {
            self.error(format!(
                "flow transition '{}' has no canonical contract",
                transition.0
            ));
            return;
        };
        if contract.effect != crate::core::mir::MirTransitionEffect::SilentLocal
            || contract.targets.len() != 1
            || contract.failure.is_some()
            || contract.is_fallback
            || contract.is_ffi_pinned
        {
            self.error(format!(
                "flow transition '{}' is outside the bytecode production contract",
                transition.0
            ));
            return;
        }
        let Some(&func) = self.indices.get(&contract.owner) else {
            self.error(format!(
                "flow transition '{}' executable body is absent from MIR program",
                transition.0
            ));
            return;
        };
        self.emit_call_target(Some(result), func, arguments);
    }

    fn emit_call_target(
        &mut self,
        result: Option<&MirValueId>,
        func: FuncIdx,
        arguments: &[MirValueId],
    ) {
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
            } else if matches!(
                desc.glue.move_out,
                MirGlueKind::OwnedString
                    | MirGlueKind::List
                    | MirGlueKind::Set
                    | MirGlueKind::Aggregate
            ) {
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
        let Some(contract) = MirConversionContract::for_descriptors(from, to) else {
            self.error(format!(
                "conversion {:?}/layout {:?}/ownership {:?} -> {:?}/layout {:?}/ownership {:?} is outside the canonical contract (accepted: {})",
                from.abi,
                from.layout,
                from.ownership,
                to.abi,
                to.layout,
                to.ownership,
                MirConversionContract::accepted_description()
            ));
            return;
        };
        let opcode = match contract.kind {
            MirConversionKind::ScalarIdentity | MirConversionKind::SignedI32ToI64 => {
                Op::Mov { rd, rs: ra }
            }
        };
        self.proto.emit(opcode);
    }

    fn emit_borrow(&mut self, result: &MirValueId, source: &MirValueId, mutable: bool) {
        let (Some(result_value), Some(source_value)) = (
            self.function.values.get(result),
            self.function.values.get(source),
        ) else {
            return;
        };
        if let Err(message) =
            self.program
                .type_catalog()
                .validate_borrow(&source_value.ty, &result_value.ty, mutable)
        {
            self.error(format!("borrow is unsupported: {message}"));
            return;
        }
        for value in [result, source] {
            if let Err(message) = self.supported_type_for_value(value) {
                self.error(format!(
                    "borrow value '{}' is unsupported: {message}",
                    value
                ));
                return;
            }
        }
        let (Some(rd), Some(rs)) = (self.reg(result), self.reg(source)) else {
            return;
        };
        // The admitted immutable Copy-scalar representation is value-shaped:
        // the target scalar is copied into the reference register. The
        // checker-owned Pointer/target TypeDesc still makes this a typed
        // reference boundary; no backend may generalize this to aliases.
        self.proto.emit(Op::Mov { rd, rs });
    }

    fn emit_end_borrow(&mut self, borrow: &MirValueId) {
        let Some(value) = self.function.values.get(borrow) else {
            return;
        };
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_reference_type(&value.ty)
        {
            self.error(format!("borrow end is unsupported: {message}"));
            return;
        }
        self.proto.emit(Op::Nop);
    }

    fn emit_project(&mut self, result: &MirValueId, base: &MirValueId, projection: &MirProjection) {
        let (Some(rd), Some(ra)) = (self.reg(result), self.reg(base)) else {
            return;
        };
        let index_value = match projection {
            MirProjection::Index(index) => Some(index),
            _ => None,
        };
        for value in [result, base].into_iter().chain(index_value) {
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
            (MirLayout::List { element }, MirProjection::Index(index)) => {
                let Some(index_value) = self.function.values.get(index) else {
                    self.error("List index is absent from MIR value catalog");
                    return;
                };
                if let Err(message) = self.program.type_catalog().validate_list_index(
                    &base_desc.id,
                    &result_desc.id,
                    &index_value.ty,
                ) {
                    self.error(format!("List index is unsupported: {message}"));
                    return;
                }
                if element != &result_desc.id {
                    self.error(format!(
                        "List index result '{}' disagrees with element type '{}'",
                        result_desc.id.as_str(),
                        element.as_str()
                    ));
                    return;
                }
                let Some(rb) = self.reg(index) else { return };
                self.proto.emit(Op::ListGet { rd, ra, rb });
            }
            (_, MirProjection::Index(_)) => {
                self.error("indexed projection requires a canonical List layout");
            }
            (_, MirProjection::Dereference) => {
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_dereference(&base_desc.id, &result_desc.id)
                {
                    self.error(format!("dereference projection is unsupported: {message}"));
                    return;
                }
                self.proto.emit(Op::Mov { rd, rs: ra });
            }
            (layout, projection) => self.error(format!(
                "projection {:?} does not match base layout {:?}",
                projection, layout
            )),
        }
    }

    fn emit_move_project(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
    ) {
        let (Some(rd), Some(ra)) = (self.reg(result), self.reg(base)) else {
            return;
        };
        for value in [result, base] {
            if let Err(message) = self.supported_type_for_value(value) {
                self.error(format!(
                    "move projection value '{}' is unsupported: {message}",
                    value
                ));
                return;
            }
        }
        let Some(base_desc) = self.type_of(base).cloned() else {
            self.error(format!(
                "move projection base '{}' has no type descriptor",
                base
            ));
            return;
        };
        let Some(result_desc) = self.type_of(result).cloned() else {
            self.error(format!(
                "move projection result '{}' has no type descriptor",
                result
            ));
            return;
        };
        if let Err(message) = self.program.type_catalog().validate_move_projection(
            &base_desc.id,
            &result_desc.id,
            projection,
        ) {
            self.error(format!(
                "move projection has no canonical contract: {message}"
            ));
            return;
        }
        let MirLayout::Record { fields, .. } = &base_desc.layout else {
            self.error("move projection base has no record layout");
            return;
        };
        let MirProjection::Field(field) = projection else {
            self.error("move projection requires a direct record field");
            return;
        };
        let Some(field_desc) = fields.iter().find(|candidate| candidate.id == *field) else {
            self.error(format!(
                "move projection field '{}' is absent from TypeDesc",
                field.0
            ));
            return;
        };
        let field_idx = self
            .proto
            .add_const(ConstValue::Str(field_desc.name.clone()));
        self.proto.emit(Op::RecordMoveGet {
            rd,
            ra,
            field: field_idx,
        });
    }

    fn emit_list_construct(&mut self, result: &MirValueId, elements: &[MirValueId]) {
        let Some(rd) = self.reg(result) else { return };
        let Some(result_desc) = self.type_of(result).cloned() else {
            self.error(format!("List result '{}' has no type descriptor", result));
            return;
        };
        let MirLayout::List { element } = &result_desc.layout else {
            self.error(format!("List result '{}' has no List layout", result));
            return;
        };
        let Some(element_types) = elements
            .iter()
            .map(|value| {
                self.function
                    .values
                    .get(value)
                    .map(|value| value.ty.clone())
            })
            .collect::<Option<Vec<_>>>()
        else {
            self.error("List construction element is absent from MIR value catalog");
            return;
        };
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_list_construct(&result_desc.id, &element_types)
        {
            self.error(format!("List construction is unsupported: {message}"));
            return;
        }
        if elements.len() > u32::MAX as usize {
            self.error("List construction length exceeds bytecode capacity ABI");
            return;
        }
        self.proto.emit(Op::NewList {
            rd,
            capacity: elements.len() as u32,
        });
        for (index, value) in elements.iter().enumerate() {
            let Some(rb) = self.reg(value) else { return };
            let Some(value_info) = self.function.values.get(value) else {
                self.error(format!("List element {} is absent from MIR values", index));
                return;
            };
            if &value_info.ty != element {
                self.error(format!(
                    "List element {} type '{}' disagrees with layout element type '{}'",
                    index,
                    value_info.ty.as_str(),
                    element.as_str()
                ));
                return;
            }
            self.proto.emit(Op::ListPush { ra: rd, rb });
        }
    }

    fn emit_set_construct(&mut self, result: &MirValueId, elements: &[MirValueId]) {
        let Some(rd) = self.reg(result) else { return };
        let Some(result_desc) = self.type_of(result).cloned() else {
            self.error(format!("Set result '{}' has no type descriptor", result));
            return;
        };
        let MirLayout::Set { element } = &result_desc.layout else {
            self.error(format!("Set result '{}' has no Set<T> layout", result));
            return;
        };
        let Some(element_types) = elements
            .iter()
            .map(|value| {
                self.function
                    .values
                    .get(value)
                    .map(|value| value.ty.clone())
            })
            .collect::<Option<Vec<_>>>()
        else {
            self.error("Set construction element is absent from MIR value catalog");
            return;
        };
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_set_construct(&result_desc.id, &element_types)
        {
            self.error(format!("Set construction is unsupported: {message}"));
            return;
        }
        self.proto.emit(Op::MirSetNew { rd });
        for (index, value) in elements.iter().enumerate() {
            let Some(rb) = self.reg(value) else { return };
            let Some(value_info) = self.function.values.get(value) else {
                self.error(format!("Set element {} is absent from MIR values", index));
                return;
            };
            if &value_info.ty != element {
                self.error(format!(
                    "Set element {} type '{}' disagrees with layout element type '{}'",
                    index,
                    value_info.ty.as_str(),
                    element.as_str()
                ));
                return;
            }
            self.proto.emit(Op::MirSetInsert { rd, ra: rd, rb });
        }
    }

    fn emit_set_op(
        &mut self,
        result: &MirValueId,
        operation: MirSetOperation,
        set: &MirValueId,
        argument: Option<&MirValueId>,
    ) {
        let Some(rd) = self.reg(result) else { return };
        let Some(ra) = self.reg(set) else { return };
        let Some(result_info) = self.function.values.get(result) else {
            self.error(format!("Set operation result '{}' is absent", result));
            return;
        };
        let Some(set_info) = self.function.values.get(set) else {
            self.error(format!("Set operation receiver '{}' is absent", set));
            return;
        };
        let argument_ty = argument
            .and_then(|value| self.function.values.get(value))
            .map(|value| &value.ty);
        if let Err(message) = self.program.type_catalog().validate_set_operation(
            &result_info.ty,
            &set_info.ty,
            argument_ty,
            operation,
        ) {
            self.error(format!("Set operation is unsupported: {message}"));
            return;
        }
        match operation {
            MirSetOperation::Size => {
                self.proto.emit(Op::MirSetSize { rd, ra });
            }
            MirSetOperation::IsEmpty => {
                self.proto.emit(Op::MirSetIsEmpty { rd, ra });
            }
            MirSetOperation::Contains => {
                let Some(argument) = argument.and_then(|value| self.reg(value)) else {
                    return;
                };
                self.proto.emit(Op::MirSetContains {
                    rd,
                    ra,
                    rb: argument,
                });
            }
            MirSetOperation::Insert | MirSetOperation::Remove => {
                let Some(argument) = argument.and_then(|value| self.reg(value)) else {
                    return;
                };
                let op = if operation == MirSetOperation::Insert {
                    Op::MirSetInsert {
                        rd,
                        ra,
                        rb: argument,
                    }
                } else {
                    Op::MirSetRemove {
                        rd,
                        ra,
                        rb: argument,
                    }
                };
                self.proto.emit(op);
            }
            MirSetOperation::ToList => {
                self.proto.emit(Op::MirSetToList { rd, ra });
            }
        }
    }

    fn emit_list_op(
        &mut self,
        result: &MirValueId,
        operation: MirListOperation,
        list: &MirValueId,
    ) {
        let Some(rd) = self.reg(result) else { return };
        let Some(ra) = self.reg(list) else { return };
        let Some(result_info) = self.function.values.get(result) else {
            self.error(format!("List operation result '{}' is absent", result));
            return;
        };
        let Some(list_info) = self.function.values.get(list) else {
            self.error(format!("List operation receiver '{}' is absent", list));
            return;
        };
        if let Err(message) = self.program.type_catalog().validate_list_operation(
            &result_info.ty,
            &list_info.ty,
            operation,
        ) {
            self.error(format!("List operation is unsupported: {message}"));
            return;
        }
        match operation {
            MirListOperation::Len => {
                self.proto.emit(Op::MirListLen { rd, ra });
            }
        }
    }

    fn emit_tuple_construct(&mut self, result: &MirValueId, fields: &[MirValueId]) {
        let Some(rd) = self.reg(result) else { return };
        let Some(result_desc) = self.type_of(result).cloned() else {
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
            if !self.emit_value_transfer(destination, source, &elements[index]) {
                return;
            }
        }
        if result_desc.ownership == MirOwnership::Copy {
            self.proto.emit(Op::NewTuple {
                rd,
                base,
                arity: fields.len() as u16,
            });
        } else {
            self.proto.emit(Op::NewTupleMove {
                rd,
                base,
                arity: fields.len() as u16,
            });
        }
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
            if result_desc.ownership == MirOwnership::Copy {
                self.proto.emit(Op::Mov {
                    rd: destination,
                    rs: source,
                });
            } else if !self.emit_value_transfer(destination, source, &field_desc.ty) {
                return;
            }
        }
        let type_name = self
            .proto
            .add_const_raw(ConstValue::Str(expected_nominal.as_str().to_string()));
        for field in &layout_fields {
            self.proto
                .add_const_raw(ConstValue::Str(field.name.clone()));
        }
        if result_desc.ownership == MirOwnership::Copy {
            self.proto.emit(Op::NewRecord {
                rd,
                type_name,
                base,
                count: layout_fields.len() as u16,
            });
        } else {
            self.proto.emit(Op::NewRecordMove {
                rd,
                type_name,
                base,
                count: layout_fields.len() as u16,
            });
        }
    }

    fn emit_variant_construct(
        &mut self,
        result: &MirValueId,
        nominal: &crate::core::ir::NominalTypeId,
        variant: &crate::core::NodeId,
        fields: &[(crate::core::NodeId, MirValueId)],
        move_payload: bool,
    ) {
        let Some(rd) = self.reg(result) else { return };
        let Some(result_desc) = self.type_of(result).cloned() else {
            self.error(format!(
                "variant result '{}' has no type descriptor",
                result
            ));
            return;
        };
        let field_ids = fields
            .iter()
            .map(|(field, _)| field.clone())
            .collect::<Vec<_>>();
        let Some(field_types) = fields
            .iter()
            .map(|(_, value)| self.function.values.get(value).map(|info| info.ty.clone()))
            .collect::<Option<Vec<_>>>()
        else {
            self.error("variant payload value is absent from MIR");
            return;
        };
        let variant_desc = match self.program.type_catalog().validated_variant_construct(
            &result_desc.id,
            nominal,
            variant,
            &field_ids,
            &field_types,
        ) {
            Ok(variant_desc) => variant_desc,
            Err(message) => {
                self.error(format!("variant construction rejected: {message}"));
                return;
            }
        };
        if let Err(message) = self.supported_type(&result_desc.id) {
            self.error(format!(
                "variant result '{}' is unsupported: {message}",
                result
            ));
            return;
        }
        let mut supplied = BTreeMap::new();
        for (field, value) in fields {
            if supplied.insert(field.clone(), value.clone()).is_some() {
                self.error(format!("variant field '{}' is repeated", field.0));
                return;
            }
        }
        if variant_desc.fields.len() > u16::MAX as usize {
            self.error("variant payload arity exceeds bytecode aggregate ABI");
            return;
        }
        let Some(shapes) = self.emit_variant_shape_table(&result_desc.id) else {
            return;
        };
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
            self.proto.emit(if move_payload {
                Op::Move {
                    rd: destination,
                    rs: source,
                }
            } else {
                Op::Mov {
                    rd: destination,
                    rs: source,
                }
            });
        }
        let type_name = self
            .proto
            .add_const(ConstValue::Str(variant_desc.name.clone()));
        self.proto.emit(if move_payload {
            Op::NewVariantMove {
                rd,
                type_name,
                variant: variant_desc.discriminant,
                base,
                arity: variant_desc.fields.len() as u16,
                shapes: Some(shapes),
            }
        } else {
            Op::NewVariant {
                rd,
                type_name,
                variant: variant_desc.discriminant,
                base,
                arity: variant_desc.fields.len() as u16,
                shapes: Some(shapes),
            }
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
            MirAbiClass::OpaqueHandle if matches!(desc.layout, MirLayout::List { .. }) => {
                self.program
                    .type_catalog()
                    .validate_list_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                let MirLayout::List { element } = &desc.layout else {
                    unreachable!()
                };
                self.supported_type(element)
            }
            MirAbiClass::SetHandle => {
                self.program
                    .type_catalog()
                    .validate_set_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                let MirLayout::Set { element } = &desc.layout else {
                    return Err(format!(
                        "Set ABI class has non-Set layout for type '{}'",
                        ty.as_str()
                    ));
                };
                self.supported_type(element)
            }
            MirAbiClass::Pointer
                if matches!(&desc.kind, MirTypeKind::Reference { mutable: false }) =>
            {
                self.program
                    .type_catalog()
                    .validate_reference_type(ty)
                    .map(|_| ())
            }
            MirAbiClass::Aggregate => match &desc.layout {
                MirLayout::Tuple(elements) => {
                    if desc.ownership != MirOwnership::Copy
                        && (desc.glue.move_out != MirGlueKind::Aggregate
                            || desc.glue.clone != MirGlueKind::Aggregate
                            || desc.glue.drop != MirGlueKind::Aggregate
                            || desc.drop_plan.is_none())
                    {
                        return Err(format!(
                            "type '{}' has ownership {:?} without a canonical aggregate glue/drop plan",
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
                    if desc.ownership != MirOwnership::Copy
                        && (desc.glue.move_out != MirGlueKind::Aggregate
                            || desc.glue.clone != MirGlueKind::Aggregate
                            || desc.glue.drop != MirGlueKind::Aggregate
                            || desc.drop_plan.is_none())
                    {
                        return Err(format!(
                            "type '{}' has ownership {:?} without a canonical aggregate glue/drop plan",
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
                        for operation in [
                            crate::core::mir::types::MirGlueOperation::MoveOut,
                            crate::core::mir::types::MirGlueOperation::Clone,
                            crate::core::mir::types::MirGlueOperation::Drop,
                        ] {
                            self.program.type_catalog().validate_glue(ty, operation)?;
                        }
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
            MirTerminator::Switch { scrutinee, arms } => self.emit_switch(scrutinee, arms, false),
            MirTerminator::SwitchMove { scrutinee, arms } => {
                self.emit_switch(scrutinee, arms, true)
            }
            MirTerminator::Trap { code } => {
                if let Err(message) = crate::core::mir::types::validate_trap_code(code) {
                    self.error(format!("trap terminator is invalid: {message}"));
                } else {
                    let msg = self
                        .proto
                        .add_const(ConstValue::Str(format!("trap {code}")));
                    self.proto.emit(Op::Trap { msg });
                }
            }
            MirTerminator::Fault { .. } => {
                self.error("fault terminators are deferred to flow lowering")
            }
            MirTerminator::Unreachable => {
                self.error("unreachable terminator is not executable bytecode")
            }
        }
    }

    fn emit_switch(
        &mut self,
        scrutinee: &MirValueId,
        arms: &[crate::core::mir::MirSwitchArm],
        consume_scrutinee: bool,
    ) {
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
        let Some((nominal, _)) = self
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
        let validation = if consume_scrutinee {
            self.program
                .type_catalog()
                .validate_switch_move(&scrutinee_info.ty, arms)
        } else {
            self.program
                .type_catalog()
                .validate_switch(&scrutinee_info.ty, arms)
        };
        if let Err(message) = validation {
            self.error(format!("switch is invalid: {message}"));
            return;
        }
        let mut has_default = false;
        for arm in arms {
            match &arm.case {
                crate::core::mir::MirSwitchCase::Variant(variant) => {
                    let variant_desc = match self
                        .program
                        .type_catalog()
                        .validated_variant_switch_case(&scrutinee_info.ty, variant)
                    {
                        Ok((_, variant_desc)) => variant_desc,
                        Err(message) => {
                            self.error(message);
                            return;
                        }
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
                    if consume_scrutinee {
                        self.emit_variant_move_edge_arguments(
                            arm.target.clone(),
                            &arm.arguments,
                            &arm.bindings,
                            &scrutinee_info.ty,
                            scrutinee_reg,
                            variant_desc,
                        );
                    } else {
                        self.emit_variant_edge_arguments(
                            arm.target.clone(),
                            &arm.arguments,
                            &arm.bindings,
                            &scrutinee_info.ty,
                            scrutinee_reg,
                            variant_desc,
                        );
                    }
                    let jump = self.proto.emit(Op::Jmp { offset: 0 });
                    self.pending_jumps.push((jump, arm.target.clone()));
                    self.proto.patch_jump_to(next_arm, self.proto.code.len());
                }
                crate::core::mir::MirSwitchCase::Default => {
                    has_default = true;
                    self.emit_edge_arguments(&arm.target, &arm.arguments);
                    if consume_scrutinee {
                        self.emit_drop_variant(scrutinee_reg, &scrutinee_info.ty);
                    }
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

    fn emit_variant_move_edge_arguments(
        &mut self,
        target: crate::core::mir::MirBlockId,
        arguments: &[MirValueId],
        bindings: &[crate::core::mir::MirSwitchBinding],
        scrutinee_ty: &crate::core::ResolvedTypeId,
        scrutinee: Reg,
        variant: &crate::core::mir::types::MirVariantDesc,
    ) {
        if bindings.is_empty() {
            self.emit_edge_arguments(&target, arguments);
            self.emit_drop_variant(scrutinee, scrutinee_ty);
            return;
        }
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
        if variant.fields.len() > u16::MAX as usize {
            self.error("variant payload arity exceeds bytecode field ABI");
            return;
        }
        let Some(shapes) = self.emit_variant_shape_table(scrutinee_ty) else {
            return;
        };
        let payload_base = self.proto.alloc_reg();
        for _ in 1..variant.fields.len() {
            self.proto.alloc_reg();
        }
        let variant_tag = self.proto.add_const(ConstValue::Str(variant.name.clone()));
        self.proto.emit(Op::DestructureVariantMove {
            ra: scrutinee,
            base: payload_base,
            arity: variant.fields.len() as u16,
            variant_tag,
            shapes,
        });
        for (index, field) in variant.fields.iter().enumerate().rev() {
            if !bindings.iter().any(|binding| binding.field == field.id) {
                self.emit_drop_register(payload_base + index as u16, &field.ty);
            }
        }
        for (binding_index, binding) in bindings.iter().enumerate() {
            let Some(parameter) = block
                .parameters
                .get(arguments.len() + binding_index)
                .and_then(|parameter| self.function.values.get(&parameter.value))
            else {
                self.error("switch-move binding target type is absent");
                return;
            };
            let index = match self
                .program
                .type_catalog()
                .validate_variant_payload_projection(
                    scrutinee_ty,
                    &variant.id,
                    &binding.field,
                    &parameter.ty,
                ) {
                Ok(index) => index,
                Err(message) => {
                    self.error(message);
                    return;
                }
            };
            if binding.parameter != block.parameters[arguments.len() + binding_index].value {
                self.error("switch-move binding parameter disagrees with target block parameter");
                return;
            }
            sources.push(payload_base + index as u16);
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

    fn emit_drop_variant(&mut self, register: Reg, ty: &crate::core::ResolvedTypeId) {
        let Some(shapes) = self.emit_variant_drop_shape_table(ty) else {
            return;
        };
        self.proto.emit(Op::DropVariant {
            ra: register,
            shapes,
        });
    }

    fn emit_variant_shape_table(&mut self, ty: &crate::core::ResolvedTypeId) -> Option<ConstIdx> {
        let (nominal, variants) = match self
            .program
            .type_catalog()
            .validated_variant_shape_table(ty)
        {
            Ok(contract) => contract,
            Err(message) => {
                self.error(format!(
                    "variant shape table type '{}' is unsupported: {message}",
                    ty.as_str()
                ));
                return None;
            }
        };
        let nominal_id = match crate::core::ir::NominalTypeId::new(nominal) {
            Ok(nominal) => nominal,
            Err(error) => {
                self.error(format!(
                    "variant shape table nominal '{}' is invalid: {error}",
                    nominal
                ));
                return None;
            }
        };
        let mut shapes = Vec::with_capacity(variants.len());
        for variant in variants {
            if variant.fields.len() > u16::MAX as usize {
                self.error(format!(
                    "variant '{}' in '{}' exceeds bytecode payload ABI",
                    variant.name, nominal
                ));
                return None;
            }
            shapes.push(VariantShape {
                nominal: nominal_id.clone(),
                variant: variant.id.clone(),
                tag: variant.name.clone(),
                discriminant: variant.discriminant,
                arity: variant.fields.len() as u16,
            });
        }
        Some(self.proto.add_const(ConstValue::VariantShapes(shapes)))
    }

    fn emit_variant_drop_shape_table(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
    ) -> Option<ConstIdx> {
        if let Err(message) = self
            .program
            .type_catalog()
            .validated_variant_drop_contract_table(ty)
        {
            self.error(format!(
                "variant drop type '{}' is unsupported: {message}",
                ty.as_str()
            ));
            return None;
        }
        self.emit_variant_shape_table(ty)
    }

    fn emit_drop_register(&mut self, register: Reg, ty: &crate::core::ResolvedTypeId) {
        let Some(descriptor) = self.program.type_catalog().get(ty) else {
            self.error(format!("drop register type '{}' is absent", ty.as_str()));
            return;
        };
        if descriptor.ownership == MirOwnership::Copy {
            return;
        }
        match descriptor.glue.drop {
            MirGlueKind::OwnedString => {
                self.proto.emit(Op::Drop { ra: register });
            }
            MirGlueKind::List => {
                // The opened List shape has Copy scalar elements, so the
                // handle release is the complete canonical drop operation.
                self.proto.emit(Op::Drop { ra: register });
            }
            MirGlueKind::Set => {
                self.proto.emit(Op::Drop { ra: register });
            }
            MirGlueKind::Aggregate => match &descriptor.layout {
                MirLayout::Tuple(elements) => {
                    self.proto.emit(Op::DropAggregate {
                        ra: register,
                        arity: elements.len() as u16,
                    });
                }
                MirLayout::Record { fields, .. } => {
                    self.proto.emit(Op::DropAggregate {
                        ra: register,
                        arity: fields.len() as u16,
                    });
                }
                MirLayout::Option { .. } | MirLayout::Result { .. } => {
                    self.emit_drop_variant(register, ty);
                }
                layout => self.error(format!(
                    "drop register type '{}' has unsupported aggregate layout {:?}",
                    ty.as_str(),
                    layout
                )),
            },
            MirGlueKind::Noop => {}
            MirGlueKind::Unsupported => self.error(format!(
                "drop register type '{}' has no canonical drop glue",
                ty.as_str()
            )),
        }
    }

    fn emit_variant_edge_arguments(
        &mut self,
        target: crate::core::mir::MirBlockId,
        arguments: &[MirValueId],
        bindings: &[crate::core::mir::MirSwitchBinding],
        scrutinee_ty: &crate::core::ResolvedTypeId,
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
        let Some(shapes) = self.emit_variant_shape_table(scrutinee_ty) else {
            return;
        };
        let variant_tag = self.proto.add_const(ConstValue::Str(variant.name.clone()));
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
        for (binding_index, binding) in bindings.iter().enumerate() {
            let Some(parameter) = block
                .parameters
                .get(arguments.len() + binding_index)
                .and_then(|parameter| self.function.values.get(&parameter.value))
            else {
                self.error("switch binding target type is absent");
                return;
            };
            let index = match self
                .program
                .type_catalog()
                .validate_variant_payload_projection(
                    scrutinee_ty,
                    &variant.id,
                    &binding.field,
                    &parameter.ty,
                ) {
                Ok(index) => index,
                Err(message) => {
                    self.error(message);
                    return;
                }
            };
            if binding.parameter != block.parameters[arguments.len() + binding_index].value {
                self.error("switch binding parameter disagrees with target block parameter");
                return;
            }
            if index > u16::MAX as usize {
                self.error("variant payload index exceeds bytecode field ABI");
                return;
            }
            let scratch = self.proto.alloc_reg();
            self.proto.emit(Op::VariantGet {
                rd: scratch,
                ra: scrutinee,
                idx: index as u16,
                variant_tag,
                shapes,
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
        } else if matches!(
            desc.glue.move_out,
            MirGlueKind::OwnedString
                | MirGlueKind::List
                | MirGlueKind::Set
                | MirGlueKind::Aggregate
        ) {
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
    use crate::core::mir::reference::{
        MirExecutionObservation, MirProgram, MirReferenceInterpreter, MirRuntimeValue,
    };
    use crate::core::mir::{MirOwnershipEvent, MirOwnershipEventKind};
    use crate::interp::bytecode::compiler::BytecodeCompiler;
    use crate::interp::bytecode::BytecodeVM;
    use crate::interp::bytecode::Op;
    use crate::interp::value::Value;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum DifferentialOutcome {
        Return(MirRuntimeValue),
        Error { class: String, message: String },
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct DifferentialObservation {
        outcome: DifferentialOutcome,
        output: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct DifferentialReport {
        source: String,
        mir_text: String,
        type_desc_text: String,
        ownership_text: String,
        reference: DifferentialObservation,
        mir_bytecode: DifferentialObservation,
        legacy_bytecode: DifferentialObservation,
    }

    #[derive(Debug)]
    enum DifferentialHarnessError {
        Parse(String),
        Check(String),
        CanonicalMir(String),
        CanonicalBytecode(String),
        LegacyBytecode(String),
        Observation(String),
        Mismatch(Box<DifferentialReport>),
    }

    fn parse_and_check(
        source: &str,
    ) -> Result<(crate::ast::File, crate::core::CheckedProgram), DifferentialHarnessError> {
        let tokens = Lexer::new(source)
            .tokenize()
            .map_err(|error| DifferentialHarnessError::Parse(error.to_string()))?;
        let file = Parser::new(tokens)
            .parse_file()
            .map_err(|error| DifferentialHarnessError::Parse(error.to_string()))?;
        let checked = crate::core::check_program(&file)
            .map_err(|errors| DifferentialHarnessError::Check(format!("{errors:?}")))?;
        Ok((file, checked))
    }

    fn canonical_program_text(mir: &MirProgram) -> String {
        mir.functions()
            .values()
            .map(|function| function.canonical_text())
            .collect::<Vec<_>>()
            .join("")
    }

    fn canonical_ownership_text(mir: &MirProgram) -> String {
        mir.functions()
            .values()
            .map(|function| {
                format!(
                    "{}\n{}",
                    function.owner.0,
                    function.ownership.canonical_text()
                )
            })
            .collect::<Vec<_>>()
            .join("")
    }

    fn normalize_value(value: Value) -> Result<MirRuntimeValue, String> {
        match value {
            Value::Int(value) => Ok(MirRuntimeValue::Int(value)),
            Value::Float(value) => Ok(MirRuntimeValue::FloatBits(value.to_bits())),
            Value::Bool(value) => Ok(MirRuntimeValue::Bool(value)),
            Value::String(value) => Ok(MirRuntimeValue::String((*value).clone())),
            Value::Unit => Ok(MirRuntimeValue::Unit),
            Value::Tuple(values) => values
                .into_iter()
                .map(normalize_value)
                .collect::<Result<Vec<_>, _>>()
                .map(MirRuntimeValue::Tuple),
            Value::List(values) => values
                .iter()
                .cloned()
                .map(normalize_value)
                .collect::<Result<Vec<_>, _>>()
                .map(MirRuntimeValue::List),
            Value::Set(values) => values
                .iter()
                .cloned()
                .map(normalize_value)
                .collect::<Result<Vec<_>, _>>()
                .map(MirRuntimeValue::Set),
            other => Err(format!(
                "differential value normalization is not materialized for {other:?}"
            )),
        }
    }

    fn reference_observation(
        result: Result<MirExecutionObservation, crate::core::mir::reference::MirExecutionError>,
    ) -> DifferentialObservation {
        match result {
            Ok(observation) => DifferentialObservation {
                outcome: DifferentialOutcome::Return(observation.value),
                output: observation.output,
            },
            Err(error) => DifferentialObservation {
                outcome: DifferentialOutcome::Error {
                    class: reference_error_class(&error.message),
                    message: error.message,
                },
                output: String::new(),
            },
        }
    }

    fn bytecode_observation(
        result: Result<Value, crate::interp::error::InterpError>,
        output: String,
    ) -> Result<DifferentialObservation, DifferentialHarnessError> {
        let outcome = match result {
            Ok(value) => DifferentialOutcome::Return(
                normalize_value(value).map_err(DifferentialHarnessError::Observation)?,
            ),
            Err(error) => DifferentialOutcome::Error {
                class: format!("runtime:{}", error.code()),
                message: error.message().to_owned(),
            },
        };
        Ok(DifferentialObservation { outcome, output })
    }

    fn reference_error_class(message: &str) -> String {
        if let Some(code) = message.strip_prefix("trap ") {
            return format!("trap:{code}");
        }
        if message.contains("division by zero") {
            return "runtime:E0801".into();
        }
        if message.contains("overflow") {
            return "runtime:E0802".into();
        }
        if message.contains("E0803") || message.contains("index out of bounds") {
            return "runtime:E0803".into();
        }
        "runtime:E0800".into()
    }

    fn observations_match(left: &DifferentialObservation, right: &DifferentialObservation) -> bool {
        left.output == right.output
            && match (&left.outcome, &right.outcome) {
                (DifferentialOutcome::Return(left), DifferentialOutcome::Return(right)) => {
                    left == right
                }
                (
                    DifferentialOutcome::Error { class: left, .. },
                    DifferentialOutcome::Error { class: right, .. },
                ) => left == right,
                _ => false,
            }
    }

    fn run_canonical_differential(
        source: &str,
    ) -> Result<DifferentialReport, DifferentialHarnessError> {
        let (file, checked) = parse_and_check(source)?;

        // This is the only frontend-to-MIR boundary in the harness.  A
        // failure here is a canonical eligibility failure, never permission
        // to run the legacy backend.
        let mir = MirProgram::from_checked_program(&checked)
            .map_err(|error| DifferentialHarnessError::CanonicalMir(format!("{error:?}")))?;
        let owner = crate::core::NodeId("function:main".into());
        let reference = reference_observation(
            MirReferenceInterpreter::new(&mir).execute_with_output(&owner, &[]),
        );

        // The canonical production backend is compiled from MIR only.  Its
        // AST-free program is an explicit contract of this harness.
        let mir_bytecode = compile_mir_program(&mir)
            .map_err(|errors| DifferentialHarnessError::CanonicalBytecode(format!("{errors:?}")))?;
        if mir_bytecode.ast.is_some() {
            return Err(DifferentialHarnessError::CanonicalBytecode(
                "canonical MIR bytecode retained an AST".into(),
            ));
        }
        let mut mir_vm = BytecodeVM::new(mir_bytecode);
        let mir_bytecode = bytecode_observation(mir_vm.run_value(), mir_vm.stdout().to_owned())?;

        // The legacy compiler is comparison-only in this slice.  It is
        // intentionally called after canonical construction/emission and can
        // never rescue a canonical rejection.
        let mut legacy_compiler = BytecodeCompiler::new();
        let legacy_program = legacy_compiler
            .compile_file(&file)
            .map_err(|error| DifferentialHarnessError::LegacyBytecode(error.to_string()))?;
        let mut legacy_vm = BytecodeVM::new(legacy_program);
        let legacy_bytecode =
            bytecode_observation(legacy_vm.run_value(), legacy_vm.stdout().to_owned())?;

        let report = DifferentialReport {
            source: source.into(),
            mir_text: canonical_program_text(&mir),
            type_desc_text: mir.type_catalog().canonical_text(),
            ownership_text: canonical_ownership_text(&mir),
            reference,
            mir_bytecode,
            legacy_bytecode,
        };
        let semantic_observations = [
            &report.reference,
            &report.mir_bytecode,
            &report.legacy_bytecode,
        ];
        let first = &semantic_observations[0];
        if semantic_observations
            .iter()
            .any(|observation| !observations_match(observation, first))
        {
            return Err(DifferentialHarnessError::Mismatch(Box::new(report)));
        }
        Ok(report)
    }

    #[test]
    fn canonical_mir_differential_covers_scalar_branch_call_and_tuple_shapes() {
        let cases = [
            ("scalar-binary", "func main() -> i32 { 40 + 2 }", "binary"),
            (
                "branch",
                "func main() -> i32 { if true { 7 } else { 9 } }",
                "branch",
            ),
            (
                "call",
                "func add_one(x: i32) -> i32 { x + 1 }\nfunc main() -> i32 { add_one(41) }",
                "call",
            ),
            (
                "tuple",
                "func main() -> (i32, bool) { (40, true) }",
                "construct",
            ),
            (
                "builtin-abs-i64",
                "func abs_i64(value: i64) -> i64 { abs(value) }\nfunc main() -> i32 { if abs_i64(-4294967297) == 4294967297 { 42 } else { 0 } }",
                "builtin_call",
            ),
            (
                "builtin-abs-f64",
                "func main() -> f64 { let value: f64 = -2.5; abs(value) }",
                "builtin_call",
            ),
            (
                "builtin-min-max-i64",
                "func min_i64(left: i64, right: i64) -> i64 { min(left, right) }\nfunc max_i64(left: i64, right: i64) -> i64 { max(left, right) }\nfunc main() -> i32 { if min_i64(9223372036854775806, 9223372036854775807) == 9223372036854775806 { if max_i64(-9223372036854775807, 9223372036854775806) == 9223372036854775806 { 42 } else { 0 } } else { 0 } }",
                "builtin_call",
            ),
            (
                "conversion-i32-to-i64-min",
                "func min_i64(left: i32, right: i32) -> i64 { min(left as i64, right as i64) }\nfunc main() -> i32 { if min_i64(1, 2) == 1 { 42 } else { 0 } }",
                "convert",
            ),
        ];

        for (name, source, shape) in cases {
            let report = run_canonical_differential(source)
                .unwrap_or_else(|error| panic!("{name} differential case failed: {error:?}"));
            assert!(report.mir_text.contains(shape), "missing MIR shape {shape}");
            assert!(report
                .type_desc_text
                .starts_with("mir.type-catalog mimi-mir-type-desc-"));
            assert!(report.reference.output.is_empty());
            assert!(report.mir_bytecode.output.is_empty());
            assert!(report.legacy_bytecode.output.is_empty());
        }
    }

    #[test]
    fn canonical_mir_differential_covers_set_contains_stdout_effect() {
        let source = include_str!("../../../tests/fixtures/mir_native_set_contains_println.mimi");
        let report = run_canonical_differential(source)
            .expect("Set.contains plus println(bool) differential");
        assert!(report.mir_text.contains("set_op"));
        assert!(report.mir_text.contains("PrintlnBool"));
        assert_eq!(report.reference.output, "true\nfalse\ntrue\n");
        assert_eq!(report.mir_bytecode.output, report.reference.output);
        assert_eq!(report.legacy_bytecode.output, report.reference.output);
        assert_eq!(report.mir_bytecode.outcome, report.reference.outcome);
        assert_eq!(report.legacy_bytecode.outcome, report.reference.outcome);
    }

    #[test]
    fn canonical_mir_differential_covers_standalone_stdout_effect() {
        let source =
            include_str!("../../../tests/fixtures/mir_native_println_bool_standalone.mimi");
        let report =
            run_canonical_differential(source).expect("standalone println(bool) differential");
        assert!(!report.mir_text.contains("set_op"));
        assert!(report.mir_text.contains("PrintlnBool"));
        assert_eq!(report.reference.output, "true\nfalse\n");
        assert_eq!(report.mir_bytecode.output, report.reference.output);
        assert_eq!(report.legacy_bytecode.output, report.reference.output);
        assert_eq!(report.mir_bytecode.outcome, report.reference.outcome);
        assert_eq!(report.legacy_bytecode.outcome, report.reference.outcome);
    }

    #[test]
    fn canonical_mir_differential_covers_integer_stdout_effect() {
        let source = include_str!("../../../tests/fixtures/mir_native_println_int.mimi");
        let report =
            run_canonical_differential(source).expect("standalone println(integer) differential");
        assert!(!report.mir_text.contains("set_op"));
        assert!(report.mir_text.contains("PrintlnInt"));
        assert_eq!(report.reference.output, "-7\n9223372036854775806\n");
        assert_eq!(report.mir_bytecode.output, report.reference.output);
        assert_eq!(report.legacy_bytecode.output, report.reference.output);
        assert_eq!(report.mir_bytecode.outcome, report.reference.outcome);
        assert_eq!(report.legacy_bytecode.outcome, report.reference.outcome);
    }

    #[test]
    fn canonical_mir_differential_preserves_ownership_artifact() {
        let source = "type Named { name: string, count: i32 }\nfunc main() -> i32 { let value = Named { name: \"owned\", count: 41 }; drop(value); 42 }";
        let report = run_canonical_differential(source).expect("record ownership differential");
        assert!(report.mir_text.contains("construct"));
        assert!(report.mir_text.contains("drop"));
        assert_eq!(report.ownership_text, "function:main\n");
        assert!(report.type_desc_text.contains("OwnedString"));
        assert!(report.type_desc_text.contains("drop=true"));
    }

    #[test]
    fn canonical_mir_differential_agrees_on_runtime_error_class() {
        let report = run_canonical_differential(
            "func divide(value: i32) -> i32 { 1 / value }\nfunc main() -> i32 { divide(0) }",
        )
        .expect("division-by-zero differential");
        for observation in [
            &report.reference,
            &report.mir_bytecode,
            &report.legacy_bytecode,
        ] {
            assert!(matches!(
                &observation.outcome,
                DifferentialOutcome::Error { class, .. } if class == "runtime:E0801"
            ));
        }
    }

    #[test]
    fn canonical_mir_differential_covers_copy_scalar_list_branch_and_return() {
        let report = run_canonical_differential(
            "func main() -> List<i32> { if true { [1, 2] } else { [3, 4] } }",
        )
        .expect("Copy-scalar List differential");
        assert!(report.mir_text.contains("construct_list"));
        assert!(report.type_desc_text.contains("layout=List"));
        assert_eq!(
            report.reference.outcome,
            DifferentialOutcome::Return(MirRuntimeValue::List(vec![
                MirRuntimeValue::Int(1),
                MirRuntimeValue::Int(2),
            ]))
        );
    }

    #[test]
    fn canonical_mir_differential_covers_bool_list_elements() {
        let report = run_canonical_differential(
            "func main() -> List<bool> { if false { [true] } else { [false, true] } }",
        )
        .expect("Copy-scalar bool List differential");
        assert!(report.type_desc_text.contains("layout=List"));
        assert_eq!(
            report.reference.outcome,
            DifferentialOutcome::Return(MirRuntimeValue::List(vec![
                MirRuntimeValue::Bool(false),
                MirRuntimeValue::Bool(true),
            ]))
        );
    }

    #[test]
    fn canonical_mir_list_len_is_ast_free_and_preserves_source_ownership() {
        let source = "func main() -> i32 { let values = [3, 1, 2]; let count = len(values); drop(values); count }";
        let report = run_canonical_differential(source).expect("canonical List.len differential");
        assert!(report.mir_text.contains("list_op"));
        assert_eq!(
            report.reference.outcome,
            DifferentialOutcome::Return(MirRuntimeValue::Int(3))
        );
        assert_eq!(report.mir_bytecode.outcome, report.reference.outcome);
        assert_eq!(report.legacy_bytecode.outcome, report.reference.outcome);
    }

    #[test]
    fn canonical_mir_rejects_list_len_for_non_copy_elements() {
        let source = "func main() -> i32 { let values = [\"owned\"]; len(values) }";
        let (_, checked) = parse_and_check(source).expect("source type check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("List.len must reject unsupported element ownership before a backend");
        let crate::core::mir::reference::MirProgramBuildError::Validation(errors) = error else {
            panic!("unexpected canonical List.len rejection: {error:?}");
        };
        assert!(errors.iter().any(|error| {
            error.message.contains("Copy scalar") || error.message.contains("List construction")
        }));
    }

    #[test]
    fn canonical_mir_differential_covers_scalar_set_handle_island() {
        let report = run_canonical_differential(
            "func make_values() -> Set<i32> { let values: Set<i32> = {1, 2, 1}; values }\nfunc main() -> i32 { let values = make_values(); let inserted = values.insert(3); let present = inserted.contains(2); let nonempty = !inserted.is_empty(); let removed = inserted.remove(1); let size = removed.size(); size }",
        )
        .expect("Copy-scalar Set handle differential");
        assert!(report.mir_text.contains("construct_set"));
        assert!(report.mir_text.contains("set_op"));
        assert!(report.type_desc_text.contains("layout=Set"));
        assert!(report.type_desc_text.contains("SetHandle"));
        assert_eq!(
            report.reference.outcome,
            DifferentialOutcome::Return(MirRuntimeValue::Int(2))
        );
        assert!(matches!(
            report.mir_bytecode.outcome,
            DifferentialOutcome::Return(MirRuntimeValue::Int(2))
        ));
    }

    #[test]
    fn canonical_mir_set_to_list_is_sorted_and_ast_free() {
        let source =
            "func main() -> List<i32> { let values: Set<i32> = {3, 1, 2, 1}; values.to_list() }";
        let (_, checked) = parse_and_check(source).expect("Set.to_list source check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical Set.to_list MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference Set.to_list execution");
        assert_eq!(
            reference,
            MirRuntimeValue::List(vec![
                MirRuntimeValue::Int(1),
                MirRuntimeValue::Int(2),
                MirRuntimeValue::Int(3),
            ])
        );

        let bytecode = compile_mir_program(&mir).expect("canonical Set.to_list bytecode");
        assert!(bytecode.ast.is_none());
        assert!(bytecode.functions.iter().any(|function| {
            function
                .code
                .iter()
                .any(|op| matches!(op, Op::MirSetToList { .. }))
        }));
        let observed = BytecodeVM::new(bytecode)
            .run_value()
            .expect("canonical Set.to_list bytecode execution");
        assert_eq!(
            normalize_value(observed).expect("normalize List result"),
            reference
        );
    }

    #[test]
    fn canonical_mir_rejects_non_scalar_set_before_backend() {
        let error = run_canonical_differential(
            "func main() -> i32 { let values: Set<string> = {\"owned\", \"other\"}; drop(values); 0 }",
        )
        .expect_err("Set<string> is outside the scalar Set production island");
        match error {
            DifferentialHarnessError::CanonicalMir(message) => {
                assert!(message.contains("Set") && message.contains("Copy scalar"));
            }
            other => panic!("unsupported Set shape crossed the canonical gate: {other:?}"),
        }
    }

    #[test]
    fn canonical_mir_rejects_list_element_shape_before_backend() {
        let error = run_canonical_differential("func main() -> List<string> { [\"owned\"] }")
            .expect_err("List<string> is outside the first canonical List slice");
        match error {
            DifferentialHarnessError::CanonicalMir(message) => {
                assert!(message.contains("List") && message.contains("Copy scalar"));
            }
            other => panic!("unsupported List shape crossed the canonical gate: {other:?}"),
        }
    }

    #[test]
    fn canonical_mir_rejects_nested_list_shape_before_backend() {
        let error = run_canonical_differential("func main() -> List<List<i32>> { [[1, 2]] }")
            .expect_err("nested List is outside the first canonical List slice");
        match error {
            DifferentialHarnessError::CanonicalMir(message) => {
                assert!(message.contains("List") && message.contains("Copy scalar"));
            }
            other => panic!("nested List crossed the canonical gate: {other:?}"),
        }
    }

    #[test]
    fn canonical_mir_list_drop_uses_explicit_list_glue() {
        let report = run_canonical_differential(
            "func main() -> i32 { let values = [1, 2, 3]; drop(values); 42 }",
        )
        .expect("List drop differential");
        assert!(report.mir_text.contains("construct_list"));
        assert!(report.mir_text.contains("drop"));
        assert!(report.type_desc_text.contains("move_out: List"));
        assert_eq!(
            report.reference.outcome,
            DifferentialOutcome::Return(MirRuntimeValue::Int(42))
        );
    }

    #[test]
    fn canonical_mir_differential_covers_static_list_index_projection() {
        let report = run_canonical_differential(
            "func main() -> i32 { let values = [10, 20, 30]; values[1] }",
        )
        .expect("static List index differential");
        assert!(report.mir_text.contains("project"));
        assert!(report.mir_text.contains("Index("));
        assert_eq!(
            report.reference.outcome,
            DifferentialOutcome::Return(MirRuntimeValue::Int(20))
        );
    }

    #[test]
    fn canonical_mir_differential_covers_dynamic_list_index_projection() {
        let report = run_canonical_differential(
            "func main() -> i32 { let values = [10, 20, 30]; let index = 2; values[index] }",
        )
        .expect("dynamic List index differential");
        assert!(report.mir_text.contains("Index("));
        assert_eq!(
            report.reference.outcome,
            DifferentialOutcome::Return(MirRuntimeValue::Int(30))
        );
    }

    #[test]
    fn canonical_mir_differential_preserves_negative_list_index_semantics() {
        let report = run_canonical_differential(
            "func main() -> i32 { let values = [10, 20, 30]; values[-1] }",
        )
        .expect("negative List index differential");
        assert_eq!(
            report.reference.outcome,
            DifferentialOutcome::Return(MirRuntimeValue::Int(30))
        );
    }

    #[test]
    fn canonical_mir_differential_preserves_list_index_trap_class() {
        let report = run_canonical_differential(
            "func main() -> i32 { let values = [10, 20, 30]; values[3] }",
        )
        .expect("List index trap differential");
        for observation in [
            &report.reference,
            &report.mir_bytecode,
            &report.legacy_bytecode,
        ] {
            assert!(matches!(
                &observation.outcome,
                DifferentialOutcome::Error { class, .. } if class == "runtime:E0803"
            ));
        }
    }

    #[test]
    fn canonical_mir_rejects_indexed_assignment_without_fallback() {
        let error = run_canonical_differential(
            "func main() -> i32 { let mut values = [10, 20, 30]; values[0] = 99; 0 }",
        )
        .expect_err("indexed assignment is outside the read-only List index slice");
        match error {
            DifferentialHarnessError::CanonicalMir(message) => {
                assert!(
                    message.contains("structured control flow")
                        || message.contains("indexed place projection"),
                    "unexpected canonical rejection: {message}"
                );
            }
            other => panic!("unsupported indexed assignment crossed the canonical gate: {other:?}"),
        }
    }

    fn compile(source: &str) -> std::sync::Arc<crate::interp::bytecode::BytecodeProgram> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        compile_mir_program(&mir).expect("MIR bytecode")
    }

    fn canonical_trap_program(code: &str) -> MirProgram {
        let tokens = Lexer::new("func main() -> i32 { 42 }")
            .tokenize()
            .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let mut function = mir.functions().get(&owner).cloned().expect("main");
        let entry = function.entry.clone();
        function
            .blocks
            .get_mut(&entry)
            .expect("entry block")
            .terminator = crate::core::mir::MirTerminator::Trap { code: code.into() };
        MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            mir.type_catalog().clone(),
        )
        .expect("canonical trap program")
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
    fn executes_materialized_scalar_generic_identity_through_mir_bytecode() {
        let source =
            "func identity<T>(value: T) -> T { value }\nfunc main() -> i32 { identity(41) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        assert_eq!(mir.instances().len(), 1);

        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference execution");
        let bytecode = compile_mir_program(&mir).expect("generic identity MIR bytecode");
        assert!(bytecode.ast.is_none());
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("generic identity bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        assert!(matches!(value, Value::Int(41)));
    }

    #[test]
    fn immutable_scalar_borrow_and_dereference_agree_with_reference_oracle() {
        let source = "func main() -> i32 { let value = 41; *(&value) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let function = mir.functions().get(&owner).expect("main");
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Borrow { mutable: false, .. }
                )
            })
        }));
        assert!(function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Project {
                        projection: crate::core::mir::MirProjection::Dereference,
                        ..
                    }
                )
            })
        }));

        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        assert!(bytecode.ast.is_none());
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(41));
        assert!(matches!(value, Value::Int(41)));
    }

    #[test]
    fn canonical_gate_rejects_mutable_borrow_before_any_backend() {
        let source = "func main() -> i32 { let value = 41; (&value); 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let mut function = mir.functions().get(&owner).cloned().expect("main");
        let borrow = function
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find_map(|instruction| match &mut instruction.kind {
                crate::core::mir::MirInstructionKind::Borrow { mutable, .. } => {
                    *mutable = true;
                    Some(instruction.id.clone())
                }
                _ => None,
            })
            .expect("borrow instruction");
        let errors = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            mir.type_catalog().clone(),
        )
        .expect_err("mutable borrow must fail the canonical gate");
        assert!(errors.iter().any(|error| {
            error.subject == borrow.to_string() && error.message.contains("mutable Borrow")
        }));
    }

    #[test]
    fn canonical_gate_rejects_borrow_escape_before_any_backend() {
        let error = run_canonical_differential(
            "func main() -> i32 { let value = 41; let borrowed = &value; 42 }",
        )
        .expect_err("a borrowed pointer cannot escape its local MIR use contract");
        match error {
            DifferentialHarnessError::CanonicalMir(message) => {
                assert!(
                    message.contains("borrow value") && message.contains("escapes"),
                    "unexpected canonical rejection: {message}"
                );
            }
            other => panic!("borrow escape crossed the canonical gate: {other:?}"),
        }
    }

    #[test]
    fn canonical_gate_rejects_use_after_end_borrow_before_any_backend() {
        let source = "func main() -> i32 { let value = 41; *(&value) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let canonical = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let mut function = canonical.functions().get(&owner).cloned().expect("main");
        let borrow = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Borrow { result, .. } => Some(result.clone()),
                _ => None,
            })
            .expect("borrow instruction");
        let mut inserted = false;
        for block in function.blocks.values_mut() {
            let Some(project_index) = block.instructions.iter().position(|instruction| {
                matches!(
                    &instruction.kind,
                    crate::core::mir::MirInstructionKind::Project {
                        base,
                        projection: crate::core::mir::MirProjection::Dereference,
                        ..
                    } if base == &borrow
                )
            }) else {
                continue;
            };
            block.instructions.insert(
                project_index,
                crate::core::mir::MirInstruction {
                    id: crate::core::mir::MirInstructionId::new("test:end-borrow")
                        .expect("instruction id"),
                    kind: crate::core::mir::MirInstructionKind::EndBorrow {
                        borrow: borrow.clone(),
                    },
                },
            );
            inserted = true;
            break;
        }
        assert!(inserted, "dereference projection");
        let errors = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            canonical.type_catalog().clone(),
        )
        .expect_err("a dereference after EndBorrow must fail the canonical gate");
        assert!(
            errors
                .iter()
                .any(|error| { error.message.contains("used after EndBorrow") }),
            "{errors:?}"
        );
    }

    #[test]
    fn executes_first_class_abs_through_ast_free_bytecode() {
        let source = "func abs_i64(value: i64) -> i64 { abs(value) }\nfunc main() -> i32 { if abs_i64(-4294967297) == 4294967297 { 42 } else { 0 } }";
        let program = compile(source);
        assert!(program.ast.is_none());
        assert!(program.functions.iter().any(|function| function
            .code
            .iter()
            .any(|op| matches!(op, Op::CallBuiltin { .. }))));
        let value = BytecodeVM::new(program).run_value().expect("VM abs");
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn first_class_abs_preserves_e0802_across_reference_and_bytecode() {
        let source = "func main() -> i64 { let value: i64 = -9223372036854775808; abs(value) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect_err("reference abs overflow");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let vm_error = BytecodeVM::new(bytecode)
            .run_value()
            .expect_err("bytecode abs overflow");
        assert!(reference.message.contains("E0802"));
        assert_eq!(vm_error.code(), "E0802");
    }

    #[test]
    fn executes_first_class_min_max_through_ast_free_bytecode() {
        let source = "func min_i64(left: i64, right: i64) -> i64 { min(left, right) }\nfunc max_i64(left: i64, right: i64) -> i64 { max(left, right) }\nfunc main() -> i32 { if min_i64(9223372036854775806, 9223372036854775807) == 9223372036854775806 { if max_i64(-9223372036854775807, 9223372036854775806) == 9223372036854775806 { 42 } else { 0 } } else { 0 } }";
        let program = compile(source);
        assert!(program.ast.is_none());
        let builtin_ops = program
            .functions
            .iter()
            .flat_map(|function| function.code.iter())
            .filter(|op| matches!(op, Op::CallBuiltin { .. }))
            .count();
        assert!(builtin_ops >= 2, "expected min and max builtin opcodes");
        let value = BytecodeVM::new(program).run_value().expect("VM min/max");
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn executes_canonical_i32_to_i64_conversion_through_ast_free_bytecode() {
        let source = "func min_i64(left: i32, right: i32) -> i64 { min(left as i64, right as i64) }\nfunc main() -> i32 { if min_i64(1, 2) == 1 { 42 } else { 0 } }";
        let program = compile(source);
        assert!(program.ast.is_none());
        let conversion_ops = program
            .functions
            .iter()
            .flat_map(|function| function.code.iter())
            .filter(|op| matches!(op, Op::Mov { .. }))
            .count();
        assert!(conversion_ops >= 2, "expected canonical conversion MOVs");
        let value = BytecodeVM::new(program)
            .run_value()
            .expect("VM i32 to i64 conversion");
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn reference_and_bytecode_agree_on_canonical_trap() {
        let mir = canonical_trap_program("E0801");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect_err("reference trap");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        assert!(bytecode.ast.is_none());
        let main = &bytecode.functions[bytecode.entry as usize];
        assert!(main.code.iter().any(|op| matches!(op, Op::Trap { .. })));
        let vm_error = BytecodeVM::new(bytecode)
            .run_value()
            .expect_err("bytecode trap");
        assert_eq!(reference.message, "trap E0801");
        assert_eq!(vm_error.message(), reference.message);
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
    fn executes_owned_record_construct_and_drop_through_both_oracles() {
        let source = "type Named { name: string, count: i32 }\nfunc main() -> i32 { let p = Named { count: 41, name: \"owned\" }; drop(p); 42 }";
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
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::NewRecordMove { .. })));
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::DropAggregate { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn executes_non_copy_record_move_projection_through_both_oracles() {
        let source = "type Named { name: string, count: i32 }\nfunc main() -> string { let p = Named { name: \"owned\", count: 41 }; p.name }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference move projection");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let main = &bytecode.functions[bytecode.entry as usize];
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::RecordMoveGet { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode move projection");
        assert_eq!(reference, MirRuntimeValue::String("owned".into()));
        assert!(matches!(
            value,
            Value::String(value) if value.as_str() == "owned"
        ));
    }

    #[test]
    fn rejects_non_copy_record_move_projection_with_non_copy_sibling() {
        let source = "type Pair { left: string, right: string }\nfunc main() -> string { let p = Pair { left: \"left\", right: \"right\" }; p.left }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("partial move must fail before bytecode emission");
        let text = format!("{error:?}");
        assert!(
            text.contains("non-Copy") || text.contains("move projection"),
            "unexpected fail-closed error: {text}"
        );
    }

    #[test]
    fn returns_owned_record_through_canonical_mir_and_bytecode() {
        let source = "type Named { name: string, count: i32 }\nfunc main() -> Named { Named { count: 41, name: \"owned\" } }";
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
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::NewRecordMove { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert!(matches!(
            reference,
            MirRuntimeValue::Record { fields, .. }
                if fields == [
                    MirRuntimeValue::String("owned".into()),
                    MirRuntimeValue::Int(41)
                ]
        ));
        assert!(matches!(
            value,
            Value::Record(_, fields)
                if fields.get("name") == Some(&Value::String(std::sync::Arc::new("owned".into())))
                    && fields.get("count") == Some(&Value::Int(41))
        ));
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
    fn executes_i64_copy_variant_branch_merge_through_canonical_mir() {
        let source = "func choose(flag: bool) -> Option<i64> { if flag { Some(41) } else { None } }\nfunc main() -> i64 { let value = choose(true); match value { Some(v) => v + (1 as i64), None => (0 as i64) } }";
        let program = compile(source);
        let value = BytecodeVM::new(program)
            .run_value()
            .expect("bytecode execution");
        assert!(matches!(value, Value::Int(42)));
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
            Value::CanonicalVariant {
                nominal,
                variant,
                tag,
                payload,
            }
                if nominal.as_str() == "builtin:type:Option"
                    && variant.0 == "builtin:variant:Option::Some"
                    && tag == "Some"
                    && payload == vec![Value::Int(41)]
        ));
    }

    #[test]
    fn canonical_variant_construction_carries_type_desc_shape_contract() {
        let source = "func main() -> Option<i32> { Some(41) }";
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
        let (tag_idx, shape_idx, discriminant, arity) = main
            .code
            .iter()
            .find_map(|op| match op {
                Op::NewVariant {
                    type_name,
                    shapes: Some(shapes),
                    variant,
                    arity,
                    ..
                } => Some((*type_name, *shapes, *variant, *arity)),
                _ => None,
            })
            .expect("canonical NewVariant shape contract");
        assert!(matches!(
            main.constants.get(tag_idx as usize),
            Some(crate::interp::bytecode::ConstValue::Str(tag)) if tag == "Some"
        ));
        let shapes = match main.constants.get(shape_idx as usize) {
            Some(crate::interp::bytecode::ConstValue::VariantShapes(shapes)) => shapes,
            other => panic!("expected canonical variant shape table, got {other:?}"),
        };
        assert_eq!(
            shapes
                .iter()
                .map(|shape| {
                    (
                        shape.nominal.as_str(),
                        shape.variant.0.as_str(),
                        shape.tag.as_str(),
                        shape.discriminant,
                        shape.arity,
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    "builtin:type:Option",
                    "builtin:variant:Option::None",
                    "None",
                    0,
                    0,
                ),
                (
                    "builtin:type:Option",
                    "builtin:variant:Option::Some",
                    "Some",
                    1,
                    1,
                ),
            ]
        );
        let some = shapes
            .iter()
            .find(|shape| shape.tag == "Some")
            .expect("Some");
        assert_eq!((some.discriminant, some.arity), (discriminant, arity));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert!(matches!(
            reference,
            MirRuntimeValue::Variant { variant, payload, .. }
                if variant.0 == "builtin:variant:Option::Some"
                    && payload == vec![MirRuntimeValue::Int(41)]
        ));
        assert!(matches!(
            value,
            Value::CanonicalVariant {
                nominal,
                variant,
                tag,
                payload,
            }
                if nominal.as_str() == "builtin:type:Option"
                    && variant.0 == "builtin:variant:Option::Some"
                    && tag == "Some"
                    && payload == vec![Value::Int(41)]
        ));
    }

    #[test]
    fn rejects_variant_construction_discriminant_drift_before_moving_payload() {
        let source = "func main() -> Option<string> { Some(\"owned\") }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        assert!(matches!(
            reference,
            MirRuntimeValue::Variant { variant, .. }
                if variant.0 == "builtin:variant:Option::Some"
        ));

        let mut bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let program = std::sync::Arc::make_mut(&mut bytecode);
        let entry = program.entry as usize;
        let main = &mut program.functions[entry];
        let construction = main
            .code
            .iter()
            .position(|op| {
                matches!(
                    op,
                    Op::NewVariantMove {
                        shapes: Some(_),
                        ..
                    }
                )
            })
            .expect("canonical NewVariantMove shape contract");
        let payload_reg = match &main.code[construction] {
            Op::NewVariantMove { base, .. } => *base,
            _ => unreachable!("construction position changed"),
        };
        let forged_shapes =
            main.add_const(crate::interp::bytecode::ConstValue::VariantShapes(vec![
                crate::interp::bytecode::instr::VariantShape {
                    nominal: crate::core::ir::NominalTypeId::new("builtin:type:Option")
                        .expect("Option nominal"),
                    variant: crate::core::NodeId("builtin:variant:Option::Some".into()),
                    tag: "Some".into(),
                    discriminant: 0,
                    arity: 1,
                },
            ]));
        match &mut main.code[construction] {
            Op::NewVariantMove { shapes, .. } => *shapes = Some(forged_shapes),
            _ => unreachable!("construction position changed"),
        }

        let mut vm = BytecodeVM::new(bytecode);
        let error = vm
            .run_value()
            .expect_err("corrupted construction shape must fail closed");
        assert!(error
            .message()
            .contains("variant construction: tag 'Some' has discriminant 0, opcode carries 1"));
        assert!(matches!(
            vm.get_reg(payload_reg),
            Value::String(value) if value.as_str() == "owned"
        ));
    }

    #[test]
    fn executes_move_owned_option_payload_through_both_oracles() {
        let source = "func main() -> Option<string> { Some(\"owned\") }";
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
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::NewVariantMove { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(
            reference,
            MirRuntimeValue::Variant {
                nominal: crate::core::ir::NominalTypeId::new("builtin:type:Option")
                    .expect("option nominal"),
                variant: crate::core::NodeId("builtin:variant:Option::Some".into()),
                payload: vec![MirRuntimeValue::String("owned".into())],
            }
        );
        assert!(matches!(
            value,
            Value::CanonicalVariant {
                nominal,
                variant,
                tag,
                payload,
            }
                if nominal.as_str() == "builtin:type:Option"
                    && variant.0 == "builtin:variant:Option::Some"
                    && tag == "Some"
                    && payload == vec![Value::String(std::sync::Arc::new("owned".to_string()))]
        ));
    }

    #[test]
    fn drops_move_owned_option_payload_through_both_oracles() {
        let source =
            "func main() -> i32 { let value: Option<string> = Some(\"owned\"); drop(value); 42 }";
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
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::DropVariant { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn executes_move_owned_result_payload_through_both_oracles() {
        let source = "func main() -> Result<string, i32> { Ok(\"owned\") }";
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
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::NewVariantMove { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(
            reference,
            MirRuntimeValue::Variant {
                nominal: crate::core::ir::NominalTypeId::new("builtin:type:Result")
                    .expect("result nominal"),
                variant: crate::core::NodeId("builtin:variant:Result::Ok".into()),
                payload: vec![MirRuntimeValue::String("owned".into())],
            }
        );
        assert!(matches!(
            value,
            Value::CanonicalVariant {
                nominal,
                variant,
                tag,
                payload,
            }
                if nominal.as_str() == "builtin:type:Result"
                    && variant.0 == "builtin:variant:Result::Ok"
                    && tag == "Ok"
                    && payload == vec![Value::String(std::sync::Arc::new("owned".to_string()))]
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
    fn bytecode_and_reference_agree_on_owned_tuple_return() {
        let source = "func main() -> (string, i32) { (\"owned\", 41) }";
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
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::NewTupleMove { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(
            reference,
            MirRuntimeValue::Tuple(vec![
                MirRuntimeValue::String("owned".into()),
                MirRuntimeValue::Int(41),
            ])
        );
        assert!(matches!(
            value,
            Value::Tuple(items)
                if items.as_slice()
                    == [
                        Value::String(std::sync::Arc::new("owned".to_string())),
                        Value::Int(41),
                    ]
        ));
    }

    #[test]
    fn executes_recursive_owned_tuple_clone_and_drop_glue() {
        let source =
            "func main() -> i32 { let pair = (\"owned\", 41); let copy = pair; drop(copy); 42 }";
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
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::NewTupleMove { .. })));
        assert!(main.code.iter().any(|op| matches!(op, Op::Clone { .. })));
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::DropAggregate { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn executes_nested_owned_tuple_glue_through_both_oracles() {
        let source = "func main() -> ((string, i32), bool) { ((\"inner\", 41), true) }";
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
        assert_eq!(
            main.code
                .iter()
                .filter(|op| matches!(op, Op::NewTupleMove { .. }))
                .count(),
            2
        );
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(
            reference,
            MirRuntimeValue::Tuple(vec![
                MirRuntimeValue::Tuple(vec![
                    MirRuntimeValue::String("inner".into()),
                    MirRuntimeValue::Int(41),
                ]),
                MirRuntimeValue::Bool(true),
            ])
        );
        assert!(matches!(
            value,
            Value::Tuple(items)
                if items.as_slice() == [
                    Value::Tuple(vec![
                        Value::String(std::sync::Arc::new("inner".to_string())),
                        Value::Int(41),
                    ]),
                    Value::Bool(true)
                ]
        ));
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
    fn variant_get_carries_type_desc_identity_shape_contract() {
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
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let main = &bytecode.functions[bytecode.entry as usize];
        let (tag_idx, shape_idx, field_idx) = main
            .code
            .iter()
            .find_map(|op| match op {
                Op::VariantGet {
                    variant_tag,
                    shapes,
                    idx,
                    ..
                } => Some((*variant_tag, *shapes, *idx)),
                _ => None,
            })
            .expect("canonical variant projection");
        assert_eq!(field_idx, 0);
        assert!(matches!(
            main.constants.get(tag_idx as usize),
            Some(crate::interp::bytecode::ConstValue::Str(tag)) if tag == "Some"
        ));
        let shapes = match main.constants.get(shape_idx as usize) {
            Some(crate::interp::bytecode::ConstValue::VariantShapes(shapes)) => shapes,
            other => panic!("expected canonical projection shape table, got {other:?}"),
        };
        let some = shapes
            .iter()
            .find(|shape| shape.tag == "Some")
            .expect("Some shape");
        assert_eq!(some.nominal.as_str(), "builtin:type:Option");
        assert_eq!(some.variant.0, "builtin:variant:Option::Some");
        assert_eq!((some.discriminant, some.arity), (1, 1));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn rejects_variant_get_identity_drift_before_reading_payload() {
        let source =
            "func main() -> i32 { let value: Option<i32> = Some(41); match value { Some(v) => v + 1, None => 0 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");

        let mut bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let program = std::sync::Arc::make_mut(&mut bytecode);
        let entry = program.entry as usize;
        let main = &mut program.functions[entry];
        let forged_shapes =
            main.add_const(crate::interp::bytecode::ConstValue::VariantShapes(vec![
                crate::interp::bytecode::instr::VariantShape {
                    nominal: crate::core::ir::NominalTypeId::new("builtin:type:Result")
                        .expect("Result nominal"),
                    variant: crate::core::NodeId("builtin:variant:Result::Ok".into()),
                    tag: "Some".into(),
                    discriminant: 1,
                    arity: 1,
                },
            ]));
        let (projection_shapes, source_reg) = main
            .code
            .iter_mut()
            .find_map(|op| match op {
                Op::VariantGet { ra, shapes, .. } => Some((shapes, *ra)),
                _ => None,
            })
            .expect("canonical variant projection");
        *projection_shapes = forged_shapes;

        let mut vm = BytecodeVM::new(bytecode);
        let error = vm
            .run_value()
            .expect_err("canonical identity drift must fail closed");
        assert!(error
            .message()
            .contains("variant get: canonical identity for tag 'Some' disagrees with shape table"));
        assert!(matches!(
            vm.get_reg(source_reg),
            Value::CanonicalVariant {
                nominal,
                variant,
                tag,
                payload,
            } if nominal.as_str() == "builtin:type:Option"
                && variant.0 == "builtin:variant:Option::Some"
                && tag == "Some"
                && *payload == vec![Value::Int(41)]
        ));
    }

    #[test]
    fn executes_move_variant_switch_before_any_backend() {
        let source =
            "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(v) => v, None => \"fallback\" } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let main = &bytecode.functions[bytecode.entry as usize];
        let (shape_index, variant_tag) = main
            .code
            .iter()
            .find_map(|op| match op {
                Op::DestructureVariantMove {
                    shapes,
                    variant_tag,
                    ..
                } => Some((*shapes, *variant_tag)),
                _ => None,
            })
            .expect("canonical switch-move destructure");
        assert!(matches!(
            main.constants.get(variant_tag as usize),
            Some(crate::interp::bytecode::ConstValue::Str(tag)) if tag == "Some"
        ));
        let shapes = match main.constants.get(shape_index as usize) {
            Some(crate::interp::bytecode::ConstValue::VariantShapes(shapes)) => shapes,
            other => panic!("expected canonical switch-move shape table, got {other:?}"),
        };
        let some = shapes
            .iter()
            .find(|shape| shape.tag == "Some")
            .expect("Some shape");
        assert_eq!(some.nominal.as_str(), "builtin:type:Option");
        assert_eq!(some.variant.0, "builtin:variant:Option::Some");
        assert_eq!((some.discriminant, some.arity), (1, 1));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(
            reference,
            crate::core::mir::reference::MirRuntimeValue::String("owned".into())
        );
        assert!(matches!(value, Value::String(value) if value.as_str() == "owned"));
    }

    #[test]
    fn rejects_variant_destructure_tag_drift_before_moving_payload() {
        let source =
            "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(v) => v, None => \"fallback\" } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(reference, MirRuntimeValue::String("owned".into()));

        let mut bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let program = std::sync::Arc::make_mut(&mut bytecode);
        let entry = program.entry as usize;
        let main = &mut program.functions[entry];
        let forged_tag = main.add_const(crate::interp::bytecode::ConstValue::Str("Err".into()));
        let (destructure, scrutinee) = main
            .code
            .iter_mut()
            .find_map(|op| match op {
                Op::DestructureVariantMove {
                    ra, variant_tag, ..
                } => Some((variant_tag, *ra)),
                _ => None,
            })
            .expect("canonical switch-move destructure");
        *destructure = forged_tag;

        let mut vm = BytecodeVM::new(bytecode);
        let error = vm
            .run_value()
            .expect_err("corrupted active-variant tag must fail closed");
        assert!(error
            .message()
            .contains("variant destructure: expected tag 'Err', got 'Some'"));
        assert!(matches!(
            vm.get_reg(scrutinee),
            Value::CanonicalVariant { tag, .. } if tag == "Some"
        ));
    }

    #[test]
    fn rejects_variant_destructure_identity_drift_before_moving_payload() {
        let source =
            "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(v) => v, None => \"fallback\" } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");

        let mut bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let program = std::sync::Arc::make_mut(&mut bytecode);
        let entry = program.entry as usize;
        let main = &mut program.functions[entry];
        let forged_shapes =
            main.add_const(crate::interp::bytecode::ConstValue::VariantShapes(vec![
                crate::interp::bytecode::instr::VariantShape {
                    nominal: crate::core::ir::NominalTypeId::new("builtin:type:Result")
                        .expect("Result nominal"),
                    variant: crate::core::NodeId("builtin:variant:Result::Ok".into()),
                    tag: "Some".into(),
                    discriminant: 1,
                    arity: 1,
                },
            ]));
        let (destructure_shapes, source_reg) = main
            .code
            .iter_mut()
            .find_map(|op| match op {
                Op::DestructureVariantMove { ra, shapes, .. } => Some((shapes, *ra)),
                _ => None,
            })
            .expect("canonical switch-move destructure");
        *destructure_shapes = forged_shapes;

        let mut vm = BytecodeVM::new(bytecode);
        let error = vm
            .run_value()
            .expect_err("canonical identity drift must fail closed");
        assert!(error.message().contains(
            "variant destructure: canonical identity for tag 'Some' disagrees with shape table"
        ));
        assert!(matches!(
            vm.get_reg(source_reg),
            Value::CanonicalVariant {
                nominal,
                variant,
                tag,
                ..
            } if nominal.as_str() == "builtin:type:Option"
                && variant.0 == "builtin:variant:Option::Some"
                && tag == "Some"
        ));
    }

    #[test]
    fn rejects_variant_drop_shape_drift_before_consuming_source() {
        let source =
            "func main() -> i32 { let value: Option<string> = Some(\"owned\"); match value { Some(_) => 42, None => 0 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(reference, MirRuntimeValue::Int(42));

        let mut bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let program = std::sync::Arc::make_mut(&mut bytecode);
        let entry = program.entry as usize;
        let main = &mut program.functions[entry];
        let forged_shapes =
            main.add_const(crate::interp::bytecode::ConstValue::VariantShapes(vec![
                crate::interp::bytecode::instr::VariantShape {
                    nominal: crate::core::ir::NominalTypeId::new("builtin:type:Option")
                        .expect("Option nominal"),
                    variant: crate::core::NodeId("builtin:variant:Option::Err".into()),
                    tag: "Err".into(),
                    discriminant: 0,
                    arity: 1,
                },
            ]));
        let (drop_shapes, source_reg) = main
            .code
            .iter_mut()
            .find_map(|op| match op {
                Op::DropVariant { ra, shapes } => Some((shapes, *ra)),
                _ => None,
            })
            .expect("canonical variant drop");
        *drop_shapes = forged_shapes;

        let mut vm = BytecodeVM::new(bytecode);
        let error = vm
            .run_value()
            .expect_err("corrupted variant drop shape must fail closed");
        assert!(error
            .message()
            .contains("variant drop: tag 'Some' is absent from canonical drop shapes"));
        assert!(matches!(
            vm.get_reg(source_reg),
            Value::CanonicalVariant { tag, .. } if tag == "Some"
        ));
    }

    #[test]
    fn rejects_variant_drop_identity_drift_before_consuming_source() {
        let source =
            "func main() -> i32 { let value: Option<string> = Some(\"owned\"); drop(value); 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");

        let mut bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let program = std::sync::Arc::make_mut(&mut bytecode);
        let entry = program.entry as usize;
        let main = &mut program.functions[entry];
        let forged_shapes =
            main.add_const(crate::interp::bytecode::ConstValue::VariantShapes(vec![
                crate::interp::bytecode::instr::VariantShape {
                    nominal: crate::core::ir::NominalTypeId::new("builtin:type:Result")
                        .expect("Result nominal"),
                    variant: crate::core::NodeId("builtin:variant:Result::Ok".into()),
                    tag: "Some".into(),
                    discriminant: 1,
                    arity: 1,
                },
            ]));
        let (drop_shapes, source_reg) = main
            .code
            .iter_mut()
            .find_map(|op| match op {
                Op::DropVariant { ra, shapes } => Some((shapes, *ra)),
                _ => None,
            })
            .expect("canonical variant drop");
        *drop_shapes = forged_shapes;

        let mut vm = BytecodeVM::new(bytecode);
        let error = vm
            .run_value()
            .expect_err("canonical identity drift must fail closed");
        assert!(error.message().contains(
            "variant drop: canonical identity for tag 'Some' disagrees with shape table"
        ));
        assert!(matches!(
            vm.get_reg(source_reg),
            Value::CanonicalVariant {
                nominal,
                variant,
                tag,
                ..
            } if nominal.as_str() == "builtin:type:Option"
                && variant.0 == "builtin:variant:Option::Some"
                && tag == "Some"
        ));
    }

    #[test]
    fn bytecode_and_reference_agree_when_consuming_switch_drops_unbound_payload() {
        let source =
            "func main() -> i32 { let value: Option<string> = Some(\"owned\"); match value { Some(_) => 42, None => 0 } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let main = &bytecode.functions[bytecode.entry as usize];
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::DropVariant { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(
            reference,
            crate::core::mir::reference::MirRuntimeValue::Int(42)
        );
        assert!(matches!(value, Value::Int(42)));
    }

    #[test]
    fn bytecode_and_reference_agree_on_consuming_result_payload() {
        let source =
            "func main() -> string { let value: Result<string, string> = Err(\"error\"); match value { Ok(v) => v, Err(e) => e } }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&mir)
            .execute(&owner, &[])
            .expect("reference execution");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let main = &bytecode.functions[bytecode.entry as usize];
        assert!(main
            .code
            .iter()
            .any(|op| matches!(op, Op::DestructureVariantMove { .. })));
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(
            reference,
            crate::core::mir::reference::MirRuntimeValue::String("error".into())
        );
        assert!(matches!(value, Value::String(value) if value.as_str() == "error"));
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
    fn rejects_consumption_hidden_before_canonical_branch_join() {
        let source = "func consume_after_branch(choose: bool) -> string { let value = \"owned\"; let marker = if choose { 0 } else { 0 }; value }\nfunc main() -> i32 { 0 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:consume_after_branch".into());
        let mut function = mir
            .functions()
            .get(&owner)
            .cloned()
            .expect("branch function");
        let value = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Const {
                    result,
                    literal: crate::core::ir::ResolvedLiteral::String(value),
                } if value == "owned" => Some(result.clone()),
                _ => None,
            })
            .expect("owned string value");
        let branch_target = function
            .blocks
            .values()
            .find_map(|block| match &block.terminator {
                crate::core::mir::MirTerminator::Branch { then_target, .. } => {
                    Some(then_target.clone())
                }
                _ => None,
            })
            .expect("branch target");
        function
            .blocks
            .get_mut(&branch_target)
            .expect("branch block")
            .instructions
            .push(crate::core::mir::MirInstruction {
                id: crate::core::mir::MirInstructionId::new("synthetic/branch-drop")
                    .expect("instruction id"),
                kind: crate::core::mir::MirInstructionKind::Drop { value },
            });
        let error = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            mir.type_catalog().clone(),
        )
        .expect_err("a branch-local consumption must reach its join");
        assert!(error
            .iter()
            .any(|error| error.message.contains("use after consuming non-Copy value")));
    }

    #[test]
    fn rejects_consumption_hidden_before_canonical_loop_back_edge() {
        let source = "func consume_after_loop(choose: bool) -> string { let value = \"owned\"; while choose { 0 } value }\nfunc main() -> i32 { 0 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:consume_after_loop".into());
        let mut function = mir.functions().get(&owner).cloned().expect("loop function");
        let value = function
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Const {
                    result,
                    literal: crate::core::ir::ResolvedLiteral::String(value),
                } if value == "owned" => Some(result.clone()),
                _ => None,
            })
            .expect("owned string value");
        let body_target = function
            .blocks
            .values()
            .find_map(|block| match &block.terminator {
                crate::core::mir::MirTerminator::Branch { then_target, .. }
                    if function.blocks.get(then_target).is_some_and(|body| {
                        matches!(
                            body.terminator,
                            crate::core::mir::MirTerminator::Goto { .. }
                        )
                    }) =>
                {
                    Some(then_target.clone())
                }
                _ => None,
            })
            .expect("loop body target");
        let body = function
            .blocks
            .get_mut(&body_target)
            .expect("loop body block");
        body.instructions.push(crate::core::mir::MirInstruction {
            id: crate::core::mir::MirInstructionId::new("synthetic/loop-drop")
                .expect("instruction id"),
            kind: crate::core::mir::MirInstructionKind::Drop { value },
        });
        let error = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            mir.type_catalog().clone(),
        )
        .expect_err("a loop-body consumption must reach the loop exit");
        assert!(error
            .iter()
            .any(|error| error.message.contains("use after consuming non-Copy value")));
    }

    #[test]
    fn canonical_ownership_fixed_point_allows_unconsumed_loop_return() {
        let source = "func main() -> string { let value = \"owned\"; while false { 0 } value }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let mir = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let reference = MirReferenceInterpreter::new(&mir)
            .execute(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference execution");
        let bytecode = compile_mir_program(&mir).expect("MIR bytecode");
        let value = BytecodeVM::new(bytecode)
            .run_value()
            .expect("bytecode execution");
        assert_eq!(reference, MirRuntimeValue::String("owned".into()));
        assert!(matches!(value, Value::String(value) if value.as_str() == "owned"));
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
            kind: MirOwnershipEventKind::BorrowMut,
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
            .any(|error| error.message.contains("borrow_mut")));
    }
}
