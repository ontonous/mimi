//! Checker-sealed, no-capture local closure calls.
//!
//! Version one is deliberately smaller than “closures”: the only source
//! closure admitted here is a directly bound, immutable, zero-argument Mimi
//! lambda returning `(bool, DynamicAny)` whose own callable body has the
//! independent ordinary-Map owned-GetAny receipt.  A closure binding is a
//! static code identity, not a runtime function pointer.  Every invocation is
//! an explicit direct edge to the exact checked lambda owner.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::core::cfg::CallableCfg;
use crate::core::ir::{
    BuiltinId, FunctionTypeAbi, ResolvedBlock, ResolvedBody, ResolvedCallable, ResolvedCallee,
    ResolvedExpr, ResolvedExprKind, ResolvedLambda, ResolvedLocalId, ResolvedPattern,
    ResolvedPatternKind, ResolvedStmt, ResolvedStmtKind, ResolvedType, ResolvedTypeId,
    ResolvedTypeTable,
};
use crate::core::{CheckedProgram, NodeId, PrimitiveType};
use crate::span::SourceId;

use super::lower::{
    is_comptime_callable, lower_callable_with_type_catalog_and_permissions_and_constants,
    MirLoweringError,
};
use super::types::{MirTypeCatalog, MirTypeKind};
use super::{MirFunction, MirInstructionId, MirInstructionKind, MirValueId};

pub const MIR_CLOSURE_CALLABLE_ABI_V1_SCHEMA: &str = "mimi-mir-closure-callable-abi-v1";
pub const MIR_CLOSURE_CALLABLE_RECEIPT_V1_SCHEMA: &str = "mimi-mir-closure-callable-receipt-v1";

/// Source ABI facts are copied from one Checker-resolved function type. V1
/// requires `Mimi`, no parameters, and no captures; those empty lists are
/// explicit so a forged opcode cannot add an environment or argument suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirClosureCallableAbiV1 {
    pub schema: &'static str,
    pub source_function_type: ResolvedTypeId,
    pub source_abi: FunctionTypeAbi,
    pub source_parameter_types: Vec<ResolvedTypeId>,
    pub source_result_type: ResolvedTypeId,
    pub capture_types: Vec<ResolvedTypeId>,
}

impl MirClosureCallableAbiV1 {
    pub(crate) fn canonical_text(&self) -> String {
        format!(
            "schema={} function_type={} source_abi={:?} parameters=[{}] result={} captures=[{}]",
            self.schema,
            self.source_function_type.as_str(),
            self.source_abi,
            self.source_parameter_types
                .iter()
                .map(ResolvedTypeId::as_str)
                .collect::<Vec<_>>()
                .join(","),
            self.source_result_type.as_str(),
            self.capture_types
                .iter()
                .map(ResolvedTypeId::as_str)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckerClosureSnapshotV1 {
    parent: NodeId,
    closure_local: ResolvedLocalId,
    function_value: MirValueId,
    bind_instruction: MirInstructionId,
    lambda_expression: NodeId,
    source_id: SourceId,
    callee: NodeId,
    abi: MirClosureCallableAbiV1,
    invoke_instructions: Vec<MirInstructionId>,
    map_actions: Vec<crate::core::OrdinaryMapOwnedGetI32StringV1Action>,
    map_new_attempts: BTreeSet<NodeId>,
    map_operation_attempts: BTreeSet<crate::core::OrdinaryMapOwnedGetI32StringV1Attempt>,
    map_cfg_receipt: crate::core::OrdinaryMapOwnedGetI32StringV1CfgReceipt,
    target_body: MirClosureCallableBodySealV1,
}

/// Exact lowered callable content covered by the Checker closure plan. The
/// separate ownership summary is deliberately excluded: its independently
/// sealed source receipts are validated by their own whole-function gates.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MirClosureCallableBodySealV1 {
    owner: crate::core::NodeId,
    parameters: Vec<MirValueId>,
    parameter_permissions: Option<Vec<Option<crate::core::ir::Permission>>>,
    result: ResolvedTypeId,
    entry: super::MirBlockId,
    values: BTreeMap<MirValueId, super::MirValue>,
    blocks: BTreeMap<super::MirBlockId, super::MirBlock>,
    contracts: Vec<super::MirContract>,
    proof_worklist: Option<super::MirProofWorklist>,
}

impl MirClosureCallableBodySealV1 {
    fn from_function(function: &MirFunction) -> Self {
        Self {
            owner: function.owner.clone(),
            parameters: function.parameters.clone(),
            parameter_permissions: function.parameter_permissions.clone(),
            result: function.result.clone(),
            entry: function.entry.clone(),
            values: function.values.clone(),
            blocks: function.blocks.clone(),
            contracts: function.contracts.clone(),
            proof_worklist: function.proof_worklist.clone(),
        }
    }

    fn matches(&self, function: &MirFunction) -> bool {
        self == &Self::from_function(function)
    }
}

/// Public, versioned identity of a single static closure binding. The private
/// snapshot is minted only by the CheckedProgram-to-MIR producer and retains
/// the source Map CFG proof; consumers must run the shared preflight before
/// any observable effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirClosureCallableReceiptV1 {
    pub schema: &'static str,
    pub parent: NodeId,
    pub closure_local: ResolvedLocalId,
    pub function_value: MirValueId,
    pub bind_instruction: MirInstructionId,
    pub lambda_expression: NodeId,
    pub source_id: SourceId,
    pub callee: NodeId,
    pub abi: MirClosureCallableAbiV1,
    pub invoke_instructions: Vec<MirInstructionId>,
    checker_snapshot: CheckerClosureSnapshotV1,
}

/// Metadata consumed by the parent callable's MIR lowerer. It contains no
/// backend spelling and cannot authorize arbitrary local function values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MirClosureCallableBindingV1 {
    pub(crate) closure_local: ResolvedLocalId,
    pub(crate) function_value: MirValueId,
    pub(crate) bind_instruction: MirInstructionId,
    pub(crate) lambda_expression: NodeId,
    pub(crate) callee: NodeId,
    pub(crate) abi: MirClosureCallableAbiV1,
}

#[derive(Debug, Clone)]
struct PlannedClosureV1 {
    parent: NodeId,
    binding: MirClosureCallableBindingV1,
    source_id: SourceId,
    invoke_instructions: Vec<MirInstructionId>,
    callable: ResolvedCallable,
    map_actions: Vec<crate::core::OrdinaryMapOwnedGetI32StringV1Action>,
    map_new_attempts: BTreeSet<NodeId>,
    map_operation_attempts: BTreeSet<crate::core::OrdinaryMapOwnedGetI32StringV1Attempt>,
    map_cfg_receipt: crate::core::OrdinaryMapOwnedGetI32StringV1CfgReceipt,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CheckedClosureCallablePlanV1 {
    by_parent: BTreeMap<NodeId, BTreeMap<ResolvedLocalId, MirClosureCallableBindingV1>>,
    closures: BTreeMap<ResolvedLocalId, PlannedClosureV1>,
}

#[derive(Debug, Clone)]
struct BoundLambda<'a> {
    local: ResolvedLocalId,
    expression: &'a ResolvedExpr,
    lambda: &'a ResolvedLambda,
}

#[derive(Debug, Clone)]
struct ClosureInvokeSource {
    node: NodeId,
    source_id: SourceId,
    result: ResolvedTypeId,
    permission: Option<crate::core::ir::Permission>,
    argument_count: usize,
    type_argument_count: usize,
    effects: usize,
    has_session: bool,
}

#[derive(Debug, Default)]
struct UseCensus {
    invokes: Vec<ClosureInvokeSource>,
    rejected: Vec<String>,
}

impl CheckedClosureCallablePlanV1 {
    pub(crate) fn has_source_candidate(program: &CheckedProgram) -> bool {
        program.callables().iter().any(|(owner, callable)| {
            !is_comptime_callable(program, owner)
                && root_bound_lambdas(owner, &callable.body.root)
                    .iter()
                    .any(|bound| {
                        function_returns_owned_any_pair(
                            &bound.expression.ty,
                            program.resolved_types(),
                        )
                    })
        })
    }

    pub(crate) fn derive(
        program: &CheckedProgram,
        type_catalog: &MirTypeCatalog,
        excluded_sources: Option<&HashSet<SourceId>>,
    ) -> Result<Self, Vec<String>> {
        Self::derive_with_owner_scope(program, type_catalog, excluded_sources, None)
    }

