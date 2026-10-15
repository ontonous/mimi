//! Receipted generation-bearing ordinary Any snapshot contract.
//!
//! Descriptor materialization describes values. The private seal binds every
//! owner-bearing instruction, value identity and escape boundary to lowering
//! of the checker-owned body; it is the operation authority.
use super::types::{
    managed_any_i32_string_v1_type_id, ordinary_map_get_any_tuple_i32_string_v1_type_id,
    ordinary_map_owned_any_v2_get_result_type_id, ordinary_map_owned_any_v2_payload_type_id,
    ordinary_map_owned_any_v2_type_id, ordinary_map_owned_any_v2_values_element_type_id,
    ordinary_map_owned_any_v2_values_type_id, MirAbiClass, MirGlueContract, MirGlueKind,
    MirGlueOperation, MirLayout, MirOwnership, MirTypeCatalog, MirTypeKind,
};
use super::{
    MirAggregateKind, MirBlockId, MirFunction, MirGenericInstanceContract, MirInstance,
    MirInstanceId, MirInstruction, MirInstructionId, MirInstructionKind, MirProjection,
    MirTerminator, MirValidationError, MirValueId,
};
use crate::core::ir::{
    CheckedConversion, CheckedConversionKind, PrimitiveType, ResolvedBinaryOp, ResolvedLiteral,
    ResolvedTypeId,
};
use crate::core::NodeId;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const MIR_MANAGED_ANY_SCHEMA_VERSION: u32 = 1;
pub const MIR_MANAGED_ANY_DESCRIPTOR_VERSION: u32 = 1;

pub(super) fn managed_any_list_i32_string_v1_type_id(source: &ResolvedTypeId) -> ResolvedTypeId {
    ResolvedTypeId::synthetic(&format!("mimi-mir:ManagedAnyList:v1:{}", source.as_str()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirManagedAnyOperation {
    Clone,
    Drop,
    /// Borrow one exact canonical Any v1 owner for `println`. The source
    /// remains live; lowering emits a separate Drop for the fresh argument
    /// owner after output completes.
    Println,
    /// Materialize one Checker-certified implicit `i32 -> Any` injection as
    /// the canonical generation-bearing ManagedAny v1 owner.
    PackI32,
    UnpackI32,
    UnpackString,
}
impl MirManagedAnyOperation {
    pub const fn expected_kind(self) -> Option<u32> {
        match self {
            Self::PackI32 | Self::UnpackI32 => Some(1),
            Self::UnpackString => Some(2),
            Self::Clone | Self::Drop | Self::Println => None,
        }
    }
    pub const fn canonical_text(self) -> &'static str {
        match self {
            Self::Clone => "clone",
            Self::Drop => "drop",
            Self::Println => "println",
            Self::PackI32 => "pack_i32",
            Self::UnpackI32 => "unpack_i32",
            Self::UnpackString => "unpack_string",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirManagedAnyContract {
    pub schema_version: u32,
    pub owner: NodeId,
    pub origin: NodeId,
    pub source: MirValueId,
    pub result: Option<MirValueId>,
    pub source_type: ResolvedTypeId,
    pub target_type: Option<ResolvedTypeId>,
    /// Exact ResolvedBody conversion authority. Present only for PackI32;
    /// this prevents a TypeDesc pair or arbitrary MIR Convert from claiming
    /// that the Checker admitted a DynamicAny injection.
    pub checker_conversion: Option<CheckedConversion>,
    /// Raw Checker argument type for the one `println(Any)` borrowing
    /// operation. The private full-body seal binds this source identity to the
    /// exact call-origin and owner operand; it is absent for every other op.
    pub checker_source_type: Option<ResolvedTypeId>,
    pub expected_kind: Option<u32>,
    pub descriptor_version: u32,
}

/// This cannot be constructed outside canonical MIR. Exact typed values and
/// complete boundary shapes prevent changing the graph after Checker lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MirCheckerManagedAnySeal {
    pub(super) owner: NodeId,
    pub(super) value_types: BTreeMap<MirValueId, ResolvedTypeId>,
    pub(super) parameters: Vec<MirValueId>,
    pub(super) parameter_permissions: Option<Vec<Option<crate::core::ir::Permission>>>,
    pub(super) result: ResolvedTypeId,
    pub(super) managed_values: BTreeSet<MirValueId>,
    /// Full Checker-lowered body snapshot. Block identity is part of the
    /// commitment so moving an instruction across a branch/join cannot keep
    /// the same flattened instruction sequence.
    pub(super) block_parameters: Vec<(MirBlockId, Vec<super::MirBlockParameter>)>,
    pub(super) instructions: Vec<(MirBlockId, NodeId, MirInstruction)>,
    pub(super) terminators: Vec<(MirBlockId, MirTerminator)>,
    pub(super) source_cfg: Option<crate::core::ManagedAnyCfgReceipt>,
    pub(super) generic_identity: Option<MirCheckerManagedAnyGenericIdentitySeal>,
    pub(super) generic_trap_cleanup: Option<MirCheckerManagedAnyGenericTrapCleanupSeal>,
}

/// Private Checker-lowering proof for the one generic body admitted to carry
/// a managed Any owner across an instance boundary. The public instance
/// contract is only a profile tag; this seal binds that tag to the exact
/// canonical specialization and the closed Clone/Drop body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MirCheckerManagedAnyGenericIdentitySeal {
    pub(super) template: NodeId,
    pub(super) instance: MirInstanceId,
    /// Exact type argument selected by Checker (normally the source
    /// DynamicAny identity).
    pub(super) type_argument: ResolvedTypeId,
    /// Canonical executable TypeDesc after ManagedAny materialization.
    pub(super) managed_type_argument: ResolvedTypeId,
    pub(super) parameter: MirValueId,
    pub(super) return_value: MirValueId,
    pub(super) clone_instruction: MirInstructionId,
    pub(super) drop_instruction: MirInstructionId,
    pub(super) clone_origin: NodeId,
    pub(super) drop_origin: NodeId,
}

/// Independent Checker-lowering receipt for a generic ManagedAny parameter
/// that the exact body must release before one guaranteed arithmetic trap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MirCheckerManagedAnyGenericTrapCleanupSeal {
    pub(super) template: NodeId,
    pub(super) instance: MirInstanceId,
    pub(super) type_argument: ResolvedTypeId,
    pub(super) managed_type_argument: ResolvedTypeId,
    pub(super) parameter: MirValueId,
    pub(super) trap: super::MirManagedAnyTrapKind,
    pub(super) setup_instructions: [MirInstructionId; 4],
    pub(super) drop_instruction: MirInstructionId,
    pub(super) trap_instruction: MirInstructionId,
    pub(super) trap_result: MirValueId,
    pub(super) drop_origin: NodeId,
    pub(super) trap_origin: NodeId,
}

impl MirTypeCatalog {
    pub fn is_managed_any_i32_string_v1(&self, ty: &ResolvedTypeId) -> bool {
        ty == &managed_any_i32_string_v1_type_id() && self.validate_managed_any_glue(ty).is_ok()
    }

    /// Admit this exact TypeDesc only for the separately sealed direct
    /// generic identity specialization. This does not broaden the general
    /// scalar/variant identity contract or authorize Any elsewhere.
    pub fn validate_managed_any_identity_source_argument(
        &self,
        arguments: &[ResolvedTypeId],
    ) -> Result<ResolvedTypeId, String> {
        if arguments.len() != 1 {
            return Err(format!(
                "ManagedAny identity requires one canonical type argument, got {}",
                arguments.len()
            ));
        }
        let argument = arguments
            .first()
            .ok_or_else(|| "ManagedAny identity argument is absent".to_string())?;
        let source = self
            .get(argument)
            .ok_or("ManagedAny identity source TypeDesc is missing")?;
        if source.kind != MirTypeKind::DynamicAny
            || source.layout != MirLayout::Opaque
            || source.abi != MirAbiClass::OpaqueHandle
            || source.ownership != MirOwnership::Move
        {
            return Err(
                "ManagedAny identity source argument must be the Checker DynamicAny TypeDesc"
                    .into(),
            );
        }
        let dynamic_any_ids = self
            .iter()
            .filter_map(|(id, descriptor)| {
                (descriptor.kind == MirTypeKind::DynamicAny).then_some(id)
            })
            .collect::<Vec<_>>();
        if dynamic_any_ids.len() != 1 || dynamic_any_ids.first().copied() != Some(argument) {
            return Err(
                "ManagedAny identity source argument is not the unique Checker DynamicAny TypeDesc in this catalog"
                    .into(),
            );
        }
        let managed = self.canonical_managed_any_type(argument);
        self.validate_managed_any_glue(&managed)?;
        Ok(managed)
    }

    /// Validate the public instance-table entry against its private Checker
    /// seal. Consumers call this before interpreting a generic ManagedAny
    /// body; canonical type identity or body shape alone is insufficient.
    pub fn validate_managed_any_identity_instance(
        &self,
        instance: &MirInstance,
        function: &MirFunction,
    ) -> Result<(), String> {
        if instance.contract != MirGenericInstanceContract::ManagedAnyIdentity {
            return Err("MIR instance is not the ManagedAny identity profile".into());
        }
        let managed_argument =
            self.validate_managed_any_identity_source_argument(&instance.arguments)?;
        let expected_id = MirInstanceId::for_template(&instance.template, &instance.arguments)
            .map_err(|error| error.to_string())?;
        if instance.id != expected_id
            || instance.function != function.owner
            || function.owner != NodeId(format!("function:mir:{}", expected_id.as_str()))
        {
            return Err(
                "ManagedAny identity instance disagrees with its canonical template/argument/owner identity"
                    .into(),
            );
        }
        self.validate_managed_any_function(function)?;
        let seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .and_then(|seal| seal.generic_identity.as_ref())
            .ok_or("ManagedAny identity instance has no private generic Checker seal")?;
        self.validate_managed_any_generic_identity_shape(function, seal)?;
        if seal.template != instance.template
            || seal.instance != instance.id
            || seal.type_argument != instance.arguments[0]
            || seal.managed_type_argument != managed_argument
        {
            return Err(
                "ManagedAny identity private Checker seal disagrees with its instance-table entry"
                    .into(),
            );
        }
        Ok(())
    }

    /// Validate one exact generic arithmetic-trap instance that releases its
    /// canonical ManagedAny parameter in the callee before the trap.
    pub fn validate_managed_any_trap_cleanup_instance(
        &self,
        instance: &MirInstance,
        function: &MirFunction,
    ) -> Result<(), String> {
        let trap = match &instance.contract {
            MirGenericInstanceContract::ManagedAnyTrapCleanup { trap } => *trap,
            _ => return Err("MIR instance is not the ManagedAny trap-cleanup profile".into()),
        };
        let managed_argument =
            self.validate_managed_any_identity_source_argument(&instance.arguments)?;
        let expected_id = MirInstanceId::for_template(&instance.template, &instance.arguments)
            .map_err(|error| error.to_string())?;
        if instance.id != expected_id
            || instance.function != function.owner
            || function.owner != NodeId(format!("function:mir:{}", expected_id.as_str()))
        {
            return Err(
                "ManagedAny trap-cleanup instance disagrees with its canonical template/argument/owner identity"
                    .into(),
            );
        }
        self.validate_managed_any_function(function)?;
        let seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .and_then(|seal| seal.generic_trap_cleanup.as_ref())
            .ok_or("ManagedAny trap-cleanup instance has no private generic Checker seal")?;
        self.validate_managed_any_generic_trap_cleanup_shape(function, seal)?;
        if seal.template != instance.template
            || seal.instance != instance.id
            || seal.type_argument != instance.arguments[0]
            || seal.managed_type_argument != managed_argument
            || seal.trap != trap
        {
            return Err(
                "ManagedAny trap-cleanup private Checker seal disagrees with its instance-table entry"
                    .into(),
            );
        }
        Ok(())
    }

    fn validate_managed_any_generic_identity_shape(
        &self,
        function: &MirFunction,
        identity: &MirCheckerManagedAnyGenericIdentitySeal,
    ) -> Result<(), String> {
        let managed_type_argument = self.validate_managed_any_identity_source_argument(
            std::slice::from_ref(&identity.type_argument),
        )?;
        let expected_instance = MirInstanceId::for_template(
            &identity.template,
            std::slice::from_ref(&identity.type_argument),
        )
        .map_err(|error| error.to_string())?;
        let expected_owner = NodeId(format!("function:mir:{}", expected_instance.as_str()));
        if identity.instance != expected_instance || function.owner != expected_owner {
            return Err(
                "ManagedAny identity private specialization owner/template/type argument is inconsistent"
                    .into(),
            );
        }
        if !function.contracts.is_empty() {
            return Err("ManagedAny identity body does not admit additional contracts".into());
        }
        let [parameter] = function.parameters.as_slice() else {
            return Err("ManagedAny identity function must have exactly one parameter".into());
        };
        if parameter != &identity.parameter
            || identity.managed_type_argument != managed_type_argument
            || function.result != managed_type_argument
            || function
                .values
                .get(parameter)
                .is_none_or(|value| value.ty != managed_type_argument)
        {
            return Err(
                "ManagedAny identity parameter/result does not use the sealed canonical owner type"
                    .into(),
            );
        }
        let expected_managed_values =
            BTreeSet::from([identity.parameter.clone(), identity.return_value.clone()]);
        if function.blocks.len() != 1
            || function
                .ownership
                .checker_managed_any
                .as_ref()
                .is_none_or(|seal| seal.managed_values != expected_managed_values)
        {
            return Err("ManagedAny identity must be a closed one-block owner transfer".into());
        }
        let block = function
            .blocks
            .get(&function.entry)
            .ok_or_else(|| "ManagedAny identity entry block is absent".to_string())?;
        if !block.parameters.is_empty() || identity.return_value == identity.parameter {
            return Err(
                "ManagedAny identity requires a fresh return owner and no block captures".into(),
            );
        }
        let [clone, drop] = block.instructions.as_slice() else {
            return Err("ManagedAny identity body must contain exactly a Clone then Drop".into());
        };
        let MirInstructionKind::ManagedAny {
            result: Some(clone_result),
            source: clone_source,
            operation: MirManagedAnyOperation::Clone,
            contract: Some(clone_contract),
        } = &clone.kind
        else {
            return Err("ManagedAny identity body must start with its receipted Clone".into());
        };
        if clone.id != identity.clone_instruction
            || clone_source != &identity.parameter
            || clone_result != &identity.return_value
            || clone_contract.owner != function.owner
            || clone_contract.origin != identity.clone_origin
            || clone_contract.source != identity.parameter
            || clone_contract.result.as_ref() != Some(&identity.return_value)
            || clone_contract.source_type != identity.managed_type_argument
            || clone_contract.target_type.as_ref() != Some(&identity.managed_type_argument)
            || clone_contract.schema_version != MIR_MANAGED_ANY_SCHEMA_VERSION
            || clone_contract.descriptor_version != MIR_MANAGED_ANY_DESCRIPTOR_VERSION
            || clone_contract.expected_kind.is_some()
        {
            return Err(
                "ManagedAny identity Clone disagrees with its private Checker value/operation receipt"
                    .into(),
            );
        }
        let MirInstructionKind::ManagedAny {
            result: None,
            source: drop_source,
            operation: MirManagedAnyOperation::Drop,
            contract: Some(drop_contract),
        } = &drop.kind
        else {
            return Err(
                "ManagedAny identity body must settle its input with a receipted Drop".into(),
            );
        };
        if drop.id != identity.drop_instruction
            || drop_source != &identity.parameter
            || drop_contract.owner != function.owner
            || drop_contract.origin != identity.drop_origin
            || drop_contract.source != identity.parameter
            || drop_contract.result.is_some()
            || drop_contract.source_type != identity.managed_type_argument
            || drop_contract.target_type.is_some()
            || drop_contract.schema_version != MIR_MANAGED_ANY_SCHEMA_VERSION
            || drop_contract.descriptor_version != MIR_MANAGED_ANY_DESCRIPTOR_VERSION
            || drop_contract.expected_kind.is_some()
        {
            return Err(
                "ManagedAny identity Drop disagrees with its private Checker value/operation receipt"
                    .into(),
            );
        }
        if !matches!(
            &block.terminator,
            MirTerminator::Return { value: Some(value) } if value == &identity.return_value
        ) {
            return Err("ManagedAny identity must return exactly the cloned owner".into());
        }
        Ok(())
    }

    fn validate_managed_any_generic_trap_cleanup_shape(
        &self,
        function: &MirFunction,
        receipt: &MirCheckerManagedAnyGenericTrapCleanupSeal,
    ) -> Result<(), String> {
        let managed_type_argument = self.validate_managed_any_identity_source_argument(
            std::slice::from_ref(&receipt.type_argument),
        )?;
        let expected_instance = MirInstanceId::for_template(
            &receipt.template,
            std::slice::from_ref(&receipt.type_argument),
        )
        .map_err(|error| error.to_string())?;
        let expected_owner = NodeId(format!("function:mir:{}", expected_instance.as_str()));
        if receipt.instance != expected_instance || function.owner != expected_owner {
            return Err(
                "ManagedAny trap-cleanup private specialization owner/template/type argument is inconsistent"
                    .into(),
            );
        }
        if function
            .ownership
            .checker_managed_any
            .as_ref()
            .is_some_and(|seal| seal.generic_identity.is_some())
        {
            return Err("ManagedAny trap-cleanup cannot share an identity receipt".into());
        }
        if !function.contracts.is_empty() {
            return Err("ManagedAny trap-cleanup body does not admit additional contracts".into());
        }
        let [parameter] = function.parameters.as_slice() else {
            return Err("ManagedAny trap-cleanup function must have exactly one parameter".into());
        };
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        if parameter != &receipt.parameter
            || receipt.managed_type_argument != managed_type_argument
            || function.result != i32_type
            || function
                .values
                .get(parameter)
                .is_none_or(|value| value.ty != managed_type_argument)
        {
            return Err(
                "ManagedAny trap-cleanup parameter/result does not use its canonical ManagedAny/i32 ABI"
                    .into(),
            );
        }
        let expected_managed_values = BTreeSet::from([receipt.parameter.clone()]);
        if function.blocks.len() != 1
            || function
                .ownership
                .checker_managed_any
                .as_ref()
                .is_none_or(|seal| seal.managed_values != expected_managed_values)
        {
            return Err(
                "ManagedAny trap-cleanup must be a closed one-block callee-owned cleanup".into(),
            );
        }
        let block = function
            .blocks
            .get(&function.entry)
            .ok_or_else(|| "ManagedAny trap-cleanup entry block is absent".to_string())?;
        if !block.parameters.is_empty() {
            return Err("ManagedAny trap-cleanup block cannot capture block parameters".into());
        }
        let [first, second, third, fourth, drop, trap] = block.instructions.as_slice() else {
            return Err(
                "ManagedAny trap-cleanup body must have four pure setup ops, one Drop, and one trap op"
                    .into(),
            );
        };
        let observed_setup = [
            first.id.clone(),
            second.id.clone(),
            third.id.clone(),
            fourth.id.clone(),
        ];
        if observed_setup.as_slice() != receipt.setup_instructions.as_slice() {
            return Err(
                "ManagedAny trap-cleanup setup instruction identities disagree with the private Checker receipt"
                    .into(),
            );
        }
        fn const_value<'a>(
            instruction: &'a MirInstruction,
            function: &MirFunction,
            i32_type: &ResolvedTypeId,
        ) -> Option<(&'a MirValueId, i64)> {
            match &instruction.kind {
                MirInstructionKind::Const {
                    result,
                    literal: ResolvedLiteral::Int(value),
                } if function
                    .values
                    .get(result)
                    .is_some_and(|value| &value.ty == i32_type) =>
                {
                    Some((result, *value))
                }
                _ => None,
            }
        }
        let (initial_value, initial_constant) = const_value(first, function, &i32_type)
            .ok_or_else(|| {
                "ManagedAny trap-cleanup setup must begin with its Checker integer constant"
                    .to_string()
            })?;
        let MirInstructionKind::Move {
            result: bound_value,
            source: initializer,
        } = &second.kind
        else {
            return Err(
                "ManagedAny trap-cleanup binding must Move its literal into one local".into(),
            );
        };
        if initializer != initial_value
            || function
                .values
                .get(bound_value)
                .is_none_or(|value| value.ty != i32_type)
        {
            return Err(
                "ManagedAny trap-cleanup binding is not the exact i32 literal local".into(),
            );
        }
        let (left, right, expected_op, expected_constants) = match receipt.trap {
            super::MirManagedAnyTrapKind::DivisionByZeroI32 => {
                let Some((one_value, 1)) = const_value(third, function, &i32_type) else {
                    return Err(
                        "division cleanup receipt requires the exact i32 one numerator".into(),
                    );
                };
                let MirInstructionKind::Clone {
                    result: divisor_value,
                    source: divisor_local,
                } = &fourth.kind
                else {
                    return Err(
                        "division cleanup receipt requires a direct local divisor read".into(),
                    );
                };
                if initial_constant != 0
                    || divisor_local != bound_value
                    || function
                        .values
                        .get(divisor_value)
                        .is_none_or(|value| value.ty != i32_type)
                {
                    return Err(
                        "division cleanup divisor read does not use its exact zero local".into(),
                    );
                }
                (one_value, divisor_value, ResolvedBinaryOp::Divide, (0, 1))
            }
            super::MirManagedAnyTrapKind::SignedOverflowI32 => {
                let MirInstructionKind::Clone {
                    result: maximum_value,
                    source: maximum_local,
                } = &third.kind
                else {
                    return Err(
                        "overflow cleanup receipt requires a direct local maximum read".into(),
                    );
                };
                let Some((one_value, 1)) = const_value(fourth, function, &i32_type) else {
                    return Err("overflow cleanup receipt requires the exact i32 one addend".into());
                };
                if initial_constant != i32::MAX as i64
                    || maximum_local != bound_value
                    || function
                        .values
                        .get(maximum_value)
                        .is_none_or(|value| value.ty != i32_type)
                {
                    return Err(
                        "overflow cleanup operand does not use its exact i32::MAX local".into(),
                    );
                }
                (
                    maximum_value,
                    one_value,
                    ResolvedBinaryOp::Add,
                    (i32::MAX as i64, 1),
                )
            }
        };
        if initial_constant != expected_constants.0 {
            return Err("ManagedAny trap-cleanup bound local has the wrong exact constant".into());
        }
        let MirInstructionKind::ManagedAny {
            result: None,
            source: drop_source,
            operation: MirManagedAnyOperation::Drop,
            contract: Some(drop_contract),
        } = &drop.kind
        else {
            return Err("ManagedAny trap must be preceded by its receipted owner Drop".into());
        };
        let expected_drop_id = MirInstructionId::new(format!(
            "inst:managed_any_trap_cleanup_drop:{}",
            receipt.instance.as_str()
        ))
        .map_err(|error| error.to_string())?;
        if drop.id != receipt.drop_instruction
            || drop.id != expected_drop_id
            || drop_source != &receipt.parameter
            || drop_contract.owner != function.owner
            || drop_contract.origin != receipt.drop_origin
            || drop_contract.source != receipt.parameter
            || drop_contract.result.is_some()
            || drop_contract.source_type != receipt.managed_type_argument
            || drop_contract.target_type.is_some()
            || drop_contract.schema_version != MIR_MANAGED_ANY_SCHEMA_VERSION
            || drop_contract.descriptor_version != MIR_MANAGED_ANY_DESCRIPTOR_VERSION
            || drop_contract.expected_kind.is_some()
        {
            return Err(
                "ManagedAny trap-cleanup Drop disagrees with its private Checker receipt".into(),
            );
        }
        let MirInstructionKind::Binary {
            result,
            op,
            left: observed_left,
            right: observed_right,
        } = &trap.kind
        else {
            return Err("ManagedAny trap-cleanup body must end with one arithmetic Binary".into());
        };
        let expected_trap_id =
            MirInstructionId::new(format!("inst:binary:{}", receipt.trap_origin.0))
                .map_err(|error| error.to_string())?;
        if trap.id != receipt.trap_instruction
            || trap.id != expected_trap_id
            || observed_left != left
            || observed_right != right
            || *op != expected_op
            || function
                .values
                .get(left)
                .is_none_or(|value| value.ty != i32_type)
            || function
                .values
                .get(right)
                .is_none_or(|value| value.ty != i32_type)
            || function
                .values
                .get(result)
                .is_none_or(|value| value.ty != i32_type)
            || receipt.trap_result != *result
        {
            return Err(
                "ManagedAny trap operation does not match the sealed exact i32 trap and cleanup order"
                    .into(),
            );
        }
        if !matches!(
            &block.terminator,
            MirTerminator::Return { value: Some(value) } if value == result
        ) {
            return Err(
                "ManagedAny trap-cleanup must return exactly the trapping i32 value".into(),
            );
        }
        Ok(())
    }

    pub(super) fn validate_managed_any_glue(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let expected = MirGlueContract {
            move_out: MirGlueKind::ManagedAnyI32StringV1,
            clone: MirGlueKind::ManagedAnyI32StringV1,
            drop: MirGlueKind::ManagedAnyI32StringV1,
        };
        let descriptor = self.get(ty).ok_or("ManagedAny TypeDesc is missing")?;
        if ty != &managed_any_i32_string_v1_type_id()
            || descriptor.id != *ty
            || descriptor.kind != MirTypeKind::ManagedAnyI32StringV1
            || descriptor.layout != MirLayout::ManagedAnyI32StringV1
            || descriptor.ownership != MirOwnership::Move
            || descriptor.abi != MirAbiClass::OpaqueHandle
            || descriptor.glue != expected
            || !descriptor.needs_clone_glue
            || !descriptor.needs_drop_glue
            || descriptor.session_protocol.is_some()
            || descriptor.drop_plan.is_some()
            || descriptor.variant_drop_plan.is_some()
        {
            return Err(
                "ManagedAny exact descriptor identity/version/ownership/glue is inconsistent"
                    .into(),
            );
        }
        Ok(())
    }

    /// Retarget only Any and concrete tuple products in a program carrying an
    /// exact GetAny Checker action. No other aggregate receives permission.
    pub(crate) fn canonical_managed_any_type(&self, ty: &ResolvedTypeId) -> ResolvedTypeId {
        if self.get(&managed_any_i32_string_v1_type_id()).is_none() {
            return ty.clone();
        }
        match self.get(ty).map(|descriptor| &descriptor.kind) {
            Some(MirTypeKind::DynamicAny) => managed_any_i32_string_v1_type_id(),
            Some(MirTypeKind::List) => {
                // Checker keeps the source spelling `List<Any>` in the
                // resolved type table.  The executable MIR boundary must
                // carry the materialized ManagedAny element identity so the
                // List glue/runtime consumers cannot infer an opaque element
                // kind from a handle alone.  Only the one-level list shape is
                // admitted here; nested or aggregate Any shapes remain
                // fail-closed until they receive an independent receipt.
                let Some(MirLayout::List { element }) =
                    self.get(ty).map(|descriptor| &descriptor.layout)
                else {
                    return ty.clone();
                };
                let mapped = self.canonical_managed_any_type(element);
                if mapped != *element && mapped == managed_any_i32_string_v1_type_id() {
                    // The synthetic descriptor is materialized only for a
                    // Checker-receipted `values()` result.  Refuse to
                    // canonicalize an unrelated List<Any> merely because a
                    // different operation in the same program introduced the
                    // ManagedAny leaf descriptor.
                    let canonical = managed_any_list_i32_string_v1_type_id(ty);
                    if self.get(&canonical).is_some() {
                        return canonical;
                    }
                }
                ty.clone()
            }
            Some(MirTypeKind::Tuple { .. }) => {
                if let Some(MirLayout::Tuple(elements)) =
                    self.get(ty).map(|descriptor| &descriptor.layout)
                {
                    let mapped = elements
                        .iter()
                        .map(|element| self.canonical_managed_any_type(element))
                        .collect::<Vec<_>>();
                    if mapped != *elements {
                        let boolean = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
                        if mapped == vec![boolean, managed_any_i32_string_v1_type_id()] {
                            return ordinary_map_get_any_tuple_i32_string_v1_type_id();
                        }
                        return ResolvedTypeId::synthetic(&format!(
                            "mimi-mir:ManagedAnyTuple:v1:{}",
                            ty.as_str()
                        ));
                    }
                }
                ty.clone()
            }
            _ => ty.clone(),
        }
    }

    pub fn contains_managed_any(&self, ty: &ResolvedTypeId) -> bool {
        fn visit(
            catalog: &MirTypeCatalog,
            ty: &ResolvedTypeId,
            seen: &mut BTreeSet<ResolvedTypeId>,
        ) -> bool {
            if !seen.insert(ty.clone()) {
                return false;
            }
            if ty == &managed_any_i32_string_v1_type_id() {
                return true;
            }
            matches!(catalog.get(ty).map(|descriptor| &descriptor.layout), Some(MirLayout::Tuple(elements))
                if elements.iter().any(|child| visit(catalog, child, seen)))
                || matches!(catalog.get(ty).map(|descriptor| &descriptor.layout), Some(MirLayout::List { element })
                    if visit(catalog, element, seen))
        }
        visit(self, ty, &mut BTreeSet::new())
    }

    /// Validate the exact move-owned list shape returned by ordinary Map
    /// `values()`: one List handle whose element is the canonical
    /// generation-bearing ManagedAny owner.  This is deliberately separate
    /// from the generic Copy-scalar List validators so a backend cannot widen
    /// the ordinary list ABI merely because it sees a List layout.
    pub fn validate_managed_any_list_type(&self, ty: &ResolvedTypeId) -> Result<(), String> {
        let descriptor = self.get(ty).ok_or("ManagedAny List TypeDesc is missing")?;
        let MirLayout::List { element } = &descriptor.layout else {
            return Err("ManagedAny List result has no List layout".into());
        };
        if descriptor.kind != MirTypeKind::List || *element != managed_any_i32_string_v1_type_id() {
            return Err("ManagedAny List result must be List<ManagedAnyI32StringV1>".into());
        }
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            self.validate_list_glue(ty, operation).map_err(|message| {
                format!("ManagedAny List result has invalid {operation:?} glue: {message}")
            })?;
        }
        Ok(())
    }

    /// Detect either source DynamicAny or its canonical managed executable
    /// owner anywhere in a TypeDesc graph. This is intentionally separate
    /// from `contains_managed_any`, whose narrower behavior is already used
    /// by existing tuple-owner analyses. Missing child descriptors fail
    /// closed as unclassifiable; cycles are bounded by the visited-ID set.
    pub fn contains_dynamic_or_managed_any(&self, ty: &ResolvedTypeId) -> bool {
        fn visit(
            catalog: &MirTypeCatalog,
            ty: &ResolvedTypeId,
            seen: &mut BTreeSet<ResolvedTypeId>,
        ) -> bool {
            if !seen.insert(ty.clone()) {
                return false;
            }
            let Some(descriptor) = catalog.get(ty) else {
                return true;
            };
            if matches!(
                &descriptor.kind,
                MirTypeKind::DynamicAny
                    | MirTypeKind::ManagedAnyI32StringV1
                    | MirTypeKind::OrdinaryMapOwnedAnyV2
                    | MirTypeKind::OrdinaryMapOwnedAnyV2Payload
                    | MirTypeKind::OrdinaryMapOwnedAnyV2Values
                    | MirTypeKind::OrdinaryMapOwnedAnyV2ValuesElement
            ) || ty == &managed_any_i32_string_v1_type_id()
                || ty == &ordinary_map_owned_any_v2_type_id()
                || ty == &ordinary_map_owned_any_v2_values_element_type_id()
            {
                return true;
            }
            let contains_fields =
                |fields: &[super::types::MirFieldDesc], seen: &mut BTreeSet<ResolvedTypeId>| {
                    fields.iter().any(|field| visit(catalog, &field.ty, seen))
                };
            let contains_variants =
                |variants: &[super::types::MirVariantDesc], seen: &mut BTreeSet<ResolvedTypeId>| {
                    variants
                        .iter()
                        .any(|variant| contains_fields(&variant.fields, seen))
                };
            let layout_contains_any = match &descriptor.layout {
                MirLayout::ActorState { fields, .. } | MirLayout::Record { fields, .. } => {
                    contains_fields(fields, seen)
                }
                MirLayout::Pointer { target } => target
                    .as_ref()
                    .map_or(true, |child| visit(catalog, child, seen)),
                MirLayout::Tuple(elements) => {
                    elements.iter().any(|child| visit(catalog, child, seen))
                }
                MirLayout::Option { inner, variants } => {
                    visit(catalog, inner, seen) || contains_variants(variants, seen)
                }
                MirLayout::Result {
                    ok,
                    error,
                    variants,
                } => {
                    visit(catalog, ok, seen)
                        || visit(catalog, error, seen)
                        || contains_variants(variants, seen)
                }
                MirLayout::Enum { variants, .. } => contains_variants(variants, seen),
                MirLayout::Array { element, .. }
                | MirLayout::Newtype { inner: element, .. }
                | MirLayout::List { element }
                | MirLayout::Set { element } => visit(catalog, element, seen),
                MirLayout::Unit
                | MirLayout::Scalar
                | MirLayout::Handle
                | MirLayout::MapRoot
                | MirLayout::MapAnyOwnerV1
                | MirLayout::OrdinaryMapOwnerLifecycleI32StringV1
                | MirLayout::OrdinaryMapOwnedGetI32StringV1
                | MirLayout::OrdinaryMapOwnedAnyV2
                | MirLayout::ManagedAnyI32StringV1
                | MirLayout::Opaque => false,
                MirLayout::OrdinaryMapOwnedAnyV2Payload => true,
                MirLayout::OrdinaryMapOwnedAnyV2Values => true,
                MirLayout::OrdinaryMapOwnedAnyV2ValuesElement => true,
            };
            let resolved_children_contain_any = catalog
                .resolved_children
                .get(ty)
                .is_some_and(|children| children.iter().any(|child| visit(catalog, child, seen)));
            let unresolved_child_metadata = !catalog.resolved_children.contains_key(ty)
                && (matches!(
                    &descriptor.kind,
                    MirTypeKind::Function { .. } | MirTypeKind::Ownership(_)
                ) || (descriptor.kind == MirTypeKind::Nominal
                    && matches!(&descriptor.layout, MirLayout::Handle | MirLayout::Opaque)));
            let session_protocol_contains_any = descriptor
                .session_protocol
                .as_ref()
                .is_some_and(|child| visit(catalog, child, seen));
            let drop_plan_contains_any =
                descriptor.drop_plan.as_ref().is_some_and(|plan| {
                    plan.fields
                        .iter()
                        .any(|field| visit(catalog, &field.ty, seen))
                }) || descriptor.variant_drop_plan.as_ref().is_some_and(|plans| {
                    plans.iter().any(|plan| {
                        plan.fields
                            .iter()
                            .any(|field| visit(catalog, &field.ty, seen))
                    })
                });
            layout_contains_any
                || resolved_children_contain_any
                || unresolved_child_metadata
                || session_protocol_contains_any
                || drop_plan_contains_any
        }
        visit(self, ty, &mut BTreeSet::new())
    }

    pub fn validate_ordinary_map_owned_get_any_result_type(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<(), String> {
        let descriptor = self.get(ty).ok_or("GetAny result TypeDesc is missing")?;
        let boolean = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        if ty != &ordinary_map_get_any_tuple_i32_string_v1_type_id()
            || descriptor.kind != (MirTypeKind::Tuple { arity: 2 })
            || descriptor.layout
                != MirLayout::Tuple(vec![boolean, managed_any_i32_string_v1_type_id()])
        {
            return Err(
                "GetAny must return the exact owned (bool, ManagedAny) tuple descriptor".into(),
            );
        }
        self.validate_recursive_tuple_abi(ty)
    }

    /// Validate the independent AnyOwner-v2 GetAny product. Its payload token
    /// has a separate TypeDesc and must never be admitted by the V1
    /// ManagedAny tuple contract above.
    pub fn validate_ordinary_map_owned_any_v2_get_any_result_type(
        &self,
        ty: &ResolvedTypeId,
    ) -> Result<(), String> {
        let descriptor = self
            .get(ty)
            .ok_or("AnyOwner-v2 GetAny result TypeDesc is missing")?;
        let boolean = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        if ty != &ordinary_map_owned_any_v2_get_result_type_id()
            || descriptor.kind != (MirTypeKind::Tuple { arity: 2 })
            || descriptor.layout
                != MirLayout::Tuple(vec![
                    boolean.clone(),
                    ordinary_map_owned_any_v2_payload_type_id(),
                ])
        {
            return Err(
                "AnyOwner-v2 GetAny must return its exact owned (bool, V2 payload token) tuple"
                    .into(),
            );
        }
        self.validate_copy_scalar(&boolean)?;
        for operation in [
            MirGlueOperation::MoveOut,
            MirGlueOperation::Clone,
            MirGlueOperation::Drop,
        ] {
            self.validate_glue(ty, operation)?;
        }
        Ok(())
    }

    pub fn validate_managed_any_contract(
        &self,
        function: &MirFunction,
        source: &MirValueId,
        result: Option<&MirValueId>,
        operation: MirManagedAnyOperation,
        contract: Option<&MirManagedAnyContract>,
    ) -> Result<(), String> {
        let receipt =
            contract.ok_or("ManagedAny operation is missing its exact owner/type receipt")?;
        let source_type = function
            .values
            .get(source)
            .ok_or("ManagedAny source identity is missing")?
            .ty
            .clone();
        let target_type = result
            .map(|result| {
                function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or("ManagedAny result identity is missing")
            })
            .transpose()?;
        if operation != MirManagedAnyOperation::PackI32 {
            self.validate_managed_any_glue(&source_type)?;
            if receipt.checker_conversion.is_some() {
                return Err(
                    "ManagedAny non-pack operation carries a stray Checker conversion".into(),
                );
            }
        }
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let managed_type = managed_any_i32_string_v1_type_id();
        if operation == MirManagedAnyOperation::PackI32 {
            let conversion = receipt
                .checker_conversion
                .as_ref()
                .ok_or("ManagedAny PackI32 has no exact Checker DynamicAnyPack conversion")?;
            let dynamic_target_is_canonical = self
                .get(&conversion.to)
                .is_some_and(|descriptor| descriptor.kind == MirTypeKind::DynamicAny)
                && self.canonical_managed_any_type(&conversion.to) == managed_type;
            if source_type != i32_type
                || conversion.kind != CheckedConversionKind::DynamicAnyPack
                || conversion.from != source_type
                || !dynamic_target_is_canonical
                || target_type.as_ref() != Some(&managed_type)
            {
                return Err(
                    "ManagedAny PackI32 requires exact i32 -> Checker DynamicAny and canonical ManagedAny v1 types"
                        .into(),
                );
            }
        }
        let checker_source_valid = if operation == MirManagedAnyOperation::Println {
            let source = receipt
                .checker_source_type
                .as_ref()
                .ok_or("ManagedAny Println has no Checker DynamicAny source type")?;
            self.validate_managed_any_identity_source_argument(std::slice::from_ref(source))?
                == managed_type
        } else {
            receipt.checker_source_type.is_none()
        };
        let expected_target = match operation {
            MirManagedAnyOperation::Clone => Some(source_type.clone()),
            MirManagedAnyOperation::Drop => None,
            MirManagedAnyOperation::Println => {
                Some(crate::core::ir::primitive_type_id(PrimitiveType::Unit))
            }
            MirManagedAnyOperation::PackI32 => Some(managed_type),
            MirManagedAnyOperation::UnpackI32 => {
                Some(crate::core::ir::primitive_type_id(PrimitiveType::I32))
            }
            MirManagedAnyOperation::UnpackString => {
                Some(crate::core::ir::primitive_type_id(PrimitiveType::String))
            }
        };
        if receipt.schema_version != MIR_MANAGED_ANY_SCHEMA_VERSION
            || receipt.descriptor_version != MIR_MANAGED_ANY_DESCRIPTOR_VERSION
            || receipt.owner != function.owner
            || receipt.origin.0.trim().is_empty()
            || receipt.source != *source
            || receipt.result.as_ref() != result
            || receipt.source_type != source_type
            || receipt.target_type != target_type
            || target_type != expected_target
            || (operation == MirManagedAnyOperation::PackI32)
                != receipt.checker_conversion.is_some()
            || !checker_source_valid
            || (operation == MirManagedAnyOperation::Println)
                != receipt.checker_source_type.is_some()
            || receipt.expected_kind != operation.expected_kind()
            || result.is_some_and(|result| result == source)
        {
            return Err(
                "ManagedAny operation disagrees with exact owner/value/kind/version/borrow receipt"
                    .into(),
            );
        }
        let seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .ok_or("ManagedAny has no private Checker authority")?;
        let matches = seal.instructions.iter().filter(|(_, origin, instruction)| *origin == receipt.origin
            && matches!(&instruction.kind, MirInstructionKind::ManagedAny { source: actual_source,
                result: actual_result, operation: actual_operation, contract: Some(actual_contract) }
                if actual_source == source && actual_result.as_ref() == result
                    && *actual_operation == operation && actual_contract == receipt)).count();
        if matches != 1 {
            return Err(
                "ManagedAny operation has no unique sealed Checker conversion/owner receipt".into(),
            );
        }
        Ok(())
    }
}

pub(super) fn validate_managed_any_receipts(function: &MirFunction) -> Vec<MirValidationError> {
    let mut errors = Vec::new();
    let owner_type = managed_any_i32_string_v1_type_id();
    let owner_values = function
        .values
        .values()
        .filter(|value| {
            value.ty == owner_type
                || value.ty == ordinary_map_get_any_tuple_i32_string_v1_type_id()
                || function
                    .ownership
                    .checker_managed_any
                    .as_ref()
                    .is_some_and(|seal| seal.managed_values.contains(&value.id))
        })
        .map(|value| value.id.clone())
        .collect::<BTreeSet<_>>();
    let uses_owner = |kind: &MirInstructionKind| {
        instruction_value_ids(kind)
            .iter()
            .any(|id| owner_values.contains(id))
    };
    let observed = function
        .blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .map(|instruction| (block.id.clone(), instruction.clone()))
        })
        .collect::<Vec<_>>();
    let has_managed_any_marker = !owner_values.is_empty()
        || function.ownership.checker_managed_any.is_some()
        || observed.iter().any(|(_, instruction)| {
            matches!(
                &instruction.kind,
                MirInstructionKind::ManagedAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack { .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { .. }
            )
        });
    if !has_managed_any_marker {
        return errors;
    }
    let mut error = |subject: String, message: &str| {
        errors.push(MirValidationError {
            subject,
            message: message.into(),
        })
    };
    let Some(seal) = &function.ownership.checker_managed_any else {
        error(
            function.owner.0.clone(),
            "ManagedAny TypeDesc/value presence cannot authorize an owner: private Checker seal is missing",
        );
        return errors;
    };
    if seal.owner != function.owner
        || seal.parameters != function.parameters
        || seal.block_parameters
            != function
                .blocks
                .values()
                .map(|block| (block.id.clone(), block.parameters.clone()))
                .collect::<Vec<_>>()
        || seal.parameter_permissions != function.parameter_permissions
        || seal.result != function.result
        || seal.source_cfg != function.ownership.checker_managed_any_cfg_receipt
        || (function.blocks.len() > 1 && seal.source_cfg.is_none())
        || seal.value_types
            != function
                .values
                .iter()
                .map(|(id, value)| (id.clone(), value.ty.clone()))
                .collect()
    {
        error(
            function.owner.0.clone(),
            "ManagedAny value/parameter/return identities disagree with the private Checker seal",
        );
    }
    let expected_body = seal
        .instructions
        .iter()
        .map(|(block, _, instruction)| (block.clone(), instruction.clone()))
        .collect::<Vec<_>>();
    if observed != expected_body {
        error(
            function.owner.0.clone(),
            "MIR block/instruction/origin sequence disagrees with the complete private Checker body seal",
        );
    }
    let mut seen = BTreeSet::<MirInstructionId>::new();
    for (_, origin, expected) in &seal.instructions {
        if origin.0.trim().is_empty() || !seen.insert(expected.id.clone()) {
            error(
                expected.id.to_string(),
                "ManagedAny body seal has an empty origin or duplicate instruction identity",
            );
            continue;
        }
        // The full body snapshot binds all lowered instructions. This
        // profile-specific check additionally ensures owner-bearing operations
        // still fit the narrow typed ManagedAny/tuple/call vocabulary.
        if !uses_owner(&expected.kind)
            && !matches!(
                &expected.kind,
                MirInstructionKind::ManagedAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                    | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
            )
        {
            continue;
        }
        let permitted = match &expected.kind {
            MirInstructionKind::ManagedAny {
                result,
                source,
                operation,
                contract,
            } => contract.as_ref().is_some_and(|receipt| {
                let (source_type, target_type, conversion_valid) =
                    if *operation == MirManagedAnyOperation::PackI32 {
                        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
                        (
                            i32_type.clone(),
                            Some(owner_type.clone()),
                            receipt
                                .checker_conversion
                                .as_ref()
                                .is_some_and(|conversion| {
                                    conversion.kind == CheckedConversionKind::DynamicAnyPack
                                        && conversion.from == i32_type
                                        && conversion.to != owner_type
                                }),
                        )
                    } else {
                        (
                            owner_type.clone(),
                            match operation {
                                MirManagedAnyOperation::Clone => Some(owner_type.clone()),
                                MirManagedAnyOperation::Drop => None,
                                MirManagedAnyOperation::Println => {
                                    Some(crate::core::ir::primitive_type_id(PrimitiveType::Unit))
                                }
                                MirManagedAnyOperation::PackI32 => unreachable!(),
                                MirManagedAnyOperation::UnpackI32 => {
                                    Some(crate::core::ir::primitive_type_id(PrimitiveType::I32))
                                }
                                MirManagedAnyOperation::UnpackString => {
                                    Some(crate::core::ir::primitive_type_id(PrimitiveType::String))
                                }
                            },
                            receipt.checker_conversion.is_none()
                                && ((*operation == MirManagedAnyOperation::Println)
                                    == receipt.checker_source_type.is_some()),
                        )
                    };
                receipt.owner == function.owner
                    && receipt.origin == *origin
                    && receipt.source == *source
                    && receipt.result == *result
                    && receipt.source_type == source_type
                    && receipt.target_type == target_type
                    && receipt.schema_version == MIR_MANAGED_ANY_SCHEMA_VERSION
                    && receipt.descriptor_version == MIR_MANAGED_ANY_DESCRIPTOR_VERSION
                    && receipt.expected_kind == operation.expected_kind()
                    && conversion_valid
            }),
            MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
            | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. } => true,
            MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, .. } => {
                has_checker_ordinary_map_owned_any_v2_action(function, expected)
                    && has_checker_ordinary_map_owned_any_v2_get_any_result(
                        function,
                        &expected.id,
                        result,
                    )
                    && function.values.get(result).is_some_and(|value| {
                        value.ty == ordinary_map_owned_any_v2_get_result_type_id()
                    })
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, .. } => {
                has_checker_ordinary_map_owned_any_v2_action(function, expected)
                    && has_checker_ordinary_map_owned_any_v2_values_result(
                        function,
                        &expected.id,
                        result,
                    )
                    && function
                        .values
                        .get(result)
                        .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                result,
                pair,
                projection,
            } => {
                has_checker_ordinary_map_owned_any_v2_unpack(
                    function,
                    &expected.id,
                    result,
                    pair,
                    projection,
                ) && function
                    .values
                    .get(pair)
                    .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_get_result_type_id())
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                result,
                values,
                index,
                projection,
            } => {
                has_checker_ordinary_map_owned_any_v2_values_unpack(
                    function,
                    &expected.id,
                    result,
                    values,
                    index,
                    projection,
                ) && function
                    .values
                    .get(values)
                    .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
            }
            MirInstructionKind::OrdinaryMapOwnedAnyV2New { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Set { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Size { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { .. }
            | MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { .. } => {
                has_checker_ordinary_map_owned_any_v2_action(function, expected)
            }
            MirInstructionKind::ListOp {
                operation: super::MirListOperation::Len,
                list,
                argument: None,
                ..
            } => seal.managed_values.contains(list),
            MirInstructionKind::Move { source, result }
            | MirInstructionKind::Clone { source, result } => {
                let moves_v2_map = [source, result].iter().any(|value| {
                    function.values.get(*value).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                    })
                });
                !moves_v2_map || has_checker_ordinary_map_owned_any_v2_action(function, expected)
            }
            MirInstructionKind::MoveProject {
                base, projection, ..
            } => {
                seal.managed_values.contains(base)
                    && matches!(projection, MirProjection::Field(_))
                    && has_checker_ordinary_map_owned_any_v2_unpack_lineage(function, base)
            }
            MirInstructionKind::Drop { value } => !function
                .values
                .get(value)
                .is_some_and(|value| value.ty == super::types::ordinary_map_owned_any_v2_type_id()),
            MirInstructionKind::Construct {
                kind: MirAggregateKind::Tuple,
                ..
            } => true,
            MirInstructionKind::Project {
                base,
                projection: MirProjection::Tuple(_),
                ..
            } => {
                seal.managed_values.contains(base)
                    && (function.values.get(base).is_none_or(|value| {
                        value.ty != ordinary_map_owned_any_v2_get_result_type_id()
                    }) || matches!(
                        &expected.kind,
                        MirInstructionKind::Project {
                            base: project_base,
                            result,
                            projection: MirProjection::Tuple(0),
                            ..
                        } if project_base == base
                            && function.values.get(result).is_some_and(|value| {
                                value.ty == crate::core::ir::primitive_type_id(PrimitiveType::Bool)
                            })
                    ))
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => managed_any_string_comparison_owner(
                function,
                result,
                *op,
                left,
                right,
                &owner_values,
            )
            .is_some(),
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(_),
                result,
                arguments,
                ..
            }
            | MirInstructionKind::ClosureInvoke {
                result, arguments, ..
            } => {
                !arguments.iter().any(|argument| {
                    function.values.get(argument).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                    })
                }) && !result.as_ref().is_some_and(|result| {
                    function.values.get(result).is_some_and(|value| {
                        value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                    })
                })
            }
            _ => false,
        };
        if !permitted {
            error(
                expected.id.to_string(),
                "ManagedAny operation is outside its typed owner/tuple/call contract",
            );
        }
    }
    let actual_terminators = function
        .blocks
        .values()
        .map(|block| (block.id.clone(), block.terminator.clone()))
        .collect::<Vec<_>>();
    if seal.terminators != actual_terminators {
        error(
            function.owner.0.clone(),
            "MIR terminator/block sequence disagrees with the complete private Checker body seal",
        );
    }
    errors
}

