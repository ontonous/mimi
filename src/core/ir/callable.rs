use super::{
    ContractKind, EffectId, Permission, ResolvedBlock, ResolvedBody, ResolvedExpr,
    ResolvedExprKind, ResolvedParameterId, ResolvedStmtKind, ResolvedTypeId, ResolvedTypeTable,
};
use crate::core::{cfg::CallableCfg, NodeId, Origin, ResourceAnalysis};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedParameter {
    pub id: ResolvedParameterId,
    pub name: String,
    pub ty: ResolvedTypeId,
    pub mutable: bool,
    pub permission: Option<Permission>,
    pub has_default: bool,
}

/// Checker-finalized callable signature using only canonical type identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSignature {
    pub owner: NodeId,
    pub generic_parameters: Vec<NodeId>,
    /// Declaration order is parameter order and is semantically significant.
    pub parameters: Vec<ResolvedParameter>,
    pub result: ResolvedTypeId,
    pub effects: Vec<EffectId>,
}

/// A checker-owned contract attached to one callable body.
#[derive(Debug, Clone)]
pub struct ResolvedContract {
    pub node_id: NodeId,
    pub kind: ContractKind,
    pub condition: ResolvedExpr,
    pub origin: Origin,
}

/// Atomic semantic unit consumed by every executable backend.
#[derive(Debug, Clone)]
pub struct ResolvedCallable {
    pub owner: NodeId,
    pub signature: ResolvedSignature,
    pub body: ResolvedBody,
    pub contracts: Vec<ResolvedContract>,
    pub cfg: CallableCfg,
    pub resources: ResourceAnalysis,
}

impl ResolvedCallable {
    pub(crate) fn assemble(
        signature: ResolvedSignature,
        body: ResolvedBody,
        cfg: CallableCfg,
        resources: ResourceAnalysis,
    ) -> Result<Self, ResolvedSignatureError> {
        let owner = signature.owner.clone();
        if body.owner != owner || cfg.owner != owner || resources.owner != owner {
            return Err(ResolvedSignatureError::new(
                &owner,
                "signature, body, CFG, and resource analysis owners disagree",
            ));
        }
        let mut contracts = Vec::new();
        collect_contracts(&body.root, &mut contracts);
        Ok(Self {
            owner,
            signature,
            body,
            contracts,
            cfg,
            resources,
        })
    }
}

fn collect_contracts(block: &ResolvedBlock, out: &mut Vec<ResolvedContract>) {
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Contract { kind, condition } => out.push(ResolvedContract {
                node_id: statement.node_id.clone(),
                kind: *kind,
                condition: condition.clone(),
                origin: statement.origin.clone(),
            }),
            ResolvedStmtKind::While { condition, body } => {
                collect_expr_contracts(condition, out);
                collect_contracts(body, out);
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                collect_expr_contracts(initializer, out);
                collect_contracts(body, out);
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                collect_contracts(body, out);
            }
            ResolvedStmtKind::Pinned {
                value,
                timeout,
                body,
                ..
            } => {
                collect_expr_contracts(value, out);
                if let Some(timeout) = timeout {
                    collect_expr_contracts(timeout, out);
                }
                collect_contracts(body, out);
            }
            ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(initializer) = initializer {
                    collect_expr_contracts(initializer, out);
                }
            }
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Return {
                value: Some(value), ..
            }
            | ResolvedStmtKind::Break(Some(value)) => collect_expr_contracts(value, out),
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    collect_expr_contracts(expression, out);
                }
            }
            ResolvedStmtKind::Return { value: None, .. }
            | ResolvedStmtKind::Break(None)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Delegate { .. }
            | ResolvedStmtKind::NestedCallable(_) => {}
        }
    }
    if let Some(result) = &block.result {
        collect_expr_contracts(result, out);
    }
}