    pub(crate) fn derive_with_owner_scope(
        program: &CheckedProgram,
        type_catalog: &MirTypeCatalog,
        excluded_sources: Option<&HashSet<SourceId>>,
        owner_scope: Option<&BTreeSet<NodeId>>,
    ) -> Result<Self, Vec<String>> {
        let plan = Self::derive_source_with_owner_scope(program, excluded_sources, owner_scope)?;
        for closure in plan.closures.values() {
            type_catalog
                .validate_ordinary_map_owned_get_any_result_type(
                    &type_catalog
                        .canonical_managed_any_type(&closure.binding.abi.source_result_type),
                )
                .map_err(|message| {
                    vec![format!(
                        "checked closure '{}' is outside the no-capture Map GetAny ABI v1: {message}",
                        closure.binding.callee.0
                    )]
                })?;
        }
        Ok(plan)
    }

    /// Derive source identities, complete callable-local CFG receipts, and
    /// closure-use census without consulting TypeDesc materialization. This
    /// source-only projection is used while `MirTypeCatalog` is being built;
    /// it must stay terminal with respect to the catalog to avoid a
    /// type-catalog -> route-admission -> closure-plan -> type-catalog cycle.
    pub(crate) fn derive_source(
        program: &CheckedProgram,
        excluded_sources: Option<&HashSet<SourceId>>,
    ) -> Result<Self, Vec<String>> {
        Self::derive_source_with_owner_scope(program, excluded_sources, None)
    }

    pub(crate) fn derive_source_with_owner_scope(
        program: &CheckedProgram,
        excluded_sources: Option<&HashSet<SourceId>>,
        owner_scope: Option<&BTreeSet<NodeId>>,
    ) -> Result<Self, Vec<String>> {
        let mut plan = Self::default();
        let mut errors = Vec::new();
        let mut owners = BTreeSet::new();
        for (parent_owner, parent) in program.callables() {
            if is_comptime_callable(program, parent_owner)
                || owner_scope.is_some_and(|scope| !scope.contains(parent_owner))
                || excluded_sources.is_some_and(|excluded| {
                    excluded.contains(&parent.body.root.origin.user_span().source_id)
                })
            {
                continue;
            }
            for bound in root_bound_lambdas(parent_owner, &parent.body.root) {
                if excluded_sources.is_some_and(|excluded| {
                    excluded.contains(&bound.expression.origin.user_span().source_id)
                }) {
                    continue;
                }
                if !function_returns_owned_any_pair(&bound.expression.ty, program.resolved_types())
                {
                    continue;
                }
                let attempt = plan_one_closure(program, parent_owner, parent, bound);
                match attempt {
                    Ok(closure) => {
                        if !owners.insert(closure.binding.callee.clone()) {
                            errors.push(format!(
                                "closure owner '{}' is bound from more than one checked source site",
                                closure.binding.callee.0
                            ));
                            continue;
                        }
                        let local = closure.binding.closure_local.clone();
                        let binding = closure.binding.clone();
                        plan.by_parent
                            .entry(parent_owner.clone())
                            .or_default()
                            .insert(local.clone(), binding);
                        if plan.closures.insert(local.clone(), closure).is_some() {
                            errors.push(format!(
                                "closure local '{}' has duplicate checked binding receipts",
                                local.0 .0
                            ));
                        }
                    }
                    Err(message) => errors.push(message),
                }
            }
        }
        if errors.is_empty() {
            Ok(plan)
        } else {
            Err(errors)
        }
    }

    pub(crate) fn bindings_by_parent(
        &self,
    ) -> BTreeMap<NodeId, BTreeMap<ResolvedLocalId, MirClosureCallableBindingV1>> {
        self.by_parent.clone()
    }

    pub(crate) fn ordinary_map_ledgers(
        &self,
    ) -> Vec<(
        NodeId,
        Vec<crate::core::OrdinaryMapOwnedGetI32StringV1Action>,
        BTreeSet<NodeId>,
        BTreeSet<crate::core::OrdinaryMapOwnedGetI32StringV1Attempt>,
    )> {
        self.closures
            .values()
            .map(|closure| {
                (
                    closure.binding.callee.clone(),
                    closure.map_actions.clone(),
                    closure.map_new_attempts.clone(),
                    closure.map_operation_attempts.clone(),
                )
            })
            .collect()
    }

    /// Whether one source-sealed closure has the requested action in its
    /// independent ordinary-Map owned-GetAny ledger. This is descriptive
    /// TypeDesc evidence only; whole-program Map route admission still checks
    /// attempt coverage separately.
    pub(crate) fn has_ordinary_map_action(
        &self,
        kind: crate::core::OrdinaryMapOwnedGetI32StringV1ActionKind,
    ) -> bool {
        self.closures.values().any(|closure| {
            closure.map_actions.iter().any(|action| {
                action.profile_version
                    == crate::core::OrdinaryMapOwnedGetI32StringV1ProfileVersion::V1
                    && action.kind == kind
            })
        })
    }

    pub(crate) fn lower_functions(
        &self,
        type_catalog: &MirTypeCatalog,
        checked_constants: &std::collections::HashMap<NodeId, crate::core::ResolvedConstant>,
    ) -> Result<BTreeMap<NodeId, MirFunction>, Vec<MirLoweringError>> {
        let mut functions = BTreeMap::new();
        let mut errors = Vec::new();
        for closure in self.closures.values() {
            match lower_callable_with_type_catalog_and_permissions_and_constants(
                &closure.callable,
                type_catalog,
                None,
                checked_constants,
            ) {
                Ok(function) => {
                    if functions.insert(function.owner.clone(), function).is_some() {
                        errors.push(MirLoweringError {
                            node_id: closure.binding.callee.clone(),
                            message: "two source closure receipts lower to the same callable owner"
                                .into(),
                        });
                    }
                }
                Err(mut body_errors) => errors.append(&mut body_errors),
            }
        }
        if errors.is_empty() {
            Ok(functions)
        } else {
            Err(errors)
        }
    }

    pub(crate) fn materialize_receipts(
        &self,
        functions: &BTreeMap<NodeId, MirFunction>,
    ) -> Result<BTreeMap<ResolvedLocalId, MirClosureCallableReceiptV1>, Vec<String>> {
        let mut receipts = BTreeMap::new();
        let mut errors = Vec::new();
        for (local, closure) in &self.closures {
            let Some(parent) = functions.get(&closure.parent) else {
                errors.push(format!(
                    "closure parent '{}' has no lowered MIR function",
                    closure.parent.0
                ));
                continue;
            };
            let Some(bind) = find_instruction(parent, &closure.binding.bind_instruction) else {
                errors.push(format!(
                    "closure '{}' has no exact ClosureBind instruction",
                    local.0 .0
                ));
                continue;
            };
            if !matches!(
                &bind.kind,
                MirInstructionKind::ClosureBind {
                    closure_local,
                    function_value,
                    callee,
                    abi,
                } if closure_local == local
                    && function_value == &closure.binding.function_value
                    && callee == &closure.binding.callee
                    && abi == &closure.binding.abi
            ) {
                errors.push(format!(
                    "closure '{}' ClosureBind differs from its Checker source identity",
                    local.0 .0
                ));
                continue;
            }
            let mut found_invokes = Vec::new();
            for block in parent.blocks.values() {
                for instruction in &block.instructions {
                    if let MirInstructionKind::ClosureInvoke {
                        closure_local,
                        function_value,
                        callee,
                        type_arguments,
                        arguments,
                        abi,
                        ..
                    } = &instruction.kind
                    {
                        if closure_local == local {
                            if function_value != &closure.binding.function_value
                                || callee != &closure.binding.callee
                                || !type_arguments.is_empty()
                                || !arguments.is_empty()
                                || abi != &closure.binding.abi
                            {
                                errors.push(format!(
                                    "closure '{}' ClosureInvoke has a forged static-call ABI",
                                    local.0 .0
                                ));
                            }
                            found_invokes.push(instruction.id.clone());
                        }
                    }
                }
            }
            found_invokes.sort();
            if found_invokes != closure.invoke_instructions {
                errors.push(format!(
                    "closure '{}' invoke census differs from its Checker use census",
                    local.0 .0
                ));
                continue;
            }
            let Some(target) = functions.get(&closure.binding.callee) else {
                errors.push(format!(
                    "closure target '{}' has no lowered MIR function",
                    closure.binding.callee.0
                ));
                continue;
            };
            let checker_snapshot = CheckerClosureSnapshotV1 {
                parent: closure.parent.clone(),
                closure_local: local.clone(),
                function_value: closure.binding.function_value.clone(),
                bind_instruction: closure.binding.bind_instruction.clone(),
                lambda_expression: closure.binding.lambda_expression.clone(),
                source_id: closure.source_id,
                callee: closure.binding.callee.clone(),
                abi: closure.binding.abi.clone(),
                invoke_instructions: closure.invoke_instructions.clone(),
                map_actions: closure.map_actions.clone(),
                map_new_attempts: closure.map_new_attempts.clone(),
                map_operation_attempts: closure.map_operation_attempts.clone(),
                map_cfg_receipt: closure.map_cfg_receipt.clone(),
                target_body: MirClosureCallableBodySealV1::from_function(target),
            };
            receipts.insert(
                local.clone(),
                MirClosureCallableReceiptV1 {
                    schema: MIR_CLOSURE_CALLABLE_RECEIPT_V1_SCHEMA,
                    parent: closure.parent.clone(),
                    closure_local: local.clone(),
                    function_value: closure.binding.function_value.clone(),
                    bind_instruction: closure.binding.bind_instruction.clone(),
                    lambda_expression: closure.binding.lambda_expression.clone(),
                    source_id: closure.source_id,
                    callee: closure.binding.callee.clone(),
                    abi: closure.binding.abi.clone(),
                    invoke_instructions: closure.invoke_instructions.clone(),
                    checker_snapshot,
                },
            );
        }
        if errors.is_empty() {
            Ok(receipts)
        } else {
            Err(errors)
        }
    }
}