pub(super) fn instruction_value_ids(kind: &MirInstructionKind) -> Vec<MirValueId> {
    let mut ids = super::mir_instruction_value_uses(kind);
    ids.extend(instruction_result_values(kind).into_iter().cloned());
    ids
}

fn instruction_result_values(kind: &MirInstructionKind) -> Vec<&MirValueId> {
    match kind {
        MirInstructionKind::Actor { operation } => operation.result().into_iter().collect(),
        MirInstructionKind::ClosureBind { function_value, .. } => vec![function_value],
        MirInstructionKind::ClosureInvoke { result, .. }
        | MirInstructionKind::ManagedAny { result, .. }
        | MirInstructionKind::Call { result, .. } => result.iter().collect(),
        MirInstructionKind::MapRootNew { result }
        | MirInstructionKind::MapRootSet { result, .. }
        | MirInstructionKind::MapRootRemove { result, .. }
        | MirInstructionKind::MapRootSize { result, .. }
        | MirInstructionKind::MapRootContains { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleNew { result }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleSet { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleRemove { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleSize { result, .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleContains { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2New { result }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Set { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1New { result }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Set { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Remove { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Size { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Contains { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetI32 { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetString { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { result, .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { result, .. }
        | MirInstructionKind::Const { result, .. }
        | MirInstructionKind::Load { result, .. }
        | MirInstructionKind::Copy { result, .. }
        | MirInstructionKind::Move { result, .. }
        | MirInstructionKind::Clone { result, .. }
        | MirInstructionKind::Borrow { result, .. }
        | MirInstructionKind::Project { result, .. }
        | MirInstructionKind::MoveProject { result, .. }
        | MirInstructionKind::MoveProjectDrop { result, .. }
        | MirInstructionKind::VariantProject { result, .. }
        | MirInstructionKind::VariantProjectOr { result, .. }
        | MirInstructionKind::VariantProjectMove { result, .. }
        | MirInstructionKind::Construct { result, .. }
        | MirInstructionKind::ConstructList { result, .. }
        | MirInstructionKind::ListOp { result, .. }
        | MirInstructionKind::VariantPredicate { result, .. }
        | MirInstructionKind::ConstructSet { result, .. }
        | MirInstructionKind::SetOp { result, .. }
        | MirInstructionKind::ConstructVariant { result, .. }
        | MirInstructionKind::ConstructVariantMove { result, .. }
        | MirInstructionKind::UpdateRecord { result, .. }
        | MirInstructionKind::Binary { result, .. }
        | MirInstructionKind::Unary { result, .. }
        | MirInstructionKind::FlowTransition { result, .. }
        | MirInstructionKind::BuiltinCall { result, .. }
        | MirInstructionKind::SessionCall { result, .. }
        | MirInstructionKind::Convert { result, .. } => vec![result],
        MirInstructionKind::SessionPairBind { lo, hi, .. } => vec![lo, hi],
        MirInstructionKind::MapRootDrop { .. }
        | MirInstructionKind::OrdinaryMapOwnerLifecycleDrop { .. }
        | MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { .. }
        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Drop { .. }
        | MirInstructionKind::Drop { .. }
        | MirInstructionKind::EndBorrow { .. }
        | MirInstructionKind::Nop => Vec::new(),
    }
}

/// Recognize the only ordinary Binary read admitted for a ManagedAny owner:
/// exact equality/inequality against an owned Mimi String, producing Bool.
/// The owner is borrowed and remains live; all other Binary use of an owner
/// remains outside the profile.
fn managed_any_string_comparison_owner<'a>(
    function: &'a MirFunction,
    result: &MirValueId,
    operation: ResolvedBinaryOp,
    left: &'a MirValueId,
    right: &'a MirValueId,
    owner_values: &BTreeSet<MirValueId>,
) -> Option<&'a MirValueId> {
    if !matches!(
        operation,
        ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual
    ) {
        return None;
    }
    let owner_type = managed_any_i32_string_v1_type_id();
    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
    let is_owner = |value: &MirValueId| {
        owner_values.contains(value)
            && function
                .values
                .get(value)
                .is_some_and(|descriptor| descriptor.ty == owner_type)
    };
    if function
        .values
        .get(result)
        .is_none_or(|descriptor| descriptor.ty != bool_type)
    {
        return None;
    }
    match (is_owner(left), is_owner(right)) {
        (true, false)
            if function
                .values
                .get(right)
                .is_some_and(|descriptor| descriptor.ty == string_type) =>
        {
            Some(left)
        }
        (false, true)
            if function
                .values
                .get(left)
                .is_some_and(|descriptor| descriptor.ty == string_type) =>
        {
            Some(right)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedAnyCfgFlowState {
    owners: BTreeSet<MirValueId>,
    unpacked_strings: BTreeSet<MirValueId>,
}

#[derive(Debug, Clone)]
struct ManagedAnyCfgSuccessor {
    target: MirBlockId,
    arguments: Vec<MirValueId>,
}

fn require_managed_any_cfg_owner(
    live: &BTreeSet<MirValueId>,
    managed_values: &BTreeSet<MirValueId>,
    id: &MirValueId,
) -> Result<(), String> {
    if managed_values.contains(id) && !live.contains(id) {
        Err(format!(
            "ManagedAny owner '{}' is used after transfer, drop, or before creation",
            id
        ))
    } else {
        Ok(())
    }
}

fn create_managed_any_cfg_owner(
    live: &mut BTreeSet<MirValueId>,
    managed_values: &BTreeSet<MirValueId>,
    id: &MirValueId,
) -> Result<(), String> {
    // MIR values are static SSA identities. A loop may execute the same
    // definition again after its previous dynamic owner was settled, so
    // creation is invalid only when that identity is still live on this path.
    if managed_values.contains(id) && !live.insert(id.clone()) {
        return Err(format!(
            "ManagedAny owner '{}' is recreated while its prior path value is still live",
            id
        ));
    }
    Ok(())
}

/// Bind one owner-v2 MIR operation to the complete Checker action and CFG
/// census before the path ledger is allowed to create, borrow, or settle a
/// Map root. The generic ManagedAny body seal commits the MIR itself; this
/// second receipt is what grants the operation its Map-specific meaning.
fn has_checker_ordinary_map_owned_any_v2_action(
    function: &MirFunction,
    instruction: &MirInstruction,
) -> bool {
    use crate::core::{
        OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
        OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
        OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion,
    };

    let (kind, root_operand, result_operand, key_operand, value_operand) = match &instruction.kind {
        MirInstructionKind::Clone { result, source } => {
            (ActionKind::Clone, source, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2New { result } => {
            (ActionKind::New, result, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
            result,
            source,
            key,
            value,
        } => (
            ActionKind::Set,
            source,
            Some(result),
            Some(key),
            Some(value),
        ),
        MirInstructionKind::OrdinaryMapOwnedAnyV2Remove {
            result,
            source,
            key,
        } => (ActionKind::Remove, source, Some(result), Some(key), None),
        MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root } => {
            (ActionKind::Size, root, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, root, key } => {
            (ActionKind::HasKey, root, Some(result), Some(key), None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny { result, root, key } => {
            (ActionKind::GetAny, root, Some(result), Some(key), None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, root } => {
            (ActionKind::Values, root, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, root } => {
            (ActionKind::IsEmpty, root, Some(result), None, None)
        }
        MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
            (ActionKind::Drop, root, None, None, None)
        }
        _ => return false,
    };

    let matching_actions = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| action.instruction == instruction.id)
        .collect::<Vec<_>>();
    let [action] = matching_actions.as_slice() else {
        return false;
    };
    if action.profile_version != ProfileVersion::V2 || action.kind != kind {
        return false;
    }

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    if cfg.actions.len() != function.ownership.checker_ordinary_map_owned_any_v2.len()
        || cfg.actions.len() != cfg.attempts.len()
    {
        return false;
    }
    let matching_cfg_actions = cfg
        .actions
        .iter()
        .filter(|candidate| candidate.point == action.point && candidate.kind == kind)
        .collect::<Vec<_>>();
    let [cfg_action] = matching_cfg_actions.as_slice() else {
        return false;
    };

    let local = MirValueId::new(format!("local:{}", cfg_action.local.0 .0)).ok();
    let source_local = cfg_action
        .source_local
        .as_ref()
        .and_then(|local| MirValueId::new(format!("local:{}", local.0 .0)).ok());
    let key = cfg_action
        .key_local
        .as_ref()
        .and_then(|local| MirValueId::new(format!("local:{}", local.0 .0)).ok())
        .or_else(|| {
            cfg_action
                .key
                .as_ref()
                .and_then(|node| MirValueId::new(format!("expr:{}", node.0)).ok())
        });
    let value = cfg_action
        .value
        .as_ref()
        .and_then(|node| MirValueId::new(format!("expr:{}", node.0)).ok());
    let expected_result = match kind {
        ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove => local.clone(),
        ActionKind::Drop => None,
        ActionKind::Size
        | ActionKind::HasKey
        | ActionKind::IsEmpty
        | ActionKind::GetAny
        | ActionKind::Values => MirValueId::new(format!("expr:{}", action.point.0)).ok(),
    };
    let canonical_root_identity = format!("ordinary-map-owned-any-v2:{}", cfg_action.local.0 .0);
    let expected_root_operand = source_local.as_ref().unwrap_or(&action.local);
    let source_matches = match (&cfg_action.source_local, &cfg_action.source) {
        (Some(source_local), Some(source)) => {
            source.0 .0 == format!("ordinary-map-owned-any-v2:{}", source_local.0 .0)
        }
        (None, None) => true,
        _ => false,
    };
    if cfg_action.profile_version != ProfileVersion::V2
        || local.as_ref() != Some(&action.local)
        || action.root != canonical_root_identity
        || cfg_action.root.0 .0 != action.root
        || action.key_node != cfg_action.key
        || action.key_type != cfg_action.key_type
        || action.key_local != cfg_action.key_local
        || action.key_provenance != cfg_action.key_provenance
        || action.key != key
        || action.value != value
        || action.value_type != cfg_action.value_type
        || action.payload_clone != cfg_action.payload_clone
        || action.source_local != source_local
        || action.source != cfg_action.source.as_ref().map(|source| source.0 .0.clone())
        || !source_matches
        || action.source_node != cfg_action.source_node
        || action.source_type != cfg_action.source_type
        || action.result_source_type != cfg_action.result_type
        || action.result_local != cfg_action.result_local
        || action.result != expected_result
        || root_operand != expected_root_operand
        || result_operand != action.result.as_ref()
        || key_operand != action.key.as_ref()
        || value_operand != action.value.as_ref()
    {
        return false;
    }

    let attempt_kind = match kind {
        ActionKind::New => AttemptKind::New,
        ActionKind::Clone => AttemptKind::Clone,
        ActionKind::Set => AttemptKind::Set,
        ActionKind::Remove => AttemptKind::Remove,
        ActionKind::Size => AttemptKind::Size,
        ActionKind::HasKey => AttemptKind::HasKey,
        ActionKind::IsEmpty => AttemptKind::IsEmpty,
        ActionKind::GetAny => AttemptKind::GetAny,
        ActionKind::Values => AttemptKind::Values,
        ActionKind::Drop => AttemptKind::Drop,
    };
    let matching_attempts = cfg
        .attempts
        .iter()
        .filter(|attempt| attempt.point == action.point && attempt.kind == attempt_kind)
        .collect::<Vec<_>>();
    let [attempt] = matching_attempts.as_slice() else {
        return false;
    };
    let expected_receiver = match kind {
        ActionKind::New => None,
        ActionKind::Clone | ActionKind::Set | ActionKind::Remove => {
            cfg_action.source_local.as_ref()
        }
        ActionKind::Size
        | ActionKind::HasKey
        | ActionKind::IsEmpty
        | ActionKind::GetAny
        | ActionKind::Values
        | ActionKind::Drop => Some(&cfg_action.local),
    };
    let expected_attempt_result = match kind {
        ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove => {
            Some(&cfg_action.local)
        }
        ActionKind::GetAny | ActionKind::Values => cfg_action.result_local.as_ref(),
        ActionKind::Size | ActionKind::HasKey | ActionKind::IsEmpty | ActionKind::Drop => None,
    };
    let arguments_match = match kind {
        ActionKind::New => attempt.arguments.is_empty(),
        ActionKind::Clone => {
            attempt.arguments.len() == 1
                && attempt.arguments.first() == cfg_action.source_node.as_ref()
        }
        ActionKind::Set => {
            attempt.arguments.len() == 3
                && attempt.arguments.get(1) == cfg_action.key.as_ref()
                && attempt.arguments.get(2) == cfg_action.value.as_ref()
        }
        ActionKind::Remove | ActionKind::HasKey => {
            attempt.arguments.len() == 2 && attempt.arguments.get(1) == cfg_action.key.as_ref()
        }
        ActionKind::Size | ActionKind::IsEmpty => attempt.arguments.len() == 1,
        ActionKind::GetAny => {
            attempt.arguments.len() == 2
                && attempt.arguments.first() == cfg_action.source_node.as_ref()
                && attempt.arguments.get(1) == cfg_action.key.as_ref()
        }
        ActionKind::Values => {
            attempt.arguments.len() == 1
                && attempt.arguments.first() == cfg_action.source_node.as_ref()
        }
        ActionKind::Drop => attempt.arguments.as_slice() == [cfg_action.local.0.clone()],
    };
    if attempt.receiver_local.as_ref() != expected_receiver
        || attempt.result_local.as_ref() != expected_attempt_result
        || !arguments_match
    {
        return false;
    }

    let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
    if key_operand.is_some_and(|key| {
        action.key_type.as_ref() != Some(&string_type)
            || function
                .values
                .get(key)
                .is_none_or(|value| value.ty != string_type)
    }) || value_operand.is_some_and(|payload| {
        action.value_type.as_ref().is_none_or(|expected| {
            function
                .values
                .get(payload)
                .is_none_or(|actual| &actual.ty != expected)
        })
    }) {
        return false;
    }

    let Some(root_value) = function.values.get(root_operand) else {
        return false;
    };
    if root_value.ty != super::types::ordinary_map_owned_any_v2_type_id() {
        return false;
    }
    if matches!(
        kind,
        ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove
    ) && result_operand.is_none_or(|result| {
        function
            .values
            .get(result)
            .is_none_or(|value| value.ty != super::types::ordinary_map_owned_any_v2_type_id())
    }) {
        return false;
    }
    match (
        kind,
        result_operand.and_then(|result| function.values.get(result)),
    ) {
        (ActionKind::Size, Some(value)) => {
            value.ty == crate::core::ir::primitive_type_id(PrimitiveType::I32)
        }
        (ActionKind::HasKey | ActionKind::IsEmpty, Some(value)) => {
            value.ty == crate::core::ir::primitive_type_id(PrimitiveType::Bool)
        }
        (ActionKind::GetAny, Some(value)) => {
            value.ty == ordinary_map_owned_any_v2_get_result_type_id()
        }
        (ActionKind::Values, Some(value)) => value.ty == ordinary_map_owned_any_v2_values_type_id(),
        (ActionKind::Drop, None)
        | (ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove, Some(_)) => {
            true
        }
        _ => false,
    }
}

fn has_checker_ordinary_map_owned_any_v2_get_any_result(
    function: &MirFunction,
    instruction: &MirInstructionId,
    result: &MirValueId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let matching_actions = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| action.instruction == *instruction)
        .collect::<Vec<_>>();
    let [action] = matching_actions.as_slice() else {
        return false;
    };
    if action.profile_version != ProfileVersion::V2
        || action.kind != ActionKind::GetAny
        || action.result.as_ref() != Some(result)
        || action.result_source_type.is_none()
    {
        return false;
    }
    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    let matching_cfg_actions = cfg
        .actions
        .iter()
        .filter(|candidate| candidate.point == action.point && candidate.kind == ActionKind::GetAny)
        .collect::<Vec<_>>();
    let [cfg_action] = matching_cfg_actions.as_slice() else {
        return false;
    };
    let cfg_local_value = MirValueId::new(format!("local:{}", cfg_action.local.0 .0)).ok();
    let cfg_key_value = cfg_action
        .key_local
        .as_ref()
        .and_then(|local| MirValueId::new(format!("local:{}", local.0 .0)).ok())
        .or_else(|| {
            cfg_action
                .key
                .as_ref()
                .and_then(|key| MirValueId::new(format!("expr:{}", key.0)).ok())
        });
    let action_matches = cfg_action.profile_version == ProfileVersion::V2
        && cfg_local_value.as_ref() == Some(&action.local)
        && cfg_action.root.0 .0 == action.root
        && cfg_action.source_local.as_ref() == Some(&cfg_action.local)
        && cfg_action.source.as_ref().map(|source| &source.0 .0) == Some(&action.root)
        && cfg_action.source_node == action.source_node
        && cfg_action.source_type == action.source_type
        && cfg_action.key.as_ref() == action.key_node.as_ref()
        && cfg_action.key_local == action.key_local
        && cfg_key_value.as_ref() == action.key.as_ref()
        && cfg_action.key_type == action.key_type
        && cfg_action.result_type == action.result_source_type
        && cfg_action.result_local == action.result_local;
    if !action_matches {
        return false;
    }
    let matching_results = cfg
        .result_receipts
        .iter()
        .filter(|receipt| receipt.operation == action.point && receipt.kind == ActionKind::GetAny)
        .collect::<Vec<_>>();
    let [receipt] = matching_results.as_slice() else {
        return false;
    };
    receipt.profile_version == ProfileVersion::V2
        && receipt.result == action.point
        && action
            .result_source_type
            .as_ref()
            .is_some_and(|ty| receipt.result_type == *ty)
        && Some(&receipt.binding) == action.result_local.as_ref()
        && receipt.source_local == cfg_action.local
        && receipt.source == cfg_action.root
        && cfg_action.source_node.as_ref() == Some(&receipt.source_node)
        && cfg_action.source_type.as_ref() == Some(&receipt.source_type)
        && receipt.key == cfg_action.key
        && receipt.key_type == cfg_action.key_type
        && receipt.key_local == cfg_action.key_local
        && receipt.settlement == Settlement::ExplicitDrop
        && receipt.settlement_point.is_some()
}

fn has_checker_ordinary_map_owned_any_v2_unpack(
    function: &MirFunction,
    instruction: &MirInstructionId,
    result: &MirValueId,
    pair: &MirValueId,
    projection: &crate::core::NodeId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    let matches = cfg
        .unpack_receipts
        .iter()
        .filter(|receipt| {
            receipt.profile_version == ProfileVersion::V2
                && receipt.conversion_kind
                    == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                && (receipt.conversion_to == crate::core::ir::primitive_type_id(PrimitiveType::I32)
                    || receipt.conversion_to
                        == crate::core::ir::primitive_type_id(PrimitiveType::String)
                    || function
                        .ownership
                        .checker_ordinary_map_owned_any_v2
                        .iter()
                        .any(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                                && action.value_type.as_ref() == Some(&receipt.conversion_to)
                        }))
                && receipt.settlement == Settlement::ExplicitDrop
                && receipt.projection == *projection
                && receipt.pair_binding.0 .0
                    == pair.as_str().strip_prefix("local:").unwrap_or_default()
                && receipt.cast.0 == result.as_str().strip_prefix("expr:").unwrap_or_default()
                && instruction.as_str()
                    == format!("inst:ordinary_map_owned_any_v2_unpack:{}", receipt.cast.0)
                && receipt.operation.0.trim() != ""
                && receipt.pair_binding_point.0.trim() != ""
                && receipt.unpack_binding_point.0.trim() != ""
                && receipt.settlement_point.0.trim() != ""
        })
        .collect::<Vec<_>>();
    let [receipt] = matches.as_slice() else {
        return false;
    };

    let action_matches = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| {
            action.point == receipt.operation
                && action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                && action.result_local.as_ref() == Some(&receipt.pair_binding)
                && action.result.is_some()
        })
        .collect::<Vec<_>>();
    let [action] = action_matches.as_slice() else {
        return false;
    };
    let get_any_instruction = action.instruction.clone();
    let get_any_result = action.result.as_ref().expect("checked GetAny result");
    has_checker_ordinary_map_owned_any_v2_get_any_result(
        function,
        &get_any_instruction,
        get_any_result,
    ) && function
        .values
        .get(pair)
        .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_get_result_type_id())
        && function
            .values
            .get(result)
            .is_some_and(|value| value.ty == receipt.conversion_to)
}

fn has_checker_ordinary_map_owned_any_v2_values_unpack(
    function: &MirFunction,
    instruction: &MirInstructionId,
    result: &MirValueId,
    values: &MirValueId,
    index: &MirValueId,
    projection: &crate::core::NodeId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    let candidates = cfg
        .values_unpack_receipts
        .iter()
        .filter(|receipt| {
            let expected_index = if let Some(local) = &receipt.index_local {
                MirValueId::new(format!("local:{}", local.0 .0)).ok()
            } else {
                MirValueId::new(format!("expr:{}", receipt.index_expression.0)).ok()
            };
            receipt.profile_version == ProfileVersion::V2
                && receipt.conversion_kind
                    == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                && receipt.settlement == Settlement::ExplicitDrop
                && receipt.cast.0 == result.as_str().strip_prefix("expr:").unwrap_or_default()
                && receipt.snapshot_binding.0 .0
                    == values.as_str().strip_prefix("local:").unwrap_or_default()
                && expected_index.as_ref() == Some(index)
                && receipt.index_projection == *projection
                && instruction.as_str()
                    == format!(
                        "inst:ordinary_map_owned_any_v2_values_unpack:{}",
                        receipt.cast.0
                    )
                && !receipt.payload_selection.is_empty()
                && function
                    .values
                    .get(values)
                    .is_some_and(|value| value.ty == ordinary_map_owned_any_v2_values_type_id())
                && function
                    .values
                    .get(index)
                    .is_some_and(|value| value.ty == receipt.index_type)
                && function
                    .values
                    .get(result)
                    .is_some_and(|value| value.ty == receipt.conversion_to)
        })
        .collect::<Vec<_>>();
    let [receipt] = candidates.as_slice() else {
        return false;
    };
    let values_results = cfg
        .result_receipts
        .iter()
        .filter(|candidate| {
            candidate.operation == receipt.operation
                && candidate.kind == ActionKind::Values
                && candidate.binding == receipt.snapshot_binding
                && candidate.binding_point == receipt.snapshot_binding_point
                && candidate.result_type == receipt.snapshot_type
                && candidate.source_local == receipt.source_local
                && candidate.source == receipt.source
                && candidate.source_node == receipt.source_node
                && candidate.source_type == receipt.source_type
                && candidate.settlement == Settlement::ExplicitDrop
                && candidate.settlement_point.as_ref() == Some(&receipt.settlement_point)
        })
        .count();
    if values_results != 1 {
        return false;
    }

    let mut current = receipt.source_local.clone();
    let mut seen = BTreeSet::new();
    let mut lineage = Vec::new();
    loop {
        if !seen.insert(current.clone()) {
            return false;
        }
        let actions = cfg
            .actions
            .iter()
            .filter(|action| {
                action.local == current
                    && matches!(
                        action.kind,
                        ActionKind::New | ActionKind::Clone | ActionKind::Set | ActionKind::Remove
                    )
            })
            .collect::<Vec<_>>();
        let [action] = actions.as_slice() else {
            return false;
        };
        match action.kind {
            ActionKind::New => return false,
            ActionKind::Set => {
                lineage.push(action.point.clone());
                if action.payload_clone
                    && action.value_type.as_ref() == Some(&receipt.conversion_to)
                {
                    return lineage == receipt.payload_selection;
                }
                let Some(source) = action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            ActionKind::Remove => {
                lineage.push(action.point.clone());
                let Some(source) = action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            ActionKind::Clone => {
                lineage.push(action.point.clone());
                let Some(source) = action.source_local.as_ref() else {
                    return false;
                };
                current = source.clone();
            }
            _ => unreachable!("root action filter is exhaustive"),
        }
    }
}

/// Recognize a value produced by an exact V2 GetAny or Values unpack,
/// optionally moved through same-type MIR locals. This lets the body receipt
/// validator admit a candidate consuming field projection while keeping
/// unrelated ManagedAny values outside its vocabulary. Type/layout/glue
/// validity is checked by the catalog's ManagedAny flow validator before any
/// consumer executes or emits the MIR.
fn has_checker_ordinary_map_owned_any_v2_unpack_lineage(
    function: &MirFunction,
    value: &MirValueId,
) -> bool {
    let instructions = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    let mut unpacked_values = instructions
        .iter()
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                result,
                pair,
                projection,
            } => has_checker_ordinary_map_owned_any_v2_unpack(
                function,
                &instruction.id,
                result,
                pair,
                projection,
            )
            .then(|| result.clone()),
            MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                result,
                values,
                index,
                projection,
            } => has_checker_ordinary_map_owned_any_v2_values_unpack(
                function,
                &instruction.id,
                result,
                values,
                index,
                projection,
            )
            .then(|| result.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();

    loop {
        let mut changed = false;
        for instruction in &instructions {
            let (source, result) = match &instruction.kind {
                MirInstructionKind::Move { source, result }
                | MirInstructionKind::Clone { source, result } => (source, result),
                _ => continue,
            };
            let same_type = function
                .values
                .get(source)
                .zip(function.values.get(result))
                .is_some_and(|(source, result)| source.ty == result.ty);
            if same_type && unpacked_values.contains(source) {
                changed |= unpacked_values.insert(result.clone());
            }
        }
        if !changed {
            break;
        }
    }

    unpacked_values.contains(value)
}

fn has_checker_ordinary_map_owned_any_v2_values_result(
    function: &MirFunction,
    instruction: &MirInstructionId,
    result: &MirValueId,
) -> bool {
    use crate::core::OrdinaryMapOwnedAnyV2ActionKind as ActionKind;
    use crate::core::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;
    use crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind as Settlement;

    let matching_actions = function
        .ownership
        .checker_ordinary_map_owned_any_v2
        .iter()
        .filter(|action| action.instruction == *instruction)
        .collect::<Vec<_>>();
    let [action] = matching_actions.as_slice() else {
        return false;
    };
    if action.profile_version != ProfileVersion::V2
        || action.kind != ActionKind::Values
        || action.result.as_ref() != Some(result)
        || action.result_source_type.is_none()
        || action.key.is_some()
        || action.key_type.is_some()
        || action.key_node.is_some()
        || action.key_local.is_some()
        || action.value.is_some()
        || action.value_type.is_some()
        || action.payload_clone
    {
        return false;
    }
    let Some(cfg) = function
        .ownership
        .checker_ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()
    else {
        return false;
    };
    if cfg.owner != function.owner || cfg.profile_version != ProfileVersion::V2 {
        return false;
    }
    let matching_cfg_actions = cfg
        .actions
        .iter()
        .filter(|candidate| candidate.point == action.point && candidate.kind == ActionKind::Values)
        .collect::<Vec<_>>();
    let [cfg_action] = matching_cfg_actions.as_slice() else {
        return false;
    };
    let cfg_local_value = MirValueId::new(format!("local:{}", cfg_action.local.0 .0)).ok();
    let action_matches = cfg_action.profile_version == ProfileVersion::V2
        && cfg_local_value.as_ref() == Some(&action.local)
        && cfg_action.root.0 .0 == action.root
        && cfg_action.source_local.as_ref() == Some(&cfg_action.local)
        && cfg_action.source.as_ref().map(|source| &source.0 .0) == Some(&action.root)
        && cfg_action.source_node == action.source_node
        && cfg_action.source_type == action.source_type
        && cfg_action.key.is_none()
        && cfg_action.key_type.is_none()
        && cfg_action.value.is_none()
        && cfg_action.value_type.is_none()
        && !cfg_action.payload_clone
        && cfg_action.result_type == action.result_source_type
        && cfg_action.result_local == action.result_local;
    if !action_matches {
        return false;
    }
    let matching_results = cfg
        .result_receipts
        .iter()
        .filter(|receipt| receipt.operation == action.point && receipt.kind == ActionKind::Values)
        .collect::<Vec<_>>();
    let [receipt] = matching_results.as_slice() else {
        return false;
    };
    receipt.profile_version == ProfileVersion::V2
        && receipt.result == action.point
        && action
            .result_source_type
            .as_ref()
            .is_some_and(|ty| receipt.result_type == *ty)
        && Some(&receipt.binding) == action.result_local.as_ref()
        && receipt.source_local == cfg_action.local
        && receipt.source == cfg_action.root
        && cfg_action.source_node.as_ref() == Some(&receipt.source_node)
        && cfg_action.source_type.as_ref() == Some(&receipt.source_type)
        && receipt.result == action.point
        && receipt.key.is_none()
        && receipt.key_type.is_none()
        && receipt.settlement == Settlement::ExplicitDrop
        && receipt.settlement_point.is_some()
}

fn managed_any_values_for_function(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    static_closure_values: &BTreeSet<MirValueId>,
) -> BTreeSet<MirValueId> {
    let mut managed_values = function
        .values
        .values()
        .filter(|value| {
            !static_closure_values.contains(&value.id)
                && catalog.contains_dynamic_or_managed_any(&value.ty)
        })
        .map(|value| value.id.clone())
        .collect::<BTreeSet<_>>();
    // A typed AnyOwner-v2 unpack can produce a move-only concrete payload
    // whose TypeDesc does not itself contain DynamicAny (for example the
    // canonical `Label { text: string }` ABI). Keep that exact Checker-sealed
    // result in the ownership ledger so a later field move cannot bypass the
    // aggregate owner's settlement checks. Existing DynamicAny-containing
    // values are already covered by the catalog predicate above.
    for instruction in function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
    {
        let MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
            result,
            pair,
            projection,
        } = &instruction.kind
        else {
            continue;
        };
        let Some(value) = function.values.get(result) else {
            continue;
        };
        let Some(descriptor) = catalog.get(&value.ty) else {
            continue;
        };
        if descriptor.ownership != MirOwnership::Copy
            && matches!(&descriptor.layout, MirLayout::Record { .. })
            && !catalog.contains_dynamic_or_managed_any(&value.ty)
            && has_checker_ordinary_map_owned_any_v2_unpack(
                function,
                &instruction.id,
                result,
                pair,
                projection,
            )
        {
            managed_values.insert(result.clone());
        }
    }
    for instruction in function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
    {
        let MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
            result,
            values,
            index,
            projection,
        } = &instruction.kind
        else {
            continue;
        };
        let Some(value) = function.values.get(result) else {
            continue;
        };
        let Some(descriptor) = catalog.get(&value.ty) else {
            continue;
        };
        if descriptor.ownership != MirOwnership::Copy
            && matches!(&descriptor.layout, MirLayout::Record { .. })
            && !catalog.contains_dynamic_or_managed_any(&value.ty)
            && has_checker_ordinary_map_owned_any_v2_values_unpack(
                function,
                &instruction.id,
                result,
                values,
                index,
                projection,
            )
        {
            managed_values.insert(result.clone());
        }
    }
    // MIR materializes a let-bound cast as an exact Move from the typed cast
    // result into its local. Carry the receipt-derived aggregate owner through
    // those same-type owner transfers so the later field projection/Drop is
    // checked against the live binding, not the already-consumed expression.
    loop {
        let transferred_records = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| {
                let (source, result) = match &instruction.kind {
                    MirInstructionKind::Move { source, result }
                    | MirInstructionKind::Clone { source, result } => (source, result),
                    _ => return None,
                };
                if !managed_values.contains(source) {
                    return None;
                }
                let source_type = function.values.get(source)?.ty.clone();
                let result_value = function.values.get(result)?;
                let result_type = &result_value.ty;
                let descriptor = catalog.get(result_type)?;
                (source_type == *result_type
                    && descriptor.ownership != MirOwnership::Copy
                    && matches!(&descriptor.layout, MirLayout::Record { .. }))
                .then(|| result.clone())
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        for result in transferred_records {
            changed |= managed_values.insert(result);
        }
        if !changed {
            break;
        }
    }
    managed_values
}

pub(super) fn seal_checked_lowering(
    function: &MirFunction,
    catalog: &MirTypeCatalog,
    origins: &BTreeMap<MirInstructionId, NodeId>,
    source_cfg: Option<&crate::core::ManagedAnyCfgReceipt>,
) -> Option<MirCheckerManagedAnySeal> {
    // ClosureBind creates a static code identity. Its function signature may
    // recursively mention Any, but it is not a payload owner; the separate
    // whole-program closure receipt validates this exact value before any
    // consumer can execute or emit it.
    let static_closure_values = function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::ClosureBind { function_value, .. } => Some(function_value.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let managed_values = managed_any_values_for_function(function, catalog, &static_closure_values);
    if managed_values.is_empty() {
        return None;
    }
    Some(MirCheckerManagedAnySeal {
        owner: function.owner.clone(),
        value_types: function
            .values
            .iter()
            .map(|(id, value)| (id.clone(), value.ty.clone()))
            .collect(),
        parameters: function.parameters.clone(),
        parameter_permissions: function.parameter_permissions.clone(),
        result: function.result.clone(),
        managed_values: managed_values.clone(),
        block_parameters: function
            .blocks
            .values()
            .map(|block| (block.id.clone(), block.parameters.clone()))
            .collect(),
        instructions: function
            .blocks
            .values()
            .flat_map(|block| {
                block.instructions.iter().map(move |instruction| {
                    (
                        block.id.clone(),
                        origins
                            .get(&instruction.id)
                            .cloned()
                            .unwrap_or_else(|| function.owner.clone()),
                        instruction.clone(),
                    )
                })
            })
            .collect(),
        terminators: function
            .blocks
            .values()
            .map(|block| (block.id.clone(), block.terminator.clone()))
            .collect(),
        source_cfg: source_cfg.cloned(),
        generic_identity: None,
        generic_trap_cleanup: None,
    })
}

impl MirTypeCatalog {
    fn managed_any_value_type<'a>(
        function: &'a MirFunction,
        value: &MirValueId,
    ) -> Result<&'a ResolvedTypeId, String> {
        function
            .values
            .get(value)
            .map(|value| &value.ty)
            .ok_or_else(|| format!("ManagedAny MIR value '{}' is absent", value))
    }

    fn managed_any_is_string(&self, function: &MirFunction, value: &MirValueId) -> bool {
        function.values.get(value).is_some_and(|value| {
            value.ty == crate::core::ir::primitive_type_id(PrimitiveType::String)
        })
    }

    /// Prove that each ordinary owned String MIR identity is definitely live
    /// at every use in a ManagedAny-bearing callable. Free String values are
    /// not Checker-linear, so the resource ledger intentionally does not give
    /// them exactly-once semantics. They still have runtime ownership, though:
    /// reading an SSA identity after its `Drop` or `Move` is a use-after-free.
    ///
    /// This is a forward must-live analysis. At CFG joins it intersects the
    /// incoming owner sets, so a value is usable only when every path reaching
    /// the join preserves it. The finite set lattice also gives loops a real
    /// fixed point instead of treating a backedge as a second visit to a
    /// source-ordered block. Any-derived Strings continue to have the stricter
    /// path-local settlement checks below; this analysis supplements those
    /// obligations for all other exact String identities.
    fn validate_managed_any_string_liveness(
        &self,
        function: &MirFunction,
        successors: &BTreeMap<MirBlockId, Vec<ManagedAnyCfgSuccessor>>,
    ) -> Result<(), String> {
        let string_values = function
            .values
            .iter()
            .filter_map(|(id, value)| {
                (value.ty == crate::core::ir::primitive_type_id(PrimitiveType::String))
                    .then_some(id.clone())
            })
            .collect::<BTreeSet<_>>();
        if string_values.is_empty() {
            return Ok(());
        }

        let mut predecessors = BTreeMap::<MirBlockId, Vec<(MirBlockId, usize)>>::new();
        for (source, outgoing) in successors {
            for (index, edge) in outgoing.iter().enumerate() {
                predecessors
                    .entry(edge.target.clone())
                    .or_default()
                    .push((source.clone(), index));
            }
        }

        let initial = function
            .parameters
            .iter()
            .filter(|parameter| string_values.contains(*parameter))
            .cloned()
            .collect::<BTreeSet<_>>();
        let top = string_values.clone();
        let mut incoming = function
            .blocks
            .keys()
            .map(|block| (block.clone(), top.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut outgoing = incoming.clone();
        incoming.insert(function.entry.clone(), initial.clone());

        // Apply one instruction's String-owner effect. The non-diagnostic
        // pass computes the dataflow fixed point; the diagnostic replay below
        // checks every use and proves glue before committing the transfer.
        let transfer_instruction = |live: &mut BTreeSet<MirValueId>,
                                    instruction: &MirInstruction,
                                    check: bool|
         -> Result<(), String> {
            let uses = super::mir_instruction_value_uses(&instruction.kind);
            if check {
                for value in &uses {
                    if string_values.contains(value) && !live.contains(value) {
                        return Err(format!(
                            "ManagedAny String '{}' is not definitely live at instruction '{}' (it was dropped, moved, or is not yet created)",
                            value, instruction.id
                        ));
                    }
                }
            }

            let string_arguments = |arguments: &[MirValueId]| {
                arguments
                    .iter()
                    .filter(|value| string_values.contains(*value))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let consumed = match &instruction.kind {
                MirInstructionKind::Drop { value } => vec![value.clone()],
                MirInstructionKind::Move { source, .. } => vec![source.clone()],
                MirInstructionKind::Construct { fields, .. }
                | MirInstructionKind::ConstructList {
                    elements: fields, ..
                }
                | MirInstructionKind::ConstructSet {
                    elements: fields, ..
                } => fields.clone(),
                MirInstructionKind::ConstructVariant { fields, .. }
                | MirInstructionKind::ConstructVariantMove { fields, .. } => {
                    fields.iter().map(|(_, value)| value.clone()).collect()
                }
                MirInstructionKind::UpdateRecord { fields, .. } => fields.clone(),
                MirInstructionKind::Call { arguments, .. }
                | MirInstructionKind::ClosureInvoke { arguments, .. }
                | MirInstructionKind::FlowTransition { arguments, .. } => {
                    let arguments = string_arguments(arguments);
                    if check && !arguments.is_empty() {
                        return Err(format!(
                            "ManagedAny String call boundary '{}' has no Checker ownership-transfer summary",
                            instruction.id
                        ));
                    }
                    Vec::new()
                }
                MirInstructionKind::SessionCall { payload, .. } => {
                    let arguments = payload
                        .iter()
                        .filter(|value| string_values.contains(*value))
                        .cloned()
                        .collect::<Vec<_>>();
                    if check && !arguments.is_empty() {
                        return Err(format!(
                            "ManagedAny String SessionCall '{}' has no Checker ownership-transfer summary",
                            instruction.id
                        ));
                    }
                    Vec::new()
                }
                MirInstructionKind::Actor { operation } => {
                    let arguments = uses
                        .iter()
                        .filter(|value| string_values.contains(*value))
                        .cloned()
                        .collect::<Vec<_>>();
                    if check && !arguments.is_empty() {
                        return Err(format!(
                            "ManagedAny String Actor boundary '{}' has no Checker ownership-transfer summary",
                            instruction.id
                        ));
                    }
                    let _ = operation;
                    Vec::new()
                }
                MirInstructionKind::BuiltinCall {
                    kind, arguments, ..
                } => {
                    let arguments = string_arguments(arguments);
                    if check
                        && !arguments.is_empty()
                        && *kind != super::types::MirBuiltinKind::PrintlnString
                    {
                        return Err(format!(
                            "ManagedAny String builtin boundary '{}' has no Checker ownership-transfer summary",
                            instruction.id
                        ));
                    }
                    Vec::new()
                }
                MirInstructionKind::ListOp { .. }
                | MirInstructionKind::SetOp { .. }
                | MirInstructionKind::VariantProjectOr { .. } => {
                    let arguments = uses
                        .iter()
                        .filter(|value| string_values.contains(*value))
                        .cloned()
                        .collect::<Vec<_>>();
                    if check && !arguments.is_empty() {
                        return Err(format!(
                            "ManagedAny String operation '{}' has no exact borrow/transfer receipt",
                            instruction.id
                        ));
                    }
                    Vec::new()
                }
                MirInstructionKind::Copy { source, .. } if string_values.contains(source) => {
                    return Err(format!(
                        "ManagedAny String '{}' cannot be copied by instruction '{}'",
                        source, instruction.id
                    ));
                }
                _ => Vec::new(),
            };

            let consumed_by_move = !matches!(&instruction.kind, MirInstructionKind::Drop { .. });
            let mut consumed_string_ids = BTreeSet::new();
            for value in consumed {
                if string_values.contains(&value) {
                    if check && !consumed_string_ids.insert(value.clone()) {
                        return Err(format!(
                            "ManagedAny String '{}' is transferred more than once by instruction '{}'",
                            value, instruction.id
                        ));
                    }
                    if check && consumed_by_move {
                        self.validate_glue(
                            Self::managed_any_value_type(function, &value)?,
                            MirGlueOperation::MoveOut,
                        )?;
                    }
                    live.remove(&value);
                }
            }

            if let MirInstructionKind::Drop { value } = &instruction.kind {
                if string_values.contains(value) && check {
                    self.validate_glue(
                        Self::managed_any_value_type(function, value)?,
                        MirGlueOperation::Drop,
                    )?;
                }
            }
            if let MirInstructionKind::Move { source, result } = &instruction.kind {
                if string_values.contains(source) && string_values.contains(result) && check {
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                }
            }
            if let MirInstructionKind::Clone { source, result } = &instruction.kind {
                if string_values.contains(source) {
                    if check {
                        self.validate_glue(
                            Self::managed_any_value_type(function, source)?,
                            MirGlueOperation::Clone,
                        )?;
                    }
                    if string_values.contains(result) && check {
                        self.validate_glue(
                            Self::managed_any_value_type(function, result)?,
                            MirGlueOperation::Drop,
                        )?;
                    }
                }
            }

            for result in instruction_result_values(&instruction.kind) {
                if string_values.contains(result) {
                    if check {
                        if live.contains(result) {
                            return Err(format!(
                                "ManagedAny String '{}' is redefined while its previous owner is still live at instruction '{}'",
                                result, instruction.id
                            ));
                        }
                        self.validate_glue(
                            Self::managed_any_value_type(function, result)?,
                            MirGlueOperation::Drop,
                        )?;
                    }
                    live.insert(result.clone());
                }
            }
            Ok(())
        };

        let transfer_block = |block: &super::MirBlock,
                              input: &BTreeSet<MirValueId>,
                              check: bool|
         -> Result<BTreeSet<MirValueId>, String> {
            let mut live = input.clone();
            for instruction in &block.instructions {
                transfer_instruction(&mut live, instruction, check)?;
            }
            Ok(live)
        };

        let transfer_edge = |live: &BTreeSet<MirValueId>,
                             source: &MirBlockId,
                             edge: &ManagedAnyCfgSuccessor,
                             check: bool|
         -> Result<BTreeSet<MirValueId>, String> {
            let target = function
                .blocks
                .get(&edge.target)
                .ok_or("ManagedAny String liveness edge target is absent")?;
            if edge.arguments.len() != target.parameters.len() {
                return Err(format!(
                    "ManagedAny String liveness edge '{}' -> '{}' has inconsistent arity",
                    source, edge.target
                ));
            }
            let mut edge_live = live.clone();
            let mut transferred_string_arguments = BTreeSet::new();
            for (argument, parameter) in edge.arguments.iter().zip(&target.parameters) {
                let argument_is_string = string_values.contains(argument);
                let parameter_is_string = string_values.contains(&parameter.value);
                if argument_is_string != parameter_is_string {
                    return Err(format!(
                        "ManagedAny String liveness edge '{}' -> '{}' changes String TypeDesc identity",
                        source, edge.target
                    ));
                }
                if argument_is_string {
                    if check && !transferred_string_arguments.insert(argument.clone()) {
                        return Err(format!(
                            "ManagedAny String edge '{}' -> '{}' transfers the same SSA owner to multiple block parameters",
                            source, edge.target
                        ));
                    }
                    let was_live = edge_live.remove(argument);
                    if was_live {
                        if check {
                            self.validate_glue(
                                Self::managed_any_value_type(function, argument)?,
                                MirGlueOperation::MoveOut,
                            )?;
                        }
                        if check && edge_live.contains(&parameter.value) {
                            return Err(format!(
                                "ManagedAny String block parameter '{}' replaces an unsettled owner on edge '{}' -> '{}'",
                                parameter.value, source, edge.target
                            ));
                        }
                        edge_live.insert(parameter.value.clone());
                    }
                }
            }
            Ok(edge_live)
        };

        loop {
            let mut changed = false;
            for (block_id, block) in &function.blocks {
                let next_in = if block_id == &function.entry {
                    let mut joined = initial.clone();
                    for (source, index) in predecessors.get(block_id).into_iter().flatten() {
                        let edge = successors
                            .get(source)
                            .and_then(|edges| edges.get(*index))
                            .ok_or("ManagedAny String entry backedge is absent")?;
                        let path = transfer_edge(
                            outgoing
                                .get(source)
                                .ok_or("ManagedAny String entry backedge state is absent")?,
                            source,
                            edge,
                            false,
                        )?;
                        joined = joined.intersection(&path).cloned().collect();
                    }
                    joined
                } else {
                    let incoming_edges = predecessors
                        .get(block_id)
                        .ok_or("ManagedAny String CFG block has no predecessor")?;
                    if incoming_edges.is_empty() {
                        return Err(format!(
                            "ManagedAny String CFG block '{}' has no incoming path",
                            block_id
                        ));
                    }
                    let mut paths = incoming_edges.iter().map(|(source, index)| {
                        let edge = successors
                            .get(source)
                            .and_then(|edges| edges.get(*index))
                            .ok_or("ManagedAny String CFG predecessor edge is absent")?;
                        transfer_edge(
                            outgoing
                                .get(source)
                                .ok_or("ManagedAny String CFG predecessor state is absent")?,
                            source,
                            edge,
                            false,
                        )
                    });
                    let mut joined = paths
                        .next()
                        .ok_or("ManagedAny String CFG block has no incoming path")??;
                    for path in paths {
                        joined = joined
                            .intersection(&path?)
                            .cloned()
                            .collect::<BTreeSet<_>>();
                    }
                    joined
                };
                let next_out = transfer_block(block, &next_in, false)?;
                if incoming.get(block_id) != Some(&next_in) {
                    incoming.insert(block_id.clone(), next_in);
                    changed = true;
                }
                if outgoing.get(block_id) != Some(&next_out) {
                    outgoing.insert(block_id.clone(), next_out);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        // Replay the converged definite-live states and check every MIR use,
        // including values in terminators and edge arguments. `incoming` is
        // the intersection of all actual predecessor paths, so a branch-local
        // Drop or a loop backedge that loses the peer cannot be hidden by a
        // different predecessor.
        for (block_id, block) in &function.blocks {
            let mut live = incoming
                .get(block_id)
                .cloned()
                .ok_or("ManagedAny String CFG block has no converged live state")?;
            for instruction in &block.instructions {
                transfer_instruction(&mut live, instruction, true)?;
            }
            for value in super::mir_terminator_value_uses(&block.terminator) {
                if string_values.contains(&value) && !live.contains(&value) {
                    return Err(format!(
                        "ManagedAny String '{}' is not definitely live at the terminator of '{}' (it was dropped, moved, or is not yet created)",
                        value, block_id
                    ));
                }
            }
            if let MirTerminator::Return { value: Some(value) } = &block.terminator {
                if string_values.contains(value) {
                    self.validate_glue(
                        Self::managed_any_value_type(function, value)?,
                        MirGlueOperation::MoveOut,
                    )?;
                    live.remove(value);
                }
            }
            for edge in successors.get(block_id).into_iter().flatten() {
                let _ = transfer_edge(&live, block_id, edge, true)?;
            }
        }
        Ok(())
    }

    fn managed_any_is_copy_value(&self, function: &MirFunction, value: &MirValueId) -> bool {
        function.values.get(value).is_some_and(|value| {
            self.get(&value.ty).is_some_and(|descriptor| {
                descriptor.ownership == MirOwnership::Copy
                    && matches!(
                        &descriptor.kind,
                        MirTypeKind::Primitive(primitive)
                            if *primitive != PrimitiveType::String
                                && *primitive != PrimitiveType::Unit
                    )
            })
        })
    }

    fn validate_managed_any_read_arm_instruction(
        &self,
        function: &MirFunction,
        instruction: &MirInstruction,
        managed_values: &BTreeSet<MirValueId>,
        state: &ManagedAnyCfgFlowState,
    ) -> Result<(), String> {
        let ordinary_map_observation = match &instruction.kind {
            MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root } => Some((
                result,
                root,
                crate::core::ir::primitive_type_id(PrimitiveType::I32),
                None,
            )),
            MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, root, key } => Some((
                result,
                root,
                crate::core::ir::primitive_type_id(PrimitiveType::Bool),
                Some(key),
            )),
            MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, root } => Some((
                result,
                root,
                crate::core::ir::primitive_type_id(PrimitiveType::Bool),
                None,
            )),
            _ => None,
        };
        if let Some((result, root, expected_result_type, key)) = ordinary_map_observation {
            let root_type = super::types::ordinary_map_owned_any_v2_type_id();
            let has_checker_receipt =
                has_checker_ordinary_map_owned_any_v2_action(function, instruction);
            let root_is_live = managed_values.contains(root) && state.owners.contains(root);
            let actual_root_type = Self::managed_any_value_type(function, root)?;
            let actual_result_type = Self::managed_any_value_type(function, result)?;
            let key_is_string = key.is_none_or(|key| self.managed_any_is_string(function, key));
            if !has_checker_receipt
                || !root_is_live
                || actual_root_type != &root_type
                || actual_result_type != &expected_result_type
                || !key_is_string
            {
                return Err(format!(
                    "ManagedAny CFG branch Map observation '{}' lacks a live root, exact result/key type, or Checker borrow receipt (checker={}, live={}, root_type={}, result_type={}, expected_result_type={}, key_is_string={})",
                    instruction.id,
                    has_checker_receipt,
                    root_is_live,
                    actual_root_type.as_str(),
                    actual_result_type.as_str(),
                    expected_result_type.as_str(),
                    key_is_string,
                ));
            }
            return Ok(());
        }
        if let MirInstructionKind::ManagedAny {
            result,
            source,
            operation: MirManagedAnyOperation::UnpackI32 | MirManagedAnyOperation::UnpackString,
            contract,
        } = &instruction.kind
        {
            if !managed_values.contains(source)
                || result
                    .as_ref()
                    .is_some_and(|result| managed_values.contains(result))
                || !state.owners.contains(source)
            {
                return Err(format!(
                    "ManagedAny CFG branch '{}' may only borrow-unpack a live owner",
                    instruction.id
                ));
            }
            self.validate_managed_any_contract(
                function,
                source,
                result.as_ref(),
                match &instruction.kind {
                    MirInstructionKind::ManagedAny { operation, .. } => *operation,
                    _ => unreachable!(),
                },
                contract.as_ref(),
            )?;
            return Ok(());
        }
        if instruction_value_ids(&instruction.kind)
            .iter()
            .any(|value| managed_values.contains(value))
        {
            return Err(format!(
                "ManagedAny CFG branch '{}' performs ownership work instead of a borrowed unpack",
                instruction.id
            ));
        }

        let scalar_or_string = |value: &MirValueId| {
            self.managed_any_is_copy_value(function, value)
                || self.managed_any_is_string(function, value)
        };
        match &instruction.kind {
            MirInstructionKind::Const { result, literal } => {
                if !scalar_or_string(result) {
                    return Err(format!(
                        "ManagedAny CFG branch constant '{}' has a non-scalar owned type",
                        instruction.id
                    ));
                }
                if matches!(literal, ResolvedLiteral::String(_)) {
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                }
            }
            MirInstructionKind::Binary {
                result,
                left,
                right,
                ..
            } => {
                if !scalar_or_string(result) || !scalar_or_string(left) || !scalar_or_string(right)
                {
                    return Err(format!(
                        "ManagedAny CFG branch Binary '{}' is outside scalar/String read evaluation",
                        instruction.id
                    ));
                }
                if self.managed_any_is_string(function, result) {
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                }
            }
            MirInstructionKind::Unary {
                result, operand, ..
            } => {
                if !self.managed_any_is_copy_value(function, result)
                    || !self.managed_any_is_copy_value(function, operand)
                {
                    return Err(format!(
                        "ManagedAny CFG branch Unary '{}' is outside the scalar read face",
                        instruction.id
                    ));
                }
            }
            MirInstructionKind::Convert { result, source } => {
                if !self.managed_any_is_copy_value(function, result)
                    || !self.managed_any_is_copy_value(function, source)
                {
                    return Err(format!(
                        "ManagedAny CFG branch conversion '{}' is outside the scalar read face",
                        instruction.id
                    ));
                }
            }
            MirInstructionKind::Move { result, source } => {
                if self.managed_any_is_string(function, source) {
                    if !self.managed_any_is_string(function, result) {
                        return Err(format!(
                            "ManagedAny CFG branch Move '{}' cannot transfer an unsealed String owner",
                            instruction.id
                        ));
                    }
                    self.validate_glue(
                        Self::managed_any_value_type(function, source)?,
                        MirGlueOperation::MoveOut,
                    )?;
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                } else if !self.managed_any_is_copy_value(function, source)
                    || !self.managed_any_is_copy_value(function, result)
                {
                    return Err(format!(
                        "ManagedAny CFG branch Move '{}' is outside the scalar/String read face",
                        instruction.id
                    ));
                }
            }
            MirInstructionKind::Clone { result, source } => {
                if self.managed_any_is_string(function, source) {
                    if !self.managed_any_is_string(function, result) {
                        return Err(format!(
                            "ManagedAny CFG branch Clone '{}' cannot duplicate an unsealed String owner",
                            instruction.id
                        ));
                    }
                    self.validate_glue(
                        Self::managed_any_value_type(function, source)?,
                        MirGlueOperation::Clone,
                    )?;
                    self.validate_glue(
                        Self::managed_any_value_type(function, result)?,
                        MirGlueOperation::Drop,
                    )?;
                } else if !self.managed_any_is_copy_value(function, source)
                    || !self.managed_any_is_copy_value(function, result)
                {
                    return Err(format!(
                        "ManagedAny CFG branch Clone '{}' is outside the scalar/String read face",
                        instruction.id
                    ));
                }
            }
            MirInstructionKind::Drop { value } => {
                if !self.managed_any_is_string(function, value) {
                    return Err(format!(
                        "ManagedAny CFG branch Drop '{}' does not release a branch-owned String result",
                        instruction.id
                    ));
                }
                self.validate_glue(
                    Self::managed_any_value_type(function, value)?,
                    MirGlueOperation::Drop,
                )?;
            }
            _ => {
                return Err(format!(
                    "ManagedAny CFG branch instruction '{}' is outside the read-only arm contract",
                    instruction.id
                ));
            }
        }
        Ok(())
    }

    fn validate_managed_any_cfg_flow(
        &self,
        function: &MirFunction,
        managed_values: &BTreeSet<MirValueId>,
    ) -> Result<(), String> {
        let mut successors = BTreeMap::<MirBlockId, Vec<ManagedAnyCfgSuccessor>>::new();
        for (block_id, block) in &function.blocks {
            let outgoing = match &block.terminator {
                MirTerminator::Goto {
                    target, arguments, ..
                } => vec![(target.clone(), arguments.clone())],
                MirTerminator::Branch {
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                    ..
                } => {
                    if then_target == else_target {
                        return Err(format!(
                            "ManagedAny CFG branch '{}' aliases its Then and Else targets",
                            block_id
                        ));
                    }
                    vec![
                        (then_target.clone(), then_arguments.clone()),
                        (else_target.clone(), else_arguments.clone()),
                    ]
                }
                MirTerminator::Switch { arms, .. } => {
                    if arms.iter().any(|arm| !arm.bindings.is_empty()) {
                        return Err(
                            "ManagedAny CFG Switch payload bindings need an exact per-arm owner projection receipt"
                                .into(),
                        );
                    }
                    arms.iter()
                        .map(|arm| (arm.target.clone(), arm.arguments.clone()))
                        .collect()
                }
                MirTerminator::SwitchMove { .. } => {
                    return Err(
                        "ManagedAny CFG does not admit consuming SwitchMove without a per-arm payload ownership receipt"
                            .into(),
                    );
                }
                MirTerminator::Return { .. }
                | MirTerminator::Trap { .. }
                | MirTerminator::Fault { .. }
                | MirTerminator::Unreachable => Vec::new(),
            };
            let mut block_successors = Vec::with_capacity(outgoing.len());
            for (target, arguments) in outgoing {
                if !function.blocks.contains_key(&target) {
                    return Err(format!(
                        "ManagedAny CFG block '{}' targets missing block '{}'",
                        block_id, target
                    ));
                }
                block_successors.push(ManagedAnyCfgSuccessor { target, arguments });
            }
            successors.insert(block_id.clone(), block_successors);
        }

        // The operation-neutral Checker CFG cannot grant Map ownership by
        // itself. Any typed owned GetAny/Values still needs its independent
        // Map action and path receipt before this generic path analysis runs.
        let has_typed_get_any = function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                        | MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { .. }
                )
            })
        });
        if has_typed_get_any {
            let actions = &function
                .ownership
                .checker_ordinary_map_owned_get_i32_string_v1;
            let cfg_receipt = function
                .ownership
                .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt
                .as_ref()
                .ok_or(
                    "typed owned-get ManagedAny operation has no Map-specific Checker receipt",
                )?;
            if let Some(error) =
                super::validate_ordinary_map_owned_get_cfg_receipt(function, actions, cfg_receipt)
                    .first()
            {
                return Err(format!(
                    "ManagedAny Map operation cannot trust its specific Checker receipt: {}",
                    error.message
                ));
            }
            if actions.is_empty() {
                return Err("typed owned-get ManagedAny operation has no Map action ledger".into());
            }
        }

        for (block_id, block) in &function.blocks {
            for parameter in &block.parameters {
                let value = function.values.get(&parameter.value).ok_or_else(|| {
                    format!(
                        "ManagedAny CFG block '{}' has an absent parameter",
                        block_id
                    )
                })?;
                if value.ty == super::types::ordinary_map_owned_any_v2_type_id() {
                    return Err(format!(
                        "ManagedAny CFG block '{}' has an AnyOwner-v2 root parameter without a callable transfer receipt",
                        block_id
                    ));
                }
                let is_string = self.managed_any_is_string(function, &parameter.value);
                let descriptor = self.get(&value.ty).ok_or_else(|| {
                    format!(
                        "ManagedAny CFG block '{}' parameter has no TypeDesc",
                        block_id
                    )
                })?;
                if managed_values.contains(&parameter.value) || is_string {
                    self.validate_glue(&value.ty, MirGlueOperation::Drop)?;
                } else if self.contains_dynamic_or_managed_any(&value.ty)
                    || descriptor.ownership != MirOwnership::Copy
                {
                    return Err(format!(
                        "ManagedAny CFG block '{}' has an unsupported non-Copy block parameter",
                        block_id
                    ));
                }
            }
        }

        let mut reachable = BTreeSet::new();
        let mut discover = VecDeque::from([function.entry.clone()]);
        while let Some(block_id) = discover.pop_front() {
            if !reachable.insert(block_id.clone()) {
                continue;
            }
            for successor in successors.get(&block_id).into_iter().flatten() {
                discover.push_back(successor.target.clone());
            }
        }
        if reachable.len() != function.blocks.len() {
            return Err(
                "ManagedAny CFG contains unreachable blocks outside its sealed path proof".into(),
            );
        }

        self.validate_managed_any_string_liveness(function, &successors)?;

        // Mark control-flow regions reached from exactly one arm of a
        // Branch/Switch before their paths reconverge. These regions use the
        // narrow read-expression instruction whitelist for non-owner values;
        // Any owner transfers themselves are checked by the path ledger
        // below, so each arm can independently move or drop an owner.
        let mut read_arm_blocks = BTreeSet::new();
        for (control_id, block) in &function.blocks {
            let targets = match &block.terminator {
                MirTerminator::Branch {
                    condition,
                    then_target,
                    else_target,
                    ..
                } => {
                    let condition_value = function.values.get(condition).ok_or_else(|| {
                        format!(
                            "ManagedAny CFG branch '{}' references an absent condition value",
                            control_id
                        )
                    })?;
                    if managed_values.contains(condition)
                        || self.contains_dynamic_or_managed_any(&condition_value.ty)
                        || condition_value.ty
                            != crate::core::ir::primitive_type_id(PrimitiveType::Bool)
                    {
                        return Err(format!(
                            "ManagedAny CFG branch '{}' condition must be the exact Bool primitive and cannot inspect or move an Any owner",
                            control_id
                        ));
                    }
                    vec![then_target.clone(), else_target.clone()]
                }
                MirTerminator::Switch { scrutinee, arms } => {
                    let value = function.values.get(scrutinee).ok_or_else(|| {
                        format!(
                            "ManagedAny CFG switch '{}' references an absent scrutinee",
                            control_id
                        )
                    })?;
                    if managed_values.contains(scrutinee)
                        || self.contains_dynamic_or_managed_any(&value.ty)
                        || self
                            .get(&value.ty)
                            .is_none_or(|descriptor| descriptor.ownership != MirOwnership::Copy)
                    {
                        return Err(format!(
                            "ManagedAny CFG switch '{}' must inspect a Copy value without nested Any",
                            control_id
                        ));
                    }
                    arms.iter().map(|arm| arm.target.clone()).collect()
                }
                MirTerminator::SwitchMove { scrutinee, arms } => {
                    let value = function.values.get(scrutinee).ok_or_else(|| {
                        format!(
                            "ManagedAny CFG SwitchMove '{}' references an absent scrutinee",
                            control_id
                        )
                    })?;
                    if managed_values.contains(scrutinee)
                        || self.contains_dynamic_or_managed_any(&value.ty)
                        || self
                            .get(&value.ty)
                            .is_none_or(|descriptor| descriptor.ownership == MirOwnership::Copy)
                        || self.managed_any_is_string(function, scrutinee)
                        || arms.iter().any(|arm| {
                            arm.bindings.iter().any(|binding| {
                                managed_values.contains(&binding.parameter)
                                    || self.managed_any_is_string(function, &binding.parameter)
                                    || function.values.get(&binding.parameter).is_none_or(|value| {
                                        self.contains_dynamic_or_managed_any(&value.ty)
                                    })
                            })
                        })
                    {
                        return Err(format!(
                            "ManagedAny CFG SwitchMove '{}' requires an explicit owner-transfer receipt",
                            control_id
                        ));
                    }
                    arms.iter().map(|arm| arm.target.clone()).collect()
                }
                _ => continue,
            };
            let target_reachability = targets
                .iter()
                .map(|target| {
                    let mut seen = BTreeSet::new();
                    let mut worklist = VecDeque::from([target.clone()]);
                    while let Some(current) = worklist.pop_front() {
                        if !seen.insert(current.clone()) {
                            continue;
                        }
                        for successor in successors.get(&current).into_iter().flatten() {
                            worklist.push_back(successor.target.clone());
                        }
                    }
                    seen
                })
                .collect::<Vec<_>>();
            for (index, region) in target_reachability.iter().enumerate() {
                for candidate in region {
                    if !target_reachability
                        .iter()
                        .enumerate()
                        .any(|(other, other_region)| {
                            other != index && other_region.contains(candidate)
                        })
                    {
                        read_arm_blocks.insert(candidate.clone());
                    }
                }
            }
        }

        // Keep the identities of every String owner derived from an Any
        // unpack, even after its path-local owner has been consumed.  The
        // live set below is deliberately path-sensitive; this separate
        // lineage set lets the validator distinguish an unrelated String
        // operation from a stale use after Drop/Move/Return on a prior path.
        // Without it, an injected Drop after a diamond join is invisible once
        // the value has been removed from `unpacked_strings`.
        let mut unpacked_string_lineage = BTreeSet::new();
        loop {
            let mut changed = false;
            for (block_id, block) in &function.blocks {
                for instruction in &block.instructions {
                    let produced = match &instruction.kind {
                        MirInstructionKind::ManagedAny {
                            result: Some(result),
                            operation: MirManagedAnyOperation::UnpackString,
                            ..
                        } => Some(result),
                        MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack { result, .. }
                            if self.managed_any_is_string(function, result) =>
                        {
                            Some(result)
                        }
                        MirInstructionKind::Const {
                            result,
                            literal: ResolvedLiteral::String(_),
                        } if read_arm_blocks.contains(block_id) => Some(result),
                        MirInstructionKind::Binary { result, .. }
                            if read_arm_blocks.contains(block_id)
                                && self.managed_any_is_string(function, result) =>
                        {
                            Some(result)
                        }
                        MirInstructionKind::Move { source, result }
                        | MirInstructionKind::Clone { source, result }
                            if unpacked_string_lineage.contains(source)
                                && self.managed_any_is_string(function, result) =>
                        {
                            Some(result)
                        }
                        MirInstructionKind::MoveProject { base, result, .. }
                        | MirInstructionKind::MoveProjectDrop { base, result, .. }
                            if (managed_values.contains(base)
                                || unpacked_string_lineage.contains(base))
                                && self.managed_any_is_string(function, result) =>
                        {
                            Some(result)
                        }
                        MirInstructionKind::Binary {
                            result,
                            left,
                            right,
                            ..
                        } if self.managed_any_is_string(function, result)
                            && (unpacked_string_lineage.contains(left)
                                || unpacked_string_lineage.contains(right)) =>
                        {
                            Some(result)
                        }
                        MirInstructionKind::Call {
                            result: Some(result),
                            ..
                        }
                        | MirInstructionKind::ClosureInvoke {
                            result: Some(result),
                            ..
                        } if self.managed_any_is_string(function, result) => Some(result),
                        _ => None,
                    };
                    if let Some(produced) = produced {
                        changed |= unpacked_string_lineage.insert(produced.clone());
                    }
                }
                for successor in successors.get(block_id).into_iter().flatten() {
                    let Some(target) = function.blocks.get(&successor.target) else {
                        continue;
                    };
                    for (argument, parameter) in successor.arguments.iter().zip(&target.parameters)
                    {
                        if unpacked_string_lineage.contains(argument)
                            && self.managed_any_is_string(function, &parameter.value)
                        {
                            changed |= unpacked_string_lineage.insert(parameter.value.clone());
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }

        let initial = ManagedAnyCfgFlowState {
            owners: function
                .parameters
                .iter()
                .filter(|parameter| managed_values.contains(*parameter))
                .cloned()
                .collect(),
            unpacked_strings: BTreeSet::new(),
        };
        for parameter in &initial.owners {
            if !managed_values.contains(parameter) {
                return Err("ManagedAny CFG entry contains an unsealed owner parameter".into());
            }
        }
        let mut incoming_states = BTreeMap::from([(function.entry.clone(), initial.clone())]);
        let mut pending = VecDeque::from([function.entry.clone()]);
        let mut processed = BTreeSet::new();
        while let Some(block_id) = pending.pop_front() {
            if !processed.insert(block_id.clone()) {
                continue;
            }
            let block = function
                .blocks
                .get(&block_id)
                .ok_or("ManagedAny CFG worklist reached an absent block")?;
            let mut state = incoming_states
                .get(&block_id)
                .cloned()
                .ok_or("ManagedAny CFG block has no path-live input state")?;
            let in_read_arm = read_arm_blocks.contains(&block_id);

            for instruction in &block.instructions {
                for value in super::mir_instruction_value_uses(&instruction.kind) {
                    if unpacked_string_lineage.contains(&value)
                        && !state.unpacked_strings.contains(&value)
                    {
                        return Err(format!(
                            "ManagedAny-derived String '{}' is used after transfer, drop, or before creation at instruction '{}'",
                            value, instruction.id
                        ));
                    }
                }
                if in_read_arm
                    && !instruction_value_ids(&instruction.kind)
                        .iter()
                        .any(|value| managed_values.contains(value))
                {
                    self.validate_managed_any_read_arm_instruction(
                        function,
                        instruction,
                        managed_values,
                        &state,
                    )?;
                }
                match &instruction.kind {
                    MirInstructionKind::Clone { result, source }
                        if function
                            .values
                            .get(source)
                            .is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            }) =>
                    {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot clone an AnyOwner-v2 Map root",
                                instruction.id
                            ));
                        }
                        let owner_type = super::types::ordinary_map_owned_any_v2_type_id();
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, result)? != &owner_type
                        {
                            return Err(format!(
                                "AnyOwner-v2 Clone '{}' has no exact Checker root-owner receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        self.validate_glue(&owner_type, MirGlueOperation::Clone)?;
                        self.validate_glue(&owner_type, MirGlueOperation::Drop)?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2New { result } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create an AnyOwner-v2 Map root",
                                instruction.id
                            ));
                        }
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, result)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 New '{}' has no exact Checker root-owner receipt",
                                instruction.id
                            ));
                        }
                        self.validate_glue(
                            Self::managed_any_value_type(function, result)?,
                            MirGlueOperation::Drop,
                        )?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Set {
                        result, source, ..
                    }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2Remove {
                        result, source, ..
                    } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create an AnyOwner-v2 persistent root",
                                instruction.id
                            ));
                        }
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, source)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                            || Self::managed_any_value_type(function, result)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 persistent update '{}' has no exact Checker root lineage receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        self.validate_glue(
                            Self::managed_any_value_type(function, result)?,
                            MirGlueOperation::Drop,
                        )?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Size { result, root }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2Contains { result, root, .. }
                    | MirInstructionKind::OrdinaryMapOwnedAnyV2IsEmpty { result, root } => {
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 root observation '{}' has no exact Checker borrow receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        if result_type == &ordinary_map_owned_any_v2_type_id()
                            || self.contains_dynamic_or_managed_any(result_type)
                        {
                            return Err(format!(
                                "AnyOwner-v2 root observation '{}' has an owner-bearing result",
                                instruction.id
                            ));
                        }
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Drop { root } => {
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 Drop '{}' has no exact Checker owner-disposition receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        self.validate_glue(
                            Self::managed_any_value_type(function, root)?,
                            MirGlueOperation::Drop,
                        )?;
                        state.owners.remove(root);
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2GetAny {
                        result, root, ..
                    } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create an AnyOwner-v2 GetAny result",
                                instruction.id
                            ));
                        }
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 GetAny '{}' has no exact Checker root borrow receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        self.validate_ordinary_map_owned_any_v2_get_any_result_type(
                            Self::managed_any_value_type(function, result)?,
                        )?;
                        if !has_checker_ordinary_map_owned_any_v2_get_any_result(
                            function,
                            &instruction.id,
                            result,
                        ) {
                            return Err(format!(
                                "AnyOwner-v2 GetAny '{}' has no exact Checker CFG result/settlement receipt",
                                instruction.id
                            ));
                        }
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Unpack {
                        result,
                        pair,
                        projection,
                    } => {
                        if !has_checker_ordinary_map_owned_any_v2_unpack(
                            function,
                            &instruction.id,
                            result,
                            pair,
                            projection,
                        ) || Self::managed_any_value_type(function, pair)?
                            != &ordinary_map_owned_any_v2_get_result_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 unpack '{}' has no exact Checker pair/projection/conversion/Drop receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, pair)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
                        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
                        let selected_set_payload = function
                            .ownership
                            .checker_ordinary_map_owned_any_v2
                            .iter()
                            .any(|action| {
                                action.kind
                                    == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                                    && action.value_type.as_ref() == Some(result_type)
                            });
                        if result_type != &i32_type
                            && result_type != &string_type
                            && !selected_set_payload
                        {
                            return Err(format!(
                                "AnyOwner-v2 unpack '{}' target is outside the receipted i32/String/selected-Set profile",
                                instruction.id
                            ));
                        }
                        if result_type == &i32_type {
                            self.validate_copy_scalar(result_type)?;
                        } else if result_type == &string_type {
                            self.validate_owned_string(result_type)?;
                        }
                        self.validate_glue(result_type, MirGlueOperation::Clone)?;
                        self.validate_glue(result_type, MirGlueOperation::Drop)?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                        if result_type == &string_type
                            && !state.unpacked_strings.insert(result.clone())
                        {
                            return Err(format!(
                                "AnyOwner-v2 unpacked String '{}' is produced more than once",
                                result
                            ));
                        }
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2ValuesUnpack {
                        result,
                        values,
                        index,
                        projection,
                    } => {
                        if !has_checker_ordinary_map_owned_any_v2_values_unpack(
                            function,
                            &instruction.id,
                            result,
                            values,
                            index,
                            projection,
                        ) || Self::managed_any_value_type(function, values)?
                            != &ordinary_map_owned_any_v2_values_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 Values unpack '{}' has no exact Checker snapshot/index/conversion/Drop receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, values)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
                        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
                        if result_type != &i32_type
                            && result_type != &string_type
                            && !function
                                .ownership
                                .checker_ordinary_map_owned_any_v2_cfg_receipt
                                .as_ref()
                                .is_some_and(|cfg| {
                                    cfg.values_unpack_receipts.iter().any(|receipt| {
                                        receipt.cast.0
                                            == result.as_str().strip_prefix("expr:").unwrap_or_default()
                                            && receipt.conversion_to == *result_type
                                            && !receipt.payload_selection.is_empty()
                                    })
                                })
                        {
                            return Err(format!(
                                "AnyOwner-v2 Values unpack '{}' target is outside its selected Set payload profile",
                                instruction.id
                            ));
                        }
                        if result_type == &i32_type {
                            self.validate_copy_scalar(result_type)?;
                        } else if result_type == &string_type {
                            self.validate_owned_string(result_type)?;
                        }
                        self.validate_glue(result_type, MirGlueOperation::Clone)?;
                        self.validate_glue(result_type, MirGlueOperation::Drop)?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                        if result_type == &string_type
                            && !state.unpacked_strings.insert(result.clone())
                        {
                            return Err(format!(
                                "AnyOwner-v2 Values unpacked String '{}' is produced more than once",
                                result
                            ));
                        }
                    }
                    MirInstructionKind::OrdinaryMapOwnedAnyV2Values { result, root } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create an AnyOwner-v2 Values result",
                                instruction.id
                            ));
                        }
                        if !has_checker_ordinary_map_owned_any_v2_action(function, instruction)
                            || Self::managed_any_value_type(function, root)?
                                != &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 Values '{}' has no exact Checker root borrow receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, root)?;
                        let result_type = Self::managed_any_value_type(function, result)?;
                        if result_type != &ordinary_map_owned_any_v2_values_type_id() {
                            return Err(format!(
                                "AnyOwner-v2 Values '{}' has a noncanonical MIR result TypeDesc",
                                instruction.id
                            ));
                        }
                        self.validate_list_glue(result_type, MirGlueOperation::MoveOut)?;
                        if !has_checker_ordinary_map_owned_any_v2_values_result(
                            function,
                            &instruction.id,
                            result,
                        ) {
                            return Err(format!(
                                "AnyOwner-v2 Values '{}' has no exact Checker CFG result/settlement receipt",
                                instruction.id
                            ));
                        }
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedGetI32StringV1GetAny { result, .. } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create a Map snapshot",
                                instruction.id
                            ));
                        }
                        self.validate_ordinary_map_owned_get_any_result_type(
                            Self::managed_any_value_type(function, result)?,
                        )?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::OrdinaryMapOwnedGetI32StringV1Values { result, .. } => {
                        if in_read_arm {
                            return Err(format!(
                                "ManagedAny CFG read arm '{}' cannot create a Map values snapshot",
                                instruction.id
                            ));
                        }
                        self.validate_managed_any_list_type(Self::managed_any_value_type(
                            function, result,
                        )?)?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Binary {
                        result,
                        op,
                        left,
                        right,
                    } if managed_values.contains(left) || managed_values.contains(right) => {
                        let owner = managed_any_string_comparison_owner(
                            function,
                            result,
                            *op,
                            left,
                            right,
                            managed_values,
                        )
                        .ok_or_else(|| {
                            format!(
                                "ManagedAny Binary '{}' is not an exact borrowed equality against String",
                                instruction.id
                            )
                        })?;
                        require_managed_any_cfg_owner(&state.owners, managed_values, owner)?;
                    }
                    MirInstructionKind::ManagedAny {
                        result,
                        source,
                        operation,
                        contract,
                    } => {
                        if *operation != MirManagedAnyOperation::PackI32 {
                            require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        }
                        self.validate_managed_any_contract(
                            function,
                            source,
                            result.as_ref(),
                            *operation,
                            contract.as_ref(),
                        )?;
                        if *operation == MirManagedAnyOperation::Drop {
                            state.owners.remove(source);
                        }
                        if *operation == MirManagedAnyOperation::PackI32 {
                            let result = result
                                .as_ref()
                                .ok_or("ManagedAny PackI32 has no owner result")?;
                            create_managed_any_cfg_owner(
                                &mut state.owners,
                                managed_values,
                                result,
                            )?;
                        } else if let Some(result) = result {
                            create_managed_any_cfg_owner(
                                &mut state.owners,
                                managed_values,
                                result,
                            )?;
                        }
                        if *operation == MirManagedAnyOperation::UnpackString {
                            let result = result
                                .as_ref()
                                .ok_or("ManagedAny UnpackString has no owned String result")?;
                            self.validate_glue(
                                Self::managed_any_value_type(function, result)?,
                                MirGlueOperation::Drop,
                            )?;
                            if !state.unpacked_strings.insert(result.clone()) {
                                return Err(format!(
                                    "ManagedAny unpacked String '{}' is produced more than once",
                                    result
                                ));
                            }
                        }
                    }
                    MirInstructionKind::Move { source, result }
                        if managed_values.contains(source) || managed_values.contains(result) =>
                    {
                        if [source, result].iter().any(|value| {
                            function.values.get(*value).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root Move '{}' has no exact Checker transfer receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        if Self::managed_any_value_type(function, source)?
                            != Self::managed_any_value_type(function, result)?
                        {
                            return Err("ManagedAny Move changes owner TypeDesc identity".into());
                        }
                        state.owners.remove(source);
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Clone { source, result }
                        if managed_values.contains(source) || managed_values.contains(result) =>
                    {
                        if [source, result].iter().any(|value| {
                            function.values.get(*value).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root Clone '{}' has no exact Checker clone receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, source)?;
                        if Self::managed_any_value_type(function, source)?
                            != Self::managed_any_value_type(function, result)?
                        {
                            return Err(
                                "ManagedAny tuple Clone changes owner TypeDesc identity".into()
                            );
                        }
                        self.validate_glue(
                            Self::managed_any_value_type(function, source)?,
                            MirGlueOperation::Clone,
                        )?;
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Drop { value } if managed_values.contains(value) => {
                        if function.values.get(value).is_some_and(|value| {
                            value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root Drop '{}' must use its dedicated Checker action",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, value)?;
                        self.validate_glue(
                            Self::managed_any_value_type(function, value)?,
                            MirGlueOperation::Drop,
                        )?;
                        state.owners.remove(value);
                    }
                    MirInstructionKind::ListOp {
                        result,
                        operation: super::MirListOperation::Len,
                        list,
                        argument: None,
                        list_operation_contract: Some(contract),
                    } if managed_values.contains(list) => {
                        require_managed_any_cfg_owner(&state.owners, managed_values, list)?;
                        let list_type = Self::managed_any_value_type(function, list)?;
                        if list_type == &ordinary_map_owned_any_v2_values_type_id() {
                            self.validate_list_glue(list_type, MirGlueOperation::MoveOut)?;
                        } else {
                            self.validate_managed_any_list_type(list_type)?;
                        }
                        self.validate_list_operation_receipt(
                            Self::managed_any_value_type(function, result)?,
                            list_type,
                            super::MirListOperation::Len,
                            contract,
                        )?;
                    }
                    MirInstructionKind::Construct {
                        result,
                        kind: MirAggregateKind::Tuple,
                        fields,
                    } if managed_values.contains(result) => {
                        if fields.iter().any(|field| {
                            function.values.get(field).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root aggregate construction '{}' has no exact Checker transfer receipt",
                                instruction.id
                            ));
                        }
                        let result_type = Self::managed_any_value_type(function, result)?;
                        self.validate_recursive_tuple_abi(result_type)?;
                        let Some(MirLayout::Tuple(elements)) = self
                            .get(result_type)
                            .map(|descriptor| &descriptor.layout)
                        else {
                            return Err("ManagedAny Construct has no tuple descriptor".into());
                        };
                        if elements.len() != fields.len() {
                            return Err("ManagedAny tuple Construct fields disagree with exact TypeDesc identities".into());
                        }
                        for (field, ty) in fields.iter().zip(elements) {
                            if Self::managed_any_value_type(function, field)? != ty {
                                return Err("ManagedAny tuple Construct fields disagree with exact TypeDesc identities".into());
                            }
                        }
                        for field in fields {
                            require_managed_any_cfg_owner(&state.owners, managed_values, field)?;
                            if managed_values.contains(field) {
                                state.owners.remove(field);
                            }
                            if state.unpacked_strings.contains(field) {
                                self.validate_glue(
                                    Self::managed_any_value_type(function, field)?,
                                    MirGlueOperation::MoveOut,
                                )?;
                                state.unpacked_strings.remove(field);
                            }
                        }
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::Project {
                        result,
                        base,
                        projection: MirProjection::Tuple(index),
                        ..
                    } if managed_values.contains(base) => {
                        if Self::managed_any_value_type(function, result)?
                            == &super::types::ordinary_map_owned_any_v2_type_id()
                        {
                            return Err(format!(
                                "AnyOwner-v2 root projection '{}' has no exact Checker transfer receipt",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, base)?;
                        if Self::managed_any_value_type(function, base)?
                            == &ordinary_map_owned_any_v2_get_result_type_id()
                            && (*index != 0
                                || Self::managed_any_value_type(function, result)?
                                    != &crate::core::ir::primitive_type_id(PrimitiveType::Bool))
                        {
                            return Err(
                                "AnyOwner-v2 GetAny permits only its non-owning bool result projection"
                                    .into(),
                            );
                        }
                        self.validate_projection(
                            Self::managed_any_value_type(function, base)?,
                            Self::managed_any_value_type(function, result)?,
                            &MirProjection::Tuple(*index),
                        )?;
                        if managed_values.contains(result) {
                            self.validate_glue(
                                Self::managed_any_value_type(function, result)?,
                                MirGlueOperation::Clone,
                            )?;
                        }
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                    }
                    MirInstructionKind::MoveProject {
                        result,
                        base,
                        projection,
                    } if managed_values.contains(base)
                        || managed_values.contains(result)
                        || self.managed_any_is_string(function, result) =>
                    {
                        if [base, result].iter().any(|value| {
                            function.values.get(*value).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root projection move '{}' has no exact Checker transfer receipt",
                                instruction.id
                            ));
                        }
                        if !managed_values.contains(base) {
                            return Err(format!(
                                "ManagedAny MoveProject '{}' has no tracked aggregate owner",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, base)?;
                        self.validate_move_projection(
                            Self::managed_any_value_type(function, base)?,
                            Self::managed_any_value_type(function, result)?,
                            projection,
                        )?;
                        state.owners.remove(base);
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                        if self.managed_any_is_string(function, result) {
                            self.validate_glue(
                                Self::managed_any_value_type(function, result)?,
                                MirGlueOperation::Drop,
                            )?;
                            if !state.unpacked_strings.insert(result.clone()) {
                                return Err(format!(
                                    "ManagedAny MoveProject String result '{}' is already live",
                                    result
                                ));
                            }
                        }
                    }
                    MirInstructionKind::MoveProjectDrop {
                        result,
                        base,
                        projection,
                        contract,
                    } if managed_values.contains(base)
                        || managed_values.contains(result)
                        || self.managed_any_is_string(function, result) =>
                    {
                        if [base, result].iter().any(|value| {
                            function.values.get(*value).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root residual projection '{}' has no exact Checker transfer receipt",
                                instruction.id
                            ));
                        }
                        if !managed_values.contains(base) {
                            return Err(format!(
                                "ManagedAny MoveProjectDrop '{}' has no tracked aggregate owner",
                                instruction.id
                            ));
                        }
                        require_managed_any_cfg_owner(&state.owners, managed_values, base)?;
                        if !matches!(projection, MirProjection::Field(_)) {
                            return Err(
                                "ManagedAny MoveProjectDrop requires a direct Checker-sealed record field"
                                    .into(),
                            );
                        }
                        let contract = contract.as_ref().ok_or(
                            "ManagedAny MoveProjectDrop has no Checker residual-drop contract",
                        )?;
                        self.validate_record_move_projection_drop_receipt(
                            Self::managed_any_value_type(function, base)?,
                            Self::managed_any_value_type(function, result)?,
                            contract,
                        )?;
                        state.owners.remove(base);
                        create_managed_any_cfg_owner(&mut state.owners, managed_values, result)?;
                        if self.managed_any_is_string(function, result) {
                            self.validate_glue(
                                Self::managed_any_value_type(function, result)?,
                                MirGlueOperation::Drop,
                            )?;
                            if !state.unpacked_strings.insert(result.clone()) {
                                return Err(format!(
                                    "ManagedAny MoveProjectDrop String result '{}' is already live",
                                    result
                                ));
                            }
                        }
                    }
                    MirInstructionKind::Call {
                        result,
                        callee: crate::core::ir::ResolvedCallee::Function(_),
                        arguments,
                        ..
                    }
                    // This is only local owner accounting. The separate
                    // whole-program ClosureCallable receipt is mandatory
                    // before any consumer can execute or emit this transfer.
                    | MirInstructionKind::ClosureInvoke {
                        result, arguments, ..
                    } => {
                        if arguments.iter().any(|argument| {
                            function.values.get(argument).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) || result.as_ref().is_some_and(|result| {
                            function.values.get(result).is_some_and(|value| {
                                value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                            })
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root callable transfer '{}' has no exact Checker boundary receipt",
                                instruction.id
                            ));
                        }
                        for argument in arguments {
                            require_managed_any_cfg_owner(&state.owners, managed_values, argument)?;
                            if managed_values.contains(argument) {
                                state.owners.remove(argument);
                            }
                            if state.unpacked_strings.contains(argument) {
                                self.validate_glue(
                                    Self::managed_any_value_type(function, argument)?,
                                    MirGlueOperation::MoveOut,
                                )?;
                                state.unpacked_strings.remove(argument);
                            }
                        }
                        if let Some(result) = result {
                            create_managed_any_cfg_owner(
                                &mut state.owners,
                                managed_values,
                                result,
                            )?;
                            if self.managed_any_is_string(function, result) {
                                self.validate_glue(
                                    Self::managed_any_value_type(function, result)?,
                                    MirGlueOperation::Drop,
                                )?;
                                if !state.unpacked_strings.insert(result.clone()) {
                                    return Err(format!(
                                        "ManagedAny call String result '{}' is already live",
                                        result
                                    ));
                                }
                            }
                        }
                    }
                    MirInstructionKind::BuiltinCall {
                        result,
                        kind: super::types::MirBuiltinKind::PrintlnString,
                        arguments,
                        string_field_contract: None,
                    } if arguments
                        .iter()
                        .any(|argument| state.unpacked_strings.contains(argument)) =>
                    {
                        let [argument] = arguments.as_slice() else {
                            return Err(format!(
                                "ManagedAny-derived String println '{}' does not have one exact argument",
                                instruction.id
                            ));
                        };
                        if !state.unpacked_strings.contains(argument)
                            || !self.managed_any_is_string(function, argument)
                            || function.values.get(result).is_none_or(|value| {
                                value.ty
                                    != crate::core::ir::primitive_type_id(PrimitiveType::Unit)
                            })
                        {
                            return Err(format!(
                                "ManagedAny-derived String println '{}' disagrees with its owned String/Unit ABI",
                                instruction.id
                            ));
                        }
                        self.validate_glue(
                            Self::managed_any_value_type(function, argument)?,
                            MirGlueOperation::MoveOut,
                        )?;
                        state.unpacked_strings.remove(argument);
                    }
                    MirInstructionKind::Drop { value }
                        if state.unpacked_strings.contains(value) =>
                    {
                        self.validate_glue(
                            Self::managed_any_value_type(function, value)?,
                            MirGlueOperation::Drop,
                        )?;
                        state.unpacked_strings.remove(value);
                    }
                    MirInstructionKind::Move { source, result }
                        if state.unpacked_strings.contains(source) =>
                    {
                        self.validate_glue(
                            Self::managed_any_value_type(function, source)?,
                            MirGlueOperation::MoveOut,
                        )?;
                        if !self.managed_any_is_string(function, result) {
                            return Err(
                                "ManagedAny unpacked String Move changes its owned result type"
                                    .into(),
                            );
                        }
                        state.unpacked_strings.remove(source);
                        if !state.unpacked_strings.insert(result.clone()) {
                            return Err(
                                "ManagedAny unpacked String Move duplicates an owner identity"
                                    .into(),
                            );
                        }
                    }
                    MirInstructionKind::Clone { source, result }
                        if state.unpacked_strings.contains(source) =>
                    {
                        self.validate_glue(
                            Self::managed_any_value_type(function, source)?,
                            MirGlueOperation::Clone,
                        )?;
                        if !self.managed_any_is_string(function, result) {
                            return Err(
                                "ManagedAny unpacked String Clone changes its owned result type"
                                    .into(),
                            );
                        }
                        self.validate_glue(
                            Self::managed_any_value_type(function, result)?,
                            MirGlueOperation::Drop,
                        )?;
                        if !state.unpacked_strings.insert(result.clone()) {
                            return Err(
                                "ManagedAny unpacked String Clone duplicates an owner identity"
                                    .into(),
                            );
                        }
                    }
                    other => {
                        let ids = instruction_value_ids(other);
                        if ids.iter().any(|value| managed_values.contains(value)) {
                            return Err(format!(
                                "ManagedAny operation '{}' lacks an admitted CFG owner/borrow/escape contract",
                                instruction.id
                            ));
                        }
                        let string_unary_or_conversion = match other {
                            MirInstructionKind::Unary {
                                result, operand, ..
                            } => {
                                self.managed_any_is_string(function, result)
                                    || self.managed_any_is_string(function, operand)
                            }
                            MirInstructionKind::Convert { result, source } => {
                                self.managed_any_is_string(function, result)
                                    || self.managed_any_is_string(function, source)
                            }
                            _ => false,
                        };
                        if string_unary_or_conversion {
                            return Err(format!(
                                "ManagedAny ownership proof does not admit String Unary/Convert instruction '{}'",
                                instruction.id
                            ));
                        }
                        let tracked_input = ids
                            .iter()
                            .any(|value| state.unpacked_strings.contains(value));
                        if tracked_input
                            && !matches!(
                                other,
                                MirInstructionKind::Binary { .. }
                                    | MirInstructionKind::Unary { .. }
                                    | MirInstructionKind::Convert { .. }
                            )
                        {
                            return Err(format!(
                                "ManagedAny unpacked String is used by unsupported operation '{}'",
                                instruction.id
                            ));
                        }
                        for result in instruction_result_values(other) {
                            if self.managed_any_is_string(function, result)
                                && (in_read_arm || unpacked_string_lineage.contains(result))
                            {
                                self.validate_glue(
                                    Self::managed_any_value_type(function, result)?,
                                    MirGlueOperation::Drop,
                                )?;
                                if !state.unpacked_strings.insert(result.clone()) {
                                    return Err(format!(
                                        "ManagedAny branch String '{}' is produced more than once",
                                        result
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            for value in super::mir_terminator_value_uses(&block.terminator) {
                if managed_values.contains(&value) {
                    require_managed_any_cfg_owner(&state.owners, managed_values, &value)?;
                }
                if unpacked_string_lineage.contains(&value)
                    && !state.unpacked_strings.contains(&value)
                {
                    return Err(format!(
                        "ManagedAny-derived String '{}' is used after transfer, drop, or before creation at the terminator of '{}'",
                        value, block_id
                    ));
                }
            }

            match &block.terminator {
                MirTerminator::Goto { .. } => {}
                MirTerminator::Branch { condition, .. } => {
                    if managed_values.contains(condition)
                        || state.unpacked_strings.contains(condition)
                    {
                        return Err(format!(
                            "ManagedAny CFG branch '{}' inspects an owner-bearing condition",
                            block_id
                        ));
                    }
                }
                MirTerminator::Return { value } => {
                    if let Some(value) = value {
                        if function.values.get(value).is_some_and(|value| {
                            value.ty == super::types::ordinary_map_owned_any_v2_type_id()
                        }) {
                            return Err(format!(
                                "AnyOwner-v2 root return from '{}' has no exact Checker escape receipt",
                                block_id
                            ));
                        }
                        if managed_values.contains(value) {
                            require_managed_any_cfg_owner(&state.owners, managed_values, value)?;
                            state.owners.remove(value);
                        }
                        if state.unpacked_strings.contains(value) {
                            self.validate_glue(
                                Self::managed_any_value_type(function, value)?,
                                MirGlueOperation::MoveOut,
                            )?;
                            state.unpacked_strings.remove(value);
                        }
                    }
                    if !state.owners.is_empty() || !state.unpacked_strings.is_empty() {
                        return Err(format!(
                            "ManagedAny owners or unpacked String values remain live at return from '{}': owners={:?}, unpacked_strings={:?}",
                            block_id, state.owners, state.unpacked_strings
                        ));
                    }
                }
                MirTerminator::Trap { .. }
                | MirTerminator::Fault { .. }
                | MirTerminator::Unreachable => {
                    if !state.owners.is_empty() || !state.unpacked_strings.is_empty() {
                        return Err(format!(
                            "ManagedAny owners or unpacked String values remain live at exit from '{}'",
                            block_id
                        ));
                    }
                }
                MirTerminator::Switch { scrutinee, .. } => {
                    if managed_values.contains(scrutinee)
                        || state.unpacked_strings.contains(scrutinee)
                    {
                        return Err(format!(
                            "ManagedAny CFG switch '{}' cannot inspect an owned value",
                            block_id
                        ));
                    }
                }
                MirTerminator::SwitchMove { scrutinee, .. } => {
                    if managed_values.contains(scrutinee)
                        || state.unpacked_strings.contains(scrutinee)
                    {
                        return Err(format!(
                            "ManagedAny CFG SwitchMove '{}' cannot consume a tracked owner without a payload-transfer receipt",
                            block_id
                        ));
                    }
                }
            }

            for successor in successors.get(&block_id).into_iter().flatten() {
                let target = &successor.target;
                let mut edge_state = state.clone();
                let target_block = function
                    .blocks
                    .get(target)
                    .ok_or("ManagedAny CFG edge target is absent")?;
                if successor.arguments.len() != target_block.parameters.len() {
                    return Err(format!(
                        "ManagedAny CFG edge '{}' -> '{}' argument count disagrees with target parameters",
                        block_id, target
                    ));
                }
                for (argument, parameter) in
                    successor.arguments.iter().zip(&target_block.parameters)
                {
                    let argument_type = function
                        .values
                        .get(argument)
                        .ok_or("ManagedAny CFG edge argument is absent")?;
                    let parameter_type = function
                        .values
                        .get(&parameter.value)
                        .ok_or("ManagedAny CFG block parameter value is absent")?;
                    if argument_type.ty != parameter_type.ty {
                        return Err(format!(
                            "ManagedAny CFG edge '{}' -> '{}' changes block parameter TypeDesc identity",
                            block_id, target
                        ));
                    }
                    if managed_values.contains(&parameter.value) {
                        require_managed_any_cfg_owner(
                            &edge_state.owners,
                            managed_values,
                            argument,
                        )?;
                        edge_state.owners.remove(argument);
                        create_managed_any_cfg_owner(
                            &mut edge_state.owners,
                            managed_values,
                            &parameter.value,
                        )?;
                    } else if unpacked_string_lineage.contains(&parameter.value) {
                        if !self.managed_any_is_string(function, argument)
                            || !edge_state.unpacked_strings.remove(argument)
                        {
                            return Err(format!(
                                "ManagedAny CFG edge '{}' -> '{}' does not transfer a live owned String into its block parameter",
                                block_id, target
                            ));
                        }
                        self.validate_glue(&parameter_type.ty, MirGlueOperation::Drop)?;
                        if !edge_state.unpacked_strings.insert(parameter.value.clone()) {
                            return Err(format!(
                                "ManagedAny CFG edge '{}' -> '{}' aliases a live String block parameter",
                                block_id, target
                            ));
                        }
                    } else if managed_values.contains(argument)
                        || edge_state.unpacked_strings.contains(argument)
                    {
                        return Err(format!(
                            "ManagedAny CFG edge '{}' -> '{}' transfers an owner without an owner-bearing block parameter",
                            block_id, target
                        ));
                    }
                }
                match incoming_states.get(target) {
                    Some(previous) if previous != &edge_state => {
                        return Err(format!(
                            "ManagedAny CFG live-owner state disagrees at join block '{}'",
                            target
                        ));
                    }
                    Some(_) => {}
                    None => {
                        incoming_states.insert(target.clone(), edge_state);
                        pending.push_back(target.clone());
                    }
                }
            }
        }

        if processed.len() != function.blocks.len() {
            return Err("ManagedAny CFG path-live worklist did not cover every block".into());
        }
        Ok(())
    }

    /// Bind canonical MIR's reachable edges and branch operands to the
    /// operation-neutral CFG snapshot emitted by Checker. This receipt
    /// establishes correspondence only; owner flow and cleanup are checked
    /// separately by `validate_managed_any_cfg_flow`.
    fn validate_managed_any_source_cfg(
        &self,
        function: &MirFunction,
        seal: &MirCheckerManagedAnySeal,
    ) -> Result<(), String> {
        let Some(receipt) = seal.source_cfg.as_ref() else {
            if function.blocks.len() > 1 {
                return Err(
                    "multi-block ManagedAny function has no Checker source-CFG receipt".into(),
                );
            }
            return Ok(());
        };
        if receipt.owner != function.owner
            || !receipt.reachable_blocks.contains_key(&receipt.entry)
            || receipt.reachable_blocks.len() != function.blocks.len()
        {
            return Err(
                "ManagedAny source-CFG owner, entry, or block coverage disagrees with MIR".into(),
            );
        }

        let entry_role = receipt
            .entry
            .0
             .0
            .rsplit_once("/cfg:block:")
            .map(|(_, role)| role);
        if entry_role != Some("entry") {
            return Err(
                "ManagedAny Checker source-CFG entry is not the canonical entry block".into(),
            );
        }

        let mut referenced_edges = BTreeSet::new();
        for (block_id, block) in &receipt.reachable_blocks {
            for edge_id in block.terminator.outgoing_edges() {
                let edge = receipt
                    .reachable_edges
                    .get(edge_id)
                    .ok_or("ManagedAny source-CFG terminator references an absent edge")?;
                if &edge.from != block_id
                    || !receipt.reachable_blocks.contains_key(&edge.to)
                    || !referenced_edges.insert(edge_id.clone())
                {
                    return Err(
                        "ManagedAny source-CFG edge endpoint or reference is inconsistent".into(),
                    );
                }
            }
        }
        if referenced_edges.len() != receipt.reachable_edges.len() {
            return Err(
                "ManagedAny source-CFG has an edge outside its reachable terminators".into(),
            );
        }
        let mut reachable = BTreeSet::new();
        let mut worklist = VecDeque::from([receipt.entry.clone()]);
        while let Some(block) = worklist.pop_front() {
            if !reachable.insert(block.clone()) {
                continue;
            }
            let source_block = receipt
                .reachable_blocks
                .get(&block)
                .ok_or("ManagedAny source-CFG traversal reached an absent block")?;
            for edge_id in source_block.terminator.outgoing_edges() {
                let edge = receipt
                    .reachable_edges
                    .get(edge_id)
                    .ok_or("ManagedAny source-CFG traversal found an absent edge")?;
                worklist.push_back(edge.to.clone());
            }
        }
        if reachable.len() != receipt.reachable_blocks.len() {
            return Err("ManagedAny source-CFG snapshot contains unreachable blocks".into());
        }

        let mut source_to_mir = BTreeMap::new();
        let mut mapped_mir_blocks = BTreeSet::new();
        for (source_id, source_block) in &receipt.reachable_blocks {
            let source_role = source_id
                .0
                 .0
                .rsplit_once("/cfg:block:")
                .map(|(_, role)| role)
                .ok_or("ManagedAny source-CFG block has no canonical role")?;
            let mir_block = if source_role == "entry" {
                function.entry.clone()
            } else {
                let construct_target = matches!(
                    source_role,
                    "if-then" | "if-else" | "iflet-then" | "iflet-else" | "match-arm"
                ) || source_role.ends_with("-body")
                    || source_role.ends_with("-header");
                let node = if construct_target {
                    let incoming = receipt
                        .reachable_edges
                        .values()
                        .filter(|edge| &edge.to == source_id)
                        .find(|edge| {
                            matches!(
                                edge.kind,
                                crate::core::cfg::EdgeKind::Then
                                    | crate::core::cfg::EdgeKind::Else
                                    | crate::core::cfg::EdgeKind::MatchArm
                                    | crate::core::cfg::EdgeKind::LoopBody
                                    | crate::core::cfg::EdgeKind::Fallthrough
                                    | crate::core::cfg::EdgeKind::Backedge
                            )
                        })
                        .ok_or(
                            "ManagedAny source-CFG control block has no incoming construct edge",
                        )?;
                    &incoming.source.node
                } else {
                    &source_block.source.node
                };
                MirBlockId::new(format!("bb:{}:{}", source_role.replace('-', "."), node.0))
                    .map_err(|error| error.to_string())?
            };
            if !function.blocks.contains_key(&mir_block)
                || !mapped_mir_blocks.insert(mir_block.clone())
            {
                return Err("ManagedAny Checker block has no unique canonical MIR block".into());
            }
            source_to_mir.insert(source_id.clone(), mir_block);
        }
        if mapped_mir_blocks != function.blocks.keys().cloned().collect() {
            return Err(
                "ManagedAny source-CFG block identities do not cover MIR blocks exactly".into(),
            );
        }
        if source_to_mir.get(&receipt.entry) != Some(&function.entry) {
            return Err(
                "ManagedAny Checker source-CFG entry does not map to the MIR entry block".into(),
            );
        }

        let source_edge_id_to_mir = |source_edge: &crate::core::cfg::EdgeId| {
            let edge = receipt
                .reachable_edges
                .get(source_edge)
                .ok_or("ManagedAny source-CFG terminator references an absent edge")?;
            let encoded_role = edge
                .id
                .0
                 .0
                .rsplit_once("/cfg:edge:")
                .map(|(_, role)| role)
                .ok_or("ManagedAny source-CFG edge has no canonical role")?;
            let mir_role = encoded_role.replace('-', ".");
            let edge_node = if encoded_role.contains("join") {
                &receipt
                    .reachable_blocks
                    .get(&edge.to)
                    .ok_or("ManagedAny source-CFG join edge targets an absent block")?
                    .source
                    .node
            } else {
                &edge.source.node
            };
            super::MirEdgeId::new(format!("edge:{mir_role}:{}", edge_node.0))
                .map_err(|error| error.to_string())
        };
        let mut expected_edges = BTreeMap::<super::MirEdgeId, (MirBlockId, MirBlockId)>::new();
        for edge in receipt.reachable_edges.values() {
            let mir_edge = source_edge_id_to_mir(&edge.id)?;
            let from = source_to_mir
                .get(&edge.from)
                .cloned()
                .ok_or("ManagedAny source-CFG edge has no mapped MIR source")?;
            let to = source_to_mir
                .get(&edge.to)
                .cloned()
                .ok_or("ManagedAny source-CFG edge has no mapped MIR target")?;
            if expected_edges.insert(mir_edge, (from, to)).is_some() {
                return Err("ManagedAny source-CFG edge roles collide in canonical MIR".into());
            }
        }

        let mut actual_edges = BTreeMap::<super::MirEdgeId, (MirBlockId, MirBlockId)>::new();
        let mut actual_branches = Vec::new();
        let mut actual_switches = Vec::new();
        for block in function.blocks.values() {
            let mut insert = |edge: &super::MirEdgeId, target: &MirBlockId| {
                actual_edges
                    .insert(edge.clone(), (block.id.clone(), target.clone()))
                    .is_none()
            };
            match &block.terminator {
                MirTerminator::Goto { edge, target, .. } => {
                    if !insert(edge, target) {
                        return Err("ManagedAny MIR repeats a CFG edge identity".into());
                    }
                }
                MirTerminator::Branch {
                    condition,
                    then_edge,
                    then_target,
                    else_edge,
                    else_target,
                    ..
                } => {
                    if !insert(then_edge, then_target) || !insert(else_edge, else_target) {
                        return Err("ManagedAny MIR repeats a CFG branch edge identity".into());
                    }
                    actual_branches.push((block.id.clone(), condition.clone()));
                }
                MirTerminator::Switch { scrutinee, arms }
                | MirTerminator::SwitchMove { scrutinee, arms } => {
                    for arm in arms {
                        if !insert(&arm.edge, &arm.target) {
                            return Err("ManagedAny MIR repeats a switch edge identity".into());
                        }
                    }
                    actual_switches.push((block.id.clone(), scrutinee.clone()));
                }
                MirTerminator::Return { .. }
                | MirTerminator::Trap { .. }
                | MirTerminator::Fault { .. }
                | MirTerminator::Unreachable => {}
            }
        }
        if actual_edges != expected_edges {
            return Err(
                "ManagedAny MIR edge identities/targets disagree with the exact Checker CFG".into(),
            );
        }

        let mut expected_branches = Vec::new();
        let mut expected_switches = Vec::new();
        for (source_id, block) in &receipt.reachable_blocks {
            let mir_block = source_to_mir
                .get(source_id)
                .cloned()
                .ok_or("ManagedAny source-CFG selector block has no MIR mapping")?;
            match &block.terminator {
                crate::core::cfg::Terminator::Branch { condition, .. } => {
                    let value = MirValueId::new(format!("expr:{}", condition.0))
                        .map_err(|error| error.to_string())?;
                    let descriptor = function
                        .values
                        .get(&value)
                        .ok_or("ManagedAny Checker branch condition has no canonical MIR value")?;
                    if descriptor.ty != crate::core::ir::primitive_type_id(PrimitiveType::Bool) {
                        return Err(
                            "ManagedAny Checker branch condition is not the exact Bool primitive"
                                .into(),
                        );
                    }
                    expected_branches.push((mir_block, value));
                }
                crate::core::cfg::Terminator::Match { scrutinee, .. } => {
                    expected_switches.push((
                        mir_block,
                        MirValueId::new(format!("expr:{}", scrutinee.0))
                            .map_err(|error| error.to_string())?,
                    ));
                }
                _ => {}
            }
        }
        expected_branches.sort();
        actual_branches.sort();
        expected_switches.sort();
        actual_switches.sort();
        if actual_branches != expected_branches || actual_switches != expected_switches {
            return Err("ManagedAny MIR branch/match selector block and value identities disagree with Checker source expressions".into());
        }
        for (_, condition) in actual_branches {
            let value = function
                .values
                .get(&condition)
                .ok_or("ManagedAny MIR branch references an absent condition value")?;
            if value.ty != crate::core::ir::primitive_type_id(PrimitiveType::Bool) {
                return Err(
                    "ManagedAny MIR branch condition must use the exact Bool primitive".into(),
                );
            }
        }

        for (source_id, source_block) in &receipt.reachable_blocks {
            let mir_id = source_to_mir
                .get(source_id)
                .ok_or("ManagedAny source-CFG terminator block has no MIR mapping")?;
            let mir_block = function
                .blocks
                .get(mir_id)
                .ok_or("ManagedAny source-CFG terminator maps to an absent MIR block")?;
            match (&source_block.terminator, &mir_block.terminator) {
                (
                    crate::core::cfg::Terminator::Goto { edge }
                    | crate::core::cfg::Terminator::Break { edge }
                    | crate::core::cfg::Terminator::Continue { edge },
                    MirTerminator::Goto {
                        edge: actual_edge,
                        target,
                        ..
                    },
                ) => {
                    let source_edge = receipt
                        .reachable_edges
                        .get(edge)
                        .ok_or("ManagedAny Checker Goto edge is absent")?;
                    if source_edge_id_to_mir(edge)? != *actual_edge
                        || source_to_mir.get(&source_edge.to) != Some(target)
                    {
                        return Err(
                            "ManagedAny MIR Goto disagrees with its exact Checker edge and target"
                                .into(),
                        );
                    }
                }
                (
                    crate::core::cfg::Terminator::Branch {
                        condition,
                        then_edge,
                        else_edge,
                    },
                    MirTerminator::Branch {
                        condition: actual_condition,
                        then_edge: actual_then_edge,
                        else_edge: actual_else_edge,
                        ..
                    },
                ) => {
                    let expected_condition = MirValueId::new(format!("expr:{}", condition.0))
                        .map_err(|error| error.to_string())?;
                    if actual_condition != &expected_condition
                        || source_edge_id_to_mir(then_edge)? != *actual_then_edge
                        || source_edge_id_to_mir(else_edge)? != *actual_else_edge
                    {
                        return Err("ManagedAny MIR Branch disagrees with its exact Checker condition or edge roles".into());
                    }
                }
                (
                    crate::core::cfg::Terminator::Match { scrutinee, arms },
                    MirTerminator::Switch {
                        scrutinee: actual_scrutinee,
                        arms: actual_arms,
                    }
                    | MirTerminator::SwitchMove {
                        scrutinee: actual_scrutinee,
                        arms: actual_arms,
                    },
                ) => {
                    let expected_scrutinee = MirValueId::new(format!("expr:{}", scrutinee.0))
                        .map_err(|error| error.to_string())?;
                    let expected_arm_edges = arms
                        .iter()
                        .map(source_edge_id_to_mir)
                        .collect::<Result<Vec<_>, _>>()?;
                    let actual_arm_edges = actual_arms
                        .iter()
                        .map(|arm| arm.edge.clone())
                        .collect::<Vec<_>>();
                    let source_arm_receipts = receipt
                        .match_arms
                        .get(source_id)
                        .ok_or("ManagedAny Checker Match has no sealed pattern/case receipt")?;
                    if source_arm_receipts.len() != arms.len()
                        || source_arm_receipts.len() != actual_arms.len()
                    {
                        return Err(
                            "ManagedAny Checker Match arm receipt has incomplete case coverage"
                                .into(),
                        );
                    }
                    let mut expected_arm_cases = Vec::with_capacity(source_arm_receipts.len());
                    for ((source_edge_id, arm_receipt), actual_arm) in
                        arms.iter().zip(source_arm_receipts).zip(actual_arms)
                    {
                        let source_edge = receipt
                            .reachable_edges
                            .get(source_edge_id)
                            .ok_or("ManagedAny Checker Match arm edge is absent")?;
                        if arm_receipt.edge != *source_edge_id
                            || arm_receipt.source_arm != source_edge.source.node
                            || arm_receipt.guarded
                        {
                            return Err(
                                "ManagedAny Checker Match arm identity or guard is not admitted"
                                    .into(),
                            );
                        }
                        let expected_case = match &arm_receipt.case {
                            crate::core::ManagedAnyCfgMatchCase::Default => {
                                super::MirSwitchCase::Default
                            }
                            crate::core::ManagedAnyCfgMatchCase::Literal(literal) => {
                                super::MirSwitchCase::Literal(literal.clone())
                            }
                            crate::core::ManagedAnyCfgMatchCase::Variant(variant) => {
                                super::MirSwitchCase::Variant(variant.clone())
                            }
                            crate::core::ManagedAnyCfgMatchCase::Unsupported => {
                                return Err(
                                    "ManagedAny Checker Match pattern has no supported exact MIR switch case"
                                        .into(),
                                )
                            }
                        };
                        if actual_arm.edge != source_edge_id_to_mir(source_edge_id)?
                            || actual_arm.case != expected_case
                        {
                            return Err(
                                "ManagedAny MIR Switch case/edge disagrees with its exact Checker Match arm"
                                    .into(),
                            );
                        }
                        expected_arm_cases.push(expected_case);
                    }
                    if actual_scrutinee != &expected_scrutinee
                        || actual_arm_edges != expected_arm_edges
                        || actual_arms
                            .iter()
                            .map(|arm| arm.case.clone())
                            .collect::<Vec<_>>()
                            != expected_arm_cases
                    {
                        return Err("ManagedAny MIR Switch disagrees with its exact Checker scrutinee, cases, or arm edges".into());
                    }
                }
                (
                    crate::core::cfg::Terminator::Return { value, .. },
                    MirTerminator::Return {
                        value: actual_value,
                    },
                ) => {
                    let expected_value = value
                        .as_ref()
                        .map(|node| MirValueId::new(format!("expr:{}", node.0)))
                        .transpose()
                        .map_err(|error| error.to_string())?;
                    if actual_value != &expected_value {
                        return Err("ManagedAny MIR Return value disagrees with its exact Checker return node".into());
                    }
                }
                (
                    crate::core::cfg::Terminator::Diverge,
                    MirTerminator::Trap { .. }
                    | MirTerminator::Fault { .. }
                    | MirTerminator::Unreachable,
                )
                | (crate::core::cfg::Terminator::Unreachable, MirTerminator::Unreachable) => {}
                _ => {
                    return Err("ManagedAny MIR terminator kind disagrees with the mapped Checker CFG block".into());
                }
            }
        }

        // Bind every sealed instruction to an origin point in the exact
        // Checker block that maps to its MIR block. This also prevents moving
        // an instruction from one sibling arm to another while retaining its
        // private full-body seal.
        for (source_id, source_block) in &receipt.reachable_blocks {
            let mir_id = source_to_mir
                .get(source_id)
                .ok_or("ManagedAny Checker block has no MIR block mapping")?;
            let mir_block = function
                .blocks
                .get(mir_id)
                .ok_or("ManagedAny Checker block maps to an absent MIR block")?;
            for instruction in &mir_block.instructions {
                let origin = seal
                    .instructions
                    .iter()
                    .find(|(block, _, sealed)| block == mir_id && sealed.id == instruction.id)
                    .map(|(_, origin, _)| origin)
                    .ok_or("ManagedAny instruction is missing from its private body seal")?;
                let checker_point_matches = source_block
                    .points
                    .iter()
                    .any(|point| &point.source.node == origin);
                let synthesized_owned_string_drop = origin == &function.owner
                    && matches!(
                        &instruction.kind,
                        MirInstructionKind::Drop { value }
                            if self.managed_any_is_string(function, value)
                    );
                if !checker_point_matches && !synthesized_owned_string_drop {
                    return Err(format!(
                        "ManagedAny instruction '{}' has no Checker origin point in its own source block",
                        instruction.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whole-function owner accounting is shared by every consumer, including
    /// tuple-only and cross-frame callables with no direct GetAny operation.
    pub fn validate_managed_any_function(&self, function: &MirFunction) -> Result<(), String> {
        let static_closure_values = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match &instruction.kind {
                MirInstructionKind::ClosureBind { function_value, .. } => {
                    Some(function_value.clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        self.validate_managed_any_function_with_static_closure_values(
            function,
            &static_closure_values,
        )
    }

    /// A Checker-lowered `ClosureBind` produces a static code identity, not an
    /// owned runtime Any payload. The local owner proof excludes those output
    /// identities; before execution or emission, the whole-program preflight
    /// must still validate the private closure receipt and invocation census.
    pub(super) fn validate_managed_any_function_with_static_closure_values(
        &self,
        function: &MirFunction,
        static_closure_values: &BTreeSet<MirValueId>,
    ) -> Result<(), String> {
        let v2_root_type = super::types::ordinary_map_owned_any_v2_type_id();
        if function.result == v2_root_type
            || function.parameters.iter().any(|parameter| {
                function
                    .values
                    .get(parameter)
                    .is_some_and(|value| value.ty == v2_root_type)
            })
        {
            return Err(
                "AnyOwner-v2 callable root parameters/returns need an exact Checker boundary receipt"
                    .into(),
            );
        }
        let managed_values = managed_any_values_for_function(function, self, static_closure_values);
        let any_in_signature = self.contains_dynamic_or_managed_any(&function.result)
            || function.parameters.iter().any(|parameter| {
                function
                    .values
                    .get(parameter)
                    .is_some_and(|value| self.contains_dynamic_or_managed_any(&value.ty))
            });
        if managed_values.is_empty()
            && !any_in_signature
            && function.ownership.checker_managed_any.is_none()
        {
            return Ok(());
        }
        for (index, parameter) in function.parameters.iter().enumerate() {
            let is_any = function
                .values
                .get(parameter)
                .is_some_and(|value| self.contains_dynamic_or_managed_any(&value.ty));
            let permission = function
                .parameter_permissions
                .as_ref()
                .and_then(|permissions| permissions.get(index))
                .copied()
                .flatten();
            if is_any
                && matches!(
                    permission,
                    Some(crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate)
                )
            {
                return Err(format!(
                    "ManagedAny parameter '{}' has a borrowed permission without a Checker loan/escape receipt",
                    parameter
                ));
            }
        }
        let seal = function
            .ownership
            .checker_managed_any
            .as_ref()
            .ok_or("ManagedAny tuple/parameter has no private Checker seal")?;
        let sealed_runtime_values = seal
            .managed_values
            .difference(static_closure_values)
            .cloned()
            .collect::<BTreeSet<_>>();
        if seal.generic_identity.is_some() && seal.generic_trap_cleanup.is_some() {
            return Err("ManagedAny function cannot carry two generic profile seals".into());
        }
        let observed = function
            .blocks
            .values()
            .flat_map(|block| {
                block
                    .instructions
                    .iter()
                    .map(move |instruction| (block.id.clone(), instruction.clone()))
            })
            .collect::<Vec<_>>();
        let actual_terminators = function
            .blocks
            .values()
            .map(|block| (block.id.clone(), block.terminator.clone()))
            .collect::<Vec<_>>();
        if seal.owner != function.owner
            || seal.parameters != function.parameters
            || seal.block_parameters
                != function
                    .blocks
                    .values()
                    .map(|block| (block.id.clone(), block.parameters.clone()))
                    .collect::<Vec<_>>()
            || seal.parameter_permissions != function.parameter_permissions
            || seal.result != function.result
            || sealed_runtime_values != managed_values
            || seal.source_cfg != function.ownership.checker_managed_any_cfg_receipt
            || seal.terminators != actual_terminators
            || seal.value_types
                != function
                    .values
                    .iter()
                    .map(|(id, value)| (id.clone(), value.ty.clone()))
                    .collect()
            || observed
                != seal
                    .instructions
                    .iter()
                    .map(|(block, _, instruction)| (block.clone(), instruction.clone()))
                    .collect::<Vec<_>>()
        {
            return Err("ManagedAny private Checker seal disagrees with exact value/operation/parameter/return coverage".into());
        }
        self.validate_managed_any_source_cfg(function, seal)?;
        if let Some(identity) = &seal.generic_identity {
            self.validate_managed_any_generic_identity_shape(function, identity)?;
        }
        if let Some(trap_cleanup) = &seal.generic_trap_cleanup {
            self.validate_managed_any_generic_trap_cleanup_shape(function, trap_cleanup)?;
        }
        self.validate_managed_any_cfg_flow(function, &managed_values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ir::ResolvedType;
    use crate::core::mir::reference::MirProgram;
    use crate::core::mir::types::{MirFieldDesc, MirTypeDesc, MirVariantDesc};
    fn checked(source: &str) -> crate::core::CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex managed Any fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse managed Any fixture");
        crate::core::check_program(&file).expect("check managed Any fixture")
    }

    #[test]
    fn ordinary_scalar_functions_skip_managed_any_receipt_gate_but_any_markers_do_not() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    let one = 1
    one + 2
}
"#,
        ))
        .expect("ordinary scalar MIR must not require a ManagedAny Checker seal");
        let scalar = get_main(&program);
        assert!(scalar.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(&instruction.kind, MirInstructionKind::Const { .. }))
                && block.instructions.iter().any(|instruction| {
                    matches!(&instruction.kind, MirInstructionKind::Binary { .. })
                })
        }));
        assert!(validate_managed_any_receipts(&scalar).is_empty());

        let source = scalar
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::Const { result, .. } => Some(result.clone()),
                _ => None,
            })
            .expect("scalar Const operand");
        let mut forged_opcode = scalar.clone();
        let entry_id = forged_opcode.entry.clone();
        let entry = forged_opcode
            .blocks
            .get_mut(&entry_id)
            .expect("scalar entry block");
        entry.instructions.push(MirInstruction {
            id: MirInstructionId::new("forged:bare-managed-any-drop").expect("forged operation id"),
            kind: MirInstructionKind::ManagedAny {
                result: None,
                source,
                operation: MirManagedAnyOperation::Drop,
                contract: None,
            },
        });
        assert!(validate_managed_any_receipts(&forged_opcode)
            .iter()
            .any(|error| error.message.contains("private Checker seal is missing")));

        let mut forged_tuple_marker = scalar;
        let marker =
            MirValueId::new("forged:ordinary-map-get-any-tuple").expect("tuple marker value id");
        forged_tuple_marker.values.insert(
            marker.clone(),
            super::super::MirValue {
                id: marker,
                ty: ordinary_map_get_any_tuple_i32_string_v1_type_id(),
            },
        );
        assert!(validate_managed_any_receipts(&forged_tuple_marker)
            .iter()
            .any(|error| error.message.contains("private Checker seal is missing")));
    }

    #[test]
    fn managed_any_source_cfg_receipt_binds_match_arm_cases_in_checker_order() {
        let checked = checked(
            r#"
func main() -> i32 {
    let selector = 5
    match selector {
        5 => 17,
        _ => 23,
    }
}
"#,
        );
        let owner = NodeId("function:main".into());
        let cfg = checked.callable_cfg(&owner).expect("main source CFG");
        let body = checked.resolved_body(&owner).expect("main ResolvedBody");
        let receipt = crate::core::ManagedAnyCfgReceipt::from_cfg(cfg, body);
        assert_eq!(receipt.match_arms.len(), 1);
        let arms = receipt
            .match_arms
            .values()
            .next()
            .expect("one Checker match with its sealed arm sequence");
        assert_eq!(arms.len(), 2);
        assert!(matches!(
            arms[0].case,
            crate::core::ManagedAnyCfgMatchCase::Literal(ResolvedLiteral::Int(5))
        ));
        assert!(matches!(
            arms[1].case,
            crate::core::ManagedAnyCfgMatchCase::Default
        ));
        assert_eq!(cfg.edges[&arms[0].edge].source.node, arms[0].source_arm);
        assert!(!arms[0].guarded && !arms[1].guarded);
    }

    const SOURCE: &str = r#"
func carry<T>(value: T) -> T { value }
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "k\0🧪", "owned\0🧪值")
    let pair = map_get(root, "k\0🧪")
    drop(empty)
    drop(root)

    let found = pair.0
    let payload = pair.1
    let carried = carry(payload)
    let equal = if found { carried == "owned\0🧪值" } else { false }
    println(found)
    println(equal)
    drop(pair)
    drop(payload)
    drop(carried)

    let number_empty = map_new()
    let number_root = map_set(number_empty, "zero", 0)
    let number_pair = map_get(number_root, "zero")
    drop(number_empty)
    drop(number_root)

    let number_found = number_pair.0
    let zero_payload = number_pair.1
    let zero_carried = carry(zero_payload)
    let zero = if number_found { zero_carried } else { 0 }
    println(number_found)
    println(zero)
    drop(number_pair)
    drop(zero_payload)
    drop(zero_carried)
    0
}
"#;

    #[test]
    fn managed_any_println_uses_checker_dynamic_any_borrow_then_drops_only_its_temporary() {
        let program = MirProgram::from_checked_program(&checked(SOURCE))
            .expect("Checker-sealed Any println fixture");
        let function = get_main(&program);
        let catalog = program.type_catalog();
        let managed_type = managed_any_i32_string_v1_type_id();
        let unit_type = crate::core::ir::primitive_type_id(PrimitiveType::Unit);
        let mut found = false;
        for block in function.blocks.values() {
            for (index, instruction) in block.instructions.iter().enumerate() {
                let MirInstructionKind::ManagedAny {
                    result: Some(result),
                    source,
                    operation: MirManagedAnyOperation::Println,
                    contract: Some(contract),
                } = &instruction.kind
                else {
                    continue;
                };
                found = true;
                assert_eq!(contract.owner, function.owner);
                assert_eq!(
                    contract.origin,
                    NodeId("function:main/node:expr.call@unknown-source:31:5-31:18".into())
                );
                assert_eq!(&contract.source, source);
                assert_eq!(contract.result.as_ref(), Some(result));
                assert_eq!(contract.source_type, managed_type);
                assert_eq!(contract.target_type.as_ref(), Some(&unit_type));
                assert_eq!(contract.checker_conversion, None);
                assert_eq!(contract.expected_kind, None);
                let checker_source = contract
                    .checker_source_type
                    .as_ref()
                    .expect("exact Checker Any argument identity");
                let descriptor = catalog
                    .get(checker_source)
                    .expect("Checker source TypeDesc");
                assert_eq!(descriptor.kind, MirTypeKind::DynamicAny);
                assert_eq!(
                    catalog.canonical_managed_any_type(checker_source),
                    managed_type
                );
                assert_eq!(
                    function.values.get(source).map(|value| &value.ty),
                    Some(&managed_type)
                );
                assert_eq!(
                    function.values.get(result).map(|value| &value.ty),
                    Some(&unit_type)
                );
                assert!(matches!(
                    block.instructions.get(index + 1).map(|next| &next.kind),
                    Some(MirInstructionKind::ManagedAny {
                        source: dropped,
                        operation: MirManagedAnyOperation::Drop,
                        ..
                    }) if dropped == source
                ));
                assert!(
                    !block.instructions.iter().any(|other| matches!(
                        &other.kind,
                        MirInstructionKind::BuiltinCall {
                            kind: super::super::types::MirBuiltinKind::PrintlnInt
                                | super::super::types::MirBuiltinKind::PrintlnString,
                            arguments,
                            ..
                        } if arguments.contains(source)
                    )),
                    "Any output must not escape as a scalar/string BuiltinCall"
                );
                catalog
                    .validate_managed_any_contract(
                        &function,
                        source,
                        Some(result),
                        MirManagedAnyOperation::Println,
                        Some(contract),
                    )
                    .expect("the borrowed print contract is exact");
            }
        }
        assert!(
            found,
            "the Any println must lower to its dedicated borrow operation"
        );
        catalog
            .validate_managed_any_function(&function)
            .expect("the complete Checker body and owner flow are sealed");
    }

    #[test]
    fn managed_any_println_rejects_dropping_the_same_snapshot_before_the_borrow() {
        let program = MirProgram::from_checked_program(&checked(SOURCE))
            .expect("Checker-sealed Any println fixture");
        let catalog = program.type_catalog();
        let valid_function = get_main(&program);
        catalog
            .validate_managed_any_function(&valid_function)
            .expect("the live-println-drop control shape is admitted");

        let mut forged_function = valid_function;
        let mut dropped_snapshot = None;
        for block in forged_function.blocks.values_mut() {
            for index in 0..block.instructions.len().saturating_sub(1) {
                let Some((source, checker_source)) =
                    block
                        .instructions
                        .get(index)
                        .and_then(|instruction| match &instruction.kind {
                            MirInstructionKind::ManagedAny {
                                source,
                                operation: MirManagedAnyOperation::Println,
                                contract: Some(contract),
                                ..
                            } => Some((source.clone(), contract.checker_source_type.clone())),
                            _ => None,
                        })
                else {
                    continue;
                };
                let checker_source = checker_source.expect("sealed Checker source TypeId");
                if !catalog
                    .get(&checker_source)
                    .is_some_and(|descriptor| descriptor.kind == MirTypeKind::DynamicAny)
                {
                    continue;
                }
                assert!(matches!(
                    block.instructions.get(index + 1).map(|next| &next.kind),
                    Some(MirInstructionKind::ManagedAny {
                        source: dropped,
                        operation: MirManagedAnyOperation::Drop,
                        ..
                    }) if dropped == &source
                ));
                assert!(
                    dropped_snapshot.replace(source.clone()).is_none(),
                    "the fixture has one exact DynamicAny Println snapshot"
                );
                // This is the same argument snapshot owner that Println borrows,
                // not its still-live source local or an independently-owned clone.
                block.instructions.swap(index, index + 1);
                break;
            }
            if dropped_snapshot.is_some() {
                break;
            }
        }
        let dropped_snapshot = dropped_snapshot.expect("DynamicAny Println snapshot");

        let managed_values = forged_function
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("the original body has an exact Checker seal")
            .managed_values
            .clone();
        assert!(managed_values.contains(&dropped_snapshot));
        let flow_error = catalog
            .validate_managed_any_cfg_flow(&forged_function, &managed_values)
            .expect_err("the exact owner is dead when Println attempts to borrow it");
        assert!(
            flow_error.contains("used after transfer, drop, or before creation"),
            "unexpected owner-flow rejection: {flow_error}"
        );

        let mut forged_program = program.clone();
        forged_program.replace_function_for_test_only(forged_function);
        let interpreter =
            crate::core::mir::reference::MirReferenceInterpreter::new(&forged_program);
        let error = interpreter
            .execute_with_output(&NodeId("function:main".into()), &[])
            .expect_err("the source-bound MIR snapshot cannot be dropped before Println");
        assert!(error.to_string().contains("ManagedAny"), "{error}");
        assert_eq!(
            interpreter.captured_output(),
            "",
            "whole-program receipt preflight rejects the forged body before earlier Println effects"
        );
    }

    #[test]
    fn managed_any_println_preserves_the_string_payload_tag_and_source_owner() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "k", "string payload")
    let pair = map_get(root, "k")
    drop(empty)
    drop(root)
    let payload = pair.1
    println(payload)
    drop(pair)
    drop(payload)
    0
}
"#,
        ))
        .expect("Checker-sealed String-tag Any println fixture");
        let function = get_main(&program);
        let catalog = program.type_catalog();
        let unit_type = crate::core::ir::primitive_type_id(PrimitiveType::Unit);
        let print = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::ManagedAny {
                    result: Some(result),
                    source,
                    operation: MirManagedAnyOperation::Println,
                    contract: Some(contract),
                } => Some((source, result, contract)),
                _ => None,
            })
            .expect("Any String println uses the typed operation");
        assert_eq!(print.2.source_type, managed_any_i32_string_v1_type_id());
        assert_eq!(print.2.target_type.as_ref(), Some(&unit_type));
        assert!(catalog
            .get(
                print
                    .2
                    .checker_source_type
                    .as_ref()
                    .expect("Any source type")
            )
            .is_some_and(|descriptor| descriptor.kind == MirTypeKind::DynamicAny));
        assert!(function
            .blocks
            .values()
            .any(|block| block.instructions.windows(2).any(|pair| {
                matches!(
                    &pair[0].kind,
                    MirInstructionKind::ManagedAny {
                        source,
                        operation: MirManagedAnyOperation::Println,
                        ..
                    } if source == print.0
                ) && (matches!(
                    &pair[1].kind,
                    MirInstructionKind::Drop { value } if value == print.0
                ) || matches!(
                    &pair[1].kind,
                    MirInstructionKind::ManagedAny {
                        source,
                        operation: MirManagedAnyOperation::Drop,
                        ..
                    } if source == print.0
                ))
            })));
        catalog
            .validate_managed_any_function(&function)
            .expect("the borrowed String-tag print leaves the source owner available");
    }

    #[test]
    fn managed_any_println_rejects_missing_or_non_dynamic_checker_source_identity() {
        let program = MirProgram::from_checked_program(&checked(SOURCE))
            .expect("Checker-sealed Any println fixture");
        let mut function = get_main(&program);
        let unit_type = crate::core::ir::primitive_type_id(PrimitiveType::Unit);
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let (source, result, contract) = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::ManagedAny {
                    result: Some(result),
                    source,
                    operation: MirManagedAnyOperation::Println,
                    contract: Some(contract),
                } => Some((source.clone(), result.clone(), contract.clone())),
                _ => None,
            })
            .expect("Checker-sealed Any println");
        let catalog = program.type_catalog();
        let mut missing = contract.clone();
        missing.checker_source_type = None;
        assert!(catalog
            .validate_managed_any_contract(
                &function,
                &source,
                Some(&result),
                MirManagedAnyOperation::Println,
                Some(&missing),
            )
            .is_err());
        let mut wrong = contract;
        wrong.checker_source_type = Some(i32_type);
        assert!(catalog
            .validate_managed_any_contract(
                &function,
                &source,
                Some(&result),
                MirManagedAnyOperation::Println,
                Some(&wrong),
            )
            .is_err());
        assert_eq!(
            function.values.get(&result).map(|value| &value.ty),
            Some(&unit_type)
        );
        let forged_instruction = function
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Println,
                        ..
                    }
                )
            })
            .expect("Any println to forge");
        let MirInstructionKind::ManagedAny {
            contract: Some(actual),
            ..
        } = &mut forged_instruction.kind
        else {
            unreachable!()
        };
        actual.checker_source_type = Some(crate::core::ir::primitive_type_id(PrimitiveType::I32));
        assert!(catalog.validate_managed_any_function(&function).is_err());

        // Replacing the exact Any operation with a type-only builtin must be
        // rejected by the Checker body seal before any output can occur.
        let mut forged_builtin = get_main(&program);
        let block = forged_builtin
            .blocks
            .values_mut()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::ManagedAny {
                            operation: MirManagedAnyOperation::Println,
                            ..
                        }
                    )
                })
            })
            .expect("println block");
        let print = block
            .instructions
            .iter_mut()
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Println,
                        ..
                    }
                )
            })
            .expect("dedicated Any output instruction");
        let (source, result) = match &print.kind {
            MirInstructionKind::ManagedAny {
                source,
                result: Some(result),
                operation: MirManagedAnyOperation::Println,
                ..
            } => (source.clone(), result.clone()),
            _ => unreachable!(),
        };
        print.kind = MirInstructionKind::BuiltinCall {
            result,
            kind: super::super::types::MirBuiltinKind::PrintlnInt,
            arguments: vec![source],
            string_field_contract: None,
        };
        assert!(catalog
            .validate_managed_any_function(&forged_builtin)
            .is_err());
    }

    fn get_main(program: &MirProgram) -> MirFunction {
        program
            .functions()
            .values()
            .find(|function| function.owner.0.ends_with("main"))
            .expect("GetAny fixture main")
            .clone()
    }

    fn managed_any_flow_function(blocks: Vec<crate::core::mir::MirBlock>) -> MirFunction {
        let owner = MirValueId::new("param:owner").expect("owner value id");
        let condition = MirValueId::new("const:condition").expect("condition value id");
        MirFunction {
            owner: NodeId("managed-any-flow-test".into()),
            parameters: vec![owner.clone()],
            parameter_permissions: Some(vec![None]),
            result: crate::core::ir::primitive_type_id(PrimitiveType::Unit),
            entry: MirBlockId::new("bb.entry").expect("entry block id"),
            values: BTreeMap::from([
                (
                    owner.clone(),
                    crate::core::mir::MirValue {
                        id: owner,
                        ty: managed_any_i32_string_v1_type_id(),
                    },
                ),
                (
                    condition.clone(),
                    crate::core::mir::MirValue {
                        id: condition,
                        ty: crate::core::ir::primitive_type_id(PrimitiveType::Bool),
                    },
                ),
            ]),
            blocks: blocks
                .into_iter()
                .map(|block| (block.id.clone(), block))
                .collect(),
            contracts: Vec::new(),
            proof_worklist: None,
            ownership: crate::core::mir::MirOwnershipSummary::default(),
        }
    }

    /// Build a small branch whose then arm creates a read-arm String value.
    /// The source fixture used to obtain this shape through an explicit
    /// `Any as string` cast, but that cast is intentionally outside the
    /// resolved MIR slice.  The CFG tests only need the path-local String
    /// ownership shape, so keep the test at canonical MIR level instead.
    fn managed_any_branch_string_fixture(
        drop_string_in_then: bool,
    ) -> (MirFunction, BTreeSet<MirValueId>, MirValueId) {
        let owner = MirValueId::new("param:owner").expect("owner value id");
        let string = MirValueId::new("local:branch-string").expect("string value id");
        let mut then_instructions = vec![MirInstruction {
            id: MirInstructionId::new("inst:branch-string.const").expect("const instruction id"),
            kind: MirInstructionKind::Const {
                result: string.clone(),
                literal: ResolvedLiteral::String("branch".into()),
            },
        }];
        if drop_string_in_then {
            then_instructions.push(flow_test_drop(
                "local:branch-string",
                "inst:branch-string.drop",
            ));
        }
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").expect("condition value id"),
                then_edge: crate::core::mir::MirEdgeId::new("edge:branch.then")
                    .expect("then edge id"),
                then_target: MirBlockId::new("bb.then").expect("then block id"),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:branch.else")
                    .expect("else edge id"),
                else_target: MirBlockId::new("bb.else").expect("else block id"),
                else_arguments: Vec::new(),
            },
        );
        let then_block = flow_test_block(
            "bb.then",
            then_instructions,
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:then.join")
                    .expect("then join edge id"),
                target: MirBlockId::new("bb.join").expect("join block id"),
                arguments: Vec::new(),
            },
        );
        let else_block = flow_test_block(
            "bb.else",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:else.join")
                    .expect("else join edge id"),
                target: MirBlockId::new("bb.join").expect("join block id"),
                arguments: Vec::new(),
            },
        );
        let join_block =
            flow_test_block("bb.join", Vec::new(), MirTerminator::Return { value: None });
        let mut function =
            managed_any_flow_function(vec![entry, then_block, else_block, join_block]);
        function.values.insert(
            string.clone(),
            crate::core::mir::MirValue {
                id: string.clone(),
                ty: crate::core::ir::primitive_type_id(PrimitiveType::String),
            },
        );
        (function, BTreeSet::from([owner]), string)
    }

    fn flow_test_block(
        id: &str,
        instructions: Vec<MirInstruction>,
        terminator: MirTerminator,
    ) -> crate::core::mir::MirBlock {
        crate::core::mir::MirBlock {
            id: MirBlockId::new(id).expect("block id"),
            parameters: Vec::new(),
            instructions,
            terminator,
        }
    }

    fn flow_test_drop(value: &str, id: &str) -> MirInstruction {
        MirInstruction {
            id: MirInstructionId::new(id).expect("instruction id"),
            kind: MirInstructionKind::Drop {
                value: MirValueId::new(value).expect("drop value"),
            },
        }
    }

    fn string_test_value(id: &str, ty: ResolvedTypeId) -> crate::core::mir::MirValue {
        let id = MirValueId::new(id).expect("test value id");
        crate::core::mir::MirValue { id, ty }
    }

    fn string_liveness_test_function(
        parameters: Vec<MirValueId>,
        values: Vec<crate::core::mir::MirValue>,
        blocks: Vec<crate::core::mir::MirBlock>,
    ) -> MirFunction {
        MirFunction {
            owner: NodeId("managed-any-string-liveness-test".into()),
            parameters,
            parameter_permissions: None,
            result: crate::core::ir::primitive_type_id(PrimitiveType::Unit),
            entry: MirBlockId::new("bb.entry").expect("entry block id"),
            values: values
                .into_iter()
                .map(|value| (value.id.clone(), value))
                .collect(),
            blocks: blocks
                .into_iter()
                .map(|block| (block.id.clone(), block))
                .collect(),
            contracts: Vec::new(),
            proof_worklist: None,
            ownership: crate::core::mir::MirOwnershipSummary::default(),
        }
    }

    fn string_liveness_test_successors(
        edges: &[(&str, &str)],
    ) -> BTreeMap<MirBlockId, Vec<ManagedAnyCfgSuccessor>> {
        let mut successors = BTreeMap::<MirBlockId, Vec<ManagedAnyCfgSuccessor>>::new();
        for (source, target) in edges {
            successors
                .entry(MirBlockId::new(*source).expect("source block id"))
                .or_default()
                .push(ManagedAnyCfgSuccessor {
                    target: MirBlockId::new(*target).expect("target block id"),
                    arguments: Vec::new(),
                });
        }
        successors
    }

    #[test]
    fn managed_any_string_liveness_tracks_clone_move_and_drop_identity() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    let text = "catalog"
    println(text)
    0
}
"#,
        ))
        .expect("String catalog MIR");
        let catalog = program.type_catalog();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        let peer = MirValueId::new("param:peer").unwrap();
        let alias = MirValueId::new("local:alias").unwrap();
        let equality = MirValueId::new("result:equality").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            vec![
                MirInstruction {
                    id: MirInstructionId::new("inst:clone.alias").unwrap(),
                    kind: MirInstructionKind::Clone {
                        result: alias.clone(),
                        source: peer.clone(),
                    },
                },
                flow_test_drop("param:peer", "inst:drop.original"),
                MirInstruction {
                    id: MirInstructionId::new("inst:compare.alias").unwrap(),
                    kind: MirInstructionKind::Binary {
                        result: equality.clone(),
                        op: ResolvedBinaryOp::Equal,
                        left: alias.clone(),
                        right: alias.clone(),
                    },
                },
                flow_test_drop("local:alias", "inst:drop.alias"),
            ],
            MirTerminator::Return { value: None },
        );
        let function = string_liveness_test_function(
            vec![peer.clone()],
            vec![
                string_test_value("param:peer", string_type.clone()),
                string_test_value("local:alias", string_type.clone()),
                string_test_value("result:equality", bool_type.clone()),
            ],
            vec![entry],
        );
        catalog
            .validate_managed_any_string_liveness(
                &function,
                &BTreeMap::from([(function.entry.clone(), Vec::new())]),
            )
            .expect("Clone creates an independent live String which survives Drop of its source");

        let moved_peer = MirValueId::new("param:peer").unwrap();
        let moved_alias = MirValueId::new("local:alias").unwrap();
        let stale_equality = MirValueId::new("result:stale").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            vec![
                MirInstruction {
                    id: MirInstructionId::new("inst:move.alias").unwrap(),
                    kind: MirInstructionKind::Move {
                        result: moved_alias.clone(),
                        source: moved_peer.clone(),
                    },
                },
                MirInstruction {
                    id: MirInstructionId::new("inst:compare.stale").unwrap(),
                    kind: MirInstructionKind::Binary {
                        result: stale_equality,
                        op: ResolvedBinaryOp::Equal,
                        left: moved_peer.clone(),
                        right: moved_alias.clone(),
                    },
                },
            ],
            MirTerminator::Return { value: None },
        );
        let function = string_liveness_test_function(
            vec![moved_peer.clone()],
            vec![
                string_test_value("param:peer", string_type.clone()),
                string_test_value("local:alias", string_type.clone()),
                string_test_value("result:stale", bool_type),
            ],
            vec![entry],
        );
        let error = catalog
            .validate_managed_any_string_liveness(
                &function,
                &BTreeMap::from([(function.entry.clone(), Vec::new())]),
            )
            .expect_err("Move consumes its old SSA owner and a later read must fail");
        assert!(error.contains("not definitely live"), "{error}");
    }

    #[test]
    fn managed_any_string_liveness_rejects_duplicate_owner_transfers() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    let text = "catalog"
    println(text)
    0
}
"#,
        ))
        .expect("String catalog MIR");
        let catalog = program.type_catalog();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let peer = MirValueId::new("param:peer").unwrap();
        let aggregate = MirValueId::new("result:aggregate").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            vec![MirInstruction {
                id: MirInstructionId::new("inst:duplicate.construct").unwrap(),
                kind: MirInstructionKind::Construct {
                    result: aggregate.clone(),
                    kind: MirAggregateKind::Tuple,
                    fields: vec![peer.clone(), peer.clone()],
                },
            }],
            MirTerminator::Return { value: None },
        );
        let function = string_liveness_test_function(
            vec![peer.clone()],
            vec![
                string_test_value("param:peer", string_type.clone()),
                string_test_value(
                    "result:aggregate",
                    crate::core::ir::primitive_type_id(PrimitiveType::I32),
                ),
            ],
            vec![entry],
        );
        let error = catalog
            .validate_managed_any_string_liveness(
                &function,
                &BTreeMap::from([(function.entry.clone(), Vec::new())]),
            )
            .expect_err("a single aggregate cannot consume one String owner twice");
        assert!(error.contains("transferred more than once"), "{error}");

        let first_parameter = MirValueId::new("block:first").unwrap();
        let second_parameter = MirValueId::new("block:second").unwrap();
        let target_id = MirBlockId::new("bb.target").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:duplicate").unwrap(),
                target: target_id.clone(),
                arguments: vec![peer.clone(), peer.clone()],
            },
        );
        let mut target = flow_test_block(
            "bb.target",
            Vec::new(),
            MirTerminator::Return { value: None },
        );
        target.parameters = vec![
            crate::core::mir::MirBlockParameter {
                value: first_parameter.clone(),
            },
            crate::core::mir::MirBlockParameter {
                value: second_parameter.clone(),
            },
        ];
        let function = string_liveness_test_function(
            vec![peer.clone()],
            vec![
                string_test_value("param:peer", string_type.clone()),
                string_test_value("block:first", string_type.clone()),
                string_test_value("block:second", string_type),
            ],
            vec![entry, target],
        );
        let successors = BTreeMap::from([
            (
                function.entry.clone(),
                vec![ManagedAnyCfgSuccessor {
                    target: target_id.clone(),
                    arguments: vec![peer.clone(), peer],
                }],
            ),
            (target_id, Vec::new()),
        ]);
        let error = catalog
            .validate_managed_any_string_liveness(&function, &successors)
            .expect_err("one String SSA owner cannot initialize two block parameters");
        assert!(error.contains("same SSA owner"), "{error}");
    }

    #[test]
    fn managed_any_string_liveness_intersects_branch_and_loop_paths() {
        let program = MirProgram::from_checked_program(&checked(
            r#"
func main() -> i32 {
    let text = "catalog"
    println(text)
    0
}
"#,
        ))
        .expect("String catalog MIR");
        let catalog = program.type_catalog();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        let peer = MirValueId::new("param:peer").unwrap();
        let condition = MirValueId::new("param:condition").unwrap();
        let comparison = MirValueId::new("result:equality").unwrap();
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Branch {
                condition: condition.clone(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:then").unwrap(),
                then_target: MirBlockId::new("bb.then").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:else").unwrap(),
                else_target: MirBlockId::new("bb.else").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let then_block = flow_test_block(
            "bb.then",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:then.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let else_block = flow_test_block(
            "bb.else",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:else.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let join = flow_test_block(
            "bb.join",
            vec![MirInstruction {
                id: MirInstructionId::new("inst:join.compare").unwrap(),
                kind: MirInstructionKind::Binary {
                    result: comparison.clone(),
                    op: ResolvedBinaryOp::Equal,
                    left: peer.clone(),
                    right: peer.clone(),
                },
            }],
            MirTerminator::Return { value: None },
        );
        let values = vec![
            string_test_value("param:peer", string_type.clone()),
            string_test_value("param:condition", bool_type.clone()),
            string_test_value("result:equality", bool_type.clone()),
        ];
        let parameters = vec![peer.clone(), condition.clone()];
        let successors = string_liveness_test_successors(&[
            ("bb.entry", "bb.then"),
            ("bb.entry", "bb.else"),
            ("bb.then", "bb.join"),
            ("bb.else", "bb.join"),
        ]);
        let function = string_liveness_test_function(
            parameters.clone(),
            values.clone(),
            vec![
                entry.clone(),
                then_block.clone(),
                else_block.clone(),
                join.clone(),
            ],
        );
        catalog
            .validate_managed_any_string_liveness(&function, &successors)
            .expect("the peer is live on both arms and at their join");

        let mut consuming_then = then_block;
        consuming_then
            .instructions
            .push(flow_test_drop("param:peer", "inst:then.drop"));
        let function = string_liveness_test_function(
            parameters.clone(),
            values.clone(),
            vec![entry.clone(), consuming_then, else_block, join.clone()],
        );
        let error = catalog
            .validate_managed_any_string_liveness(&function, &successors)
            .expect_err("the join intersects live peers from each incoming path");
        assert!(error.contains("not definitely live"), "{error}");

        let header = flow_test_block(
            "bb.header",
            Vec::new(),
            MirTerminator::Branch {
                condition: condition.clone(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:body").unwrap(),
                then_target: MirBlockId::new("bb.body").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:exit").unwrap(),
                else_target: MirBlockId::new("bb.exit").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:entry.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let loop_body = flow_test_block(
            "bb.body",
            vec![MirInstruction {
                id: MirInstructionId::new("inst:loop.compare").unwrap(),
                kind: MirInstructionKind::Binary {
                    result: comparison,
                    op: ResolvedBinaryOp::Equal,
                    left: peer.clone(),
                    right: peer.clone(),
                },
            }],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:backedge").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let exit = flow_test_block("bb.exit", Vec::new(), MirTerminator::Return { value: None });
        let loop_values = vec![
            string_test_value("param:peer", string_type.clone()),
            string_test_value("param:condition", bool_type.clone()),
            string_test_value("result:equality", bool_type.clone()),
        ];
        let loop_parameters = vec![peer.clone(), condition.clone()];
        let loop_successors = string_liveness_test_successors(&[
            ("bb.entry", "bb.header"),
            ("bb.header", "bb.body"),
            ("bb.header", "bb.exit"),
            ("bb.body", "bb.header"),
        ]);
        let function = string_liveness_test_function(
            loop_parameters.clone(),
            loop_values.clone(),
            vec![
                entry.clone(),
                header.clone(),
                loop_body.clone(),
                exit.clone(),
            ],
        );
        catalog
            .validate_managed_any_string_liveness(&function, &loop_successors)
            .expect("a loop fixed point preserves the peer when every backedge preserves it");

        let mut dropping_body = loop_body;
        dropping_body
            .instructions
            .insert(0, flow_test_drop("param:peer", "inst:loop.drop"));
        let function = string_liveness_test_function(
            loop_parameters,
            loop_values,
            vec![entry, header, dropping_body, exit],
        );
        let error = catalog
            .validate_managed_any_string_liveness(&function, &loop_successors)
            .expect_err("a loop backedge that consumes the peer invalidates the next iteration");
        assert!(error.contains("not definitely live"), "{error}");
    }

    #[test]
    fn managed_any_cfg_accepts_balanced_owner_drop_on_each_branch() {
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").unwrap(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:then").unwrap(),
                then_target: MirBlockId::new("bb.then").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:else").unwrap(),
                else_target: MirBlockId::new("bb.else").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let then_block = flow_test_block(
            "bb.then",
            vec![flow_test_drop("param:owner", "inst:drop.then")],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:then.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let else_block = flow_test_block(
            "bb.else",
            vec![flow_test_drop("param:owner", "inst:drop.else")],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:else.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let join = flow_test_block("bb.join", Vec::new(), MirTerminator::Return { value: None });
        let function = managed_any_flow_function(vec![entry, then_block, else_block, join]);
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let managed_values = BTreeSet::from([MirValueId::new("param:owner").unwrap()]);

        catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect("every mutually exclusive path drops the owner exactly once");
    }

    #[test]
    fn managed_any_cfg_rejects_owner_state_mismatch_between_match_arms() {
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").unwrap(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:then").unwrap(),
                then_target: MirBlockId::new("bb.then").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:else").unwrap(),
                else_target: MirBlockId::new("bb.else").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let then_block = flow_test_block(
            "bb.then",
            vec![flow_test_drop("param:owner", "inst:drop.then")],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:then.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let else_block = flow_test_block(
            "bb.else",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:else.join").unwrap(),
                target: MirBlockId::new("bb.join").unwrap(),
                arguments: Vec::new(),
            },
        );
        let join = flow_test_block(
            "bb.join",
            vec![flow_test_drop("param:owner", "inst:drop.join")],
            MirTerminator::Return { value: None },
        );
        let function = managed_any_flow_function(vec![entry, then_block, else_block, join]);
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let managed_values = BTreeSet::from([MirValueId::new("param:owner").unwrap()]);

        let error = catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect_err("branch paths with different live owners must fail at their join");
        assert!(error.contains("state disagrees at join"), "{error}");
    }

    #[test]
    fn managed_any_cfg_accepts_loop_with_stable_owner_fixed_point() {
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:entry.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let header = flow_test_block(
            "bb.header",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").unwrap(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:header.body").unwrap(),
                then_target: MirBlockId::new("bb.body").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:header.exit").unwrap(),
                else_target: MirBlockId::new("bb.exit").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let body = flow_test_block(
            "bb.body",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:body.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let exit = flow_test_block(
            "bb.exit",
            vec![flow_test_drop("param:owner", "inst:drop.exit")],
            MirTerminator::Return { value: None },
        );
        let function = managed_any_flow_function(vec![entry, header, body, exit]);
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let managed_values = BTreeSet::from([MirValueId::new("param:owner").unwrap()]);

        catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect("loop backedge preserves the exact owner state until exit cleanup");
    }

    #[test]
    fn managed_any_cfg_rejects_loop_backedge_that_loses_owner_state() {
        let entry = flow_test_block(
            "bb.entry",
            Vec::new(),
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:entry.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let header = flow_test_block(
            "bb.header",
            Vec::new(),
            MirTerminator::Branch {
                condition: MirValueId::new("const:condition").unwrap(),
                then_edge: crate::core::mir::MirEdgeId::new("edge:header.body").unwrap(),
                then_target: MirBlockId::new("bb.body").unwrap(),
                then_arguments: Vec::new(),
                else_edge: crate::core::mir::MirEdgeId::new("edge:header.exit").unwrap(),
                else_target: MirBlockId::new("bb.exit").unwrap(),
                else_arguments: Vec::new(),
            },
        );
        let body = flow_test_block(
            "bb.body",
            vec![flow_test_drop("param:owner", "inst:drop.body")],
            MirTerminator::Goto {
                edge: crate::core::mir::MirEdgeId::new("edge:body.header").unwrap(),
                target: MirBlockId::new("bb.header").unwrap(),
                arguments: Vec::new(),
            },
        );
        let exit = flow_test_block("bb.exit", Vec::new(), MirTerminator::Return { value: None });
        let function = managed_any_flow_function(vec![entry, header, body, exit]);
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let managed_values = BTreeSet::from([MirValueId::new("param:owner").unwrap()]);

        let error = catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect_err("a loop backedge cannot silently lose a live owner");
        assert!(error.contains("state disagrees at join"), "{error}");
    }

    #[test]
    fn managed_any_string_equality_is_a_sealed_borrowed_read() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let main = get_main(&program);
        let managed_type = managed_any_i32_string_v1_type_id();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let bool_type = crate::core::ir::primitive_type_id(PrimitiveType::Bool);
        let (block, position, comparison, owner) = main
            .blocks
            .values()
            .find_map(|block| {
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .find_map(|(position, instruction)| {
                        let MirInstructionKind::Binary {
                            result,
                            op: ResolvedBinaryOp::Equal,
                            left,
                            right,
                        } = &instruction.kind
                        else {
                            return None;
                        };
                        if main
                            .values
                            .get(result)
                            .is_none_or(|value| value.ty != bool_type)
                        {
                            return None;
                        }
                        let owner = if main
                            .values
                            .get(left)
                            .is_some_and(|value| value.ty == managed_type)
                            && main
                                .values
                                .get(right)
                                .is_some_and(|value| value.ty == string_type)
                        {
                            left
                        } else if main
                            .values
                            .get(right)
                            .is_some_and(|value| value.ty == managed_type)
                            && main
                                .values
                                .get(left)
                                .is_some_and(|value| value.ty == string_type)
                        {
                            right
                        } else {
                            return None;
                        };
                        Some((block, position, instruction, owner.clone()))
                    })
            })
            .expect("Any == String lowers through the exact borrowed comparison profile");
        assert!(comparison.id.0.contains("binary.managed_any_string_read"));
        assert!(
            block.instructions[position + 1..]
                .iter()
                .any(|instruction| {
                    matches!(
                        &instruction.kind,
                        MirInstructionKind::Drop { value } if value == &owner
                    ) || matches!(
                        &instruction.kind,
                        MirInstructionKind::ManagedAny {
                            source,
                            operation: MirManagedAnyOperation::Drop,
                            ..
                        } if source == &owner
                    )
                })
                || main.blocks.values().any(|later_block| {
                    later_block.instructions.iter().any(|instruction| {
                        matches!(
                            &instruction.kind,
                            MirInstructionKind::ManagedAny {
                                source,
                                operation: MirManagedAnyOperation::Drop,
                                ..
                            } if source == &owner
                        )
                    })
                }),
            "the borrowed comparison leaves its owner available for the source Drop"
        );
        assert!(
            main.ownership
                .checker_managed_any
                .as_ref()
                .is_some_and(|seal| seal.instructions.iter().any(|(_, _, instruction)| {
                    instruction.id == comparison.id && instruction.kind == comparison.kind
                })),
            "the borrowed comparison is part of the complete Checker-origin body seal"
        );

        let MirInstructionKind::Binary {
            result,
            left,
            right,
            ..
        } = &comparison.kind
        else {
            panic!("selected comparison instruction must stay Binary");
        };
        let managed_values = &main
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("private Any seal")
            .managed_values;
        assert!(
            managed_any_string_comparison_owner(
                &main,
                result,
                ResolvedBinaryOp::Add,
                left,
                right,
                managed_values,
            )
            .is_none(),
            "other operators cannot borrow an Any owner"
        );
    }

    #[test]
    fn managed_any_i32_pack_requires_the_exact_implicit_checker_conversion() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let main = get_main(&program);
        let (source, result, contract) = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::ManagedAny {
                    source,
                    result: Some(result),
                    operation: MirManagedAnyOperation::PackI32,
                    contract: Some(contract),
                } => Some((source.clone(), result.clone(), contract.clone())),
                _ => None,
            })
            .expect("the scalar Any fallback lowers to one checked PackI32");
        let conversion = contract
            .checker_conversion
            .as_ref()
            .expect("the pack carries ResolvedBody conversion evidence");
        assert_eq!(conversion.kind, CheckedConversionKind::DynamicAnyPack);
        assert_eq!(
            conversion.from,
            crate::core::ir::primitive_type_id(PrimitiveType::I32)
        );
        assert_eq!(contract.source_type, conversion.from);
        assert!(catalog
            .get(&conversion.to)
            .is_some_and(|descriptor| { descriptor.kind == MirTypeKind::DynamicAny }));
        assert_eq!(
            contract.target_type,
            Some(managed_any_i32_string_v1_type_id())
        );
        catalog
            .validate_managed_any_contract(
                &main,
                &source,
                Some(&result),
                MirManagedAnyOperation::PackI32,
                Some(&contract),
            )
            .expect("the exact checked i32 -> Any conversion has a valid v1 materialization");

        let mut missing_conversion = contract.clone();
        missing_conversion.checker_conversion = None;
        let error = catalog
            .validate_managed_any_contract(
                &main,
                &source,
                Some(&result),
                MirManagedAnyOperation::PackI32,
                Some(&missing_conversion),
            )
            .expect_err("a type-only i32/Any pair cannot authorize packing");
        assert!(error.contains("no exact Checker DynamicAnyPack"), "{error}");
    }

    #[test]
    fn managed_any_free_string_drop_preserves_source_binding_and_both_equality_orders() {
        let checked = checked(
            r#"
func carry<T>(value: T) -> T { value }
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "k", "payload")
    let pair = map_get(root, "k")
    drop(empty)
    drop(root)
    let found = pair.0
    let payload = pair.1
    let carried = carry(payload)
    let peer = "payload"
    drop(peer)
    let left_first = if found { carried == peer } else { false }
    let right_first = if found { peer == carried } else { false }
    println(left_first)
    println(right_first)
    drop(pair)
    drop(payload)
    drop(carried)
    0
}
"#,
        );
        let program = MirProgram::from_checked_program(&checked)
            .expect("Free String Drop must preserve the binding used by both comparisons");
        let main = get_main(&program);
        let catalog = program.type_catalog();
        let managed_type = managed_any_i32_string_v1_type_id();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let mut comparisons = Vec::new();
        let mut temporary = None;
        let mut dropped_temporary = false;
        for block in main.blocks.values() {
            for instruction in &block.instructions {
                match &instruction.kind {
                    MirInstructionKind::Binary {
                        op: ResolvedBinaryOp::Equal,
                        left,
                        right,
                        ..
                    } if (main
                        .values
                        .get(left)
                        .is_some_and(|value| value.ty == managed_type)
                        && main
                            .values
                            .get(right)
                            .is_some_and(|value| value.ty == string_type))
                        || (main
                            .values
                            .get(right)
                            .is_some_and(|value| value.ty == managed_type)
                            && main
                                .values
                                .get(left)
                                .is_some_and(|value| value.ty == string_type)) =>
                    {
                        comparisons.push((left.clone(), right.clone()));
                    }
                    MirInstructionKind::Clone { result, source }
                        if instruction.id.0.contains("drop.string.clone")
                            && main
                                .values
                                .get(source)
                                .is_some_and(|value| value.ty == string_type) =>
                    {
                        temporary = Some((result.clone(), source.clone()));
                    }
                    MirInstructionKind::Drop { value }
                        if temporary
                            .as_ref()
                            .is_some_and(|(result, _)| result == value) =>
                    {
                        dropped_temporary = true;
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(
            comparisons.len(),
            2,
            "both operand orders must use the profile"
        );
        assert!(comparisons.iter().any(|(left, right)| {
            main.values
                .get(left)
                .is_some_and(|value| value.ty == managed_type)
                && main
                    .values
                    .get(right)
                    .is_some_and(|value| value.ty == string_type)
        }));
        assert!(comparisons.iter().any(|(left, right)| {
            main.values
                .get(right)
                .is_some_and(|value| value.ty == managed_type)
                && main
                    .values
                    .get(left)
                    .is_some_and(|value| value.ty == string_type)
        }));
        let (temporary, peer) = temporary.expect("source Drop evaluates a fresh String clone");
        assert!(
            dropped_temporary,
            "the fresh evaluation temporary is settled"
        );
        assert!(
            comparisons
                .iter()
                .all(|(left, right)| { left == &peer || right == &peer }),
            "both comparisons continue to read the original String binding"
        );
        assert_ne!(
            temporary, peer,
            "source Drop cannot consume the original owner"
        );
        catalog
            .validate_managed_any_function(&main)
            .expect("the exact source seal and path-liveness proof accept the legal no-op Drop");

        let mut forged = main.clone();
        let drop_instruction = forged
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Drop { value } if value == &temporary
                )
            })
            .expect("temporary Drop to forge");
        drop_instruction.kind = MirInstructionKind::Drop {
            value: peer.clone(),
        };
        let managed_values = forged
            .values
            .values()
            .filter(|value| catalog.contains_dynamic_or_managed_any(&value.ty))
            .map(|value| value.id.clone())
            .collect::<BTreeSet<_>>();
        let error = catalog
            .validate_managed_any_cfg_flow(&forged, &managed_values)
            .expect_err("a MIR Drop that consumes the binding before equality must be rejected");
        assert!(error.contains("not definitely live"), "{error}");
    }

    #[test]
    fn managed_any_string_equality_borrows_the_source_owner() {
        let checked = checked(
            r#"
func carry<T>(value: T) -> T { value }
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "k", "payload")
    let pair = map_get(root, "k")
    drop(empty)
    drop(root)
    let found = pair.0
    let payload = pair.1
    let carried = carry(payload)
    let peer = "payload"
    let equal = if found { carried == peer } else { false }
    println(equal)
    drop(pair)
    drop(payload)
    drop(carried)
    drop(peer)
    0
}
"#,
        );
        let program = MirProgram::from_checked_program(&checked)
            .expect("a checked Any owner can be borrowed by the String equality profile");
        let main = get_main(&program);
        let managed_type = managed_any_i32_string_v1_type_id();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let has_borrowed_any_string_equality = main.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Binary {
                        op: ResolvedBinaryOp::Equal,
                        left,
                        right,
                        ..
                    } if (main.values.get(left).is_some_and(|value| value.ty == managed_type)
                        && main.values.get(right).is_some_and(|value| value.ty == string_type))
                        || (main.values.get(right).is_some_and(|value| value.ty == managed_type)
                            && main.values.get(left).is_some_and(|value| value.ty == string_type))
                )
            })
        });
        let has_owned_string_drop = main.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    &instruction.kind,
                    MirInstructionKind::Drop { value }
                        if main.values.get(value).is_some_and(|entry| entry.ty == string_type)
                )
            })
        });
        assert!(
            has_borrowed_any_string_equality && has_owned_string_drop,
            "the equality reads the Any owner beside an owned String peer"
        );
        program
            .type_catalog()
            .validate_managed_any_function(&main)
            .expect("the borrowed peer remains live and settles after the comparison");
    }

    #[test]
    fn managed_any_tuple_pattern_synthetics_map_to_the_checker_binding_point() {
        let checked = checked(
            r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "k", "payload")
    let pair = map_get(root, "k")
    drop(empty)
    drop(root)
    let (found, payload) = pair
    println(found)
    drop(payload)
    0
}
"#,
        );
        let program = MirProgram::from_checked_program(&checked)
            .expect("tuple patterns use the source binding point for synthetic MIR identities");
        let main = get_main(&program);
        let seal = main
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("private Checker ManagedAny body seal");
        let source_cfg = seal
            .source_cfg
            .as_ref()
            .expect("Checker source CFG receipt");
        let pattern_instructions = seal
            .instructions
            .iter()
            .filter(|(_, _, instruction)| {
                instruction.id.0.contains("managed_any_pattern_bind")
                    || instruction.id.0.contains("managed_any_tuple_project")
            })
            .collect::<Vec<_>>();
        assert!(!pattern_instructions.is_empty());
        for (_, origin, instruction) in pattern_instructions {
            assert!(
                source_cfg.reachable_blocks.values().any(|block| {
                    block
                        .points
                        .iter()
                        .any(|point| &point.source.node == origin)
                }),
                "synthetic pattern instruction '{}' must use its enclosing Checker binding point '{}'",
                instruction.id,
                origin.0
            );
        }
    }

    #[test]
    fn managed_any_tuple_project_clones_and_keeps_residual_bundle_live() {
        let source = r#"
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "k", "payload")
    let pair = map_get(root, "k")
    let payload = pair.1
    let bundle = (payload, "owned sibling")
    drop(pair)
    drop(empty)
    drop(root)
    let selected = bundle.0
    drop(selected)
    0
}
"#;
        let checked = checked(source);
        let program = MirProgram::from_checked_program(&checked)
            .expect("ordinary Project clones the selected Any and preserves the bundle owner");
        let main = get_main(&program);
        let catalog = program.type_catalog();
        let managed_type = managed_any_i32_string_v1_type_id();
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let (projection, bundle, selected) = main
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::Project {
                    result,
                    base,
                    projection: MirProjection::Tuple(0),
                    ..
                } if main.values.get(base).is_some_and(|value| {
                    catalog.get(&value.ty).is_some_and(|descriptor| {
                        descriptor.layout
                            == MirLayout::Tuple(vec![managed_type.clone(), string_type.clone()])
                    })
                }) =>
                {
                    Some((instruction, base, result))
                }
                _ => None,
            })
            .expect("the source field read must lower to a non-consuming tuple Project");
        assert_eq!(
            main.values.get(selected).map(|value| &value.ty),
            Some(&managed_type)
        );
        catalog
            .validate_glue(&managed_type, MirGlueOperation::Clone)
            .expect("Project has an owned Any clone contract");
        assert!(
            main.blocks.values().any(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(&instruction.kind, MirInstructionKind::Drop { value } if value == bundle)
                })
            }),
            "the original tuple remains live and is explicitly dropped at function exit"
        );
        assert!(projection.id.0.contains("managed_any_tuple_load"));

        let bundle_type = &main.values.get(bundle).expect("bundle TypeDesc").ty;
        let tuple = catalog.get(bundle_type).expect("bundle descriptor");
        let MirLayout::Tuple(elements) = &tuple.layout else {
            panic!("bundle must retain its exact tuple layout");
        };
        assert_eq!(elements.as_slice(), &[managed_type, string_type]);
        for (selected_index, selected_type) in elements.iter().enumerate() {
            let error = catalog
                .validate_move_projection(
                    bundle_type,
                    selected_type,
                    &MirProjection::Tuple(selected_index),
                )
                .expect_err("consuming one field cannot discard the other owned field");
            assert!(
                error.contains("tuple leaves non-Copy sibling"),
                "selected field {selected_index}: {error}"
            );
        }
    }

    #[test]
    fn managed_any_cfg_rejects_a_stale_owner_in_a_fault_terminator() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let owner = MirValueId::new("param:owner").expect("owner value");
        let block = flow_test_block(
            "bb.entry",
            vec![flow_test_drop("param:owner", "inst:drop.before.fault")],
            MirTerminator::Fault {
                value: Some(owner.clone()),
            },
        );
        let function = managed_any_flow_function(vec![block]);
        let managed_values = BTreeSet::from([owner]);

        let error = catalog
            .validate_managed_any_cfg_flow(&function, &managed_values)
            .expect_err("Fault cannot reference an Any owner already dropped on this path");
        assert!(
            error.contains("used after transfer, drop") || error.contains("not definitely live"),
            "{error}"
        );
    }

    #[test]
    fn managed_any_cfg_requires_the_checker_map_path_projection() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let mut main = get_main(&program);
        assert!(main.blocks.len() > 1);
        main.ownership
            .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt = None;

        let error = program
            .type_catalog()
            .validate_managed_any_function(&main)
            .expect_err("multi-block ManagedAny must be bound to Checker CFG evidence");
        assert!(error.contains("no Map-specific Checker receipt"), "{error}");
    }

    #[test]
    fn managed_any_multiblock_body_requires_the_generic_checker_cfg_projection() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let mut main = get_main(&program);
        let mut seal = main
            .ownership
            .checker_managed_any
            .clone()
            .expect("private ManagedAny body seal");
        main.ownership.checker_managed_any_cfg_receipt = None;
        seal.source_cfg = None;

        let error = program
            .type_catalog()
            .validate_managed_any_source_cfg(&main, &seal)
            .expect_err("a full-body seal does not substitute for source-CFG evidence");
        assert!(error.contains("no Checker source-CFG receipt"), "{error}");
    }

    #[test]
    fn managed_any_source_cfg_binds_entry_branch_and_instruction_origins() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let main = get_main(&program);
        let seal = main
            .ownership
            .checker_managed_any
            .as_ref()
            .expect("private ManagedAny body seal");

        let mut wrong_entry = seal.clone();
        let source_cfg = wrong_entry.source_cfg.as_mut().expect("Checker source CFG");
        let wrong_entry_block = source_cfg
            .reachable_blocks
            .keys()
            .find(|block| *block != &source_cfg.entry)
            .expect("non-entry source CFG block")
            .clone();
        source_cfg.entry = wrong_entry_block;
        let error = catalog
            .validate_managed_any_source_cfg(&main, &wrong_entry)
            .expect_err("the Checker entry is bound to the MIR entry");
        assert!(error.contains("entry"), "{error}");

        let (branch_block, then_target, else_target, original_condition) = main
            .blocks
            .iter()
            .find_map(|(id, block)| match &block.terminator {
                MirTerminator::Branch {
                    condition,
                    then_target,
                    else_target,
                    ..
                } => Some((
                    id.clone(),
                    then_target.clone(),
                    else_target.clone(),
                    condition.clone(),
                )),
                _ => None,
            })
            .expect("source fixture contains If branches");
        let other_bool = main
            .values
            .values()
            .find(|value| {
                value.id != original_condition
                    && value.ty == crate::core::ir::primitive_type_id(PrimitiveType::Bool)
            })
            .expect("source fixture has another distinct Bool value")
            .id
            .clone();
        let mut swapped_condition = main.clone();
        let MirTerminator::Branch { condition, .. } = &mut swapped_condition
            .blocks
            .get_mut(&branch_block)
            .expect("branch block")
            .terminator
        else {
            unreachable!();
        };
        *condition = other_bool;
        let error = catalog
            .validate_managed_any_source_cfg(&swapped_condition, seal)
            .expect_err("a dominating Bool cannot replace this source If condition");
        assert!(
            error.contains("condition") || error.contains("selector"),
            "{error}"
        );

        let then_origin = main
            .blocks
            .get(&then_target)
            .and_then(|block| block.instructions.first())
            .map(|instruction| instruction.id.clone())
            .expect("then arm instruction");
        let else_origin = main
            .blocks
            .get(&else_target)
            .and_then(|block| block.instructions.first())
            .and_then(|instruction| {
                seal.instructions
                    .iter()
                    .find(|(block, _, sealed)| block == &else_target && sealed.id == instruction.id)
                    .map(|(_, origin, _)| origin.clone())
            })
            .expect("else arm instruction has a sealed source origin");
        let mut crossed_origin = seal.clone();
        let (_, origin, _) = crossed_origin
            .instructions
            .iter_mut()
            .find(|(block, _, instruction)| block == &then_target && instruction.id == then_origin)
            .expect("then arm instruction has a sealed source origin");
        *origin = else_origin;
        let error = catalog
            .validate_managed_any_source_cfg(&main, &crossed_origin)
            .expect_err("an instruction origin from the opposite arm cannot authorize this block");
        assert!(error.contains("own source block"), "{error}");
    }

    #[test]
    fn managed_any_cfg_rejects_unsettled_string_from_one_if_arm_at_the_join() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let (main, managed_values, _string) = managed_any_branch_string_fixture(false);

        let error = catalog
            .validate_managed_any_cfg_flow(&main, &managed_values)
            .expect_err("unsettled path-local String must fail at the diamond join");
        assert!(error.contains("state disagrees at join"), "{error}");
    }

    #[test]
    fn managed_any_cfg_rejects_stale_string_uses_after_an_arm_drop() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        for forged_kind in ["duplicate-drop", "stale-clone"] {
            let (mut main, managed_values, unpacked) = managed_any_branch_string_fixture(true);
            let join = MirBlockId::new("bb.join").expect("join block id");
            let forged_id = MirInstructionId::new(format!("forged:{forged_kind}"))
                .expect("synthetic test instruction id");
            let forged_instruction = match forged_kind {
                "duplicate-drop" => MirInstructionKind::Drop {
                    value: unpacked.clone(),
                },
                "stale-clone" => MirInstructionKind::Clone {
                    source: unpacked.clone(),
                    result: MirValueId::new("forged:stale-clone-result")
                        .expect("synthetic result id"),
                },
                _ => unreachable!(),
            };
            if forged_kind == "stale-clone" {
                let stale_result =
                    MirValueId::new("forged:stale-clone-result").expect("synthetic result id");
                main.values.insert(
                    stale_result.clone(),
                    crate::core::mir::MirValue {
                        id: stale_result,
                        ty: crate::core::ir::primitive_type_id(PrimitiveType::String),
                    },
                );
            }
            main.blocks
                .get_mut(&join)
                .expect("diamond join exists")
                .instructions
                .insert(
                    0,
                    MirInstruction {
                        id: forged_id,
                        kind: forged_instruction,
                    },
                );

            let error = catalog
                .validate_managed_any_cfg_flow(&main, &managed_values)
                .expect_err("an arm-consumed String is never live at the common join");
            assert!(
                error.contains("used after transfer, drop")
                    || error.contains("not definitely live"),
                "{error}"
            );
        }

        let (mut main, managed_values, unpacked) = managed_any_branch_string_fixture(true);
        let arm = MirBlockId::new("bb.then").expect("then block id");
        main.blocks
            .get_mut(&arm)
            .expect("read arm exists")
            .terminator = MirTerminator::Return {
            value: Some(unpacked),
        };
        let error = catalog
            .validate_managed_any_cfg_flow(&main, &managed_values)
            .expect_err("a stale String cannot be returned after its arm Drop");
        assert!(
            error.contains("used after transfer, drop") || error.contains("not definitely live"),
            "{error}"
        );
    }

    #[test]
    fn managed_any_cfg_tracks_string_binary_results_even_with_unrelated_inputs() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let catalog = program.type_catalog();
        let owner = MirValueId::new("param:owner").expect("ManagedAny parameter");
        let condition = MirValueId::new("const:condition").expect("branch condition");
        let left = MirValueId::new("const:left").expect("left String");
        let right = MirValueId::new("const:right").expect("right String");
        let result = MirValueId::new("result:branch-string").expect("String Binary result");
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let make_function = |settle_result: bool| {
            let entry = flow_test_block(
                "bb.entry",
                vec![
                    MirInstruction {
                        id: MirInstructionId::new("inst:const.left").unwrap(),
                        kind: MirInstructionKind::Const {
                            result: left.clone(),
                            literal: ResolvedLiteral::String("left".into()),
                        },
                    },
                    MirInstruction {
                        id: MirInstructionId::new("inst:const.right").unwrap(),
                        kind: MirInstructionKind::Const {
                            result: right.clone(),
                            literal: ResolvedLiteral::String("right".into()),
                        },
                    },
                ],
                MirTerminator::Branch {
                    condition: condition.clone(),
                    then_edge: crate::core::mir::MirEdgeId::new("edge:then").unwrap(),
                    then_target: MirBlockId::new("bb.then").unwrap(),
                    then_arguments: Vec::new(),
                    else_edge: crate::core::mir::MirEdgeId::new("edge:else").unwrap(),
                    else_target: MirBlockId::new("bb.else").unwrap(),
                    else_arguments: Vec::new(),
                },
            );
            let mut then_instructions = vec![MirInstruction {
                id: MirInstructionId::new("inst:branch.concat").unwrap(),
                kind: MirInstructionKind::Binary {
                    result: result.clone(),
                    op: ResolvedBinaryOp::Add,
                    left: left.clone(),
                    right: right.clone(),
                },
            }];
            if settle_result {
                then_instructions.push(flow_test_drop(
                    "result:branch-string",
                    "inst:branch.concat.drop",
                ));
            }
            then_instructions.push(flow_test_drop("param:owner", "inst:owner.then.drop"));
            let then_block = flow_test_block(
                "bb.then",
                then_instructions,
                MirTerminator::Goto {
                    edge: crate::core::mir::MirEdgeId::new("edge:then.join").unwrap(),
                    target: MirBlockId::new("bb.join").unwrap(),
                    arguments: Vec::new(),
                },
            );
            let else_block = flow_test_block(
                "bb.else",
                vec![flow_test_drop("param:owner", "inst:owner.else.drop")],
                MirTerminator::Goto {
                    edge: crate::core::mir::MirEdgeId::new("edge:else.join").unwrap(),
                    target: MirBlockId::new("bb.join").unwrap(),
                    arguments: Vec::new(),
                },
            );
            let join =
                flow_test_block("bb.join", Vec::new(), MirTerminator::Return { value: None });
            let mut function = managed_any_flow_function(vec![entry, then_block, else_block, join]);
            for value in [
                super::super::MirValue {
                    id: left.clone(),
                    ty: string_type.clone(),
                },
                super::super::MirValue {
                    id: right.clone(),
                    ty: string_type.clone(),
                },
                super::super::MirValue {
                    id: result.clone(),
                    ty: string_type.clone(),
                },
            ] {
                function.values.insert(value.id.clone(), value);
            }
            function
        };
        let managed_values = BTreeSet::from([owner]);
        catalog
            .validate_managed_any_cfg_flow(&make_function(true), &managed_values)
            .expect("each read-arm String result is dropped on its path");
        let error = catalog
            .validate_managed_any_cfg_flow(&make_function(false), &managed_values)
            .expect_err("every owned String Binary result in an arm must settle on all paths");
        assert!(error.contains("state disagrees at join"), "{error}");
    }

    #[test]
    fn original_get_any_pure_i32_and_any_escape_are_checker_sealed() {
        let checked = checked(SOURCE);
        assert!(
            crate::core::mir::classify_canonical_mir_route_admission(&checked)
                .ordinary_map_owned_get_i32_string_v1_complete()
        );
        let program =
            MirProgram::from_checked_program(&checked).expect("canonical pure i32 ordinary GetAny");
        program
            .validate_managed_any_identity_instances()
            .expect("closed generic ManagedAny identity table");
        assert!(program
            .type_catalog()
            .is_managed_any_i32_string_v1(&managed_any_i32_string_v1_type_id()));
        for function in program.functions().values() {
            program
                .type_catalog()
                .validate_managed_any_function(function)
                .expect("sealed owner ledger");
        }
    }

    #[test]
    fn generic_any_identity_is_bound_to_checker_template_and_exact_owner_body() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let instance = program
            .instances()
            .values()
            .find(|instance| instance.contract == MirGenericInstanceContract::ManagedAnyIdentity)
            .expect("one sealed ManagedAny identity instance");
        let function = &program.functions()[&instance.function];
        let managed = program
            .type_catalog()
            .validate_managed_any_identity_source_argument(&instance.arguments)
            .expect("source Any maps to canonical ManagedAny");
        assert_eq!(instance.arguments.len(), 1);
        assert_ne!(instance.arguments[0], managed);
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_source_argument(&[])
            .is_err());
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_source_argument(std::slice::from_ref(&managed))
            .is_err());
        assert_eq!(function.result, managed);
        assert_eq!(function.values[&function.parameters[0]].ty, managed);
        assert!(matches!(
            function.blocks[&function.entry].instructions.as_slice(),
            [
                MirInstruction {
                    kind: MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Clone,
                        ..
                    },
                    ..
                },
                MirInstruction {
                    kind: MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Drop,
                        ..
                    },
                    ..
                }
            ]
        ));
        program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, function)
            .expect("Checker seal binds template, source arg, owner, ABI and operations");
        program
            .validate_managed_any_identity_instances()
            .expect("every call and table entry matches");

        let mut forged_instance = instance.clone();
        forged_instance.template = NodeId("function:forged-template".into());
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(&forged_instance, function)
            .is_err());

        let mut forged = function.clone();
        let identity = forged
            .ownership
            .checker_managed_any
            .as_mut()
            .and_then(|seal| seal.generic_identity.as_mut())
            .expect("private generic identity seal");
        identity.drop_instruction = MirInstructionId::new("inst:forged-drop").unwrap();
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, &forged)
            .is_err());

        let mut forged_source = function.clone();
        forged_source
            .ownership
            .checker_managed_any
            .as_mut()
            .and_then(|seal| seal.generic_identity.as_mut())
            .expect("private generic identity seal")
            .type_argument = managed.clone();
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, &forged_source)
            .is_err());

        let mut missing_seal = function.clone();
        missing_seal.ownership.checker_managed_any = None;
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, &missing_seal)
            .is_err());

        let mut reordered = function.clone();
        reordered
            .blocks
            .get_mut(&reordered.entry)
            .unwrap()
            .instructions
            .swap(0, 1);
        assert!(program
            .type_catalog()
            .validate_managed_any_identity_instance(instance, &reordered)
            .is_err());
    }

    #[test]
    fn recursive_any_detector_covers_every_aggregate_layout_and_cycles() {
        let checked = checked(SOURCE);
        let mut catalog = MirTypeCatalog::from_checked_program(&checked)
            .expect("checker-derived catalog with GetAny DynamicAny");
        let source_any = catalog
            .iter()
            .find_map(|(id, descriptor)| {
                (descriptor.kind == MirTypeKind::DynamicAny).then_some(id.clone())
            })
            .expect("one canonical source DynamicAny TypeDesc");
        let managed_any = managed_any_i32_string_v1_type_id();
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let install = |catalog: &mut MirTypeCatalog, name: &str, layout: MirLayout| {
            let id = ResolvedTypeId::synthetic(&format!("test:recursive-any:{name}"));
            catalog.replace_for_test_only(
                id.clone(),
                MirTypeDesc {
                    id: id.clone(),
                    kind: MirTypeKind::Nominal,
                    layout,
                    session_protocol: None,
                    ownership: MirOwnership::Copy,
                    abi: MirAbiClass::Aggregate,
                    needs_drop_glue: false,
                    needs_clone_glue: false,
                    glue: MirGlueContract {
                        move_out: MirGlueKind::Noop,
                        clone: MirGlueKind::Noop,
                        drop: MirGlueKind::Noop,
                    },
                    drop_plan: None,
                    variant_drop_plan: None,
                },
            );
            catalog.resolved_children.insert(id.clone(), Vec::new());
            id
        };
        let variant = |name: &str, ty: &ResolvedTypeId| MirVariantDesc {
            id: NodeId(format!("test:variant:{name}")),
            name: name.into(),
            discriminant: 0,
            fields: vec![MirFieldDesc {
                id: NodeId(format!("test:field:{name}")),
                name: "payload".into(),
                ty: ty.clone(),
            }],
        };

        let layouts = [
            ("tuple", MirLayout::Tuple(vec![source_any.clone()])),
            (
                "option-inner",
                MirLayout::Option {
                    inner: managed_any.clone(),
                    variants: vec![],
                },
            ),
            (
                "option-variant",
                MirLayout::Option {
                    inner: i32_type.clone(),
                    variants: vec![variant("option", &source_any)],
                },
            ),
            (
                "result",
                MirLayout::Result {
                    ok: i32_type.clone(),
                    error: source_any.clone(),
                    variants: vec![],
                },
            ),
            (
                "result-variant",
                MirLayout::Result {
                    ok: i32_type.clone(),
                    error: i32_type.clone(),
                    variants: vec![variant("result", &managed_any)],
                },
            ),
            (
                "enum",
                MirLayout::Enum {
                    nominal: crate::core::NominalTypeId::new("test:Enum").unwrap(),
                    variants: vec![variant("enum", &managed_any)],
                },
            ),
            (
                "array",
                MirLayout::Array {
                    element: source_any.clone(),
                    length: 2,
                },
            ),
            (
                "newtype",
                MirLayout::Newtype {
                    nominal: crate::core::NominalTypeId::new("test:Newtype").unwrap(),
                    inner: managed_any.clone(),
                },
            ),
            (
                "pointer",
                MirLayout::Pointer {
                    target: Some(source_any.clone()),
                },
            ),
            (
                "list",
                MirLayout::List {
                    element: managed_any.clone(),
                },
            ),
            (
                "set",
                MirLayout::Set {
                    element: source_any.clone(),
                },
            ),
            (
                "record",
                MirLayout::Record {
                    nominal: crate::core::NominalTypeId::new("test:Record").unwrap(),
                    fields: vec![variant("record", &managed_any).fields[0].clone()],
                },
            ),
            (
                "actor-state",
                MirLayout::ActorState {
                    actor: NodeId("test:Actor".into()),
                    fields: vec![variant("actor-state", &source_any).fields[0].clone()],
                },
            ),
        ];
        for (name, layout) in layouts {
            let wrapper = install(&mut catalog, name, layout);
            assert!(
                catalog.contains_dynamic_or_managed_any(&wrapper),
                "layout {name} must recursively detect Any"
            );
        }

        let scalar = install(&mut catalog, "scalar", MirLayout::Scalar);
        assert!(!catalog.contains_dynamic_or_managed_any(&scalar));
        for (name, layout) in [
            ("unit", MirLayout::Unit),
            ("handle", MirLayout::Handle),
            ("opaque", MirLayout::Opaque),
            ("map-root", MirLayout::MapRoot),
            ("map-any-owner", MirLayout::MapAnyOwnerV1),
            (
                "ordinary-map-owner",
                MirLayout::OrdinaryMapOwnerLifecycleI32StringV1,
            ),
            (
                "ordinary-map-get",
                MirLayout::OrdinaryMapOwnedGetI32StringV1,
            ),
        ] {
            let wrapper = install(&mut catalog, name, layout);
            assert!(
                !catalog.contains_dynamic_or_managed_any(&wrapper),
                "layout {name} with no Any descendants must be classified false"
            );
        }

        let known_empty_handle = install(&mut catalog, "known-empty-handle", MirLayout::Handle);
        assert!(
            !catalog.contains_dynamic_or_managed_any(&known_empty_handle),
            "a Handle with an explicit empty Checker child-edge row is known not to hide Any"
        );
        let mut missing_handle_edges = catalog.clone();
        missing_handle_edges
            .resolved_children
            .remove(&known_empty_handle);
        assert!(
            missing_handle_edges.contains_dynamic_or_managed_any(&known_empty_handle),
            "a Handle without its required source-child metadata is unclassifiable and fails closed"
        );

        let unknown_pointer = install(
            &mut catalog,
            "pointer-no-target",
            MirLayout::Pointer { target: None },
        );
        assert!(catalog.contains_dynamic_or_managed_any(&unknown_pointer));

        let protocol_only = install(&mut catalog, "session-protocol-edge", MirLayout::Handle);
        let mut protocol_descriptor = catalog.get(&protocol_only).unwrap().clone();
        protocol_descriptor.session_protocol = Some(source_any.clone());
        catalog.replace_for_test_only(protocol_only.clone(), protocol_descriptor);
        assert!(catalog.contains_dynamic_or_managed_any(&protocol_only));

        let drop_plan_only = install(&mut catalog, "drop-plan-edge", MirLayout::Handle);
        let mut drop_plan_descriptor = catalog.get(&drop_plan_only).unwrap().clone();
        drop_plan_descriptor.drop_plan = Some(crate::core::mir::types::MirDropGluePlan {
            fields: vec![crate::core::mir::types::MirDropGlueField {
                index: 0,
                ty: source_any.clone(),
                glue: MirGlueKind::Unsupported,
            }],
        });
        catalog.replace_for_test_only(drop_plan_only.clone(), drop_plan_descriptor);
        assert!(catalog.contains_dynamic_or_managed_any(&drop_plan_only));

        let variant_drop_plan_only =
            install(&mut catalog, "variant-drop-plan-edge", MirLayout::Handle);
        let mut variant_drop_plan_descriptor =
            catalog.get(&variant_drop_plan_only).unwrap().clone();
        variant_drop_plan_descriptor.variant_drop_plan =
            Some(vec![crate::core::mir::types::MirVariantDropGluePlan {
                variant: NodeId("test:variant-drop-edge".into()),
                fields: vec![crate::core::mir::types::MirDropGlueField {
                    index: 0,
                    ty: source_any.clone(),
                    glue: MirGlueKind::Unsupported,
                }],
            }]);
        catalog.replace_for_test_only(variant_drop_plan_only.clone(), variant_drop_plan_descriptor);
        assert!(catalog.contains_dynamic_or_managed_any(&variant_drop_plan_only));

        let cycle_a = ResolvedTypeId::synthetic("test:recursive-any:cycle-a");
        let cycle_b = ResolvedTypeId::synthetic("test:recursive-any:cycle-b");
        let cyclic_desc = |id: ResolvedTypeId, layout: MirLayout| MirTypeDesc {
            id,
            kind: MirTypeKind::Nominal,
            layout,
            session_protocol: None,
            ownership: MirOwnership::Copy,
            abi: MirAbiClass::Aggregate,
            needs_drop_glue: false,
            needs_clone_glue: false,
            glue: MirGlueContract {
                move_out: MirGlueKind::Noop,
                clone: MirGlueKind::Noop,
                drop: MirGlueKind::Noop,
            },
            drop_plan: None,
            variant_drop_plan: None,
        };
        catalog.replace_for_test_only(
            cycle_a.clone(),
            cyclic_desc(
                cycle_a.clone(),
                MirLayout::Newtype {
                    nominal: crate::core::NominalTypeId::new("test:CycleA").unwrap(),
                    inner: cycle_b.clone(),
                },
            ),
        );
        catalog.replace_for_test_only(
            cycle_b.clone(),
            cyclic_desc(cycle_b.clone(), MirLayout::Tuple(vec![cycle_a.clone()])),
        );
        assert!(!catalog.contains_dynamic_or_managed_any(&cycle_a));
        catalog.replace_for_test_only(
            cycle_b.clone(),
            cyclic_desc(cycle_b, MirLayout::Tuple(vec![cycle_a.clone(), source_any])),
        );
        assert!(catalog.contains_dynamic_or_managed_any(&cycle_a));
    }

    #[test]
    fn recursive_any_detector_follows_erased_resolved_type_edges() {
        let checked = checked(SOURCE);
        let mut resolved = checked.resolved_types().clone();
        let source_any = resolved
            .iter()
            .find_map(|(id, ty)| {
                matches!(ty, ResolvedType::DynamicAny { .. }).then_some(id.clone())
            })
            .expect("Checker DynamicAny type");
        let i32_type = crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let unit_type = resolved
            .intern_resolved(ResolvedType::Primitive(PrimitiveType::Unit))
            .expect("unit type");
        let function_parameter_any = resolved
            .intern_resolved(ResolvedType::Function {
                abi: crate::core::ir::FunctionTypeAbi::Mimi,
                parameters: vec![source_any.clone()],
                result: i32_type.clone(),
            })
            .expect("function parameter with Any");
        let function_result_any = resolved
            .intern_resolved(ResolvedType::Function {
                abi: crate::core::ir::FunctionTypeAbi::Mimi,
                parameters: vec![i32_type.clone()],
                result: source_any.clone(),
            })
            .expect("function result with Any");
        let ownership_target_any = resolved
            .intern_resolved(ResolvedType::Ownership {
                kind: crate::core::ir::OwnershipTypeKind::Shared,
                target: source_any.clone(),
            })
            .expect("Ownership<Any>");
        let weak_ownership_target_any = resolved
            .intern_resolved(ResolvedType::Ownership {
                kind: crate::core::ir::OwnershipTypeKind::Weak,
                target: source_any.clone(),
            })
            .expect("Weak<Any>");
        let opaque_nominal_any = resolved
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("test:OpaqueBox").unwrap(),
                arguments: vec![source_any.clone()],
                is_linear: false,
            })
            .expect("opaque nominal with Any argument");
        let function_without_any = resolved
            .intern_resolved(ResolvedType::Function {
                abi: crate::core::ir::FunctionTypeAbi::Mimi,
                parameters: vec![i32_type.clone()],
                result: unit_type,
            })
            .expect("function without Any");
        let ownership_without_any = resolved
            .intern_resolved(ResolvedType::Ownership {
                kind: crate::core::ir::OwnershipTypeKind::Shared,
                target: i32_type.clone(),
            })
            .expect("Ownership<i32>");
        let opaque_nominal_without_any = resolved
            .intern_resolved(ResolvedType::Nominal {
                item: crate::core::NominalTypeId::new("test:OpaquePlain").unwrap(),
                arguments: vec![i32_type.clone()],
                is_linear: false,
            })
            .expect("opaque nominal without Any");
        let string_type = crate::core::ir::primitive_type_id(PrimitiveType::String);
        let function_any_tuple = resolved
            .intern_resolved(ResolvedType::Tuple(vec![
                string_type.clone(),
                function_result_any.clone(),
            ]))
            .expect("tuple with owned String and function metadata containing Any");
        let reversed_function_any_tuple = resolved
            .intern_resolved(ResolvedType::Tuple(vec![
                function_parameter_any.clone(),
                string_type.clone(),
            ]))
            .expect("tuple with function metadata containing Any and owned String");
        let catalog = MirTypeCatalog::from_resolved_types(&resolved).expect("catalog");

        assert_eq!(
            catalog.resolved_type_children(&function_parameter_any),
            Some(&[source_any.clone(), i32_type.clone()][..]),
            "callers can serialize the exact canonical function parameter and result child IDs"
        );
        assert_eq!(
            catalog.resolved_type_children(&function_result_any),
            Some(&[i32_type.clone(), source_any.clone()][..]),
            "function result child identity is included after all parameter IDs"
        );
        assert_eq!(
            catalog.resolved_type_children(&ownership_target_any),
            Some(&[source_any.clone()][..])
        );
        assert_eq!(
            catalog.resolved_type_children(&weak_ownership_target_any),
            Some(&[source_any.clone()][..])
        );
        assert_eq!(
            catalog.resolved_type_children(&opaque_nominal_any),
            Some(&[source_any.clone()][..])
        );

        for ty in [
            function_parameter_any.clone(),
            function_result_any.clone(),
            ownership_target_any.clone(),
            weak_ownership_target_any.clone(),
            opaque_nominal_any.clone(),
        ] {
            assert!(
                catalog.contains_dynamic_or_managed_any(&ty),
                "hidden resolved edge in '{}' must be traversed",
                ty.as_str()
            );
        }
        for ty in [
            function_without_any.clone(),
            ownership_without_any.clone(),
            opaque_nominal_without_any.clone(),
        ] {
            assert!(
                !catalog.contains_dynamic_or_managed_any(&ty),
                "ordinary opaque handle '{}' must not be treated as Any",
                ty.as_str()
            );
        }
        let projection_error = catalog
            .validate_move_projection(
                &function_any_tuple,
                &string_type,
                &crate::core::mir::MirProjection::Tuple(0),
            )
            .expect_err(
                "a Copy function handle with recursive Any metadata is not a discardable residual",
            );
        assert!(
            projection_error.contains("tuple leaves non-Copy sibling"),
            "{projection_error}"
        );
        let reversed_projection_error = catalog
            .validate_move_projection(
                &reversed_function_any_tuple,
                &string_type,
                &crate::core::mir::MirProjection::Tuple(1),
            )
            .expect_err(
                "a reversed tuple cannot discard a Copy function handle whose signature contains Any",
            );
        assert!(
            reversed_projection_error.contains("tuple leaves non-Copy sibling"),
            "{reversed_projection_error}"
        );

        let mut absent_child_catalog = catalog.clone();
        let absent_child = ResolvedTypeId::synthetic("test:recursive-any:missing-child");
        absent_child_catalog
            .resolved_children
            .insert(i32_type, vec![absent_child]);
        assert!(absent_child_catalog.contains_dynamic_or_managed_any(
            &crate::core::ir::primitive_type_id(PrimitiveType::I32)
        ));

        let mut missing_function_metadata = catalog.clone();
        missing_function_metadata
            .resolved_children
            .remove(&function_without_any);
        assert!(missing_function_metadata.contains_dynamic_or_managed_any(&function_without_any));
        let mut missing_ownership_metadata = catalog.clone();
        missing_ownership_metadata
            .resolved_children
            .remove(&ownership_without_any);
        assert!(missing_ownership_metadata.contains_dynamic_or_managed_any(&ownership_without_any));
        let mut missing_opaque_nominal_metadata = catalog.clone();
        missing_opaque_nominal_metadata
            .resolved_children
            .remove(&opaque_nominal_without_any);
        assert!(missing_opaque_nominal_metadata
            .contains_dynamic_or_managed_any(&opaque_nominal_without_any));
    }

    #[test]
    fn same_type_desc_does_not_authorize_forged_or_missing_owner_receipts() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let original = program
            .functions()
            .values()
            .find(|function| function.ownership.checker_managed_any.is_some())
            .expect("Any function");
        let mut missing = original.clone();
        missing.ownership.checker_managed_any = None;
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&missing)
            .is_err());
        let mut substituted = original.clone();
        let id = substituted
            .values
            .values()
            .find(|value| program.type_catalog().contains_managed_any(&value.ty))
            .unwrap()
            .id
            .clone();
        let same_type = substituted.values[&id].ty.clone();
        let forged = MirValueId::new("forged:same-managed-descriptor").unwrap();
        substituted.values.insert(
            forged.clone(),
            super::super::MirValue {
                id: forged,
                ty: same_type,
            },
        );
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&substituted)
            .is_err());
    }
    #[test]
    fn managed_any_receipt_rejects_wrong_owner_kind_version_and_origin_before_consumption() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let original = program
            .functions()
            .values()
            .find(|function| {
                function.blocks.values().any(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(
                            instruction.kind,
                            MirInstructionKind::ManagedAny {
                                operation: MirManagedAnyOperation::PackI32,
                                ..
                            }
                        )
                    })
                })
            })
            .expect("PackI32 function");
        for field in 0..4 {
            let mut forged = original.clone();
            let instruction = forged
                .blocks
                .values_mut()
                .flat_map(|block| &mut block.instructions)
                .find(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ManagedAny {
                            operation: MirManagedAnyOperation::PackI32,
                            ..
                        }
                    )
                })
                .unwrap();
            let MirInstructionKind::ManagedAny {
                contract: Some(receipt),
                ..
            } = &mut instruction.kind
            else {
                unreachable!()
            };
            match field {
                0 => receipt.owner = NodeId("function:forged".into()),
                1 => receipt.expected_kind = Some(2),
                2 => receipt.descriptor_version = 2,
                _ => receipt.origin = NodeId("node:forged".into()),
            }
            assert!(forged.validate().is_err());
            assert!(program
                .type_catalog()
                .validate_managed_any_function(&forged)
                .is_err());
        }
    }
    #[test]
    fn aggregate_any_owner_cannot_be_raw_copied_or_reorder_a_drop() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let original = program
            .functions()
            .values()
            .find(|function| {
                function.blocks.values().any(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(
                            instruction.kind,
                            MirInstructionKind::ManagedAny {
                                operation: MirManagedAnyOperation::Clone,
                                ..
                            }
                        )
                    })
                })
            })
            .unwrap();
        let mut forged = original.clone();
        let instruction = forged
            .blocks
            .values_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Clone,
                        ..
                    }
                )
            })
            .unwrap();
        let MirInstructionKind::ManagedAny {
            source,
            result: Some(result),
            ..
        } = &instruction.kind
        else {
            unreachable!()
        };
        instruction.kind = MirInstructionKind::Copy {
            source: source.clone(),
            result: result.clone(),
        };
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&forged)
            .is_err());
        let clone_block_id = original
            .blocks
            .iter()
            .find(|(_, block)| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction.kind,
                        MirInstructionKind::ManagedAny {
                            operation: MirManagedAnyOperation::Clone,
                            ..
                        }
                    )
                })
            })
            .map(|(id, _)| id.clone())
            .expect("the selected owner body contains its Clone");
        let mut reordered = original.clone();
        // The canonical ManagedAny identity specialization may return its
        // cloned owner directly, so its matching Drop can live in the caller
        // rather than this one-block helper. Reorder the sealed Clone itself
        // when a block has more than one instruction; the private body seal
        // must reject that mutation just as it rejects a reordered Drop.
        let block = reordered
            .blocks
            .get_mut(&clone_block_id)
            .expect("the selected owner body contains its clone block");
        let clone_index = block
            .instructions
            .iter()
            .position(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::ManagedAny {
                        operation: MirManagedAnyOperation::Clone,
                        ..
                    }
                )
            })
            .expect("the selected owner body contains its canonical Clone");
        if block.instructions.len() > 1 {
            let last_index = block.instructions.len() - 1;
            if clone_index == last_index {
                block.instructions.swap(clone_index, 0);
            } else {
                block.instructions.swap(clone_index, last_index);
            }
        } else {
            // A one-instruction helper has no legal in-block reorder. The
            // forged Copy above already demonstrates that raw duplication is
            // rejected; mutate the instruction identity so the sealed body is
            // still proven to reject a reordered/rewritten owner event.
            block.instructions[clone_index].id =
                MirInstructionId::new("forged:reordered-clone").expect("forged instruction id");
        }
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&reordered)
            .is_err());
    }
    #[test]
    fn owned_any_parameter_receipt_cannot_be_rewritten_as_a_loan() {
        let program = MirProgram::from_checked_program(&checked(SOURCE)).expect("fixture MIR");
        let managed_type = managed_any_i32_string_v1_type_id();
        // `Any` is an internal DynamicAny source shape.  The user-facing
        // fixture reaches it through the generic `carry<T>` identity, whose
        // canonical ManagedAny specialization is the only function with an
        // owned ManagedAny parameter.  The old test looked up a removed
        // `relay(Any)` declaration and panicked before exercising the seal.
        let original = program
            .functions()
            .values()
            .find(|function| {
                function.parameters.len() == 1
                    && function
                        .values
                        .get(&function.parameters[0])
                        .is_some_and(|value| value.ty == managed_type)
                    && function
                        .ownership
                        .checker_managed_any
                        .as_ref()
                        .is_some_and(|seal| seal.generic_identity.is_some())
            })
            .expect("the fixture has one sealed generic ManagedAny identity specialization");
        for permission in [
            crate::core::ir::Permission::View,
            crate::core::ir::Permission::Mutate,
        ] {
            let mut forged = original.clone();
            forged.parameter_permissions = Some(vec![Some(permission)]);
            assert!(program
                .type_catalog()
                .validate_managed_any_function(&forged)
                .is_err());
        }
        let mut missing = original.clone();
        missing.parameter_permissions = None;
        assert!(program
            .type_catalog()
            .validate_managed_any_function(&missing)
            .is_err());
    }

    #[test]
    fn view_and_mutate_any_parameters_stay_outside_owned_snapshot_profile() {
        for permission in ["view", "mutate"] {
            let source = format!(
                r#"
func inspect(value: {permission} Any) -> i32 {{ let number: i32 = value; number }}
func main() -> i32 {{
    let root = map_new()
    let inserted = map_set(root, "n", 7)
    let (found, value) = map_get(inserted, "n")
    let number = inspect(value)
    let retained: i32 = value
    drop(value)
    number + retained
}}
"#
            );
            let tokens = crate::lexer::Lexer::new(&source).tokenize().unwrap();
            let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
            if let Ok(checked) = crate::core::check_program(&file) {
                assert!(
                    MirProgram::from_checked_program(&checked).is_err(),
                    "borrowed Any needs a loan receipt before canonical owner cleanup can run"
                );
            }
        }
    }

    #[test]
    fn managed_any_ingress_does_not_erase_a_strict_linear_payload() {
        let source = r#"func main() -> i32 {
            let root = map_new()
            let inserted = map_set(root, "token", make_token())
            drop(root)
            drop(inserted)
            0
        }"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
        let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
        let errors = crate::core::check_program(&file)
            .expect_err("linear payload cannot enter ordinary Any");
        assert!(errors
            .iter()
            .any(|error| error.code.as_deref() == Some("E0432")));
    }
}