fn collect_expr_contracts(expression: &ResolvedExpr, out: &mut Vec<ResolvedContract>) {
    match &expression.kind {
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => collect_contracts(block, out),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_expr_contracts(condition, out);
            collect_contracts(then_block, out);
            collect_contracts(else_block, out);
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            collect_expr_contracts(scrutinee, out);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_expr_contracts(guard, out);
                }
                collect_expr_contracts(&arm.body, out);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSignatureError {
    pub owner: NodeId,
    pub message: String,
}

impl ResolvedSignatureError {
    fn new(owner: &NodeId, message: impl Into<String>) -> Self {
        Self {
            owner: owner.clone(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ResolvedSignatureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "resolved signature '{}': {}",
            self.owner.0, self.message
        )
    }
}

impl std::error::Error for ResolvedSignatureError {}

impl ResolvedSignature {
    pub fn validate(&self, types: &ResolvedTypeTable) -> Result<(), Vec<ResolvedSignatureError>> {
        let mut errors = Vec::new();
        if self.owner.0.trim().is_empty() {
            errors.push(ResolvedSignatureError::new(
                &self.owner,
                "callable owner identity is empty",
            ));
        }
        if types.get(&self.result).is_none() {
            errors.push(ResolvedSignatureError::new(
                &self.owner,
                format!("result type '{}' is missing", self.result.as_str()),
            ));
        }

        let mut generic_ids = BTreeSet::new();
        for parameter in &self.generic_parameters {
            if parameter.0.trim().is_empty() || !generic_ids.insert(parameter) {
                errors.push(ResolvedSignatureError::new(
                    &self.owner,
                    "generic parameter identities must be non-empty and unique",
                ));
            }
        }

        let mut parameter_ids = BTreeSet::new();
        let mut parameter_names = BTreeSet::new();
        for parameter in &self.parameters {
            if parameter.id.0 .0.trim().is_empty() || !parameter_ids.insert(&parameter.id) {
                errors.push(ResolvedSignatureError::new(
                    &self.owner,
                    "parameter identities must be non-empty and unique",
                ));
            }
            if parameter.name.trim().is_empty() || !parameter_names.insert(&parameter.name) {
                errors.push(ResolvedSignatureError::new(
                    &self.owner,
                    "parameter names must be non-empty and unique",
                ));
            }
            if types.get(&parameter.ty).is_none() {
                errors.push(ResolvedSignatureError::new(
                    &self.owner,
                    format!(
                        "parameter '{}' type '{}' is missing",
                        parameter.name,
                        parameter.ty.as_str()
                    ),
                ));
            }
        }

        let mut effects = BTreeSet::new();
        for effect in &self.effects {
            if !effects.insert(effect) {
                errors.push(ResolvedSignatureError::new(
                    &self.owner,
                    "effect identities must be unique",
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Type;
    use crate::core::ir::{ResolvedTypeCapabilities, ResolvedTypeName};
    use crate::core::phase::ZonkedTy;

    fn node(value: &str) -> NodeId {
        NodeId(format!("test:{value}"))
    }

    #[test]
    fn signature_rejects_duplicate_parameter_identity() {
        let mut types = ResolvedTypeTable::new();
        let ty = types
            .intern_zonked(
                &ZonkedTy::from_resolved(Type::Name("i32".into(), Vec::new())).unwrap(),
                &ResolvedTypeCapabilities::default(),
                ResolvedTypeName::primitive,
            )
            .unwrap();
        let parameter_id = ResolvedParameterId(node("param"));
        let parameter = |name: &str| ResolvedParameter {
            id: parameter_id.clone(),
            name: name.into(),
            ty: ty.clone(),
            mutable: false,
            permission: None,
            has_default: false,
        };
        let signature = ResolvedSignature {
            owner: node("function"),
            generic_parameters: Vec::new(),
            parameters: vec![parameter("left"), parameter("right")],
            result: ty,
            effects: Vec::new(),
        };
        assert!(signature
            .validate(&types)
            .unwrap_err()
            .iter()
            .any(|error| error.message.contains("parameter identities")));
    }
}