fn plan_one_closure(
    program: &CheckedProgram,
    parent_owner: &NodeId,
    parent: &ResolvedCallable,
    bound: BoundLambda<'_>,
) -> Result<PlannedClosureV1, String> {
    let reject = |reason: &str| {
        format!(
            "checked closure '{}' is outside the no-capture Map GetAny ABI v1: {reason}",
            bound.lambda.owner.0
        )
    };
    if bound.lambda.captures.len() != 0 {
        return Err(reject("captures are not admitted"));
    }
    if !bound.lambda.parameters.is_empty() {
        return Err(reject("v1 requires a zero-argument callable"));
    }
    let Some(local_info) = parent.body.locals.get(&bound.local) else {
        return Err(reject(
            "binding local is absent from its parent ResolvedBody",
        ));
    };
    if local_info.mutable || local_info.ty != bound.expression.ty {
        return Err(reject(
            "binding is mutable or its type differs from the lambda",
        ));
    }
    let ResolvedType::Function {
        abi: source_abi,
        parameters: source_parameter_types,
        result: source_result_type,
    } = program
        .resolved_types()
        .get(&bound.expression.ty)
        .ok_or_else(|| reject("function TypeDesc is absent from the checked type table"))?
    else {
        return Err(reject("bound lambda has no canonical function type"));
    };
    if *source_abi != FunctionTypeAbi::Mimi || !source_parameter_types.is_empty() {
        return Err(reject("source callable ABI or arity is not exact"));
    }
    if !returns_owned_any_pair(source_result_type, program.resolved_types()) {
        return Err(reject("result is not the complete (bool, DynamicAny) pair"));
    }
    let abi = MirClosureCallableAbiV1 {
        schema: MIR_CLOSURE_CALLABLE_ABI_V1_SCHEMA,
        source_function_type: bound.expression.ty.clone(),
        source_abi: *source_abi,
        source_parameter_types: source_parameter_types.clone(),
        source_result_type: source_result_type.clone(),
        capture_types: Vec::new(),
    };

    let mut census = UseCensus::default();
    scan_block_uses(
        &parent.body.root,
        &bound.local,
        &bound.lambda.owner,
        &mut census,
    );
    for expression in parent
        .body
        .place_inputs
        .values()
        .chain(parent.body.default_values.values())
    {
        scan_expr_uses(expression, &bound.local, &bound.lambda.owner, &mut census);
    }
    for (owner, callable) in program.callables() {
        if owner != parent_owner && callable.body.captures.contains(&bound.local) {
            census
                .rejected
                .push(format!("capture by callable '{}'", owner.0));
        }
    }
    if !census.rejected.is_empty() {
        return Err(reject(&census.rejected.join("; ")));
    }
    if census.invokes.is_empty() {
        return Err(reject("the bound local has no direct invocation"));
    }
    let mut invoke_instructions = Vec::with_capacity(census.invokes.len());
    let source_id = bound.expression.origin.user_span().source_id;
    if bound.lambda.body.origin.user_span().source_id != source_id {
        return Err(reject(
            "lambda body SourceId differs from its binding expression",
        ));
    }
    for invoke in &census.invokes {
        if invoke.source_id != source_id
            || invoke.result != *source_result_type
            || invoke.permission.is_some()
            || invoke.argument_count != 0
            || invoke.type_argument_count != 0
            || invoke.effects != 0
            || invoke.has_session
        {
            return Err(reject(
                "an invocation disagrees with the sealed zero-argument ABI",
            ));
        }
        invoke_instructions.push(
            MirInstructionId::new(format!("inst:closure_invoke:{}", invoke.node.0))
                .map_err(|error| reject(&error.to_string()))?,
        );
    }
    invoke_instructions.sort();
    if invoke_instructions
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        return Err(reject("invocation identity is duplicated"));
    }

    let (synthetic_body, signature, cfg, map_collection) =
        build_lambda_callable(program, parent, &bound, &abi)?;
    if map_collection.owner != bound.lambda.owner
        || map_collection.cfg_receipt.owner != bound.lambda.owner
        || cfg.owner != bound.lambda.owner
        || synthetic_body.owner != bound.lambda.owner
    {
        return Err(reject("lambda body, CFG, and Map receipt owners disagree"));
    }
    validate_snapshot_body_effects_and_calls(
        &bound.lambda.body,
        &map_collection.operation_attempts,
    )
    .map_err(|message| reject(&message))?;
    let mut actions = map_collection.actions.clone();
    actions.sort_by(|left, right| left.point.cmp(&right.point));
    use crate::core::{
        OrdinaryMapOwnedGetI32StringV1ActionKind as Action,
        OrdinaryMapOwnedGetI32StringV1AttemptKind as Attempt,
    };
    if actions
        .iter()
        .filter(|action| action.kind == Action::GetAny)
        .count()
        != 1
        || actions
            .iter()
            .filter(|action| action.kind == Action::New)
            .count()
            != 1
        || actions
            .iter()
            .filter(|action| action.kind == Action::Set)
            .count()
            != 1
        || actions
            .iter()
            .filter(|action| action.kind == Action::Drop)
            .count()
            < 2
        || actions.iter().any(|action| {
            !matches!(
                action.kind,
                Action::New | Action::Set | Action::GetAny | Action::Drop
            )
        })
        || map_collection.operation_attempts.iter().any(|attempt| {
            !matches!(
                attempt.kind,
                Attempt::New
                    | Attempt::Set
                    | Attempt::UntypedGet
                    | Attempt::Drop
                    | Attempt::ScopeDrop
            )
        })
    {
        return Err(reject(
            "Map GetAny receipt does not describe one closed create/set/get/drop chain",
        ));
    }
    let map_actions = map_collection.actions;
    let map_new_attempts = map_collection.new_attempts;
    let map_operation_attempts = map_collection.operation_attempts;
    let map_cfg_receipt = map_collection.cfg_receipt;
    let mut resources = crate::core::cfg::analyze_resolved_bodies(
        &BTreeMap::from([(bound.lambda.owner.clone(), cfg.clone())]),
        &BTreeMap::from([(bound.lambda.owner.clone(), synthetic_body.clone())]),
        &BTreeMap::from([(bound.lambda.owner.clone(), signature.clone())]),
        program.resolved_types(),
        program.constants(),
        &BTreeMap::new(),
        &BTreeSet::new(),
    )
    .map_err(|diagnostics| {
        reject(&format!(
            "lambda ownership analysis failed: {}",
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ))
    })?;
    let Some(resource_analysis) = resources.get_mut(&bound.lambda.owner) else {
        return Err(reject(
            "lambda ownership analysis omitted its callable owner",
        ));
    };
    resource_analysis.ordinary_map_owned_get_i32_string_v1_actions = map_actions.clone();
    resource_analysis.ordinary_map_owned_get_i32_string_v1_new_attempts = map_new_attempts.clone();
    resource_analysis.ordinary_map_owned_get_i32_string_v1_operation_attempts =
        map_operation_attempts.clone();
    resource_analysis.ordinary_map_owned_get_i32_string_v1_cfg_receipt =
        Some(map_cfg_receipt.clone());
    let resources = resources
        .remove(&bound.lambda.owner)
        .ok_or_else(|| reject("lambda ResourceAnalysis disappeared"))?;
    let callable = ResolvedCallable::assemble(signature, synthetic_body, cfg, resources)
        .map_err(|error| reject(&error.to_string()))?;

    let closure_local = bound.local.clone();
    let function_value = MirValueId::new(format!("local:{}", local_id_text(&closure_local)))
        .map_err(|error| reject(&error.to_string()))?;
    Ok(PlannedClosureV1 {
        parent: parent_owner.clone(),
        binding: MirClosureCallableBindingV1 {
            closure_local,
            function_value,
            bind_instruction: MirInstructionId::new(format!(
                "inst:closure_bind:{}",
                bound.expression.node_id.0
            ))
            .map_err(|error| reject(&error.to_string()))?,
            lambda_expression: bound.expression.node_id.clone(),
            callee: bound.lambda.owner.clone(),
            abi,
        },
        source_id,
        invoke_instructions,
        callable,
        map_actions,
        map_new_attempts,
        map_operation_attempts,
        map_cfg_receipt,
    })
}

