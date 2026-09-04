//! Canonical contract predicates attached to MIR functions.
//!
//! Contract conditions cross the frontend/backend boundary as a deliberately
//! small, typed predicate language.  They do not retain surface names or
//! `ResolvedExpr` nodes: local references use canonical MIR value identities,
//! and the return value has an explicit `Result` marker.  This lets the MIR
//! verifier consume the same contract facts as the execution backends without
//! reparsing or re-encoding the source AST.

use crate::core::ir::{ResolvedBinaryOp, ResolvedProjection};
use crate::core::NodeId;

use super::types::{
    verifier_float_boundary_message, MirAbiClass, MirGlueKind, MirLayout, MirOwnership,
    MirTypeCatalog,
};
use super::{MirFunction, MirProjection, MirValidationError, MirValueId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirContractKind {
    Requires,
    Ensures,
    Invariant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirContractUnaryOp {
    Negate,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirContractBinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    LogicalAnd,
    LogicalOr,
}

impl MirContractBinaryOp {
    pub(crate) fn from_resolved(op: ResolvedBinaryOp) -> Option<Self> {
        Some(match op {
            ResolvedBinaryOp::Add => Self::Add,
            ResolvedBinaryOp::Subtract => Self::Subtract,
            ResolvedBinaryOp::Multiply => Self::Multiply,
            ResolvedBinaryOp::Divide => Self::Divide,
            ResolvedBinaryOp::Remainder => Self::Remainder,
            ResolvedBinaryOp::Equal => Self::Equal,
            ResolvedBinaryOp::NotEqual => Self::NotEqual,
            ResolvedBinaryOp::Less => Self::Less,
            ResolvedBinaryOp::Greater => Self::Greater,
            ResolvedBinaryOp::LessEqual => Self::LessEqual,
            ResolvedBinaryOp::GreaterEqual => Self::GreaterEqual,
            ResolvedBinaryOp::LogicalAnd => Self::LogicalAnd,
            ResolvedBinaryOp::LogicalOr => Self::LogicalOr,
            ResolvedBinaryOp::Power
            | ResolvedBinaryOp::BitAnd
            | ResolvedBinaryOp::BitOr
            | ResolvedBinaryOp::BitXor
            | ResolvedBinaryOp::ShiftLeft
            | ResolvedBinaryOp::ShiftRight => return None,
        })
    }
}

/// A scalar contract expression whose leaves are canonical MIR identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirContractExpr {
    Value(MirValueId),
    Result,
    Old(MirValueId),
    Project {
        base: Box<Self>,
        projection: MirProjection,
    },
    Int(i64),
    Bool(bool),
    Unary {
        op: MirContractUnaryOp,
        operand: Box<Self>,
    },
    Binary {
        op: MirContractBinaryOp,
        left: Box<Self>,
        right: Box<Self>,
    },
}

impl MirContractExpr {
    pub(crate) fn canonical_text(&self) -> String {
        match self {
            Self::Value(value) => value.to_string(),
            Self::Result => "result".into(),
            Self::Old(value) => format!("old({value})"),
            Self::Project { base, projection } => {
                format!("project({}, {projection:?})", base.canonical_text())
            }
            Self::Int(value) => value.to_string(),
            Self::Bool(value) => value.to_string(),
            Self::Unary { op, operand } => {
                let name = match op {
                    MirContractUnaryOp::Negate => "neg",
                    MirContractUnaryOp::Not => "not",
                };
                format!("{name}({})", operand.canonical_text())
            }
            Self::Binary { op, left, right } => {
                let name = match op {
                    MirContractBinaryOp::Add => "add",
                    MirContractBinaryOp::Subtract => "sub",
                    MirContractBinaryOp::Multiply => "mul",
                    MirContractBinaryOp::Divide => "div",
                    MirContractBinaryOp::Remainder => "rem",
                    MirContractBinaryOp::Equal => "eq",
                    MirContractBinaryOp::NotEqual => "ne",
                    MirContractBinaryOp::Less => "lt",
                    MirContractBinaryOp::Greater => "gt",
                    MirContractBinaryOp::LessEqual => "le",
                    MirContractBinaryOp::GreaterEqual => "ge",
                    MirContractBinaryOp::LogicalAnd => "and",
                    MirContractBinaryOp::LogicalOr => "or",
                };
                format!(
                    "{name}({}, {})",
                    left.canonical_text(),
                    right.canonical_text()
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirContract {
    pub id: NodeId,
    pub kind: MirContractKind,
    pub condition: MirContractExpr,
}

impl MirContract {
    pub(crate) fn canonical_text(&self) -> String {
        let kind = match self.kind {
            MirContractKind::Requires => "requires",
            MirContractKind::Ensures => "ensures",
            MirContractKind::Invariant => "invariant",
        };
        format!(
            "  contract {} {} = {}",
            self.id.0.as_str(),
            kind,
            self.condition.canonical_text()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContractValueKind {
    Int,
    Bool,
    Aggregate(crate::core::ir::ResolvedTypeId),
}

fn type_kind(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ir::ResolvedTypeId,
) -> Result<ContractValueKind, String> {
    let descriptor = catalog.get(ty).ok_or_else(|| {
        format!(
            "contract type '{}' is absent from MIR TypeDesc",
            ty.as_str()
        )
    })?;
    match descriptor.abi {
        MirAbiClass::Integer {
            bits: 32 | 64,
            signed: true,
        } => Ok(ContractValueKind::Int),
        MirAbiClass::Bool => Ok(ContractValueKind::Bool),
        _ if matches!(
            descriptor.layout,
            MirLayout::Tuple(_) | MirLayout::Record { .. }
        ) && descriptor.ownership == MirOwnership::Copy
            && descriptor.glue.move_out == MirGlueKind::Noop
            && descriptor.glue.clone == MirGlueKind::Noop
            && descriptor.glue.drop == MirGlueKind::Noop =>
        {
            Ok(ContractValueKind::Aggregate(ty.clone()))
        }
        _ if matches!(
            descriptor.layout,
            MirLayout::Tuple(_) | MirLayout::Record { .. }
        ) =>
        {
            Err(format!(
                "contract type '{}' is outside the canonical Copy aggregate contract",
                ty.as_str()
            ))
        }
        MirAbiClass::Float { .. } => Err(verifier_float_boundary_message(ty, descriptor.abi)),
        abi => Err(format!(
            "contract type '{}' ABI {:?} is outside the canonical scalar verifier contract",
            ty.as_str(),
            abi
        )),
    }
}

fn value_kind(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    value: &MirValueId,
) -> Result<ContractValueKind, String> {
    let value = function
        .values
        .get(value)
        .ok_or_else(|| format!("contract value '{}' is absent from MIR values", value))?;
    type_kind(catalog, &value.ty).map_err(|message| {
        format!(
            "contract value '{}' {}",
            value.id.as_str(),
            message.trim_start_matches("contract ")
        )
    })
}

fn result_kind(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<ContractValueKind, String> {
    type_kind(catalog, &function.result).map_err(|message| {
        format!(
            "contract result {}",
            message.trim_start_matches("contract ")
        )
    })
}

fn expression_type(
    expression: &MirContractExpr,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<crate::core::ir::ResolvedTypeId, String> {
    match expression {
        MirContractExpr::Value(value) | MirContractExpr::Old(value) => function
            .values
            .get(value)
            .map(|value| value.ty.clone())
            .ok_or_else(|| format!("contract value '{}' is absent from MIR values", value)),
        MirContractExpr::Result => Ok(function.result.clone()),
        MirContractExpr::Project { base, projection } => {
            let base_ty = expression_type(base, function, catalog)?;
            catalog.projection_result_type(&base_ty, projection)
        }
        MirContractExpr::Int(_)
        | MirContractExpr::Bool(_)
        | MirContractExpr::Unary { .. }
        | MirContractExpr::Binary { .. } => {
            Err("contract expression has no aggregate projection type".into())
        }
    }
}

fn expr_kind(
    expression: &MirContractExpr,
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Result<ContractValueKind, String> {
    match expression {
        MirContractExpr::Value(value) | MirContractExpr::Old(value) => {
            value_kind(function, catalog, value)
        }
        MirContractExpr::Result => result_kind(function, catalog),
        MirContractExpr::Project { base, projection } => {
            let base_ty = expression_type(base, function, catalog)?;
            if !matches!(
                type_kind(catalog, &base_ty)?,
                ContractValueKind::Aggregate(_)
            ) {
                return Err("contract projection base must be a Copy tuple or record".into());
            }
            let result_ty = catalog.projection_result_type(&base_ty, projection)?;
            type_kind(catalog, &result_ty)
        }
        MirContractExpr::Int(_) => Ok(ContractValueKind::Int),
        MirContractExpr::Bool(_) => Ok(ContractValueKind::Bool),
        MirContractExpr::Unary { op, operand } => {
            let kind = expr_kind(operand, function, catalog)?;
            match (op, kind.clone()) {
                (MirContractUnaryOp::Negate, ContractValueKind::Int)
                | (MirContractUnaryOp::Not, ContractValueKind::Bool) => Ok(kind),
                _ => Err("contract unary operator has an incompatible scalar operand".into()),
            }
        }
        MirContractExpr::Binary { op, left, right } => {
            let left_kind = expr_kind(left, function, catalog)?;
            let right_kind = expr_kind(right, function, catalog)?;
            match op {
                MirContractBinaryOp::Add
                | MirContractBinaryOp::Subtract
                | MirContractBinaryOp::Multiply
                | MirContractBinaryOp::Divide
                | MirContractBinaryOp::Remainder => {
                    if left_kind == ContractValueKind::Int && right_kind == ContractValueKind::Int {
                        Ok(ContractValueKind::Int)
                    } else {
                        Err("contract arithmetic requires integer operands".into())
                    }
                }
                MirContractBinaryOp::LogicalAnd | MirContractBinaryOp::LogicalOr => {
                    if left_kind == ContractValueKind::Bool && right_kind == ContractValueKind::Bool
                    {
                        Ok(ContractValueKind::Bool)
                    } else {
                        Err("contract logical operator requires boolean operands".into())
                    }
                }
                MirContractBinaryOp::Equal | MirContractBinaryOp::NotEqual => {
                    if matches!(
                        (left_kind, right_kind),
                        (ContractValueKind::Int, ContractValueKind::Int)
                            | (ContractValueKind::Bool, ContractValueKind::Bool)
                    ) {
                        Ok(ContractValueKind::Bool)
                    } else {
                        Err("contract equality operands have incompatible scalar types".into())
                    }
                }
                MirContractBinaryOp::Less
                | MirContractBinaryOp::Greater
                | MirContractBinaryOp::LessEqual
                | MirContractBinaryOp::GreaterEqual => {
                    if left_kind == ContractValueKind::Int && right_kind == ContractValueKind::Int {
                        Ok(ContractValueKind::Bool)
                    } else {
                        Err("contract ordering requires integer operands".into())
                    }
                }
            }
        }
    }
}

/// Validate contract predicates after lowering and before any consumer sees
/// the program.  This is deliberately independent of Z3 and backend ABI.
pub(crate) fn validate_contracts(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
) -> Vec<MirValidationError> {
    let mut errors = Vec::new();
    for (index, contract) in function.contracts.iter().enumerate() {
        let subject = format!("contract[{index}] {}", contract.id.0.as_str());
        let kind = match expr_kind(&contract.condition, function, catalog) {
            Ok(kind) => kind,
            Err(message) => {
                errors.push(MirValidationError { subject, message });
                continue;
            }
        };
        if kind != ContractValueKind::Bool {
            errors.push(MirValidationError {
                subject: format!("contract[{index}] {}", contract.id.0.as_str()),
                message: "contract condition must be boolean".into(),
            });
        }
        if matches!(contract.kind, MirContractKind::Requires)
            && contains_result(&contract.condition)
        {
            errors.push(MirValidationError {
                subject: format!("contract[{index}] {}", contract.id.0.as_str()),
                message: "requires contract cannot reference the function result".into(),
            });
        }
        if contains_invalid_old(function, &contract.condition) {
            errors.push(MirValidationError {
                subject: format!("contract[{index}] {}", contract.id.0.as_str()),
                message: "old() must reference a callable parameter value".into(),
            });
        }
    }
    errors
}

fn contains_result(expression: &MirContractExpr) -> bool {
    match expression {
        MirContractExpr::Result => true,
        MirContractExpr::Unary { operand, .. } => contains_result(operand),
        MirContractExpr::Binary { left, right, .. } => {
            contains_result(left) || contains_result(right)
        }
        MirContractExpr::Project { base, .. } => contains_result(base),
        MirContractExpr::Value(_)
        | MirContractExpr::Old(_)
        | MirContractExpr::Int(_)
        | MirContractExpr::Bool(_) => false,
    }
}

fn contains_invalid_old(function: &MirFunction, expression: &MirContractExpr) -> bool {
    match expression {
        MirContractExpr::Old(value) => !function.parameters.contains(value),
        MirContractExpr::Unary { operand, .. } => contains_invalid_old(function, operand),
        MirContractExpr::Binary { left, right, .. } => {
            contains_invalid_old(function, left) || contains_invalid_old(function, right)
        }
        MirContractExpr::Project { base, .. } => contains_invalid_old(function, base),
        MirContractExpr::Value(_)
        | MirContractExpr::Result
        | MirContractExpr::Int(_)
        | MirContractExpr::Bool(_) => false,
    }
}

fn lower_projection(projection: &ResolvedProjection) -> Result<MirProjection, String> {
    match projection {
        ResolvedProjection::Field { field, .. } => Ok(MirProjection::Field(field.clone())),
        ResolvedProjection::Tuple { index, .. } => Ok(MirProjection::Tuple(*index)),
        ResolvedProjection::Index { .. } => {
            Err("contract indexed projection is outside the canonical aggregate contract".into())
        }
        ResolvedProjection::Deref { .. } => Err(
            "contract dereference projection is outside the canonical aggregate contract".into(),
        ),
    }
}

fn lower_contract_place(
    place: &crate::core::ResolvedPlace,
    function: &MirFunction,
    body: &crate::core::ir::ResolvedBody,
    old: bool,
) -> Result<MirContractExpr, String> {
    let value =
        MirValueId::new(format!("local:{}", place.base.0 .0)).map_err(|error| error.to_string())?;
    let mut base = if old {
        if place.base.0 .0.ends_with("/contract-result/local")
            || !body.parameters.contains(&place.base)
        {
            return Err("old() requires a direct callable parameter load".into());
        }
        if !function.values.contains_key(&value) {
            return Err(format!(
                "old() parameter '{}' is absent from canonical MIR values",
                place.base.0 .0
            ));
        }
        MirContractExpr::Old(value)
    } else if place.base.0 .0.ends_with("/contract-result/local") {
        MirContractExpr::Result
    } else {
        if !function.values.contains_key(&value) {
            return Err(format!(
                "contract local '{}' is absent from canonical MIR values",
                place.base.0 .0
            ));
        }
        MirContractExpr::Value(value)
    };
    for projection in &place.projections {
        base = MirContractExpr::Project {
            base: Box::new(base),
            projection: lower_projection(projection)?,
        };
    }
    Ok(base)
}

/// Resolve a ResolvedExpr condition into canonical MIR identities.  This is
/// the only frontend-facing part of the contract path; all consumers receive
/// the resulting `MirContractExpr` and never see the source expression.
pub(crate) fn lower_contract_expr(
    expression: &crate::core::ir::ResolvedExpr,
    function: &MirFunction,
    body: &crate::core::ir::ResolvedBody,
) -> Result<MirContractExpr, String> {
    use crate::core::ir::{ResolvedExprKind, ResolvedLiteral, ResolvedUnaryOp};

    match &expression.kind {
        ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) => Ok(MirContractExpr::Int(*value)),
        ResolvedExprKind::Literal(ResolvedLiteral::Bool(value)) => {
            Ok(MirContractExpr::Bool(*value))
        }
        ResolvedExprKind::Load(place) => lower_contract_place(place, function, body, false),
        ResolvedExprKind::Old(inner) => {
            let ResolvedExprKind::Load(place) = &inner.kind else {
                return Err("old() requires a direct callable parameter load".into());
            };
            lower_contract_place(place, function, body, true)
        }
        ResolvedExprKind::Project { value, projection } => Ok(MirContractExpr::Project {
            base: Box::new(lower_contract_expr(value, function, body)?),
            projection: lower_value_projection(projection)?,
        }),
        ResolvedExprKind::Unary { op, operand } => {
            let op = match op {
                ResolvedUnaryOp::Negate => MirContractUnaryOp::Negate,
                ResolvedUnaryOp::Not => MirContractUnaryOp::Not,
                _ => {
                    return Err(
                        "contract unary operator is outside the canonical MIR contract".into(),
                    )
                }
            };
            Ok(MirContractExpr::Unary {
                op,
                operand: Box::new(lower_contract_expr(operand, function, body)?),
            })
        }
        ResolvedExprKind::Binary { op, left, right } => {
            let op = MirContractBinaryOp::from_resolved(*op).ok_or_else(|| {
                "contract binary operator is outside the canonical MIR contract".to_string()
            })?;
            Ok(MirContractExpr::Binary {
                op,
                left: Box::new(lower_contract_expr(left, function, body)?),
                right: Box::new(lower_contract_expr(right, function, body)?),
            })
        }
        _ => Err(format!(
            "contract expression shape {:?} is outside the canonical MIR verifier contract",
            expression.kind
        )),
    }
}

fn lower_value_projection(
    projection: &crate::core::ir::ResolvedValueProjection,
) -> Result<MirProjection, String> {
    match projection {
        crate::core::ir::ResolvedValueProjection::Field(field) => {
            Ok(MirProjection::Field(field.clone()))
        }
        crate::core::ir::ResolvedValueProjection::Tuple(index) => Ok(MirProjection::Tuple(*index)),
        crate::core::ir::ResolvedValueProjection::Index(_) => {
            Err("contract indexed projection is outside the canonical aggregate contract".into())
        }
        crate::core::ir::ResolvedValueProjection::Dereference => Err(
            "contract dereference projection is outside the canonical aggregate contract".into(),
        ),
    }
}

pub(crate) fn lower_contracts(
    callable: &crate::core::ir::ResolvedCallable,
    function: &MirFunction,
) -> Result<Vec<MirContract>, Vec<super::lower::MirLoweringError>> {
    let mut contracts = Vec::with_capacity(callable.contracts.len());
    let mut errors = Vec::new();
    for contract in &callable.contracts {
        let kind = match contract.kind {
            crate::core::ir::ContractKind::Requires => MirContractKind::Requires,
            crate::core::ir::ContractKind::Ensures => MirContractKind::Ensures,
            crate::core::ir::ContractKind::Invariant => MirContractKind::Invariant,
        };
        match lower_contract_expr(&contract.condition, function, &callable.body) {
            Ok(condition) => contracts.push(MirContract {
                id: contract.node_id.clone(),
                kind,
                condition,
            }),
            Err(message) => errors.push(super::lower::MirLoweringError {
                node_id: contract.node_id.clone(),
                message,
            }),
        }
    }
    if errors.is_empty() {
        Ok(contracts)
    } else {
        Err(errors)
    }
}
