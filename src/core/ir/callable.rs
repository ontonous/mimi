use super::{EffectId, Permission, ResolvedParameterId, ResolvedTypeId, ResolvedTypeTable};
use crate::core::NodeId;
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