fn build_lambda_callable(
    program: &CheckedProgram,
    parent: &ResolvedCallable,
    bound: &BoundLambda<'_>,
    abi: &MirClosureCallableAbiV1,
) -> Result<
    (
        ResolvedBody,
        crate::core::ir::ResolvedSignature,
        CallableCfg,
        crate::core::cfg::OrdinaryMapOwnedGetI32StringCfgCollection,
    ),
    String,
> {
    let owner = bound.lambda.owner.clone();
    let mut local_ids = BTreeSet::new();
    for parameter in &bound.lambda.parameters {
        local_ids.insert(parameter.clone());
    }
    collect_block_locals(&bound.lambda.body, &mut local_ids);
    if bound
        .lambda
        .captures
        .iter()
        .any(|capture| !local_ids.contains(capture))
    {
        return Err(format!(
            "lambda '{}' capture identity is outside its local scope",
            owner.0
        ));
    }
    let locals = parent
        .body
        .locals
        .iter()
        .filter(|(local, _)| local_ids.contains(*local))
        .map(|(local, definition)| (local.clone(), definition.clone()))
        .collect::<BTreeMap<_, _>>();
    let source_nodes = collect_body_nodes(&bound.lambda.body);
    let place_inputs = parent
        .body
        .place_inputs
        .iter()
        .filter(|(node, _)| source_nodes.contains(*node))
        .map(|(node, expression)| (node.clone(), expression.clone()))
        .collect::<BTreeMap<_, _>>();
    let body = ResolvedBody {
        owner: owner.clone(),
        locals,
        parameters: bound.lambda.parameters.clone(),
        captures: bound.lambda.captures.clone(),
        place_inputs,
        default_values: BTreeMap::new(),
        resolved_calls: parent
            .body
            .resolved_calls
            .iter()
            .filter(|(node, _)| source_nodes.contains(*node))
            .map(|(node, callee)| (node.clone(), callee.clone()))
            .collect(),
        root: bound.lambda.body.clone(),
    };
    body.validate(program.resolved_types(), program.resolved_type_targets())
        .map_err(|errors| {
            format!(
                "synthetic lambda ResolvedBody is invalid: {}",
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?;
    let signature = crate::core::ir::ResolvedSignature {
        owner: owner.clone(),
        generic_parameters: Vec::new(),
        parameters: Vec::new(),
        result: abi.source_result_type.clone(),
        effects: Vec::new(),
    };
    let cfgs =
        crate::core::cfg::lower_resolved_bodies(&BTreeMap::from([(owner.clone(), body.clone())]))
            .map_err(|errors| {
            format!(
                "synthetic lambda CallableCfg lowering failed: {}",
                errors
                    .iter()
                    .map(|error| error.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?;
    let cfg = cfgs
        .get(&owner)
        .cloned()
        .ok_or_else(|| "synthetic lambda CallableCfg is absent".to_string())?;
    let collection = crate::core::cfg::collect_ordinary_map_owned_get_i32_string_cfg(
        &owner,
        &body,
        &cfg,
        program.resolved_types(),
    )
    .ok_or_else(|| "synthetic lambda has no closed Checker Map owned-GetAny receipt".to_string())?;
    Ok((body, signature, cfg, collection))
}

fn returns_owned_any_pair(ty: &ResolvedTypeId, types: &ResolvedTypeTable) -> bool {
    let Some(ResolvedType::Tuple(fields)) = types.get(ty) else {
        return false;
    };
    matches!(fields.as_slice(), [found, payload]
        if matches!(types.get(found), Some(ResolvedType::Primitive(PrimitiveType::Bool)))
            && matches!(types.get(payload), Some(ResolvedType::DynamicAny { .. })))
}

fn function_returns_owned_any_pair(ty: &ResolvedTypeId, types: &ResolvedTypeTable) -> bool {
    matches!(types.get(ty), Some(ResolvedType::Function { result, .. })
        if returns_owned_any_pair(result, types))
}

/// V1's static invoke edge is deliberately pure apart from the three exact
/// ordinary-Map operations in its private child receipt. The Map collector
/// tracks Map ownership, not arbitrary effects or calls, so this independent
/// source walk prevents an unrelated call (for example output or mutation)
/// from being smuggled into the helper alongside a valid Map chain.
fn validate_snapshot_body_effects_and_calls(
    body: &ResolvedBlock,
    attempts: &BTreeSet<crate::core::OrdinaryMapOwnedGetI32StringV1Attempt>,
) -> Result<(), String> {
    use crate::core::OrdinaryMapOwnedGetI32StringV1AttemptKind as Attempt;

    fn visit_block(
        block: &ResolvedBlock,
        calls: &mut BTreeMap<NodeId, BuiltinId>,
    ) -> Result<(), String> {
        for statement in &block.statements {
            if !statement.backend_requirements.is_empty() {
                return Err(format!(
                    "statement '{}' has an unsupported backend requirement",
                    statement.node_id.0
                ));
            }
            match &statement.kind {
                ResolvedStmtKind::Bind { initializer, .. } => {
                    if let Some(expression) = initializer {
                        visit_expr(expression, calls)?;
                    }
                }
                ResolvedStmtKind::Assign { value, .. }
                | ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Return {
                    value: Some(value), ..
                }
                | ResolvedStmtKind::Break(Some(value)) => visit_expr(value, calls)?,
                ResolvedStmtKind::While { condition, body } => {
                    visit_expr(condition, calls)?;
                    visit_block(body, calls)?;
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                } => {
                    visit_expr(initializer, calls)?;
                    visit_block(body, calls)?;
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    visit_expr(initializer, calls)?;
                    visit_block(then_block, calls)?;
                    if let Some(block) = else_block {
                        visit_block(block, calls)?;
                    }
                }
                ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                    visit_block(body, calls)?;
                }
                ResolvedStmtKind::For { iterable, body, .. } => {
                    visit_expr(iterable, calls)?;
                    visit_block(body, calls)?;
                }
                ResolvedStmtKind::Contract { condition, .. } => visit_expr(condition, calls)?,
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        visit_expr(expression, calls)?;
                    }
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    visit_expr(value, calls)?;
                    visit_block(body, calls)?;
                }
                ResolvedStmtKind::Return { value: None, .. }
                | ResolvedStmtKind::Break(None)
                | ResolvedStmtKind::Continue
                | ResolvedStmtKind::Drop(_) => {}
                ResolvedStmtKind::NestedCallable(_) => {
                    return Err("nested callable is outside closure ABI v1".into());
                }
            }
        }
        if let Some(result) = &block.result {
            visit_expr(result, calls)?;
        }
        Ok(())
    }

    fn visit_expr(
        expression: &ResolvedExpr,
        calls: &mut BTreeMap<NodeId, BuiltinId>,
    ) -> Result<(), String> {
        if !expression.effects.is_empty() || !expression.backend_requirements.is_empty() {
            return Err(format!(
                "expression '{}' has effects or an unsupported backend requirement",
                expression.node_id.0
            ));
        }
        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                        visit_expr(value, calls)?;
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                visit_expr(value, calls)?;
                if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                    visit_expr(index, calls)?;
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                visit_expr(left, calls)?;
                visit_expr(right, calls)?;
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Old(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. } => visit_expr(operand, calls)?,
            ResolvedExprKind::Spawn(_) | ResolvedExprKind::Await(_) => {
                return Err(format!(
                    "async operation '{}' is outside closure ABI v1",
                    expression.node_id.0
                ));
            }
            ResolvedExprKind::Call(call) => {
                if !call.effects.is_empty()
                    || !call.session.is_empty()
                    || !call.type_arguments.is_empty()
                {
                    return Err(format!(
                        "call '{}' has effects, session transitions, or generic arguments",
                        expression.node_id.0
                    ));
                }
                let name = match &call.callee {
                    ResolvedCallee::Builtin(name)
                        if matches!(name.as_str(), "map_new" | "map_set" | "map_get") =>
                    {
                        name.clone()
                    }
                    _ => {
                        return Err(format!(
                            "call '{}' is not one of the receipted Map operations",
                            expression.node_id.0
                        ));
                    }
                };
                if calls.insert(expression.node_id.clone(), name).is_some() {
                    return Err(format!(
                        "call identity '{}' is duplicated in the closure source",
                        expression.node_id.0
                    ));
                }
                for argument in &call.arguments {
                    visit_expr(&argument.value, calls)?;
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    visit_expr(value, calls)?;
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    visit_expr(key, calls)?;
                    visit_expr(value, calls)?;
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                visit_expr(value, calls)?;
                visit_expr(iterable, calls)?;
                if let Some(guard) = guard {
                    visit_expr(guard, calls)?;
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => visit_expr(receiver, calls)?,
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    visit_expr(&field.value, calls)?;
                }
                if let Some(rest) = rest {
                    visit_expr(rest, calls)?;
                }
            }
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                visit_block(block, calls)?;
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_expr(condition, calls)?;
                visit_block(then_block, calls)?;
                visit_block(else_block, calls)?;
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                visit_expr(scrutinee, calls)?;
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit_expr(guard, calls)?;
                    }
                    visit_expr(&arm.body, calls)?;
                }
            }
            ResolvedExprKind::Range { start, end } => {
                visit_expr(start, calls)?;
                visit_expr(end, calls)?;
            }
            ResolvedExprKind::Slice { target, start, end } => {
                visit_expr(target, calls)?;
                if let Some(start) = start {
                    visit_expr(start, calls)?;
                }
                if let Some(end) = end {
                    visit_expr(end, calls)?;
                }
            }
            ResolvedExprKind::Comptime(_) | ResolvedExprKind::Quote(_) => {
                return Err(format!(
                    "compile-time body '{}' is outside closure ABI v1",
                    expression.node_id.0
                ));
            }
            ResolvedExprKind::Lambda(_) => {
                return Err(format!(
                    "nested closure '{}' is outside closure ABI v1",
                    expression.node_id.0
                ));
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
        Ok(())
    }

    let mut calls = BTreeMap::new();
    visit_block(body, &mut calls)?;
    let mut expected_calls = BTreeMap::new();
    for attempt in attempts {
        let name = match attempt.kind {
            Attempt::New => Some("map_new"),
            Attempt::Set => Some("map_set"),
            Attempt::UntypedGet => Some("map_get"),
            Attempt::Clone
            | Attempt::GetI32
            | Attempt::GetString
            | Attempt::Remove
            | Attempt::Size
            | Attempt::HasKey
            | Attempt::Keys
            | Attempt::Values
            | Attempt::FromList
            | Attempt::MapLiteral => {
                return Err("closure Map receipt contains an unsupported operation".into());
            }
            Attempt::Drop | Attempt::ScopeDrop => None,
        };
        if let Some(name) = name {
            let name = BuiltinId::new(name).map_err(|error| error.to_string())?;
            if expected_calls.insert(attempt.point.clone(), name).is_some() {
                return Err(format!(
                    "Map attempt identity '{}' is ambiguous",
                    attempt.point.0
                ));
            }
        }
    }
    if calls != expected_calls {
        return Err("closure helper calls differ from its exact Map New/Set/Get receipt".into());
    }
    Ok(())
}

fn root_bound_lambdas<'a>(owner: &NodeId, root: &'a ResolvedBlock) -> Vec<BoundLambda<'a>> {
    let mut bound = Vec::new();
    for statement in &root.statements {
        if let ResolvedStmtKind::Bind {
            pattern,
            initializer: Some(expression),
        } = &statement.kind
        {
            if let (
                ResolvedPatternKind::Binding {
                    local,
                    by_reference: None,
                },
                ResolvedExprKind::Lambda(lambda),
            ) = (&pattern.kind, &expression.kind)
            {
                if pattern.ty == expression.ty && !owner.0.is_empty() {
                    bound.push(BoundLambda {
                        local: local.clone(),
                        expression,
                        lambda,
                    });
                }
            }
        }
    }
    bound
}

fn local_id_text(local: &ResolvedLocalId) -> &str {
    local.0 .0.as_str()
}

fn find_instruction<'a>(
    function: &'a MirFunction,
    id: &MirInstructionId,
) -> Option<&'a super::MirInstruction> {
    function
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find(|instruction| &instruction.id == id)
}

