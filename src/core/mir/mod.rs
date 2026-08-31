//! Canonical Mimi middle-level IR (MIR).
//!
//! This module is deliberately backend-free. It is the first slice of the
//! 0.41 architecture migration: the data model and structural verifier exist
//! before any lowering from ResolvedBody or emission to bytecode/LLVM.
//!
//! A MIR consumer may lower an instruction, but it may not re-run name
//! resolution, type inference, or ownership classification.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use crate::core::ir::{
    NominalTypeId, ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedTypeId,
    ResolvedUnaryOp,
};
use crate::core::{NodeId, ResolvedPlace};

pub mod lower;
pub mod reference;
pub mod types;

macro_rules! mir_id {
    ($name:ident, $label:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, MirIdError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(MirIdError {
                        kind: $label,
                        value,
                    });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

mir_id!(MirBlockId, "block");
mir_id!(MirEdgeId, "edge");
mir_id!(MirValueId, "value");
mir_id!(MirInstructionId, "instruction");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirIdError {
    pub kind: &'static str,
    pub value: String,
}

impl fmt::Display for MirIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} identity must not be empty", self.kind)
    }
}

impl std::error::Error for MirIdError {}

/// A value available to a MIR instruction or block parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirValue {
    pub id: MirValueId,
    pub ty: ResolvedTypeId,
}