fn collect_block_locals(block: &ResolvedBlock, locals: &mut BTreeSet<ResolvedLocalId>) {
    for statement in &block.statements {
        collect_stmt_locals(statement, locals);
    }
    if let Some(result) = &block.result {
        collect_expr_locals(result, locals);
    }
}

fn collect_stmt_locals(statement: &ResolvedStmt, locals: &mut BTreeSet<ResolvedLocalId>) {
    match &statement.kind {
        ResolvedStmtKind::Bind {
            pattern,
            initializer,
        } => {
            collect_pattern_locals(pattern, locals);
            if let Some(expression) = initializer {
                collect_expr_locals(expression, locals);
            }
        }
        ResolvedStmtKind::Assign { value, .. }
        | ResolvedStmtKind::Expr(value)
        | ResolvedStmtKind::Return {
            value: Some(value), ..
        }
        | ResolvedStmtKind::Break(Some(value)) => collect_expr_locals(value, locals),
        ResolvedStmtKind::While { condition, body } => {
            collect_expr_locals(condition, locals);
            collect_block_locals(body, locals);
        }
        ResolvedStmtKind::WhileLet {
            pattern,
            initializer,
            body,
        } => {
            collect_pattern_locals(pattern, locals);
            collect_expr_locals(initializer, locals);
            collect_block_locals(body, locals);
        }
        ResolvedStmtKind::IfLet {
            pattern,
            initializer,
            then_block,
            else_block,
        } => {
            collect_pattern_locals(pattern, locals);
            collect_expr_locals(initializer, locals);
            collect_block_locals(then_block, locals);
            if let Some(block) = else_block {
                collect_block_locals(block, locals);
            }
        }
        ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
            collect_block_locals(body, locals)
        }
        ResolvedStmtKind::For {
            pattern,
            iterable,
            body,
        } => {
            collect_pattern_locals(pattern, locals);
            collect_expr_locals(iterable, locals);
            collect_block_locals(body, locals);
        }
        ResolvedStmtKind::Contract { condition, .. } => collect_expr_locals(condition, locals),
        ResolvedStmtKind::Math(expressions) => {
            for expression in expressions {
                collect_expr_locals(expression, locals);
            }
        }
        ResolvedStmtKind::Pinned {
            value,
            binding,
            body,
        } => {
            if let Some(binding) = binding {
                locals.insert(binding.clone());
            }
            collect_expr_locals(value, locals);
            collect_block_locals(body, locals);
        }
        ResolvedStmtKind::Return { value: None, .. }
        | ResolvedStmtKind::Break(None)
        | ResolvedStmtKind::Continue
        | ResolvedStmtKind::Drop(_)
        | ResolvedStmtKind::NestedCallable(_) => {}
    }
}

fn collect_pattern_locals(pattern: &ResolvedPattern, locals: &mut BTreeSet<ResolvedLocalId>) {
    match &pattern.kind {
        ResolvedPatternKind::Binding { local, .. } => {
            locals.insert(local.clone());
        }
        ResolvedPatternKind::Constructor { fields, .. } => {
            for (_, field) in fields {
                collect_pattern_locals(field, locals);
            }
        }
        ResolvedPatternKind::Tuple(fields) | ResolvedPatternKind::Array(fields) => {
            for field in fields {
                collect_pattern_locals(field, locals);
            }
        }
        ResolvedPatternKind::Slice { prefix, rest } => {
            for field in prefix {
                collect_pattern_locals(field, locals);
            }
            if let Some(rest) = rest {
                collect_pattern_locals(rest, locals);
            }
        }
        ResolvedPatternKind::Wildcard | ResolvedPatternKind::Literal(_) => {}
    }
}

fn collect_expr_locals(expression: &ResolvedExpr, locals: &mut BTreeSet<ResolvedLocalId>) {
    match &expression.kind {
        ResolvedExprKind::Load(place) => {
            locals.insert(place.base.clone());
        }
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let crate::core::ir::ResolvedFStringPart::Interpolation(expression) = part {
                    collect_expr_locals(expression, locals);
                }
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            collect_expr_locals(value, locals);
            if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                collect_expr_locals(index, locals);
            }
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            collect_expr_locals(left, locals);
            collect_expr_locals(right, locals);
        }
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::TypeOf(operand)
        | ResolvedExprKind::Old(operand)
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand) => collect_expr_locals(operand, locals),
        ResolvedExprKind::Call(call) => {
            for argument in &call.arguments {
                collect_expr_locals(&argument.value, locals);
            }
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => {
            for value in values {
                collect_expr_locals(value, locals);
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                collect_expr_locals(key, locals);
                collect_expr_locals(value, locals);
            }
        }
        ResolvedExprKind::Comprehension {
            pattern,
            value,
            iterable,
            guard,
        } => {
            collect_pattern_locals(pattern, locals);
            collect_expr_locals(value, locals);
            collect_expr_locals(iterable, locals);
            if let Some(guard) = guard {
                collect_expr_locals(guard, locals);
            }
        }
        ResolvedExprKind::OptionalChain { receiver, .. } => collect_expr_locals(receiver, locals),
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                collect_expr_locals(&field.value, locals);
            }
            if let Some(rest) = rest {
                collect_expr_locals(rest, locals);
            }
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => collect_block_locals(block, locals),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_expr_locals(condition, locals);
            collect_block_locals(then_block, locals);
            collect_block_locals(else_block, locals);
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            collect_expr_locals(scrutinee, locals);
            for arm in arms {
                collect_pattern_locals(&arm.pattern, locals);
                if let Some(guard) = &arm.guard {
                    collect_expr_locals(guard, locals);
                }
                collect_expr_locals(&arm.body, locals);
            }
        }
        ResolvedExprKind::Try { value, .. } | ResolvedExprKind::Cast { value, .. } => {
            collect_expr_locals(value, locals)
        }
        ResolvedExprKind::Range { start, end } => {
            collect_expr_locals(start, locals);
            collect_expr_locals(end, locals);
        }
        ResolvedExprKind::Slice { target, start, end } => {
            collect_expr_locals(target, locals);
            if let Some(start) = start {
                collect_expr_locals(start, locals);
            }
            if let Some(end) = end {
                collect_expr_locals(end, locals);
            }
        }
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::Lambda(_)
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
    }
}

fn collect_body_nodes(block: &ResolvedBlock) -> BTreeSet<NodeId> {
    let mut nodes = BTreeSet::new();
    collect_block_nodes(block, &mut nodes);
    nodes
}

fn collect_block_nodes(block: &ResolvedBlock, nodes: &mut BTreeSet<NodeId>) {
    nodes.insert(block.node_id.clone());
    for statement in &block.statements {
        nodes.insert(statement.node_id.clone());
        match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(expression) = initializer {
                    collect_expr_nodes(expression, nodes);
                }
            }
            ResolvedStmtKind::Assign { value, .. }
            | ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Return {
                value: Some(value), ..
            }
            | ResolvedStmtKind::Break(Some(value)) => collect_expr_nodes(value, nodes),
            ResolvedStmtKind::While { condition, body } => {
                collect_expr_nodes(condition, nodes);
                collect_block_nodes(body, nodes);
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => {
                collect_expr_nodes(initializer, nodes);
                collect_block_nodes(body, nodes);
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                collect_expr_nodes(initializer, nodes);
                collect_block_nodes(then_block, nodes);
                if let Some(block) = else_block {
                    collect_block_nodes(block, nodes);
                }
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                collect_block_nodes(body, nodes)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                collect_expr_nodes(iterable, nodes);
                collect_block_nodes(body, nodes);
            }
            ResolvedStmtKind::Contract { condition, .. } => collect_expr_nodes(condition, nodes),
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    collect_expr_nodes(expression, nodes);
                }
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                collect_expr_nodes(value, nodes);
                collect_block_nodes(body, nodes);
            }
            ResolvedStmtKind::Return { value: None, .. }
            | ResolvedStmtKind::Break(None)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::NestedCallable(_) => {}
        }
    }
    if let Some(result) = &block.result {
        collect_expr_nodes(result, nodes);
    }
}

fn collect_expr_nodes(expression: &ResolvedExpr, nodes: &mut BTreeSet<NodeId>) {
    nodes.insert(expression.node_id.clone());
    match &expression.kind {
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let crate::core::ir::ResolvedFStringPart::Interpolation(expression) = part {
                    collect_expr_nodes(expression, nodes);
                }
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            collect_expr_nodes(value, nodes);
            if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                collect_expr_nodes(index, nodes);
            }
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            collect_expr_nodes(left, nodes);
            collect_expr_nodes(right, nodes);
        }
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::TypeOf(operand)
        | ResolvedExprKind::Old(operand)
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand) => collect_expr_nodes(operand, nodes),
        ResolvedExprKind::Call(call) => {
            for argument in &call.arguments {
                collect_expr_nodes(&argument.value, nodes);
            }
        }
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => {
            for value in values {
                collect_expr_nodes(value, nodes);
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                collect_expr_nodes(key, nodes);
                collect_expr_nodes(value, nodes);
            }
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            collect_expr_nodes(value, nodes);
            collect_expr_nodes(iterable, nodes);
            if let Some(guard) = guard {
                collect_expr_nodes(guard, nodes);
            }
        }
        ResolvedExprKind::OptionalChain { receiver, .. } => collect_expr_nodes(receiver, nodes),
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                collect_expr_nodes(&field.value, nodes);
            }
            if let Some(rest) = rest {
                collect_expr_nodes(rest, nodes);
            }
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => collect_block_nodes(block, nodes),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_expr_nodes(condition, nodes);
            collect_block_nodes(then_block, nodes);
            collect_block_nodes(else_block, nodes);
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            collect_expr_nodes(scrutinee, nodes);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_expr_nodes(guard, nodes);
                }
                collect_expr_nodes(&arm.body, nodes);
            }
        }
        ResolvedExprKind::Try { value, .. } | ResolvedExprKind::Cast { value, .. } => {
            collect_expr_nodes(value, nodes)
        }
        ResolvedExprKind::Range { start, end } => {
            collect_expr_nodes(start, nodes);
            collect_expr_nodes(end, nodes);
        }
        ResolvedExprKind::Slice { target, start, end } => {
            collect_expr_nodes(target, nodes);
            if let Some(start) = start {
                collect_expr_nodes(start, nodes);
            }
            if let Some(end) = end {
                collect_expr_nodes(end, nodes);
            }
        }
        ResolvedExprKind::Load(_)
        | ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::Lambda(_)
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
    }
}

fn scan_block_uses(
    block: &ResolvedBlock,
    local: &ResolvedLocalId,
    lambda_owner: &NodeId,
    census: &mut UseCensus,
) {
    for statement in &block.statements {
        scan_stmt_uses(statement, local, lambda_owner, census);
    }
    if let Some(result) = &block.result {
        scan_expr_uses(result, local, lambda_owner, census);
    }
}