/// Block parameters are the canonical join-value form; consumers must not
/// invent a second phi representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirBlockParameter {
    pub value: MirValueId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirProjection {
    Field(String),
    Tuple(usize),
    Index(MirValueId),
    Dereference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirAggregateKind {
    Tuple,
    Record {
        nominal: NominalTypeId,
        /// Stable checker field identities in the same order as `fields` in
        /// [`MirInstructionKind::Construct`].
        fields: Vec<NodeId>,
    },
}

/// Operations with explicit value and ownership boundaries. This is a
/// structural contract in Phase 0; semantic lowering and effect checking are
/// added in later phases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirInstructionKind {
    Const {
        result: MirValueId,
        literal: ResolvedLiteral,
    },
    Load {
        result: MirValueId,
        place: ResolvedPlace,
    },
    Copy {
        result: MirValueId,
        source: MirValueId,
    },
    Move {
        result: MirValueId,
        source: MirValueId,
    },
    Clone {
        result: MirValueId,
        source: MirValueId,
    },
    Drop {
        value: MirValueId,
    },
    Borrow {
        result: MirValueId,
        source: MirValueId,
        mutable: bool,
    },
    EndBorrow {
        borrow: MirValueId,
    },
    Project {
        result: MirValueId,
        base: MirValueId,
        projection: MirProjection,
    },
    Construct {
        result: MirValueId,
        kind: MirAggregateKind,
        fields: Vec<MirValueId>,
    },
    Binary {
        result: MirValueId,
        op: ResolvedBinaryOp,
        left: MirValueId,
        right: MirValueId,
    },
    Unary {
        result: MirValueId,
        op: ResolvedUnaryOp,
        operand: MirValueId,
    },
    Call {
        result: Option<MirValueId>,
        callee: ResolvedCallee,
        arguments: Vec<MirValueId>,
    },
    /// A checked conversion. Source/target facts live in the value catalog
    /// and the eventual lowering contract.
    Convert {
        result: MirValueId,
        source: MirValueId,
    },
    Nop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirInstruction {
    pub id: MirInstructionId,
    pub kind: MirInstructionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirTerminator {
    Goto {
        edge: MirEdgeId,
        target: MirBlockId,
        arguments: Vec<MirValueId>,
    },
    Branch {
        condition: MirValueId,
        then_edge: MirEdgeId,
        then_target: MirBlockId,
        then_arguments: Vec<MirValueId>,
        else_edge: MirEdgeId,
        else_target: MirBlockId,
        else_arguments: Vec<MirValueId>,
    },
    Switch {
        scrutinee: MirValueId,
        arms: Vec<MirSwitchArm>,
    },
    Return {
        value: Option<MirValueId>,
    },
    Trap {
        code: String,
    },
    Fault {
        value: Option<MirValueId>,
    },
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirSwitchCase {
    Literal(ResolvedLiteral),
    Variant(NodeId),
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirSwitchArm {
    pub edge: MirEdgeId,
    pub target: MirBlockId,
    pub arguments: Vec<MirValueId>,
    pub case: MirSwitchCase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirBlock {
    pub id: MirBlockId,
    pub parameters: Vec<MirBlockParameter>,
    pub instructions: Vec<MirInstruction>,
    pub terminator: MirTerminator,
}

/// A concrete callable after frontend resolution but before a backend is
/// selected. values is a catalog, not an implicit vector index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirFunction {
    pub owner: NodeId,
    pub parameters: Vec<MirValueId>,
    pub result: ResolvedTypeId,
    pub entry: MirBlockId,
    pub values: BTreeMap<MirValueId, MirValue>,
    pub blocks: BTreeMap<MirBlockId, MirBlock>,
    /// Checker-owned resource facts projected into a backend-neutral event
    /// stream.  Consumers must use this stream together with TypeDesc rather
    /// than infer ownership from a physical register or pointer shape.
    pub ownership: MirOwnershipSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirOwnershipEventKind {
    Read,
    Write,
    Introduce,
    Move,
    Drop,
    Return,
    TransferSession,
    TransferChild,
    BorrowShared,
    BorrowMut,
    BorrowEnd,
}

impl MirOwnershipEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Introduce => "introduce",
            Self::Move => "move",
            Self::Drop => "drop",
            Self::Return => "return",
            Self::TransferSession => "transfer_session",
            Self::TransferChild => "transfer_child",
            Self::BorrowShared => "borrow_shared",
            Self::BorrowMut => "borrow_mut",
            Self::BorrowEnd => "borrow_end",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirOwnershipEvent {
    pub kind: MirOwnershipEventKind,
    pub resource: String,
    /// Stable value identity when the resource is backed by a MIR local.
    /// Synthetic discarded/session resources intentionally leave this empty.
    pub value: Option<MirValueId>,
    pub source: Option<String>,
    pub target: Option<String>,
    pub point: NodeId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MirOwnershipSummary {
    pub events: Vec<MirOwnershipEvent>,
}

impl MirOwnershipSummary {
    pub fn validate(&self) -> Result<(), Vec<MirValidationError>> {
        let mut errors = Vec::new();
        for (index, event) in self.events.iter().enumerate() {
            if event.resource.trim().is_empty() {
                errors.push(MirValidationError {
                    subject: format!("ownership[{index}]"),
                    message: "resource identity is empty".into(),
                });
            }
            if event.point.0.trim().is_empty() {
                errors.push(MirValidationError {
                    subject: format!("ownership[{index}]"),
                    message: "event point identity is empty".into(),
                });
            }
            if matches!(
                event.kind,
                MirOwnershipEventKind::Move
                    | MirOwnershipEventKind::Drop
                    | MirOwnershipEventKind::Return
                    | MirOwnershipEventKind::TransferSession
                    | MirOwnershipEventKind::TransferChild
            ) && event.source.is_none()
            {
                errors.push(MirValidationError {
                    subject: format!("ownership[{index}]"),
                    message: format!("{} event has no source place", event.kind.as_str()),
                });
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn canonical_text(&self) -> String {
        let mut output = String::new();
        for (index, event) in self.events.iter().enumerate() {
            let source = event.source.as_deref().unwrap_or("_");
            let target = event.target.as_deref().unwrap_or("_");
            let value = event
                .value
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "_".into());
            let _ = writeln!(
                output,
                "    ownership[{index}] {} resource={} value={} source={} target={} point={}",
                event.kind.as_str(),
                event.resource,
                value,
                source,
                target,
                event.point.0
            );
        }
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirValidationError {
    pub subject: String,
    pub message: String,
}

impl fmt::Display for MirValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "MIR {}: {}", self.subject, self.message)
    }
}

impl std::error::Error for MirValidationError {}

impl MirFunction {
    /// Validate identities, graph shape, and SSA-like value dominance without
    /// depending on a backend. Kind/effect/ownership checks belong to later
    /// MIR passes, but a value may never be read from a non-dominating path.
    pub fn validate(&self) -> Result<(), Vec<MirValidationError>> {
        let mut validator = MirValidator::new(self);
        validator.check_function_header();
        validator.check_blocks();
        validator.check_ownership();
        validator.finish()
    }

    /// Deterministic, human-readable form for golden tests and differential
    /// debugging. BTreeMap ordering makes catalog insertion order irrelevant.
    pub fn canonical_text(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(
            output,
            "mir.function {} -> {}",
            self.owner.0,
            self.result.as_str()
        );
        let _ = writeln!(
            output,
            "  params [{}] entry {}",
            self.parameters
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            self.entry
        );
        for (id, value) in &self.values {
            let _ = writeln!(output, "  value {}: {}", id, value.ty.as_str());
        }
        for block in self.blocks.values() {
            let _ = writeln!(
                output,
                "  block {}({})",
                block.id,
                format_params(&block.parameters)
            );
            for instruction in &block.instructions {
                let _ = writeln!(
                    output,
                    "    {} {}",
                    instruction.id,
                    format_instruction(&instruction.kind)
                );
            }
            let _ = writeln!(output, "    -> {}", format_terminator(&block.terminator));
        }
        output.push_str(&self.ownership.canonical_text());
        output
    }
}

fn format_params(parameters: &[MirBlockParameter]) -> String {
    parameters
        .iter()
        .map(|parameter| parameter.value.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_instruction(kind: &MirInstructionKind) -> String {
    match kind {
        MirInstructionKind::Const { result, literal } => format!("const {result} = {literal:?}"),
        MirInstructionKind::Load { result, .. } => format!("load {result}"),
        MirInstructionKind::Copy { result, source } => format!("copy {result} <- {source}"),
        MirInstructionKind::Move { result, source } => format!("move {result} <- {source}"),
        MirInstructionKind::Clone { result, source } => format!("clone {result} <- {source}"),
        MirInstructionKind::Drop { value } => format!("drop {value}"),
        MirInstructionKind::Borrow {
            result,
            source,
            mutable,
        } => format!(
            "borrow{} {result} <- {source}",
            if *mutable { "_mut" } else { "" }
        ),
        MirInstructionKind::EndBorrow { borrow } => format!("end_borrow {borrow}"),
        MirInstructionKind::Project {
            result,
            base,
            projection,
        } => format!("project {result} <- {base}.{projection:?}"),
        MirInstructionKind::Construct {
            result,
            kind,
            fields,
        } => format!("construct {result} = {kind:?}({})", format_values(fields)),
        MirInstructionKind::Binary {
            result,
            op,
            left,
            right,
        } => format!("binary {result} = {op:?} {left}, {right}"),
        MirInstructionKind::Unary {
            result,
            op,
            operand,
        } => format!("unary {result} = {op:?} {operand}"),
        MirInstructionKind::Call {
            result,
            callee,
            arguments,
        } => format!(
            "call {} {:?}({})",
            result
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "_".into()),
            callee,
            arguments
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        MirInstructionKind::Convert { result, source } => {
            format!("convert {result} <- {source}")
        }
        MirInstructionKind::Nop => "nop".into(),
    }
}

fn format_terminator(terminator: &MirTerminator) -> String {
    match terminator {
        MirTerminator::Goto {
            edge,
            target,
            arguments,
        } => format!("goto {edge} {target}({})", format_values(arguments)),
        MirTerminator::Branch {
            condition,
            then_edge,
            then_target,
            then_arguments,
            else_edge,
            else_target,
            else_arguments,
        } => format!(
            "branch {condition} ? {then_edge}:{then_target}({}) : {else_edge}:{else_target}({})",
            format_values(then_arguments),
            format_values(else_arguments)
        ),
        MirTerminator::Switch { scrutinee, arms } => format!(
            "switch {scrutinee} [{}]",
            arms.iter()
                .map(|arm| {
                    format!(
                        "{:?}:{:?}:{}({})",
                        arm.case,
                        arm.edge,
                        arm.target,
                        format_values(&arm.arguments)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        MirTerminator::Return { value } => format!(
            "return {}",
            value
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "()".into())
        ),
        MirTerminator::Trap { code } => format!("trap {code}"),
        MirTerminator::Fault { value } => format!(
            "fault {}",
            value
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "()".into())
        ),
        MirTerminator::Unreachable => "unreachable".into(),
    }
}

fn format_values(values: &[MirValueId]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

struct MirValidator<'a> {
    function: &'a MirFunction,
    errors: Vec<MirValidationError>,
    definitions: BTreeMap<MirValueId, String>,
    definition_sites: BTreeMap<MirValueId, MirDefinitionSite>,
    instruction_ids: BTreeSet<MirInstructionId>,
    edge_ids: BTreeSet<MirEdgeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MirDefinitionSite {
    FunctionParameter,
    BlockParameter(MirBlockId),
    Instruction { block: MirBlockId, index: usize },
}

impl<'a> MirValidator<'a> {
    fn new(function: &'a MirFunction) -> Self {
        Self {
            function,
            errors: Vec::new(),
            definitions: BTreeMap::new(),
            definition_sites: BTreeMap::new(),
            instruction_ids: BTreeSet::new(),
            edge_ids: BTreeSet::new(),
        }
    }

    fn error(&mut self, subject: impl Into<String>, message: impl Into<String>) {
        self.errors.push(MirValidationError {
            subject: subject.into(),
            message: message.into(),
        });
    }

    fn check_function_header(&mut self) {
        if self.function.owner.0.trim().is_empty() {
            self.error("function", "owner identity is empty");
        }
        if self.function.result.as_str().trim().is_empty() {
            self.error("function", "result type identity is empty");
        }
        if !self.function.blocks.contains_key(&self.function.entry) {
            self.error(self.function.entry.to_string(), "entry block is missing");
        }
        let mut parameters = BTreeSet::new();
        let function_parameters = self.function.parameters.clone();
        for parameter in &function_parameters {
            if !parameters.insert(parameter) {
                self.error(parameter.to_string(), "function parameter is duplicated");
            }
            if !self.function.values.contains_key(parameter) {
                self.error(
                    parameter.to_string(),
                    "function parameter is absent from value catalog",
                );
            }
            self.define_at(
                parameter,
                "function parameter".into(),
                MirDefinitionSite::FunctionParameter,
            );
        }
    }

    fn check_blocks(&mut self) {
        for (id, block) in &self.function.blocks {
            if id != &block.id {
                self.error(
                    id.to_string(),
                    "block map key disagrees with block identity",
                );
            }
            let mut parameters = BTreeSet::new();
            for parameter in &block.parameters {
                if !parameters.insert(&parameter.value) {
                    self.error(parameter.value.to_string(), "block parameter is duplicated");
                }
                self.define_at(
                    &parameter.value,
                    format!("block {} parameter", id),
                    MirDefinitionSite::BlockParameter(id.clone()),
                );
            }
            for (index, instruction) in block.instructions.iter().enumerate() {
                if !self.instruction_ids.insert(instruction.id.clone()) {
                    self.error(
                        instruction.id.to_string(),
                        "instruction identity is duplicated",
                    );
                }
                self.check_instruction(instruction, id, index);
            }
            self.check_terminator(&block.terminator);
        }
        for (id, value) in &self.function.values {
            if id != &value.id {
                self.error(
                    id.to_string(),
                    "value catalog key disagrees with value identity",
                );
            }
            if value.ty.as_str().trim().is_empty() {
                self.error(id.to_string(), "value type identity is empty");
            }
            if !self.definitions.contains_key(id) {
                self.error(id.to_string(), "value is declared but never defined");
            }
        }
        self.check_dominance();
    }

    fn check_ownership(&mut self) {
        if let Err(errors) = self.function.ownership.validate() {
            self.errors.extend(errors);
        }
        for (index, event) in self.function.ownership.events.iter().enumerate() {
            if let Some(value) = &event.value {
                if !self.function.values.contains_key(value) {
                    self.error(
                        format!("ownership[{index}]"),
                        format!(
                            "event value '{}' is absent from the function value catalog",
                            value
                        ),
                    );
                }
            }
        }
    }

    fn define_at(&mut self, value: &MirValueId, subject: String, site: MirDefinitionSite) {
        if !self.function.values.contains_key(value) {
            self.error(value.to_string(), "definition is absent from value catalog");
        }
        if let Some(previous) = self.definitions.get(value).cloned() {
            self.error(
                value.to_string(),
                format!("value is defined more than once (also {previous})"),
            );
        } else {
            self.definitions.insert(value.clone(), subject);
            self.definition_sites.insert(value.clone(), site);
        }
    }

    fn use_value(&mut self, value: &MirValueId) {
        if !self.function.values.contains_key(value) {
            self.error(value.to_string(), "use is absent from value catalog");
        }
    }

    fn result_at(
        &mut self,
        value: &MirValueId,
        instruction: &MirInstructionId,
        block: &MirBlockId,
        index: usize,
    ) {
        self.define_at(
            value,
            format!("instruction {instruction}"),
            MirDefinitionSite::Instruction {
                block: block.clone(),
                index,
            },
        );
    }

    fn check_instruction(
        &mut self,
        instruction: &MirInstruction,
        block: &MirBlockId,
        index: usize,
    ) {
        use MirInstructionKind::*;
        match &instruction.kind {
            Const { result, .. } | Load { result, .. } => {
                self.result_at(result, &instruction.id, block, index)
            }
            Copy { result, source }
            | Move { result, source }
            | Clone { result, source }
            | Convert { result, source } => {
                self.use_value(source);
                self.result_at(result, &instruction.id, block, index);
            }
            Drop { value } | EndBorrow { borrow: value } => self.use_value(value),
            Borrow { result, source, .. } => {
                self.use_value(source);
                self.result_at(result, &instruction.id, block, index);
            }
            Project {
                result,
                base,
                projection,
            } => {
                self.use_value(base);
                if let MirProjection::Index(index) = projection {
                    self.use_value(index);
                }
                self.result_at(result, &instruction.id, block, index);
            }
            Construct { result, fields, .. } => {
                self.values(fields);
                self.result_at(result, &instruction.id, block, index);
            }
            Binary {
                result,
                left,
                right,
                ..
            } => {
                self.use_value(left);
                self.use_value(right);
                self.result_at(result, &instruction.id, block, index);
            }
            Unary {
                result, operand, ..
            } => {
                self.use_value(operand);
                self.result_at(result, &instruction.id, block, index);
            }
            Call {
                result, arguments, ..
            } => {
                for argument in arguments {
                    self.use_value(argument);
                }
                if let Some(result) = result {
                    self.result_at(result, &instruction.id, block, index);
                }
            }
            Nop => {}
        }
    }

    fn check_terminator(&mut self, terminator: &MirTerminator) {
        match terminator {
            MirTerminator::Goto {
                edge,
                target,
                arguments,
            } => {
                self.edge(edge);
                self.target(target);
                self.values(arguments);
                self.check_arity(target, arguments);
            }
            MirTerminator::Branch {
                condition,
                then_edge,
                then_target,
                then_arguments,
                else_edge,
                else_target,
                else_arguments,
            } => {
                self.use_value(condition);
                self.edge(then_edge);
                self.edge(else_edge);
                self.target(then_target);
                self.target(else_target);
                self.values(then_arguments);
                self.values(else_arguments);
                self.check_arity(then_target, then_arguments);
                self.check_arity(else_target, else_arguments);
            }
            MirTerminator::Switch { scrutinee, arms } => {
                self.use_value(scrutinee);
                let mut has_default = false;
                for arm in arms {
                    self.edge(&arm.edge);
                    self.target(&arm.target);
                    self.values(&arm.arguments);
                    self.check_arity(&arm.target, &arm.arguments);
                    if matches!(arm.case, MirSwitchCase::Default) {
                        if has_default {
                            self.error(
                                arm.edge.to_string(),
                                "switch has more than one default arm",
                            );
                        }
                        has_default = true;
                    }
                }
            }
            MirTerminator::Return { value } => {
                if let Some(value) = value {
                    self.use_value(value);
                    if let Some(value_ty) = self.function.values.get(value).map(|value| &value.ty) {
                        if value_ty != &self.function.result {
                            self.error(
                                value.to_string(),
                                "return value type disagrees with function result type",
                            );
                        }
                    }
                }
            }
            MirTerminator::Fault { value } => {
                if let Some(value) = value {
                    self.use_value(value);
                }
            }
            MirTerminator::Trap { code } => {
                if code.trim().is_empty() {
                    self.error("terminator", "trap code is empty");
                }
            }
            MirTerminator::Unreachable => {}
        }
    }

    fn edge(&mut self, edge: &MirEdgeId) {
        if !self.edge_ids.insert(edge.clone()) {
            self.error(edge.to_string(), "edge identity is duplicated");
        }
    }

    fn target(&mut self, target: &MirBlockId) {
        if !self.function.blocks.contains_key(target) {
            self.error(target.to_string(), "edge targets a missing block");
        }
    }

    fn values(&mut self, values: &[MirValueId]) {
        for value in values {
            self.use_value(value);
        }
    }

    fn check_arity(&mut self, target: &MirBlockId, arguments: &[MirValueId]) {
        if let Some(block) = self.function.blocks.get(target) {
            if block.parameters.len() != arguments.len() {
                self.error(
                    target.to_string(),
                    format!(
                        "edge passes {} values but target expects {} parameters",
                        arguments.len(),
                        block.parameters.len()
                    ),
                );
            }
            let parameters = block.parameters.clone();
            for (index, (argument, parameter)) in
                arguments.iter().zip(parameters.iter()).enumerate()
            {
                let argument_ty = self
                    .function
                    .values
                    .get(argument)
                    .map(|value| value.ty.clone());
                let parameter_ty = self
                    .function
                    .values
                    .get(&parameter.value)
                    .map(|value| value.ty.clone());
                if argument_ty.is_some() && parameter_ty.is_some() && argument_ty != parameter_ty {
                    self.error(
                        target.to_string(),
                        format!("edge argument {index} type disagrees with target parameter"),
                    );
                }
            }
        }
    }

    /// Reject values used outside the block that defines them unless that
    /// defining block dominates the use. This turns the value catalog from a
    /// mere name table into a real SSA-like contract while retaining explicit
    /// block parameters for control-flow joins.
    fn check_dominance(&mut self) {
        let reachable = self.reachable_blocks();
        if reachable.is_empty() {
            return;
        }
        let mut dominators: BTreeMap<MirBlockId, BTreeSet<MirBlockId>> = BTreeMap::new();
        for block in &reachable {
            if block == &self.function.entry {
                dominators.insert(block.clone(), BTreeSet::from([block.clone()]));
            } else {
                dominators.insert(block.clone(), reachable.clone());
            }
        }
        let predecessors = self.predecessors(&reachable);
        let mut changed = true;
        while changed {
            changed = false;
            for block in reachable
                .iter()
                .filter(|block| *block != &self.function.entry)
            {
                let Some(preds) = predecessors.get(block) else {
                    continue;
                };
                if preds.is_empty() {
                    continue;
                }
                let mut next = reachable.clone();
                for predecessor in preds {
                    if let Some(pred_dominators) = dominators.get(predecessor) {
                        next.retain(|candidate| pred_dominators.contains(candidate));
                    }
                }
                next.insert(block.clone());
                if dominators.get(block) != Some(&next) {
                    dominators.insert(block.clone(), next);
                    changed = true;
                }
            }
        }

        for (block_id, block) in &self.function.blocks {
            if !reachable.contains(block_id) {
                self.error(
                    block_id.to_string(),
                    "block is unreachable from function entry",
                );
                continue;
            }
            for (index, instruction) in block.instructions.iter().enumerate() {
                self.check_instruction_uses(block_id, index, instruction, &dominators, &reachable);
            }
            self.check_terminator_uses(
                block_id,
                block.instructions.len(),
                &block.terminator,
                &dominators,
                &reachable,
            );
        }
    }

    fn reachable_blocks(&self) -> BTreeSet<MirBlockId> {
        let mut reachable = BTreeSet::new();
        let mut pending = vec![self.function.entry.clone()];
        while let Some(block_id) = pending.pop() {
            if !reachable.insert(block_id.clone()) {
                continue;
            }
            let Some(block) = self.function.blocks.get(&block_id) else {
                continue;
            };
            for successor in Self::successors(&block.terminator) {
                if self.function.blocks.contains_key(&successor) {
                    pending.push(successor);
                }
            }
        }
        reachable
    }

    fn predecessors(
        &self,
        reachable: &BTreeSet<MirBlockId>,
    ) -> BTreeMap<MirBlockId, BTreeSet<MirBlockId>> {
        let mut predecessors = reachable
            .iter()
            .cloned()
            .map(|block| (block, BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();
        for (block_id, block) in &self.function.blocks {
            if !reachable.contains(block_id) {
                continue;
            }
            for successor in Self::successors(&block.terminator) {
                if let Some(preds) = predecessors.get_mut(&successor) {
                    preds.insert(block_id.clone());
                }
            }
        }
        predecessors
    }

    fn successors(terminator: &MirTerminator) -> Vec<MirBlockId> {
        match terminator {
            MirTerminator::Goto { target, .. } => vec![target.clone()],
            MirTerminator::Branch {
                then_target,
                else_target,
                ..
            } => vec![then_target.clone(), else_target.clone()],
            MirTerminator::Switch { arms, .. } => {
                arms.iter().map(|arm| arm.target.clone()).collect()
            }
            MirTerminator::Return { .. }
            | MirTerminator::Trap { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => Vec::new(),
        }
    }

    fn check_instruction_uses(
        &mut self,
        block: &MirBlockId,
        index: usize,
        instruction: &MirInstruction,
        dominators: &BTreeMap<MirBlockId, BTreeSet<MirBlockId>>,
        reachable: &BTreeSet<MirBlockId>,
    ) {
        let mut uses: Vec<MirValueId> = Vec::new();
        match &instruction.kind {
            MirInstructionKind::Const { .. } | MirInstructionKind::Nop => {}
            MirInstructionKind::Load { place, .. } => {
                if let Ok(local) = MirValueId::new(format!("local:{}", place.base.0 .0)) {
                    uses.push(local);
                }
            }
            MirInstructionKind::Copy { source, .. }
            | MirInstructionKind::Move { source, .. }
            | MirInstructionKind::Clone { source, .. }
            | MirInstructionKind::Convert { source, .. } => uses.push(source.clone()),
            MirInstructionKind::Drop { value }
            | MirInstructionKind::EndBorrow { borrow: value } => uses.push(value.clone()),
            MirInstructionKind::Borrow { source, .. } => uses.push(source.clone()),
            MirInstructionKind::Project {
                base, projection, ..
            } => {
                uses.push(base.clone());
                if let MirProjection::Index(index) = projection {
                    uses.push(index.clone());
                }
            }
            MirInstructionKind::Construct { fields, .. } => {
                uses.extend(fields.iter().cloned());
            }
            MirInstructionKind::Binary { left, right, .. } => {
                uses.push(left.clone());
                uses.push(right.clone());
            }
            MirInstructionKind::Unary { operand, .. } => uses.push(operand.clone()),
            MirInstructionKind::Call { arguments, .. } => uses.extend(arguments.iter().cloned()),
        }
        for value in uses {
            self.check_use_site(&value, block, index, dominators, reachable);
        }
    }

    fn check_terminator_uses(
        &mut self,
        block: &MirBlockId,
        index: usize,
        terminator: &MirTerminator,
        dominators: &BTreeMap<MirBlockId, BTreeSet<MirBlockId>>,
        reachable: &BTreeSet<MirBlockId>,
    ) {
        let mut uses = Vec::new();
        match terminator {
            MirTerminator::Goto { arguments, .. } => uses.extend(arguments.iter().cloned()),
            MirTerminator::Branch {
                condition,
                then_arguments,
                else_arguments,
                ..
            } => {
                uses.push(condition.clone());
                uses.extend(then_arguments.iter().cloned());
                uses.extend(else_arguments.iter().cloned());
            }
            MirTerminator::Switch { scrutinee, arms } => {
                uses.push(scrutinee.clone());
                for arm in arms {
                    uses.extend(arm.arguments.iter().cloned());
                }
            }
            MirTerminator::Return { value } | MirTerminator::Fault { value } => {
                uses.extend(value.iter().cloned());
            }
            MirTerminator::Trap { .. } | MirTerminator::Unreachable => {}
        }
        for value in uses {
            self.check_use_site(&value, block, index, dominators, reachable);
        }
    }

    fn check_use_site(
        &mut self,
        value: &MirValueId,
        use_block: &MirBlockId,
        use_index: usize,
        dominators: &BTreeMap<MirBlockId, BTreeSet<MirBlockId>>,
        reachable: &BTreeSet<MirBlockId>,
    ) {
        let Some(definition) = self.definition_sites.get(value) else {
            return;
        };
        let valid = match definition {
            MirDefinitionSite::FunctionParameter => true,
            MirDefinitionSite::BlockParameter(def_block) => {
                self.definition_dominates(def_block, use_block, dominators, reachable)
            }
            MirDefinitionSite::Instruction { block, index } => {
                if block == use_block {
                    *index < use_index
                } else {
                    self.definition_dominates(block, use_block, dominators, reachable)
                }
            }
        };
        if !valid {
            self.error(
                value.to_string(),
                format!("value is used before its definition at block {use_block}"),
            );
        }
    }

    fn definition_dominates(
        &self,
        definition_block: &MirBlockId,
        use_block: &MirBlockId,
        dominators: &BTreeMap<MirBlockId, BTreeSet<MirBlockId>>,
        reachable: &BTreeSet<MirBlockId>,
    ) -> bool {
        reachable.contains(definition_block)
            && dominators
                .get(use_block)
                .is_some_and(|blocks| blocks.contains(definition_block))
    }

    fn finish(self) -> Result<(), Vec<MirValidationError>> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors)
        }
    }
}

#[cfg(test)]
mod tests;