fn scan_stmt_uses(
    statement: &ResolvedStmt,
    local: &ResolvedLocalId,
    lambda_owner: &NodeId,
    census: &mut UseCensus,
) {
    match &statement.kind {
        ResolvedStmtKind::Bind { initializer, .. } => {
            if let Some(expression) = initializer {
                scan_expr_uses(expression, local, lambda_owner, census);
            }
        }
        ResolvedStmtKind::Assign { target, value, .. } => {
            if &target.base == local {
                census
                    .rejected
                    .push(format!("mutable assignment at '{}'", statement.node_id.0));
            }
            scan_expr_uses(value, local, lambda_owner, census);
        }
        ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
            if let Some(value) = value {
                scan_expr_uses(value, local, lambda_owner, census);
            }
        }
        ResolvedStmtKind::Expr(expression) => {
            scan_expr_uses(expression, local, lambda_owner, census)
        }
        ResolvedStmtKind::While { condition, body } => {
            scan_expr_uses(condition, local, lambda_owner, census);
            scan_block_uses(body, local, lambda_owner, census);
        }
        ResolvedStmtKind::WhileLet {
            initializer, body, ..
        } => {
            scan_expr_uses(initializer, local, lambda_owner, census);
            scan_block_uses(body, local, lambda_owner, census);
        }
        ResolvedStmtKind::IfLet {
            initializer,
            then_block,
            else_block,
            ..
        } => {
            scan_expr_uses(initializer, local, lambda_owner, census);
            scan_block_uses(then_block, local, lambda_owner, census);
            if let Some(block) = else_block {
                scan_block_uses(block, local, lambda_owner, census);
            }
        }
        ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
            scan_block_uses(body, local, lambda_owner, census)
        }
        ResolvedStmtKind::For { iterable, body, .. } => {
            scan_expr_uses(iterable, local, lambda_owner, census);
            scan_block_uses(body, local, lambda_owner, census);
        }
        ResolvedStmtKind::Drop(places) => {
            if places.iter().any(|place| &place.base == local) {
                census
                    .rejected
                    .push(format!("direct drop at '{}'", statement.node_id.0));
            }
        }
        ResolvedStmtKind::Contract { condition, .. } => {
            scan_expr_uses(condition, local, lambda_owner, census)
        }
        ResolvedStmtKind::Math(expressions) => {
            for expression in expressions {
                scan_expr_uses(expression, local, lambda_owner, census);
            }
        }
        ResolvedStmtKind::Pinned { value, body, .. } => {
            scan_expr_uses(value, local, lambda_owner, census);
            scan_block_uses(body, local, lambda_owner, census);
        }
        ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {}
    }
}

fn scan_expr_uses(
    expression: &ResolvedExpr,
    local: &ResolvedLocalId,
    lambda_owner: &NodeId,
    census: &mut UseCensus,
) {
    match &expression.kind {
        ResolvedExprKind::Load(place) => {
            if &place.base == local {
                census.rejected.push(format!(
                    "first-class local read at '{}'",
                    expression.node_id.0
                ));
            }
        }
        ResolvedExprKind::Callable(ResolvedCallee::LocalClosure(target)) if target == local => {
            census.rejected.push(format!(
                "first-class callable expression at '{}'",
                expression.node_id.0
            ));
        }
        ResolvedExprKind::Call(call) => {
            match &call.callee {
                ResolvedCallee::LocalClosure(target) if target == local => {
                    census.invokes.push(ClosureInvokeSource {
                        node: expression.node_id.clone(),
                        source_id: expression.origin.user_span().source_id,
                        result: call.result.clone(),
                        permission: call.permission,
                        argument_count: call.arguments.len(),
                        type_argument_count: call.type_arguments.len(),
                        effects: call.effects.len(),
                        has_session: !call.session.is_empty(),
                    });
                }
                ResolvedCallee::Function(target) if target == lambda_owner => census.rejected.push(
                    format!("direct function-owner call at '{}'", expression.node_id.0),
                ),
                _ => {}
            }
            for argument in &call.arguments {
                scan_expr_uses(&argument.value, local, lambda_owner, census);
            }
        }
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let crate::core::ir::ResolvedFStringPart::Interpolation(expression) = part {
                    scan_expr_uses(expression, local, lambda_owner, census);
                }
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            scan_expr_uses(value, local, lambda_owner, census);
            if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                scan_expr_uses(index, local, lambda_owner, census);
            }
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            scan_expr_uses(left, local, lambda_owner, census);
            scan_expr_uses(right, local, lambda_owner, census);
        }
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::TypeOf(operand)
        | ResolvedExprKind::Old(operand)
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand) => scan_expr_uses(operand, local, lambda_owner, census),
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => {
            for value in values {
                scan_expr_uses(value, local, lambda_owner, census);
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                scan_expr_uses(key, local, lambda_owner, census);
                scan_expr_uses(value, local, lambda_owner, census);
            }
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            scan_expr_uses(value, local, lambda_owner, census);
            scan_expr_uses(iterable, local, lambda_owner, census);
            if let Some(guard) = guard {
                scan_expr_uses(guard, local, lambda_owner, census);
            }
        }
        ResolvedExprKind::OptionalChain { receiver, .. } => {
            scan_expr_uses(receiver, local, lambda_owner, census)
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                scan_expr_uses(&field.value, local, lambda_owner, census);
            }
            if let Some(rest) = rest {
                scan_expr_uses(rest, local, lambda_owner, census);
            }
        }
        ResolvedExprKind::Block(block)
        | ResolvedExprKind::Scope { body: block, .. }
        | ResolvedExprKind::Comptime(block)
        | ResolvedExprKind::Quote(block) => scan_block_uses(block, local, lambda_owner, census),
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            scan_expr_uses(condition, local, lambda_owner, census);
            scan_block_uses(then_block, local, lambda_owner, census);
            scan_block_uses(else_block, local, lambda_owner, census);
        }
        ResolvedExprKind::Match { scrutinee, arms } => {
            scan_expr_uses(scrutinee, local, lambda_owner, census);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    scan_expr_uses(guard, local, lambda_owner, census);
                }
                scan_expr_uses(&arm.body, local, lambda_owner, census);
            }
        }
        ResolvedExprKind::Try { value, .. } | ResolvedExprKind::Cast { value, .. } => {
            scan_expr_uses(value, local, lambda_owner, census)
        }
        ResolvedExprKind::Range { start, end } => {
            scan_expr_uses(start, local, lambda_owner, census);
            scan_expr_uses(end, local, lambda_owner, census);
        }
        ResolvedExprKind::Slice { target, start, end } => {
            scan_expr_uses(target, local, lambda_owner, census);
            if let Some(start) = start {
                scan_expr_uses(start, local, lambda_owner, census);
            }
            if let Some(end) = end {
                scan_expr_uses(end, local, lambda_owner, census);
            }
        }
        ResolvedExprKind::Lambda(lambda) => {
            if lambda.captures.contains(local) {
                census.rejected.push(format!(
                    "capture by nested closure at '{}'",
                    expression.node_id.0
                ));
            }
            scan_block_uses(&lambda.body, local, lambda_owner, census);
        }
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
    }
}

impl MirClosureCallableReceiptV1 {
    pub(crate) fn validate_against(
        &self,
        functions: &BTreeMap<NodeId, MirFunction>,
        type_catalog: &MirTypeCatalog,
        receipts: &BTreeMap<ResolvedLocalId, MirClosureCallableReceiptV1>,
    ) -> Result<(), String> {
        let snapshot = &self.checker_snapshot;
        if self.schema != MIR_CLOSURE_CALLABLE_RECEIPT_V1_SCHEMA
            || self.parent != snapshot.parent
            || self.closure_local != snapshot.closure_local
            || self.function_value != snapshot.function_value
            || self.bind_instruction != snapshot.bind_instruction
            || self.lambda_expression != snapshot.lambda_expression
            || self.source_id != snapshot.source_id
            || self.callee != snapshot.callee
            || self.abi != snapshot.abi
            || self.invoke_instructions != snapshot.invoke_instructions
            || self.abi.schema != MIR_CLOSURE_CALLABLE_ABI_V1_SCHEMA
            || self.abi.source_abi != FunctionTypeAbi::Mimi
            || !self.abi.source_parameter_types.is_empty()
            || !self.abi.capture_types.is_empty()
        {
            return Err(format!(
                "closure receipt '{}' disagrees with its private Checker snapshot",
                self.closure_local.0 .0
            ));
        }
        if receipts
            .get(&self.closure_local)
            .is_none_or(|receipt| receipt != self)
        {
            return Err(format!(
                "closure receipt key '{}' disagrees with its binding identity",
                self.closure_local.0 .0
            ));
        }
        let parent = functions
            .get(&self.parent)
            .ok_or_else(|| format!("closure parent '{}' is absent", self.parent.0))?;
        let expected_function_value =
            MirValueId::new(format!("local:{}", local_id_text(&self.closure_local)))
                .map_err(|error| error.to_string())?;
        let expected_bind_instruction =
            MirInstructionId::new(format!("inst:closure_bind:{}", self.lambda_expression.0))
                .map_err(|error| error.to_string())?;
        if self.function_value != expected_function_value
            || self.bind_instruction != expected_bind_instruction
        {
            return Err(
                "closure value or bind instruction identity is not derived from the checked local/lambda".into(),
            );
        }
        let closure_value = self.function_value.clone();
        let closure_value_type = parent
            .values
            .get(&closure_value)
            .map(|value| &value.ty)
            .ok_or_else(|| "closure function value has no canonical TypeDesc".to_string())?;
        if closure_value_type != &self.abi.source_function_type {
            return Err("closure binding value TypeDesc disagrees with source function ABI".into());
        }
        if type_catalog
            .get(closure_value_type)
            .is_none_or(|descriptor| {
                descriptor.ownership != super::types::MirOwnership::Copy
                    || !matches!(
                        descriptor.kind,
                        MirTypeKind::Function {
                            abi: FunctionTypeAbi::Mimi,
                            arity: 0
                        }
                    )
            })
        {
            return Err("closure function TypeDesc is not the exact no-argument Mimi ABI".into());
        }
        let bind = find_instruction(parent, &self.bind_instruction)
            .ok_or_else(|| "closure binding instruction is absent".to_string())?;
        if !matches!(
            &bind.kind,
            MirInstructionKind::ClosureBind {
                closure_local,
                function_value,
                callee,
                abi,
            } if closure_local == &self.closure_local
                && function_value == &self.function_value
                && callee == &self.callee
                && abi == &self.abi
        ) {
            return Err("ClosureBind opcode differs from its exact source receipt".into());
        }
        let target = functions
            .get(&self.callee)
            .ok_or_else(|| format!("closure target '{}' is absent", self.callee.0))?;
        if !snapshot.target_body.matches(target) {
            return Err(
                "closure target MIR body differs from its Checker-sealed callable body".into(),
            );
        }
        if target.owner != self.callee
            || !target.parameters.is_empty()
            || target.parameter_permissions.as_deref() != Some(&[][..])
            || target.result
                != type_catalog.canonical_managed_any_type(&self.abi.source_result_type)
            || type_catalog
                .validate_ordinary_map_owned_get_any_result_type(&target.result)
                .is_err()
            || target
                .ownership
                .checker_ordinary_map_owned_get_i32_string_v1_cfg_receipt
                .as_ref()
                != Some(&snapshot.map_cfg_receipt)
            || target
                .ownership
                .checker_ordinary_map_owned_get_i32_string_v1
                .len()
                != snapshot.map_actions.len()
        {
            return Err("closure target signature or Map CFG owner receipt is inconsistent".into());
        }
        let mut found_invokes = Vec::new();
        let mut bind_count = 0usize;
        for function in functions.values() {
            for block in function.blocks.values() {
                for instruction in &block.instructions {
                    match &instruction.kind {
                        MirInstructionKind::ClosureBind {
                            closure_local,
                            function_value,
                            callee,
                            abi,
                        } if closure_local == &self.closure_local || callee == &self.callee => {
                            if function.owner != self.parent
                                || closure_local != &self.closure_local
                                || function_value != &self.function_value
                                || callee != &self.callee
                                || abi != &self.abi
                                || instruction.id != self.bind_instruction
                            {
                                return Err("ClosureBind census contains an unexpected binding, owner, local, value, or ABI".into());
                            }
                            bind_count += 1;
                        }
                        MirInstructionKind::ClosureInvoke {
                            result,
                            closure_local,
                            function_value,
                            callee,
                            type_arguments,
                            arguments,
                            abi,
                        } if closure_local == &self.closure_local => {
                            if function.owner != self.parent
                                || function_value != &self.function_value
                                || callee != &self.callee
                                || !type_arguments.is_empty()
                                || !arguments.is_empty()
                                || abi != &self.abi
                                || result.as_ref().is_none_or(|result| {
                                    function
                                        .values
                                        .get(result)
                                        .is_none_or(|value| value.ty != target.result)
                                })
                            {
                                return Err(
                                    "ClosureInvoke caller, owner, arguments, or result is forged"
                                        .into(),
                                );
                            }
                            found_invokes.push(instruction.id.clone());
                        }
                        MirInstructionKind::Call {
                            callee: ResolvedCallee::LocalClosure(local),
                            ..
                        } if local == &self.closure_local => {
                            return Err("ordinary Call cannot stand in for ClosureInvoke".into());
                        }
                        MirInstructionKind::Call {
                            callee: ResolvedCallee::Function(target),
                            ..
                        } if target == &self.callee => {
                            return Err(
                                "closure target must be reached only through ClosureInvoke".into(),
                            );
                        }
                        _ => {}
                    }
                    if function.owner == self.parent
                        && super::mir_instruction_value_uses(&instruction.kind)
                            .iter()
                            .any(|value| value == &self.function_value)
                        && !matches!(
                            &instruction.kind,
                            MirInstructionKind::ClosureInvoke {
                                closure_local,
                                function_value,
                                callee,
                                abi,
                                ..
                            } if closure_local == &self.closure_local
                                && function_value == &self.function_value
                                && callee == &self.callee
                                && abi == &self.abi
                        )
                    {
                        return Err("closure function value is copied, moved, escaped, or used outside its admitted ClosureInvoke".into());
                    }
                }
                if function.owner == self.parent
                    && super::mir_terminator_value_uses(&block.terminator)
                        .contains(&self.function_value)
                {
                    return Err("closure function value escapes through a MIR terminator".into());
                }
            }
        }
        found_invokes.sort();
        if bind_count != 1 || found_invokes != self.invoke_instructions || found_invokes.is_empty()
        {
            return Err("ClosureInvoke instruction census is incomplete or unexpected".into());
        }
        let expected_map_actions = snapshot
            .map_actions
            .iter()
            .map(|action| {
                (
                    action.kind,
                    action.point.clone(),
                    format!("local:{}", local_id_text(&action.local)),
                    action.root.0 .0.clone(),
                )
            })
            .collect::<Vec<_>>();
        let actual_map_actions = target
            .ownership
            .checker_ordinary_map_owned_get_i32_string_v1
            .iter()
            .map(|action| {
                (
                    action.kind,
                    action.point.clone(),
                    action.local.as_str().to_string(),
                    action.root.clone(),
                )
            })
            .collect::<Vec<_>>();
        if actual_map_actions != expected_map_actions
            || snapshot.map_cfg_receipt.owner != self.callee
            || snapshot.map_new_attempts.is_empty()
            || snapshot.map_operation_attempts.is_empty()
        {
            return Err("closure's private source Map operation inventory is inconsistent".into());
        }
        Ok(())
    }

    pub(crate) fn canonical_text(&self) -> String {
        format!(
            "mir.closure-callable schema={} parent={} local={} function_value={} bind={} lambda={} source_id={} callee={} [{}] invokes={:?}\n",
            self.schema,
            self.parent.0,
            self.closure_local.0.0,
            self.function_value,
            self.bind_instruction,
            self.lambda_expression.0,
            self.source_id.raw(),
            self.callee.0,
            self.abi.canonical_text(),
            self.invoke_instructions
        )
    }
}

pub(crate) fn validate_receipt_table(
    functions: &BTreeMap<NodeId, MirFunction>,
    type_catalog: &MirTypeCatalog,
    receipts: &BTreeMap<ResolvedLocalId, MirClosureCallableReceiptV1>,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    for (local, receipt) in receipts {
        if local != &receipt.closure_local {
            errors.push(format!(
                "closure receipt table key '{}' disagrees with its local identity",
                local.0 .0
            ));
            continue;
        }
        if let Err(message) = receipt.validate_against(functions, type_catalog, receipts) {
            errors.push(message);
        }
    }
    for function in functions.values() {
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                let (local, opcode) = match &instruction.kind {
                    MirInstructionKind::ClosureBind { closure_local, .. } => {
                        (closure_local, "ClosureBind")
                    }
                    MirInstructionKind::ClosureInvoke { closure_local, .. } => {
                        (closure_local, "ClosureInvoke")
                    }
                    _ => continue,
                };
                if !receipts.contains_key(local) {
                    errors.push(format!(
                        "{opcode} instruction '{}' has no Checker-sealed closure receipt for local '{}'",
                        instruction.id, local.0 .0
                    ));
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
