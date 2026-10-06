//! Checker-derived ordinary ManagedAny interprocedural ownership summaries.
//!
//! This module is intentionally separate from the MIR owner ledger.  MIR
//! values and their private seals are consumers of this plan, never its source
//! of authority.  The plan is derived from ResolvedCallable, its canonical
//! signature, the complete typed body, and the checker CallableCfg before a
//! backend can observe a program.

use crate::core::ir::{
    CheckedConversionKind, Permission, ResolvedBlock, ResolvedCallee, ResolvedExpr,
    ResolvedExprKind, ResolvedParameterId, ResolvedPlace, ResolvedStmt, ResolvedStmtKind,
    ResolvedType, ResolvedTypeId, ResolvedTypeTable,
};
use crate::core::mir::types::MirTypeCatalog;
use crate::core::mir::{MirFunction, MirInstance};
use crate::core::{CheckedProgram, NodeId, ResolvedTypeKind};
use crate::span::SourceId;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

pub(super) const MANAGED_ANY_INTERPROCEDURAL_SCHEMA_V1: &str =
    "mimi-managed-any-interprocedural-source-v1";
const MAX_SOURCE_INSTANCE_KEYS: usize = 16_384;
const MAX_SOURCE_CALLSITE_ROWS: usize = 1_048_576;
const MAX_TYPE_RECURSION: usize = 256;

/// Exact instantiated type constructor. Generic substitutions replace child
/// terms while this head preserves nominal, capability, mutability and ABI
/// identity without consulting a display name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ManagedAnySourceTypeHeadV1 {
    Primitive(crate::core::ir::PrimitiveType),
    GenericParameter(NodeId),
    Nominal {
        item: crate::core::ir::NominalTypeId,
        is_linear: bool,
    },
    FlowStateSet {
        flow: crate::core::ir::NominalTypeId,
        states: Vec<crate::core::ir::NominalTypeId>,
    },
    Reference {
        lifetime: Option<String>,
        mutable: bool,
    },
    Option,
    Result,
    Tuple(usize),
    Function {
        abi: crate::core::ir::FunctionTypeAbi,
        parameters: usize,
    },
    CBuffer,
    Capability(crate::core::ir::NominalTypeId),
    Ownership(crate::core::ir::OwnershipTypeKind),
    Newtype(crate::core::ir::NominalTypeId),
    Array(usize),
    Slice,
    Trait {
        kind: crate::core::ir::TraitTypeKind,
        traits: Vec<crate::core::ir::NominalTypeId>,
    },
    RawPointer {
        mutable: bool,
    },
    DynamicAny {
        capability: String,
    },
}

/// `source_id` is retained as Checker identity evidence but is intentionally
/// excluded from equality/order: an instantiated generic application can
/// have a different interned id from its template while retaining the same
/// exact constructor head and substituted children.
#[derive(Debug, Clone)]
pub(super) struct ManagedAnySourceTypeTermV1 {
    source_id: ResolvedTypeId,
    head: ManagedAnySourceTypeHeadV1,
    children: Vec<ManagedAnySourceTypeTermV1>,
}

impl PartialEq for ManagedAnySourceTypeTermV1 {
    fn eq(&self, other: &Self) -> bool {
        self.head == other.head && self.children == other.children
    }
}

impl Eq for ManagedAnySourceTypeTermV1 {}

impl PartialOrd for ManagedAnySourceTypeTermV1 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ManagedAnySourceTypeTermV1 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.head, &self.children).cmp(&(&other.head, &other.children))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ManagedAnySourceFunctionKeyV1 {
    pub(super) owner: NodeId,
    /// Ordered exactly as `ResolvedSignature::generic_parameters`.
    pub(super) type_arguments: Vec<ManagedAnySourceTypeTermV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ManagedAnyProjectionV1 {
    Tuple(usize),
    Field(NodeId),
    OptionSome,
    ResultOk,
    ResultError,
    ArrayElements,
    ListElements,
    SetElements,
    MapValues,
    NewtypePayload,
    OwnershipPayload,
}

pub(super) type ManagedAnyProjectionPathV1 = Vec<ManagedAnyProjectionV1>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManagedAnySourceShapeV1 {
    None,
    AnyLeaf { capability: String },
    Product(Vec<(ManagedAnyProjectionV1, ManagedAnySourceShapeV1)>),
}

impl ManagedAnySourceShapeV1 {
    pub(super) fn contains_any(&self) -> bool {
        match self {
            Self::None => false,
            Self::AnyLeaf { .. } => true,
            Self::Product(fields) => fields.iter().any(|(_, shape)| shape.contains_any()),
        }
    }

    pub(super) fn any_paths(&self) -> Vec<ManagedAnyProjectionPathV1> {
        fn walk(
            shape: &ManagedAnySourceShapeV1,
            prefix: &mut ManagedAnyProjectionPathV1,
            output: &mut Vec<ManagedAnyProjectionPathV1>,
        ) {
            match shape {
                ManagedAnySourceShapeV1::None => {}
                ManagedAnySourceShapeV1::AnyLeaf { .. } => output.push(prefix.clone()),
                ManagedAnySourceShapeV1::Product(fields) => {
                    for (projection, child) in fields {
                        prefix.push(projection.clone());
                        walk(child, prefix, output);
                        prefix.pop();
                    }
                }
            }
        }
        let mut output = Vec::new();
        walk(self, &mut Vec::new(), &mut output);
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManagedAnyParameterModeV1 {
    OwnedValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedAnyParameterSourceV1 {
    pub(super) parameter: ResolvedParameterId,
    pub(super) type_term: ManagedAnySourceTypeTermV1,
    pub(super) permission: Option<Permission>,
    pub(super) mode: ManagedAnyParameterModeV1,
    pub(super) any_paths: Vec<ManagedAnyProjectionPathV1>,
    /// This phase admits owned Any parameters only. View/Mutate arguments
    /// remain outside the call gate until a matching loan receipt is joined.
    pub(super) callee_obligation: ManagedAnyParameterObligationV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManagedAnyParameterObligationV1 {
    CalleeMustSettleEveryExit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManagedAnyResultOriginV1 {
    /// A single-block helper returns the exact typed input value. Canonical
    /// lowering emits an independent Clone result and closes the input owner
    /// at the scope exit; the MIR ledger validates both actions.
    ParameterSnapshotClone {
        parameter: ResolvedParameterId,
        source_path: ManagedAnyProjectionPathV1,
    },
    /// The result was produced by one sealed direct helper call. This is a
    /// finite call-edge fact: recursive provenance never nests or grows a
    /// call string.
    DirectHelperResult {
        call_node: NodeId,
        callee: ManagedAnySourceFunctionKeyV1,
        callee_result_path: ManagedAnyProjectionPathV1,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedAnyResultProjectionV1 {
    pub(super) result_path: ManagedAnyProjectionPathV1,
    pub(super) origin: ManagedAnyResultOriginV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManagedAnySourceFunctionSummaryStateV1 {
    /// A complete summary for the supported typed-source form.
    Proven,
    /// Monotone bottom while an SCC is being solved. It is never admitted to
    /// a consumer as a final summary.
    Pending,
    /// Unsupported typed shape/effect; the boundary remains rejected.
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedAnySourceFunctionSummaryV1 {
    pub(super) key: ManagedAnySourceFunctionKeyV1,
    pub(super) parameter_contracts: Vec<ManagedAnyParameterSourceV1>,
    pub(super) result_type: ManagedAnySourceTypeTermV1,
    pub(super) result_paths: Vec<ManagedAnyProjectionPathV1>,
    pub(super) result_origins: Vec<ManagedAnyResultProjectionV1>,
    /// Input Any-bearing values are always obligations of this callee. An
    /// output clone is additional ownership and never discharges that input.
    pub(super) settles_parameters_at_exit: BTreeSet<ResolvedParameterId>,
    pub(super) body_seal: String,
    pub(super) cfg_seal: String,
    pub(super) state: ManagedAnySourceFunctionSummaryStateV1,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ManagedAnySourceCallsiteKeyV1 {
    pub(super) caller: ManagedAnySourceFunctionKeyV1,
    pub(super) node: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedAnySourceCallArgumentV1 {
    pub(super) parameter: ResolvedParameterId,
    pub(super) parameter_type: ManagedAnySourceTypeTermV1,
    pub(super) permission: Option<Permission>,
    pub(super) parameter_any_paths: Vec<ManagedAnyProjectionPathV1>,
    pub(super) actual_node: NodeId,
    pub(super) actual_type: ManagedAnySourceTypeTermV1,
    pub(super) conversion_kind: CheckedConversionKind,
    pub(super) conversion_from: ManagedAnySourceTypeTermV1,
    pub(super) conversion_to: ManagedAnySourceTypeTermV1,
    pub(super) any_paths: Vec<ManagedAnyProjectionPathV1>,
    pub(super) transfer: ManagedAnyCallArgumentTransferV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManagedAnyCallArgumentTransferV1 {
    OwnedFreshArgument,
    OwnedMoveFromTypedPlace { place: ResolvedPlace },
    SnapshotCloneOfTypedPlace { place: ResolvedPlace },
    FreshDirectHelperResult { call_node: NodeId },
    UnsupportedAnyFreshExpression { node: NodeId, reason: String },
    BorrowedArgument { permission: Permission },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedAnySourceCallsiteV1 {
    pub(super) key: ManagedAnySourceCallsiteKeyV1,
    pub(super) callee: ManagedAnySourceFunctionKeyV1,
    pub(super) caller_source_seal: String,
    pub(super) callee_source_seal: String,
    pub(super) arguments: Vec<ManagedAnySourceCallArgumentV1>,
    pub(super) result_type: ManagedAnySourceTypeTermV1,
    pub(super) result_paths: Vec<ManagedAnyProjectionPathV1>,
    pub(super) effects_seal: String,
    pub(super) session_seal: String,
    pub(super) call_permission: Option<Permission>,
    pub(super) has_effects: bool,
    pub(super) has_session: bool,
    pub(super) complete_source_call_seal: String,
    pub(super) any_boundary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedAnySourceCallableSealV1 {
    pub(super) owner: NodeId,
    pub(super) signature_seal: String,
    pub(super) typed_body_seal: String,
    pub(super) checker_cfg_seal: String,
    pub(super) contracts_seal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedAnyInterproceduralSourcePlanV1 {
    pub(super) schema: &'static str,
    pub(super) callable_seals: BTreeMap<NodeId, ManagedAnySourceCallableSealV1>,
    pub(super) callsites: BTreeMap<ManagedAnySourceCallsiteKeyV1, ManagedAnySourceCallsiteV1>,
    pub(super) summaries:
        BTreeMap<ManagedAnySourceFunctionKeyV1, ManagedAnySourceFunctionSummaryV1>,
    pub(super) instance_sccs: Vec<Vec<ManagedAnySourceFunctionKeyV1>>,
    pub(super) source_type_schema_seal: String,
    pub(super) uncovered_boundaries: BTreeMap<ManagedAnySourceCallsiteKeyV1, String>,
    mir_call_bindings:
        BTreeMap<(NodeId, crate::core::mir::MirInstructionId), ManagedAnySourceCallsiteKeyV1>,
    canonical_snapshot: String,
}

impl ManagedAnyInterproceduralSourcePlanV1 {
    pub(super) fn canonical_snapshot(&self) -> &str {
        &self.canonical_snapshot
    }

    fn refresh_canonical_snapshot(&mut self) {
        self.canonical_snapshot = format!(
            "schema={:?}; callable_seals={:#?}; callsites={:#?}; summaries={:#?}; instance_sccs={:#?}; source_type_schema_seal={:#?}; uncovered_boundaries={:#?}; mir_call_bindings={:#?}",
            self.schema,
            self.callable_seals,
            self.callsites,
            self.summaries,
            self.instance_sccs,
            self.source_type_schema_seal,
            self.uncovered_boundaries,
            self.mir_call_bindings,
        );
    }

    pub(super) fn bind_mir_call_census(
        &mut self,
        bindings: BTreeMap<
            (NodeId, crate::core::mir::MirInstructionId),
            ManagedAnySourceCallsiteKeyV1,
        >,
    ) {
        self.mir_call_bindings = bindings;
        self.refresh_canonical_snapshot();
    }

    pub(super) fn authorizes_direct_call(
        &self,
        caller: &MirFunction,
        instruction: &crate::core::mir::MirInstruction,
        target: &NodeId,
        type_arguments: &[ResolvedTypeId],
        instances: &BTreeMap<crate::core::mir::MirInstanceId, MirInstance>,
    ) -> bool {
        let Some(key) = self
            .mir_call_bindings
            .get(&(caller.owner.clone(), instruction.id.clone()))
        else {
            return false;
        };
        let Some(callsite) = self.callsites.get(key) else {
            return false;
        };
        let Some(summary) = self.summaries.get(&callsite.callee) else {
            return false;
        };
        if self.uncovered_boundaries.contains_key(key)
            || !callsite.any_boundary
            || callsite.has_effects
            || callsite.has_session
            || callsite.call_permission.is_some()
            || summary.state != ManagedAnySourceFunctionSummaryStateV1::Proven
            || (summary.result_paths.is_empty() && summary.parameter_contracts.is_empty())
            || callsite.callee.type_arguments.len() != type_arguments.len()
            || !callsite
                .callee
                .type_arguments
                .iter()
                .zip(type_arguments)
                .all(|(term, raw)| term.source_id == *raw)
        {
            return false;
        }
        let expected_target = if callsite.callee.type_arguments.is_empty() {
            Some(callsite.callee.owner.clone())
        } else {
            let matches = instances
                .values()
                .filter(|instance| {
                    instance.template == callsite.callee.owner
                        && instance.arguments.len() == callsite.callee.type_arguments.len()
                        && instance
                            .arguments
                            .iter()
                            .zip(&callsite.callee.type_arguments)
                            .all(|(raw, term)| term.source_id == *raw)
                })
                .collect::<Vec<_>>();
            (matches.len() == 1).then(|| matches[0].function.clone())
        };
        expected_target.as_ref() == Some(target)
    }

    pub(super) fn authorizes_generic_instance(
        &self,
        instance: &MirInstance,
        functions: &BTreeMap<NodeId, MirFunction>,
        instances: &BTreeMap<crate::core::mir::MirInstanceId, MirInstance>,
    ) -> bool {
        let matching_keys = self
            .summaries
            .keys()
            .filter(|key| {
                key.owner == instance.template
                    && key.type_arguments.len() == instance.arguments.len()
                    && key
                        .type_arguments
                        .iter()
                        .zip(&instance.arguments)
                        .all(|(term, raw)| term.source_id == *raw)
            })
            .collect::<Vec<_>>();
        let [key] = matching_keys.as_slice() else {
            return false;
        };
        let Some(summary) = self.summaries.get(*key) else {
            return false;
        };
        summary.state == ManagedAnySourceFunctionSummaryStateV1::Proven
            && (!summary.result_paths.is_empty() || !summary.parameter_contracts.is_empty())
            && exact_source_mir_owner(key, functions, instances).as_ref()
                == Some(&instance.function)
    }

    pub(super) fn derive(
        program: &CheckedProgram,
        instances: &BTreeMap<crate::core::mir::MirInstanceId, MirInstance>,
        type_catalog: &MirTypeCatalog,
        excluded_sources: Option<&HashSet<SourceId>>,
    ) -> Result<Self, Vec<String>> {
        SourcePlanBuilder::new(program, instances, type_catalog, excluded_sources).derive()
    }

    pub(super) fn validate_mir_call_census(
        &self,
        functions: &BTreeMap<NodeId, MirFunction>,
        instances: &BTreeMap<crate::core::mir::MirInstanceId, MirInstance>,
        type_catalog: &MirTypeCatalog,
    ) -> Result<
        BTreeMap<(NodeId, crate::core::mir::MirInstructionId), ManagedAnySourceCallsiteKeyV1>,
        Vec<String>,
    > {
        validate_mir_call_census(self, functions, instances, type_catalog)
    }
}

#[derive(Debug, Clone)]
struct ScannedSourceCallV1 {
    node: NodeId,
    call: crate::core::ir::ResolvedCall,
    executable: bool,
}

#[derive(Debug, Clone)]
struct SourcePlanBuilder<'a> {
    program: &'a CheckedProgram,
    instances: &'a BTreeMap<crate::core::mir::MirInstanceId, MirInstance>,
    type_catalog: &'a MirTypeCatalog,
    excluded_sources: Option<&'a HashSet<SourceId>>,
}

impl<'a> SourcePlanBuilder<'a> {
    fn new(
        program: &'a CheckedProgram,
        instances: &'a BTreeMap<crate::core::mir::MirInstanceId, MirInstance>,
        type_catalog: &'a MirTypeCatalog,
        excluded_sources: Option<&'a HashSet<SourceId>>,
    ) -> Self {
        Self {
            program,
            instances,
            type_catalog,
            excluded_sources,
        }
    }

    fn is_excluded(&self, callable: &crate::core::ResolvedCallable) -> bool {
        self.excluded_sources.is_some_and(|excluded| {
            excluded.contains(&callable.body.root.origin.user_span().source_id)
        })
    }

    fn type_may_contain_any(
        &self,
        id: &ResolvedTypeId,
        substitution: &BTreeMap<NodeId, ManagedAnySourceTypeTermV1>,
    ) -> Result<bool, String> {
        let term = source_type_term(self.program.resolved_types(), id, substitution)?;
        Ok(source_shape_for_term(self.program, &term)?.contains_any())
    }

    fn call_may_contain_any(
        &self,
        call: &crate::core::ir::ResolvedCall,
        substitution: &BTreeMap<NodeId, ManagedAnySourceTypeTermV1>,
    ) -> bool {
        call.arguments.iter().any(|argument| {
            self.type_may_contain_any(&argument.value.ty, substitution)
                .unwrap_or(true)
                || self
                    .type_may_contain_any(&argument.conversion.from, substitution)
                    .unwrap_or(true)
                || self
                    .type_may_contain_any(&argument.conversion.to, substitution)
                    .unwrap_or(true)
        }) || self
            .type_may_contain_any(&call.result, substitution)
            .unwrap_or(true)
            || call.type_arguments.iter().any(|argument| {
                self.type_may_contain_any(argument, substitution)
                    .unwrap_or(true)
            })
    }

    fn derive(self) -> Result<ManagedAnyInterproceduralSourcePlanV1, Vec<String>> {
        let mut errors = Vec::new();
        let mut callable_seals = BTreeMap::new();
        let mut calls_by_owner = BTreeMap::<NodeId, Vec<ScannedSourceCallV1>>::new();
        for (owner, callable) in self.program.callables() {
            if self.is_excluded(callable) {
                continue;
            }
            callable_seals.insert(
                owner.clone(),
                ManagedAnySourceCallableSealV1 {
                    owner: owner.clone(),
                    signature_seal: format!("{:#?}", callable.signature),
                    typed_body_seal: format!("{:#?}", callable.body),
                    checker_cfg_seal: format!("{:#?}", callable.cfg),
                    contracts_seal: format!("{:#?}", callable.contracts),
                },
            );
            match scan_complete_source_call_census(&callable.body) {
                Ok(calls) => {
                    calls_by_owner.insert(owner.clone(), calls);
                }
                Err(messages) => errors.extend(messages.into_iter().map(|message| {
                    format!(
                        "Checker call census for '{}' is incomplete: {message}",
                        owner.0
                    )
                })),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        let mut instance_keys = BTreeSet::new();
        for (owner, callable) in self.program.callables() {
            if !self.is_excluded(callable) && callable.signature.generic_parameters.is_empty() {
                instance_keys.insert(ManagedAnySourceFunctionKeyV1 {
                    owner: owner.clone(),
                    type_arguments: Vec::new(),
                });
            }
        }
        for instance in self.instances.values() {
            if !callable_seals.contains_key(&instance.template) {
                continue;
            }
            let Some(callable) = self.program.callable(&instance.template) else {
                continue;
            };
            if instance.arguments.len() != callable.signature.generic_parameters.len() {
                continue;
            }
            let arguments = instance
                .arguments
                .iter()
                .map(|argument| {
                    source_type_term(self.program.resolved_types(), argument, &BTreeMap::new())
                })
                .collect::<Result<Vec<_>, _>>();
            if let Ok(type_arguments) = arguments {
                instance_keys.insert(ManagedAnySourceFunctionKeyV1 {
                    owner: instance.template.clone(),
                    type_arguments,
                });
            }
        }

        let mut uncovered_boundaries = BTreeMap::new();
        let mut queue = instance_keys.iter().cloned().collect::<VecDeque<_>>();
        let mut processed = BTreeSet::new();
        let mut callsite_templates = BTreeMap::new();
        while let Some(caller_key) = queue.pop_front() {
            if !processed.insert(caller_key.clone()) {
                continue;
            }
            if instance_keys.len() > MAX_SOURCE_INSTANCE_KEYS {
                return Err(vec![format!(
                    "ordinary ManagedAny source instance closure exceeds {MAX_SOURCE_INSTANCE_KEYS} keys"
                )]);
            }
            let Some(caller) = self.program.callable(&caller_key.owner) else {
                continue;
            };
            let Some(raw_calls) = calls_by_owner.get(&caller_key.owner) else {
                return Err(vec![format!(
                    "source specialization '{}' has no complete Checker call census",
                    caller_key.owner.0
                )]);
            };
            let caller_substitution = source_type_substitution(
                &caller.signature.generic_parameters,
                &caller_key.type_arguments,
            )
            .map_err(|message| vec![format!("{}: {message}", caller_key.owner.0)])?;

            for source_call in raw_calls {
                let call_key = (caller_key.clone(), source_call.node.clone());
                let mut boundary =
                    self.call_may_contain_any(&source_call.call, &caller_substitution);
                match &source_call.call.callee {
                    ResolvedCallee::Function(target_owner) => {
                        let Some(target) = self.program.callable(target_owner) else {
                            if boundary {
                                uncovered_boundaries.insert(
                                    ManagedAnySourceCallsiteKeyV1 {
                                        caller: caller_key.clone(),
                                        node: source_call.node.clone(),
                                    },
                                    format!(
                                        "target '{}' has no Checker callable body",
                                        target_owner.0
                                    ),
                                );
                            }
                            callsite_templates.insert(call_key, source_call.clone());
                            continue;
                        };
                        let mut target_arguments = Vec::new();
                        let mut argument_error = None;
                        for argument in &source_call.call.type_arguments {
                            match source_type_term(
                                self.program.resolved_types(),
                                argument,
                                &caller_substitution,
                            ) {
                                Ok(term) => target_arguments.push(term),
                                Err(message) => argument_error = Some(message),
                            }
                        }
                        let target_substitution = if target_arguments.len()
                            == target.signature.generic_parameters.len()
                        {
                            source_type_substitution(
                                &target.signature.generic_parameters,
                                &target_arguments,
                            )
                            .ok()
                        } else {
                            None
                        };
                        if let Some(substitution) = &target_substitution {
                            boundary |= target.signature.parameters.iter().any(|parameter| {
                                self.type_may_contain_any(&parameter.ty, substitution)
                                    .unwrap_or(true)
                            }) || self
                                .type_may_contain_any(&target.signature.result, substitution)
                                .unwrap_or(true);
                        } else if !target.signature.generic_parameters.is_empty() {
                            boundary = true;
                        }
                        if boundary {
                            let key = ManagedAnySourceCallsiteKeyV1 {
                                caller: caller_key.clone(),
                                node: source_call.node.clone(),
                            };
                            if let Some(message) = argument_error {
                                uncovered_boundaries.insert(key, message);
                            } else if target_substitution.is_none() {
                                uncovered_boundaries.insert(
                                    key,
                                    "callee has no exact fully substituted generic instance".into(),
                                );
                            }
                        }
                        if let Some(substitution) = target_substitution {
                            let instantiated = ManagedAnySourceFunctionKeyV1 {
                                owner: target_owner.clone(),
                                type_arguments: target_arguments.clone(),
                            };
                            if instance_keys.insert(instantiated.clone()) {
                                queue.push_back(instantiated);
                            }
                            validate_direct_source_call(
                                self.program,
                                &caller_substitution,
                                source_call,
                                target,
                                &substitution,
                                &mut errors,
                            );
                        }
                    }
                    _ if boundary => {
                        uncovered_boundaries.insert(
                            ManagedAnySourceCallsiteKeyV1 {
                                caller: caller_key.clone(),
                                node: source_call.node.clone(),
                            },
                            format!(
                                "{:?} Any boundary has no joined source owner/loan receipt",
                                source_call.call.callee
                            ),
                        );
                    }
                    _ => {}
                }
                callsite_templates.insert(call_key, source_call.clone());
                if callsite_templates.len() > MAX_SOURCE_CALLSITE_ROWS {
                    return Err(vec![
                        "ordinary ManagedAny source call-site census exceeds its closed size bound"
                            .into(),
                    ]);
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        let mut callsites = BTreeMap::new();
        for ((caller_key, node), source_call) in callsite_templates {
            let ResolvedCallee::Function(target_owner) = &source_call.call.callee else {
                continue;
            };
            let Some(caller) = self.program.callable(&caller_key.owner) else {
                continue;
            };
            let Some(target) = self.program.callable(target_owner) else {
                continue;
            };
            let Ok(substitution) = source_type_substitution(
                &caller.signature.generic_parameters,
                &caller_key.type_arguments,
            ) else {
                continue;
            };
            let Ok(target_arguments) = source_call
                .call
                .type_arguments
                .iter()
                .map(|argument| {
                    source_type_term(self.program.resolved_types(), argument, &substitution)
                })
                .collect::<Result<Vec<_>, _>>()
            else {
                continue;
            };
            let Ok(target_substitution) =
                source_type_substitution(&target.signature.generic_parameters, &target_arguments)
            else {
                continue;
            };
            let mut argument_rows = Vec::new();
            let mut any_boundary = false;
            for (argument, parameter) in source_call
                .call
                .arguments
                .iter()
                .zip(&target.signature.parameters)
            {
                let Ok(parameter_type) = source_type_term(
                    self.program.resolved_types(),
                    &parameter.ty,
                    &target_substitution,
                ) else {
                    continue;
                };
                let Ok(actual_type) = source_type_term(
                    self.program.resolved_types(),
                    &argument.value.ty,
                    &substitution,
                ) else {
                    continue;
                };
                let Ok(conversion_from) = source_type_term(
                    self.program.resolved_types(),
                    &argument.conversion.from,
                    &substitution,
                ) else {
                    continue;
                };
                let Ok(conversion_to) = source_type_term(
                    self.program.resolved_types(),
                    &argument.conversion.to,
                    &substitution,
                ) else {
                    continue;
                };
                let actual_shape = source_shape_for_term(self.program, &actual_type);
                let formal_shape = source_shape_for_term(self.program, &parameter_type);
                let (Ok(actual_shape), Ok(parameter_shape)) = (actual_shape, formal_shape) else {
                    let any_key = ManagedAnySourceCallsiteKeyV1 {
                        caller: caller_key.clone(),
                        node: node.clone(),
                    };
                    uncovered_boundaries.insert(
                        any_key,
                        "call argument/formal has an unsupported recursive Any source shape".into(),
                    );
                    continue;
                };
                any_boundary |= actual_shape.contains_any() || parameter_shape.contains_any();
                let actual_paths = actual_shape.any_paths();
                let parameter_paths = parameter_shape.any_paths();
                if any_boundary
                    && actual_shape.contains_any()
                    && parameter_shape.contains_any()
                    && actual_paths != parameter_paths
                {
                    uncovered_boundaries.insert(
                        ManagedAnySourceCallsiteKeyV1 {
                            caller: caller_key.clone(),
                            node: node.clone(),
                        },
                        "actual/formal Any projection paths differ".into(),
                    );
                }
                if any_boundary && argument.conversion.kind != CheckedConversionKind::Identity {
                    uncovered_boundaries.insert(
                        ManagedAnySourceCallsiteKeyV1 {
                            caller: caller_key.clone(),
                            node: node.clone(),
                        },
                        format!(
                            "Any argument conversion {:?} has no joined source owner action",
                            argument.conversion.kind
                        ),
                    );
                }
                let permission = parameter.permission;
                let needs_drop = self
                    .type_catalog
                    .get(&parameter_type.source_id)
                    .is_some_and(|descriptor| descriptor.ownership.needs_drop());
                let has_any = actual_shape.contains_any() || parameter_shape.contains_any();
                if has_any
                    && (self.type_catalog.get(&parameter_type.source_id).is_none()
                        || !needs_drop
                        || permission != Some(Permission::Consume))
                {
                    uncovered_boundaries.insert(
                        ManagedAnySourceCallsiteKeyV1 {
                            caller: caller_key.clone(),
                            node: node.clone(),
                        },
                        "Any formal lacks an exact drop-bearing TypeDesc and owned Consume permission".into(),
                    );
                }
                let transfer = match (&argument.value.kind, permission, needs_drop, has_any) {
                    (ResolvedExprKind::Load(place), _, _, true)
                        if !place.projections.is_empty() =>
                    {
                        ManagedAnyCallArgumentTransferV1::UnsupportedAnyFreshExpression {
                            node: argument.value.node_id.clone(),
                            reason: "projected Any input has no exact joined owner projection receipt".into(),
                        }
                    }
                    (ResolvedExprKind::Load(place), Some(Permission::Consume), true, true)
                        if !self.type_catalog.contains_managed_any(
                            &self.type_catalog.canonical_managed_any_type(&argument.value.ty),
                        ) =>
                    {
                        ManagedAnyCallArgumentTransferV1::OwnedMoveFromTypedPlace {
                            place: place.clone(),
                        }
                    }
                    (ResolvedExprKind::Load(place), _, _, _) => {
                        ManagedAnyCallArgumentTransferV1::SnapshotCloneOfTypedPlace {
                            place: place.clone(),
                        }
                    }
                    (ResolvedExprKind::Call(call), _, _, true)
                        if matches!(&call.callee, ResolvedCallee::Function(_)) =>
                    {
                        ManagedAnyCallArgumentTransferV1::FreshDirectHelperResult {
                            call_node: argument.value.node_id.clone(),
                        }
                    }
                    (_, Some(Permission::View | Permission::Mutate), _, _) => {
                        ManagedAnyCallArgumentTransferV1::BorrowedArgument {
                            permission: permission.expect("borrowed permission"),
                        }
                    }
                    (_, _, _, true) => {
                        ManagedAnyCallArgumentTransferV1::UnsupportedAnyFreshExpression {
                            node: argument.value.node_id.clone(),
                            reason: "fresh Any source is outside the helper-result/action-receipt profile".into(),
                        }
                    }
                    _ => ManagedAnyCallArgumentTransferV1::OwnedFreshArgument,
                };
                if has_any
                    && (actual_paths != parameter_paths
                        || argument.conversion.kind != CheckedConversionKind::Identity
                        || permission != Some(Permission::Consume))
                {
                    uncovered_boundaries.insert(
                        ManagedAnySourceCallsiteKeyV1 {
                            caller: caller_key.clone(),
                            node: node.clone(),
                        },
                        "argument owner path/permission/conversion lacks a complete transfer proof"
                            .into(),
                    );
                }
                argument_rows.push(ManagedAnySourceCallArgumentV1 {
                    parameter: parameter.id.clone(),
                    parameter_type,
                    permission,
                    parameter_any_paths: parameter_paths,
                    actual_node: argument.value.node_id.clone(),
                    actual_type,
                    conversion_kind: argument.conversion.kind,
                    conversion_from,
                    conversion_to,
                    any_paths: actual_paths,
                    transfer,
                });
            }
            let Ok(result_type) = source_type_term(
                self.program.resolved_types(),
                &source_call.call.result,
                &substitution,
            ) else {
                continue;
            };
            let result_shape = source_shape_for_term(self.program, &result_type);
            let result_paths = match result_shape {
                Ok(shape) => {
                    any_boundary |= shape.contains_any();
                    shape.any_paths()
                }
                Err(message) => {
                    uncovered_boundaries.insert(
                        ManagedAnySourceCallsiteKeyV1 {
                            caller: caller_key.clone(),
                            node: node.clone(),
                        },
                        format!("unsupported Checker result owner shape: {message}"),
                    );
                    Vec::new()
                }
            };
            any_boundary |= target.signature.parameters.iter().any(|parameter| {
                source_type_term(
                    self.program.resolved_types(),
                    &parameter.ty,
                    &target_substitution,
                )
                .ok()
                .and_then(|term| source_shape_for_term(self.program, &term).ok())
                .is_some_and(|shape| shape.contains_any())
            });
            let Some(callee_source_seal) = callable_seals.get(target_owner) else {
                if any_boundary {
                    uncovered_boundaries.insert(
                        ManagedAnySourceCallsiteKeyV1 {
                            caller: caller_key.clone(),
                            node: node.clone(),
                        },
                        "callee has no private Checker typed-body and CFG seal".into(),
                    );
                }
                continue;
            };
            let Some(caller_source_seal) = callable_seals.get(&caller_key.owner) else {
                if any_boundary {
                    uncovered_boundaries.insert(
                        ManagedAnySourceCallsiteKeyV1 {
                            caller: caller_key.clone(),
                            node: node.clone(),
                        },
                        "caller has no private Checker typed-body and CFG seal".into(),
                    );
                }
                continue;
            };
            let row = ManagedAnySourceCallsiteV1 {
                key: ManagedAnySourceCallsiteKeyV1 {
                    caller: caller_key.clone(),
                    node: node.clone(),
                },
                callee: ManagedAnySourceFunctionKeyV1 {
                    owner: target_owner.clone(),
                    type_arguments: target_arguments,
                },
                caller_source_seal: format!("{caller_source_seal:#?}"),
                callee_source_seal: format!("{callee_source_seal:#?}"),
                arguments: argument_rows,
                result_type,
                result_paths,
                effects_seal: format!("{:#?}", source_call.call.effects),
                session_seal: format!("{:#?}", source_call.call.session),
                call_permission: source_call.call.permission,
                has_effects: !source_call.call.effects.is_empty(),
                has_session: !source_call.call.session.is_empty(),
                complete_source_call_seal: format!("{:#?}", source_call.call),
                any_boundary,
            };
            if row.any_boundary
                && (row.has_effects || row.has_session || row.call_permission.is_some())
            {
                uncovered_boundaries.insert(
                    row.key.clone(),
                    "effectful, session-bearing, or loan-mode Any call has no matching interprocedural receipt".into(),
                );
            }
            callsites.insert(row.key.clone(), row);
        }

        let instance_sccs = source_call_sccs(&instance_keys, &callsites);
        let summaries = solve_source_summaries(
            self.program,
            self.type_catalog,
            &instance_keys,
            &callsites,
            &callable_seals,
            &uncovered_boundaries,
        );
        let mut plan = ManagedAnyInterproceduralSourcePlanV1 {
            schema: MANAGED_ANY_INTERPROCEDURAL_SCHEMA_V1,
            callable_seals,
            callsites,
            summaries,
            instance_sccs,
            source_type_schema_seal: source_type_schema_seal(self.program),
            uncovered_boundaries,
            mir_call_bindings: BTreeMap::new(),
            canonical_snapshot: String::new(),
        };
        plan.refresh_canonical_snapshot();
        Ok(plan)
    }
}

fn validate_direct_source_call(
    program: &CheckedProgram,
    caller_substitution: &BTreeMap<NodeId, ManagedAnySourceTypeTermV1>,
    scanned: &ScannedSourceCallV1,
    target: &crate::core::ResolvedCallable,
    target_substitution: &BTreeMap<NodeId, ManagedAnySourceTypeTermV1>,
    errors: &mut Vec<String>,
) {
    let call = &scanned.call;
    if call.arguments.len() != target.signature.parameters.len() {
        errors.push(format!(
            "direct source call '{}' argument arity disagrees with Checker callee '{}'",
            scanned.node.0, target.owner.0
        ));
        return;
    }
    if !call.effects.is_empty() || !call.session.is_empty() || call.permission.is_some() {
        // These call modes require their own effect/loan/endpoint receipt;
        // an ordinary direct helper summary cannot grant them.
        return;
    }
    for (index, (argument, parameter)) in call
        .arguments
        .iter()
        .zip(&target.signature.parameters)
        .enumerate()
    {
        if argument.parameter != parameter.id || argument.conversion.from != argument.value.ty {
            errors.push(format!(
                "direct source call '{}' argument {index} disagrees with its exact Checker parameter or conversion source",
                scanned.node.0
            ));
            continue;
        }
        let actual = source_type_term(
            program.resolved_types(),
            &argument.conversion.to,
            caller_substitution,
        );
        let formal = source_type_term(program.resolved_types(), &parameter.ty, target_substitution);
        if actual != formal {
            errors.push(format!(
                "direct source call '{}' argument {index} conversion target differs from instantiated Checker formal",
                scanned.node.0
            ));
        }
    }
    if source_type_term(program.resolved_types(), &call.result, caller_substitution)
        != source_type_term(
            program.resolved_types(),
            &target.signature.result,
            target_substitution,
        )
    {
        errors.push(format!(
            "direct source call '{}' result differs from its exact Checker callee result",
            scanned.node.0
        ));
    }
}

fn source_type_substitution(
    binders: &[NodeId],
    arguments: &[ManagedAnySourceTypeTermV1],
) -> Result<BTreeMap<NodeId, ManagedAnySourceTypeTermV1>, String> {
    if binders.len() != arguments.len() {
        return Err(format!(
            "{} generic binders cannot map to {} ordered source type arguments",
            binders.len(),
            arguments.len()
        ));
    }
    let mut result = BTreeMap::new();
    for (binder, argument) in binders.iter().zip(arguments) {
        if result.insert(binder.clone(), argument.clone()).is_some() {
            return Err(format!("generic binder '{}' is duplicated", binder.0));
        }
    }
    Ok(result)
}

fn source_type_term(
    types: &ResolvedTypeTable,
    id: &ResolvedTypeId,
    substitution: &BTreeMap<NodeId, ManagedAnySourceTypeTermV1>,
) -> Result<ManagedAnySourceTypeTermV1, String> {
    fn visit(
        types: &ResolvedTypeTable,
        id: &ResolvedTypeId,
        substitution: &BTreeMap<NodeId, ManagedAnySourceTypeTermV1>,
        visiting: &mut BTreeSet<ResolvedTypeId>,
        depth: usize,
    ) -> Result<ManagedAnySourceTypeTermV1, String> {
        if depth > MAX_TYPE_RECURSION {
            return Err("Checker source type exceeds the structural recursion bound".into());
        }
        let ty = types
            .get(id)
            .ok_or_else(|| format!("Checker canonical type '{}' is absent", id.as_str()))?;
        if let ResolvedType::GenericParameter(binder) = ty {
            return Ok(substitution.get(binder).cloned().unwrap_or_else(|| {
                ManagedAnySourceTypeTermV1 {
                    source_id: id.clone(),
                    head: ManagedAnySourceTypeHeadV1::GenericParameter(binder.clone()),
                    children: Vec::new(),
                }
            }));
        }
        if !visiting.insert(id.clone()) {
            return Err(format!(
                "cyclic Checker type term '{}' has no finite source identity",
                id.as_str()
            ));
        }
        let mut children = Vec::new();
        for child in resolved_type_children(types, id)? {
            children.push(visit(types, child, substitution, visiting, depth + 1)?);
        }
        visiting.remove(id);
        Ok(ManagedAnySourceTypeTermV1 {
            source_id: id.clone(),
            head: source_type_head(ty),
            children,
        })
    }
    visit(types, id, substitution, &mut BTreeSet::new(), 0)
}

fn source_type_head(ty: &ResolvedType) -> ManagedAnySourceTypeHeadV1 {
    use ManagedAnySourceTypeHeadV1 as H;
    match ty {
        ResolvedType::Primitive(value) => H::Primitive(*value),
        ResolvedType::GenericParameter(binder) => H::GenericParameter(binder.clone()),
        ResolvedType::Nominal {
            item, is_linear, ..
        } => H::Nominal {
            item: item.clone(),
            is_linear: *is_linear,
        },
        ResolvedType::FlowStateSet { flow, states } => H::FlowStateSet {
            flow: flow.clone(),
            states: states.clone(),
        },
        ResolvedType::Reference {
            lifetime, mutable, ..
        } => H::Reference {
            lifetime: lifetime.clone(),
            mutable: *mutable,
        },
        ResolvedType::Option(_) => H::Option,
        ResolvedType::Result { .. } => H::Result,
        ResolvedType::Tuple(elements) => H::Tuple(elements.len()),
        ResolvedType::Function {
            abi, parameters, ..
        } => H::Function {
            abi: *abi,
            parameters: parameters.len(),
        },
        ResolvedType::CBuffer(_) => H::CBuffer,
        ResolvedType::Capability(nominal) => H::Capability(nominal.clone()),
        ResolvedType::Ownership { kind, .. } => H::Ownership(*kind),
        ResolvedType::Newtype { item, .. } => H::Newtype(item.clone()),
        ResolvedType::Array { length, .. } => H::Array(*length),
        ResolvedType::Slice(_) => H::Slice,
        ResolvedType::Trait { kind, traits } => H::Trait {
            kind: *kind,
            traits: traits.clone(),
        },
        ResolvedType::RawPointer { mutable, .. } => H::RawPointer { mutable: *mutable },
        ResolvedType::DynamicAny { capability } => H::DynamicAny {
            capability: capability.clone(),
        },
    }
}

fn resolved_type_children<'a>(
    types: &'a ResolvedTypeTable,
    id: &ResolvedTypeId,
) -> Result<Vec<&'a ResolvedTypeId>, String> {
    let ty = types
        .get(id)
        .ok_or_else(|| format!("Checker canonical type '{}' is absent", id.as_str()))?;
    Ok(match ty {
        ResolvedType::Nominal { arguments, .. } | ResolvedType::Tuple(arguments) => {
            arguments.iter().collect()
        }
        ResolvedType::Reference { target, .. }
        | ResolvedType::Option(target)
        | ResolvedType::CBuffer(target)
        | ResolvedType::Ownership { target, .. }
        | ResolvedType::Newtype { inner: target, .. }
        | ResolvedType::Array {
            element: target, ..
        }
        | ResolvedType::Slice(target)
        | ResolvedType::RawPointer { target, .. } => vec![target],
        ResolvedType::Result { ok, error } => vec![ok, error],
        ResolvedType::Function {
            parameters, result, ..
        } => parameters.iter().chain(std::iter::once(result)).collect(),
        ResolvedType::Primitive(_)
        | ResolvedType::GenericParameter(_)
        | ResolvedType::FlowStateSet { .. }
        | ResolvedType::Capability(_)
        | ResolvedType::Trait { .. }
        | ResolvedType::DynamicAny { .. } => Vec::new(),
    })
}

fn source_shape_for_term(
    program: &CheckedProgram,
    term: &ManagedAnySourceTypeTermV1,
) -> Result<ManagedAnySourceShapeV1, String> {
    fn product_if_any(
        projection: ManagedAnyProjectionV1,
        child: ManagedAnySourceShapeV1,
    ) -> ManagedAnySourceShapeV1 {
        if child.contains_any() {
            ManagedAnySourceShapeV1::Product(vec![(projection, child)])
        } else {
            ManagedAnySourceShapeV1::None
        }
    }

    fn recurse(
        program: &CheckedProgram,
        term: &ManagedAnySourceTypeTermV1,
        visiting: &mut BTreeSet<ResolvedTypeId>,
        saw_cycle: &mut bool,
        depth: usize,
    ) -> Result<ManagedAnySourceShapeV1, String> {
        if depth > MAX_TYPE_RECURSION {
            return Err("Checker Any owner-path graph exceeds its recursion bound".into());
        }
        if let ManagedAnySourceTypeHeadV1::DynamicAny { capability } = &term.head {
            return Ok(ManagedAnySourceShapeV1::AnyLeaf {
                capability: capability.clone(),
            });
        }
        if matches!(&term.head, ManagedAnySourceTypeHeadV1::GenericParameter(_)) {
            return Err(format!(
                "unsubstituted Checker generic binder remains in '{}'",
                term.source_id.as_str()
            ));
        }
        if !visiting.insert(term.source_id.clone()) {
            *saw_cycle = true;
            return Ok(ManagedAnySourceShapeV1::None);
        }

        let mut fields = Vec::new();
        let shape = match &term.head {
            ManagedAnySourceTypeHeadV1::Tuple(arity) => {
                if *arity != term.children.len() {
                    return Err(format!(
                        "tuple '{}' has an incomplete instantiated child list",
                        term.source_id.as_str()
                    ));
                }
                for (index, child) in term.children.iter().enumerate() {
                    let child_shape = recurse(program, child, visiting, saw_cycle, depth + 1)?;
                    if child_shape.contains_any() {
                        fields.push((ManagedAnyProjectionV1::Tuple(index), child_shape));
                    }
                }
                if fields.is_empty() {
                    ManagedAnySourceShapeV1::None
                } else {
                    ManagedAnySourceShapeV1::Product(fields)
                }
            }
            ManagedAnySourceTypeHeadV1::Option if term.children.len() == 1 => {
                let child = recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                product_if_any(ManagedAnyProjectionV1::OptionSome, child)
            }
            ManagedAnySourceTypeHeadV1::Result if term.children.len() == 2 => {
                let ok = recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                let error = recurse(program, &term.children[1], visiting, saw_cycle, depth + 1)?;
                if ok.contains_any() {
                    fields.push((ManagedAnyProjectionV1::ResultOk, ok));
                }
                if error.contains_any() {
                    fields.push((ManagedAnyProjectionV1::ResultError, error));
                }
                if fields.is_empty() {
                    ManagedAnySourceShapeV1::None
                } else {
                    ManagedAnySourceShapeV1::Product(fields)
                }
            }
            ManagedAnySourceTypeHeadV1::Array(length) if term.children.len() == 1 => {
                let child = recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                if *length == 0 {
                    ManagedAnySourceShapeV1::None
                } else {
                    product_if_any(ManagedAnyProjectionV1::ArrayElements, child)
                }
            }
            ManagedAnySourceTypeHeadV1::Newtype(_) if term.children.len() == 1 => {
                let child = recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                product_if_any(ManagedAnyProjectionV1::NewtypePayload, child)
            }
            ManagedAnySourceTypeHeadV1::Ownership(_) if term.children.len() == 1 => {
                let child = recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                product_if_any(ManagedAnyProjectionV1::OwnershipPayload, child)
            }
            ManagedAnySourceTypeHeadV1::Nominal { item, .. } => match item.as_str() {
                "builtin:type:Option" if term.children.len() == 1 => {
                    let child =
                        recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                    product_if_any(ManagedAnyProjectionV1::OptionSome, child)
                }
                "builtin:type:Result" if term.children.len() == 2 => {
                    let ok = recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                    let error =
                        recurse(program, &term.children[1], visiting, saw_cycle, depth + 1)?;
                    if ok.contains_any() {
                        fields.push((ManagedAnyProjectionV1::ResultOk, ok));
                    }
                    if error.contains_any() {
                        fields.push((ManagedAnyProjectionV1::ResultError, error));
                    }
                    if fields.is_empty() {
                        ManagedAnySourceShapeV1::None
                    } else {
                        ManagedAnySourceShapeV1::Product(fields)
                    }
                }
                "builtin:type:List" if term.children.len() == 1 => {
                    let child =
                        recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                    product_if_any(ManagedAnyProjectionV1::ListElements, child)
                }
                "builtin:type:Set" if term.children.len() == 1 => {
                    let child =
                        recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                    product_if_any(ManagedAnyProjectionV1::SetElements, child)
                }
                "builtin:type:Map" if term.children.len() == 2 => {
                    let key = recurse(program, &term.children[0], visiting, saw_cycle, depth + 1)?;
                    let value =
                        recurse(program, &term.children[1], visiting, saw_cycle, depth + 1)?;
                    if key.contains_any() {
                        return Err(
                            "Any-bearing Map keys have no ordinary ownership path contract".into(),
                        );
                    }
                    product_if_any(ManagedAnyProjectionV1::MapValues, value)
                }
                _ => {
                    let type_id = NodeId(item.as_str().to_owned());
                    let Some(definition) = program.type_defs().get(&type_id) else {
                        let children_any =
                            term.children.iter().try_fold(false, |found, child| {
                                Ok::<_, String>(
                                    found
                                        || recurse(program, child, visiting, saw_cycle, depth + 1)?
                                            .contains_any(),
                                )
                            })?;
                        if children_any {
                            return Err(format!(
                                "Any-bearing nominal '{}' has no exact Checker declaration",
                                item.as_str()
                            ));
                        }
                        visiting.remove(&term.source_id);
                        return Ok(ManagedAnySourceShapeV1::None);
                    };
                    if definition.kind != ResolvedTypeKind::Record {
                        let children_any =
                            term.children.iter().try_fold(false, |found, child| {
                                Ok::<_, String>(
                                    found
                                        || recurse(program, child, visiting, saw_cycle, depth + 1)?
                                            .contains_any(),
                                )
                            })?;
                        if children_any {
                            return Err(format!(
                                "Any-bearing nominal '{}' is not a plain Checker record",
                                item.as_str()
                            ));
                        }
                        ManagedAnySourceShapeV1::None
                    } else {
                        let binders = definition
                            .generic_parameters
                            .iter()
                            .map(|(_, binder)| binder.clone())
                            .collect::<Vec<_>>();
                        if binders.len() != term.children.len() {
                            return Err(format!(
                                "record '{}' generic arguments disagree with its Checker binders",
                                item.as_str()
                            ));
                        }
                        let substitution = source_type_substitution(&binders, &term.children)?;
                        let mut field_ids =
                            definition.field_ids.values().cloned().collect::<Vec<_>>();
                        field_ids.sort();
                        for field in field_ids {
                            let field_type =
                                program.resolved_field_type(&field).ok_or_else(|| {
                                    format!("record field '{}' has no canonical type", field.0)
                                })?;
                            let field_term = source_type_term(
                                program.resolved_types(),
                                field_type,
                                &substitution,
                            )?;
                            let field_shape =
                                recurse(program, &field_term, visiting, saw_cycle, depth + 1)?;
                            if field_shape.contains_any() {
                                fields.push((ManagedAnyProjectionV1::Field(field), field_shape));
                            }
                        }
                        if fields.is_empty() {
                            ManagedAnySourceShapeV1::None
                        } else {
                            ManagedAnySourceShapeV1::Product(fields)
                        }
                    }
                }
            },
            ManagedAnySourceTypeHeadV1::Primitive(_)
            | ManagedAnySourceTypeHeadV1::FlowStateSet { .. }
            | ManagedAnySourceTypeHeadV1::Capability(_)
            | ManagedAnySourceTypeHeadV1::Trait { .. } => ManagedAnySourceShapeV1::None,
            ManagedAnySourceTypeHeadV1::GenericParameter(_) => {
                return Err("unsubstituted generic binder remains in source graph".into())
            }
            ManagedAnySourceTypeHeadV1::DynamicAny { .. } => unreachable!("handled above"),
            _ => {
                let children_any = term.children.iter().try_fold(false, |found, child| {
                    Ok::<_, String>(
                        found
                            || recurse(program, child, visiting, saw_cycle, depth + 1)?
                                .contains_any(),
                    )
                })?;
                if children_any {
                    return Err(format!(
                        "Any-bearing source constructor {:?} has no exact owner-path algebra",
                        term.head
                    ));
                }
                ManagedAnySourceShapeV1::None
            }
        };
        visiting.remove(&term.source_id);
        Ok(shape)
    }

    let mut saw_cycle = false;
    let shape = recurse(program, term, &mut BTreeSet::new(), &mut saw_cycle, 0)?;
    if saw_cycle && shape.contains_any() {
        return Err("recursive Checker aggregate has no finite Any owner-path algebra".into());
    }
    Ok(shape)
}

fn scan_complete_source_call_census(
    body: &crate::core::ir::ResolvedBody,
) -> Result<Vec<ScannedSourceCallV1>, Vec<String>> {
    fn visit_expr(
        expression: &ResolvedExpr,
        executable: bool,
        calls: &mut Vec<ScannedSourceCallV1>,
        seen: &mut BTreeSet<NodeId>,
        errors: &mut Vec<String>,
    ) {
        if !seen.insert(expression.node_id.clone()) {
            errors.push(format!(
                "duplicate typed expression node '{}'",
                expression.node_id.0
            ));
            return;
        }
        use ResolvedExprKind as E;
        match &expression.kind {
            E::Call(call) => {
                calls.push(ScannedSourceCallV1 {
                    node: expression.node_id.clone(),
                    call: call.clone(),
                    executable,
                });
                for argument in &call.arguments {
                    visit_expr(&argument.value, executable, calls, seen, errors);
                }
            }
            E::FString(parts) => {
                for part in parts {
                    if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                        visit_expr(value, executable, calls, seen, errors);
                    }
                }
            }
            E::Project { value, projection } => {
                visit_expr(value, executable, calls, seen, errors);
                if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                    visit_expr(index, executable, calls, seen, errors);
                }
            }
            E::Binary { left, right, .. } => {
                visit_expr(left, executable, calls, seen, errors);
                visit_expr(right, executable, calls, seen, errors);
            }
            E::Unary { operand, .. }
            | E::TypeOf(operand)
            | E::Old(operand)
            | E::Try { value: operand, .. }
            | E::Spawn(operand)
            | E::Await(operand) => visit_expr(operand, executable, calls, seen, errors),
            E::Tuple(values) | E::List(values) | E::Set(values) => {
                for value in values {
                    visit_expr(value, executable, calls, seen, errors);
                }
            }
            E::Map(entries) => {
                for (key, value) in entries {
                    visit_expr(key, executable, calls, seen, errors);
                    visit_expr(value, executable, calls, seen, errors);
                }
            }
            E::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                visit_expr(value, executable, calls, seen, errors);
                visit_expr(iterable, executable, calls, seen, errors);
                if let Some(guard) = guard {
                    visit_expr(guard, executable, calls, seen, errors);
                }
            }
            E::OptionalChain { receiver, .. } => {
                visit_expr(receiver, executable, calls, seen, errors)
            }
            E::Record { fields, rest, .. } => {
                for field in fields {
                    visit_expr(&field.value, executable, calls, seen, errors);
                }
                if let Some(rest) = rest {
                    visit_expr(rest, executable, calls, seen, errors);
                }
            }
            E::Block(block) | E::Scope { body: block, .. } => {
                visit_block(block, executable, calls, seen, errors)
            }
            // These bodies belong to distinct evaluation domains. Retain their
            // calls for exact agreement with ResolvedBody::resolved_calls, but
            // never attribute them as runtime calls of the enclosing owner.
            E::Comptime(block) | E::Quote(block) => visit_block(block, false, calls, seen, errors),
            E::Lambda(lambda) => visit_block(&lambda.body, false, calls, seen, errors),
            E::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_expr(condition, executable, calls, seen, errors);
                visit_block(then_block, executable, calls, seen, errors);
                visit_block(else_block, executable, calls, seen, errors);
            }
            E::Match { scrutinee, arms } => {
                visit_expr(scrutinee, executable, calls, seen, errors);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit_expr(guard, executable, calls, seen, errors);
                    }
                    visit_expr(&arm.body, executable, calls, seen, errors);
                }
            }
            E::Cast { value, .. } => visit_expr(value, executable, calls, seen, errors),
            E::Range { start, end } => {
                visit_expr(start, executable, calls, seen, errors);
                visit_expr(end, executable, calls, seen, errors);
            }
            E::Slice { target, start, end } => {
                visit_expr(target, executable, calls, seen, errors);
                if let Some(start) = start {
                    visit_expr(start, executable, calls, seen, errors);
                }
                if let Some(end) = end {
                    visit_expr(end, executable, calls, seen, errors);
                }
            }
            E::Literal(_)
            | E::Load(_)
            | E::Constant(_)
            | E::Callable(_)
            | E::DefaultArgument { .. }
            | E::ComptimeValue(_)
            | E::TypeValue(_) => {}
        }
    }
    fn visit_block(
        block: &ResolvedBlock,
        executable: bool,
        calls: &mut Vec<ScannedSourceCallV1>,
        seen: &mut BTreeSet<NodeId>,
        errors: &mut Vec<String>,
    ) {
        if !seen.insert(block.node_id.clone()) {
            errors.push(format!("duplicate typed block node '{}'", block.node_id.0));
            return;
        }
        for statement in &block.statements {
            visit_stmt(statement, executable, calls, seen, errors);
        }
        if let Some(result) = &block.result {
            visit_expr(result, executable, calls, seen, errors);
        }
    }
    fn visit_stmt(
        statement: &ResolvedStmt,
        executable: bool,
        calls: &mut Vec<ScannedSourceCallV1>,
        seen: &mut BTreeSet<NodeId>,
        errors: &mut Vec<String>,
    ) {
        if !seen.insert(statement.node_id.clone()) {
            errors.push(format!(
                "duplicate typed statement node '{}'",
                statement.node_id.0
            ));
            return;
        }
        match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(value) = initializer {
                    visit_expr(value, executable, calls, seen, errors);
                }
            }
            ResolvedStmtKind::Assign { value, .. } | ResolvedStmtKind::Expr(value) => {
                visit_expr(value, executable, calls, seen, errors)
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    visit_expr(value, executable, calls, seen, errors);
                }
            }
            ResolvedStmtKind::While { condition, body } => {
                visit_expr(condition, executable, calls, seen, errors);
                visit_block(body, executable, calls, seen, errors);
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            } => {
                visit_expr(initializer, executable, calls, seen, errors);
                visit_block(body, executable, calls, seen, errors);
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                visit_expr(initializer, executable, calls, seen, errors);
                visit_block(then_block, executable, calls, seen, errors);
                if let Some(block) = else_block {
                    visit_block(block, executable, calls, seen, errors);
                }
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                visit_block(body, executable, calls, seen, errors)
            }
            ResolvedStmtKind::For { iterable, body, .. } => {
                visit_expr(iterable, executable, calls, seen, errors);
                visit_block(body, executable, calls, seen, errors);
            }
            ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::NestedCallable(_) => {}
            ResolvedStmtKind::Contract { condition, .. } => {
                visit_expr(condition, executable, calls, seen, errors)
            }
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    visit_expr(expression, executable, calls, seen, errors);
                }
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                visit_expr(value, executable, calls, seen, errors);
                visit_block(body, executable, calls, seen, errors);
            }
        }
    }

    let mut calls = Vec::new();
    let mut seen = BTreeSet::new();
    let mut errors = Vec::new();
    for input in body.place_inputs.values() {
        visit_expr(input, true, &mut calls, &mut seen, &mut errors);
    }
    for input in body.default_values.values() {
        visit_expr(input, false, &mut calls, &mut seen, &mut errors);
    }
    visit_block(&body.root, true, &mut calls, &mut seen, &mut errors);
    let observed = calls
        .iter()
        .map(|call| (call.node.clone(), call.call.callee.clone()))
        .collect::<BTreeMap<_, _>>();
    if observed != body.resolved_calls {
        let nodes = observed
            .keys()
            .chain(body.resolved_calls.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for node in nodes {
            if observed.get(&node) != body.resolved_calls.get(&node) {
                errors.push(format!(
                    "typed body call census disagrees with Checker index at '{}'",
                    node.0
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(calls.into_iter().filter(|call| call.executable).collect())
    } else {
        Err(errors)
    }
}

fn exact_single_return_expression(
    callable: &crate::core::ResolvedCallable,
) -> Result<&ResolvedExpr, String> {
    if callable.cfg.owner != callable.owner
        || callable.cfg.blocks.len() != 1
        || callable.cfg.reachable.len() != 1
        || !callable.cfg.reachable.contains(&callable.cfg.entry)
        || !callable.cfg.edges.is_empty()
        || !callable.contracts.is_empty()
        || !callable.signature.effects.is_empty()
        || !callable.body.place_inputs.is_empty()
        || !callable.body.default_values.is_empty()
    {
        return Err("callee is not a pure one-block Checker normal-return body".into());
    }
    let block = callable
        .cfg
        .blocks
        .get(&callable.cfg.entry)
        .ok_or_else(|| "Checker CFG entry block is absent".to_string())?;
    let cfg_value = match &block.terminator {
        crate::core::cfg::Terminator::Return { value, .. } => value,
        _ => return Err("Checker CFG entry does not terminate in one normal Return".into()),
    };
    let expression = if callable.body.root.statements.is_empty() {
        callable
            .body
            .root
            .result
            .as_deref()
            .ok_or_else(|| "one-block Checker body has no root result".to_string())?
    } else if callable.body.root.statements.len() == 1 && callable.body.root.result.is_none() {
        let ResolvedStmtKind::Return {
            value: Some(value),
            conversion,
        } = &callable.body.root.statements[0].kind
        else {
            return Err("one-block Checker body contains a statement other than one Return".into());
        };
        if conversion
            .as_ref()
            .is_some_and(|conversion| conversion.kind != CheckedConversionKind::Identity)
        {
            return Err("one-block Checker Return uses a non-identity conversion".into());
        }
        value
    } else {
        return Err("Checker body contains statements outside the one Return".into());
    };
    if cfg_value.as_ref() != Some(&expression.node_id) {
        return Err(
            "Checker CFG Return value does not identify the exact typed source expression".into(),
        );
    }
    Ok(expression)
}

fn call_arguments_forward_every_any_parameter(
    callable: &crate::core::ResolvedCallable,
    program: &CheckedProgram,
    substitution: &BTreeMap<NodeId, ManagedAnySourceTypeTermV1>,
    callsite: &ManagedAnySourceCallsiteV1,
    expected_parameters: &BTreeSet<ResolvedParameterId>,
) -> bool {
    let Ok(calls) = scan_complete_source_call_census(&callable.body) else {
        return false;
    };
    let Some(source_call) = calls.iter().find(|call| call.node == callsite.key.node) else {
        return false;
    };
    if !matches!(&source_call.call.callee, ResolvedCallee::Function(_))
        || !source_call.call.effects.is_empty()
        || !source_call.call.session.is_empty()
        || source_call.call.permission.is_some()
    {
        return false;
    }
    let mut forwarded = BTreeSet::new();
    for argument in callsite
        .arguments
        .iter()
        .filter(|argument| !argument.parameter_any_paths.is_empty())
    {
        let Some(actual) = source_call
            .call
            .arguments
            .iter()
            .find(|actual| actual.parameter == argument.parameter)
        else {
            return false;
        };
        let ResolvedExprKind::Load(place) = &actual.value.kind else {
            return false;
        };
        if !place.projections.is_empty() || actual.value.node_id != argument.actual_node {
            return false;
        }
        let Some(index) = callable
            .body
            .parameters
            .iter()
            .position(|parameter| parameter == &place.base)
        else {
            return false;
        };
        let Some(parameter) = callable.signature.parameters.get(index) else {
            return false;
        };
        if !expected_parameters.contains(&parameter.id)
            || !forwarded.insert(parameter.id.clone())
            || actual.conversion.kind != CheckedConversionKind::Identity
            || argument.permission != Some(Permission::Consume)
        {
            return false;
        }
        let Ok(actual_type) =
            source_type_term(program.resolved_types(), &actual.value.ty, substitution)
        else {
            return false;
        };
        let Ok(parameter_type) =
            source_type_term(program.resolved_types(), &parameter.ty, substitution)
        else {
            return false;
        };
        if actual_type != parameter_type {
            return false;
        }
    }
    forwarded == *expected_parameters
}

fn solve_source_summaries(
    program: &CheckedProgram,
    type_catalog: &MirTypeCatalog,
    keys: &BTreeSet<ManagedAnySourceFunctionKeyV1>,
    callsites: &BTreeMap<ManagedAnySourceCallsiteKeyV1, ManagedAnySourceCallsiteV1>,
    callable_seals: &BTreeMap<NodeId, ManagedAnySourceCallableSealV1>,
    uncovered_boundaries: &BTreeMap<ManagedAnySourceCallsiteKeyV1, String>,
) -> BTreeMap<ManagedAnySourceFunctionKeyV1, ManagedAnySourceFunctionSummaryV1> {
    let mut summaries = BTreeMap::new();
    for key in keys {
        let Some(callable) = program.callable(&key.owner) else {
            continue;
        };
        let Ok(substitution) =
            source_type_substitution(&callable.signature.generic_parameters, &key.type_arguments)
        else {
            continue;
        };
        let mut parameter_contracts = Vec::new();
        let mut unsupported = None;
        for parameter in &callable.signature.parameters {
            let Ok(term) = source_type_term(program.resolved_types(), &parameter.ty, &substitution)
            else {
                unsupported = Some(format!(
                    "parameter '{}' has no exact source type term",
                    parameter.id.0 .0
                ));
                break;
            };
            let Ok(shape) = source_shape_for_term(program, &term) else {
                unsupported = Some(format!(
                    "parameter '{}' has an unsupported Any owner shape",
                    parameter.id.0 .0
                ));
                break;
            };
            if !shape.contains_any() {
                continue;
            }
            let Some(descriptor) = type_catalog.get(&term.source_id) else {
                unsupported = Some(format!(
                    "Any parameter '{}' has no specialized TypeDesc",
                    parameter.id.0 .0
                ));
                break;
            };
            if !descriptor.ownership.needs_drop()
                || !type_catalog.contains_dynamic_or_managed_any(&term.source_id)
                || parameter.permission != Some(Permission::Consume)
            {
                unsupported = Some(format!(
                    "Any parameter '{}' lacks an exact owned Consume/drop descriptor",
                    parameter.id.0 .0
                ));
                break;
            }
            parameter_contracts.push(ManagedAnyParameterSourceV1 {
                parameter: parameter.id.clone(),
                type_term: term,
                permission: parameter.permission,
                mode: ManagedAnyParameterModeV1::OwnedValue,
                any_paths: shape.any_paths(),
                callee_obligation: ManagedAnyParameterObligationV1::CalleeMustSettleEveryExit,
            });
        }
        let result_type = match source_type_term(
            program.resolved_types(),
            &callable.signature.result,
            &substitution,
        ) {
            Ok(term) => term,
            Err(message) => {
                unsupported.get_or_insert(message);
                ManagedAnySourceTypeTermV1 {
                    source_id: callable.signature.result.clone(),
                    head: ManagedAnySourceTypeHeadV1::GenericParameter(NodeId(
                        "unsupported-result".into(),
                    )),
                    children: Vec::new(),
                }
            }
        };
        let result_shape = source_shape_for_term(program, &result_type).unwrap_or_else(|_| {
            unsupported.get_or_insert_with(|| "result has unsupported Any owner shape".into());
            ManagedAnySourceShapeV1::None
        });
        if result_shape.contains_any()
            && type_catalog
                .get(&result_type.source_id)
                .is_none_or(|descriptor| !descriptor.ownership.needs_drop())
        {
            unsupported.get_or_insert_with(|| "Any result lacks an owner/drop TypeDesc".into());
        }
        let source_seal = callable_seals.get(&key.owner);
        summaries.insert(
            key.clone(),
            ManagedAnySourceFunctionSummaryV1 {
                key: key.clone(),
                parameter_contracts,
                result_type,
                result_paths: result_shape.any_paths(),
                result_origins: Vec::new(),
                settles_parameters_at_exit: BTreeSet::new(),
                body_seal: source_seal
                    .map(|seal| seal.typed_body_seal.clone())
                    .unwrap_or_default(),
                cfg_seal: source_seal
                    .map(|seal| seal.checker_cfg_seal.clone())
                    .unwrap_or_default(),
                state: unsupported.map_or(
                    ManagedAnySourceFunctionSummaryStateV1::Pending,
                    ManagedAnySourceFunctionSummaryStateV1::Unsupported,
                ),
            },
        );
    }

    loop {
        let mut changed = false;
        for key in keys {
            let Some(summary) = summaries.get(key) else {
                continue;
            };
            if summary.state != ManagedAnySourceFunctionSummaryStateV1::Pending {
                continue;
            }
            let Some(callable) = program.callable(&key.owner) else {
                continue;
            };
            let Ok(substitution) = source_type_substitution(
                &callable.signature.generic_parameters,
                &key.type_arguments,
            ) else {
                continue;
            };
            let Ok(expression) = exact_single_return_expression(callable) else {
                summaries.get_mut(key).expect("summary exists").state =
                    ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                        "body/CFG is outside the exact single-return summary".into(),
                    );
                changed = true;
                continue;
            };
            let expected_calls = usize::from(matches!(&expression.kind, ResolvedExprKind::Call(_)));
            if callable.body.resolved_calls.len() != expected_calls {
                summaries.get_mut(key).expect("summary exists").state =
                    ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                        "body has an extra or nested source call outside the single-return algebra"
                            .into(),
                    );
                changed = true;
                continue;
            }
            let Some(snapshot) = summaries.get(key).cloned() else {
                continue;
            };
            let parameter_ids = snapshot
                .parameter_contracts
                .iter()
                .map(|parameter| parameter.parameter.clone())
                .collect::<BTreeSet<_>>();
            if snapshot.result_paths.is_empty() {
                if parameter_ids.is_empty() {
                    let summary = summaries.get_mut(key).expect("summary exists");
                    summary.state = ManagedAnySourceFunctionSummaryStateV1::Proven;
                } else {
                    let summary = summaries.get_mut(key).expect("summary exists");
                    summary.state = ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                        "Any input with a non-Any result needs an explicit source drop/escape summary".into(),
                    );
                }
                changed = true;
                continue;
            }
            let result = match &expression.kind {
                ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                    let Some(index) = callable
                        .body
                        .parameters
                        .iter()
                        .position(|parameter| parameter == &place.base)
                    else {
                        summaries.get_mut(key).expect("summary exists").state =
                            ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                                "Any return is not a direct Checker parameter".into(),
                            );
                        changed = true;
                        continue;
                    };
                    let Some(parameter) = callable.signature.parameters.get(index) else {
                        continue;
                    };
                    let Ok(term) =
                        source_type_term(program.resolved_types(), &parameter.ty, &substitution)
                    else {
                        continue;
                    };
                    let Ok(shape) = source_shape_for_term(program, &term) else {
                        continue;
                    };
                    if term != snapshot.result_type
                        || shape.any_paths() != snapshot.result_paths
                        || !parameter_ids.contains(&parameter.id)
                    {
                        summaries.get_mut(key).expect("summary exists").state =
                            ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                                "direct return changes the exact Any type or owner path".into(),
                            );
                        changed = true;
                        continue;
                    }
                    let origins = snapshot
                        .result_paths
                        .iter()
                        .cloned()
                        .map(|source_path| ManagedAnyResultProjectionV1 {
                            result_path: source_path.clone(),
                            origin: ManagedAnyResultOriginV1::ParameterSnapshotClone {
                                parameter: parameter.id.clone(),
                                source_path,
                            },
                        })
                        .collect::<Vec<_>>();
                    Some((origins, parameter_ids.clone()))
                }
                ResolvedExprKind::Call(_) => {
                    let call_key = ManagedAnySourceCallsiteKeyV1 {
                        caller: key.clone(),
                        node: expression.node_id.clone(),
                    };
                    if let Some(reason) = uncovered_boundaries.get(&call_key) {
                        summaries.get_mut(key).expect("summary exists").state =
                            ManagedAnySourceFunctionSummaryStateV1::Unsupported(format!(
                                "nested Any call has no complete Checker-to-MIR transfer: {reason}"
                            ));
                        changed = true;
                        continue;
                    }
                    let Some(callsite) = callsites.get(&call_key) else {
                        continue;
                    };
                    let Some(callee) = summaries.get(&callsite.callee) else {
                        continue;
                    };
                    match &callee.state {
                        ManagedAnySourceFunctionSummaryStateV1::Pending => continue,
                        ManagedAnySourceFunctionSummaryStateV1::Unsupported(_) => {
                            summaries.get_mut(key).expect("summary exists").state =
                                ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                                    "returned helper has no proven callee summary".into(),
                                );
                            changed = true;
                            continue;
                        }
                        ManagedAnySourceFunctionSummaryStateV1::Proven => {}
                    }
                    if !callsite.any_boundary
                        || callee.result_type != snapshot.result_type
                        || callee.result_paths != snapshot.result_paths
                        || !call_arguments_forward_every_any_parameter(
                            callable,
                            program,
                            &substitution,
                            callsite,
                            &parameter_ids,
                        )
                    {
                        summaries.get_mut(key).expect("summary exists").state =
                            ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                                "returned helper call does not map every owned Any input/result path".into(),
                            );
                        changed = true;
                        continue;
                    }
                    let origins = callee
                        .result_paths
                        .iter()
                        .cloned()
                        .map(|callee_result_path| ManagedAnyResultProjectionV1 {
                            result_path: callee_result_path.clone(),
                            origin: ManagedAnyResultOriginV1::DirectHelperResult {
                                call_node: expression.node_id.clone(),
                                callee: callsite.callee.clone(),
                                callee_result_path,
                            },
                        })
                        .collect::<Vec<_>>();
                    Some((origins, parameter_ids.clone()))
                }
                ResolvedExprKind::Cast { conversion, .. }
                    if conversion.kind == CheckedConversionKind::DynamicAnyPack =>
                {
                    summaries.get_mut(key).expect("summary exists").state =
                        ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                            "DynamicAny Pack requires a joined Checker attempt and PackAction"
                                .into(),
                        );
                    changed = true;
                    continue;
                }
                _ => None,
            };
            if let Some((origins, settled)) = result {
                let summary = summaries.get_mut(key).expect("summary exists");
                summary.result_origins = origins;
                summary.settles_parameters_at_exit = settled;
                summary.state = ManagedAnySourceFunctionSummaryStateV1::Proven;
            } else {
                summaries.get_mut(key).expect("summary exists").state =
                    ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                        "result expression is outside the single-return owner algebra".into(),
                    );
            }
            changed = true;
        }
        if !changed {
            break;
        }
    }
    for summary in summaries.values_mut() {
        if summary.state == ManagedAnySourceFunctionSummaryStateV1::Pending {
            summary.state = ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                "recursive SCC remains at bottom; recursive normal-return ownership is not modeled"
                    .into(),
            );
        }
        if summary.state == ManagedAnySourceFunctionSummaryStateV1::Proven {
            let settled = summary
                .parameter_contracts
                .iter()
                .map(|parameter| parameter.parameter.clone())
                .collect::<BTreeSet<_>>();
            let mapped = summary
                .result_origins
                .iter()
                .map(|origin| origin.result_path.clone())
                .collect::<BTreeSet<_>>();
            let expected = summary
                .result_paths
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            if summary.settles_parameters_at_exit != settled || mapped != expected {
                summary.state = ManagedAnySourceFunctionSummaryStateV1::Unsupported(
                    "summary misses an owned input settlement or result owner path".into(),
                );
            }
        }
    }
    summaries
}

fn source_call_sccs(
    keys: &BTreeSet<ManagedAnySourceFunctionKeyV1>,
    callsites: &BTreeMap<ManagedAnySourceCallsiteKeyV1, ManagedAnySourceCallsiteV1>,
) -> Vec<Vec<ManagedAnySourceFunctionKeyV1>> {
    let mut graph = keys
        .iter()
        .cloned()
        .map(|key| (key, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    let mut reverse = graph
        .keys()
        .cloned()
        .map(|key| (key, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for callsite in callsites.values() {
        if graph.contains_key(&callsite.key.caller) && graph.contains_key(&callsite.callee) {
            graph
                .get_mut(&callsite.key.caller)
                .expect("graph key")
                .push(callsite.callee.clone());
            reverse
                .get_mut(&callsite.callee)
                .expect("reverse key")
                .push(callsite.key.caller.clone());
        }
    }
    for edges in graph.values_mut().chain(reverse.values_mut()) {
        edges.sort();
        edges.dedup();
    }
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for root in keys {
        if !seen.insert(root.clone()) {
            continue;
        }
        let mut stack = vec![(root.clone(), false)];
        while let Some((node, expanded)) = stack.pop() {
            if expanded {
                order.push(node);
                continue;
            }
            stack.push((node.clone(), true));
            for next in graph.get(&node).into_iter().flatten().rev() {
                if seen.insert(next.clone()) {
                    stack.push((next.clone(), false));
                }
            }
        }
    }
    seen.clear();
    let mut components = Vec::new();
    for root in order.into_iter().rev() {
        if !seen.insert(root.clone()) {
            continue;
        }
        let mut component = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            component.push(node.clone());
            for next in reverse.get(&node).into_iter().flatten().rev() {
                if seen.insert(next.clone()) {
                    stack.push(next.clone());
                }
            }
        }
        component.sort();
        components.push(component);
    }
    components
}

fn source_type_schema_seal(program: &CheckedProgram) -> String {
    let types = program
        .resolved_types()
        .iter()
        .map(|(id, ty)| (id.clone(), format!("{ty:#?}")))
        .collect::<BTreeMap<_, _>>();
    let fields = program
        .resolved_field_types()
        .iter()
        .map(|(id, ty)| (id.clone(), ty.clone()))
        .collect::<BTreeMap<_, _>>();
    let variants = program
        .resolved_variants()
        .iter()
        .map(|(id, variant)| (id.clone(), variant.clone()))
        .collect::<BTreeMap<_, _>>();
    let defs = program
        .type_defs()
        .iter()
        .map(|(id, definition)| (id.clone(), format!("{definition:#?}")))
        .collect::<BTreeMap<_, _>>();
    format!("types={types:#?}; fields={fields:#?}; variants={variants:#?}; defs={defs:#?}")
}

fn validate_mir_call_census(
    plan: &ManagedAnyInterproceduralSourcePlanV1,
    functions: &BTreeMap<NodeId, MirFunction>,
    instances: &BTreeMap<crate::core::mir::MirInstanceId, MirInstance>,
    type_catalog: &MirTypeCatalog,
) -> Result<
    BTreeMap<(NodeId, crate::core::mir::MirInstructionId), ManagedAnySourceCallsiteKeyV1>,
    Vec<String>,
> {
    use crate::core::mir::{MirInstructionId, MirValueId};

    let mut errors = Vec::new();
    let mut bindings = BTreeMap::new();
    for callsite in plan
        .callsites
        .values()
        .filter(|callsite| callsite.any_boundary)
    {
        let Some(summary) = plan.summaries.get(&callsite.callee) else {
            continue;
        };
        if summary.state != ManagedAnySourceFunctionSummaryStateV1::Proven
            || (summary.result_paths.is_empty() && summary.parameter_contracts.is_empty())
            || plan.uncovered_boundaries.contains_key(&callsite.key)
        {
            continue;
        }
        let Some(source_seal) = plan.callable_seals.get(&callsite.callee.owner) else {
            errors.push(format!(
                "Any-bearing call '{}' has no exact Checker callee seal",
                callsite.key.node.0
            ));
            continue;
        };
        let Some(caller_seal) = plan.callable_seals.get(&callsite.key.caller.owner) else {
            errors.push(format!(
                "Any-bearing call '{}' has no exact Checker caller seal",
                callsite.key.node.0
            ));
            continue;
        };
        if summary.body_seal != source_seal.typed_body_seal
            || summary.cfg_seal != source_seal.checker_cfg_seal
            || callsite.callee_source_seal != format!("{source_seal:#?}")
            || callsite.caller_source_seal != format!("{caller_seal:#?}")
            || summary.result_type != callsite.result_type
            || summary.result_paths != callsite.result_paths
        {
            errors.push(format!(
                "Any-bearing call '{}' summary body/CFG/result-owner seal mismatch",
                callsite.key.node.0
            ));
            continue;
        }
        let expected_any_parameters = summary
            .parameter_contracts
            .iter()
            .map(|parameter| parameter.parameter.clone())
            .collect::<BTreeSet<_>>();
        let observed_any_parameters = callsite
            .arguments
            .iter()
            .filter(|argument| !argument.parameter_any_paths.is_empty())
            .map(|argument| argument.parameter.clone())
            .collect::<BTreeSet<_>>();
        let parameter_contracts_match = expected_any_parameters == observed_any_parameters
            && callsite
                .arguments
                .iter()
                .filter(|argument| !argument.parameter_any_paths.is_empty())
                .all(|argument| {
                    summary
                        .parameter_contracts
                        .iter()
                        .find(|parameter| parameter.parameter == argument.parameter)
                        .is_some_and(|parameter| {
                            parameter.type_term == argument.parameter_type
                                && parameter.permission == argument.permission
                                && parameter.any_paths == argument.parameter_any_paths
                                && parameter.mode == ManagedAnyParameterModeV1::OwnedValue
                                && parameter.callee_obligation
                                    == ManagedAnyParameterObligationV1::CalleeMustSettleEveryExit
                        })
                });
        if !parameter_contracts_match {
            errors.push(format!(
                "Any-bearing call '{}' formal parameters disagree with its callee source summary",
                callsite.key.node.0
            ));
            continue;
        }
        let Some(caller_owner) = exact_source_mir_owner(&callsite.key.caller, functions, instances)
        else {
            errors.push(format!(
                "Any-bearing call '{}' has no exact materialized caller",
                callsite.key.node.0
            ));
            continue;
        };
        let Some(callee_owner) = exact_source_mir_owner(&callsite.callee, functions, instances)
        else {
            errors.push(format!("Any-bearing call '{}' has no exact materialized callee; no legacy fallback is authorized", callsite.key.node.0));
            continue;
        };
        let (Some(caller), Some(callee)) =
            (functions.get(&caller_owner), functions.get(&callee_owner))
        else {
            errors.push(format!(
                "Any-bearing call '{}' caller/callee MIR body is absent",
                callsite.key.node.0
            ));
            continue;
        };
        if type_catalog.validate_managed_any_function(caller).is_err()
            || type_catalog.validate_managed_any_function(callee).is_err()
        {
            errors.push(format!(
                "Any-bearing call '{}' caller or callee lacks a valid MIR owner seal",
                callsite.key.node.0
            ));
            continue;
        }
        let Ok(instruction_id) =
            MirInstructionId::new(format!("inst:call:{}", callsite.key.node.0))
        else {
            errors.push(format!(
                "Any-bearing call '{}' has an invalid canonical MIR call identity",
                callsite.key.node.0
            ));
            continue;
        };
        let hits = caller
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter(|instruction| instruction.id == instruction_id)
            .collect::<Vec<_>>();
        if hits.len() != 1 {
            errors.push(format!(
                "Any-bearing source call '{}' has {} MIR instruction matches",
                callsite.key.node.0,
                hits.len()
            ));
            continue;
        }
        let instruction = hits[0];
        let crate::core::mir::MirInstructionKind::Call {
            result,
            callee: actual_callee,
            type_arguments,
            arguments,
            ..
        } = &instruction.kind
        else {
            errors.push(format!(
                "typed Any call '{}' is not an ordinary direct MIR Call",
                callsite.key.node.0
            ));
            continue;
        };
        if super::canonical_protocol_call_target(actual_callee).as_ref() != Some(&callee_owner) {
            errors.push(format!(
                "MIR call '{}' targets a different executable than its exact Checker callee",
                callsite.key.node.0
            ));
            continue;
        }
        if type_arguments.len() != callsite.callee.type_arguments.len()
            || !type_arguments
                .iter()
                .zip(&callsite.callee.type_arguments)
                .all(|(raw, term)| term.source_id == *raw)
        {
            errors.push(format!(
                "MIR call '{}' generic arguments disagree with source specialization",
                callsite.key.node.0
            ));
            continue;
        }
        if arguments.len() != callsite.arguments.len() || arguments.len() != callee.parameters.len()
        {
            errors.push(format!(
                "MIR call '{}' argument arity disagrees with Checker source/ABI",
                callsite.key.node.0
            ));
            continue;
        }
        let mut arguments_valid = true;
        for (index, ((argument_id, source), parameter_id)) in arguments
            .iter()
            .zip(&callsite.arguments)
            .zip(&callee.parameters)
            .enumerate()
        {
            let Ok(expected_argument) = MirValueId::new(format!("expr:{}", source.actual_node.0))
            else {
                arguments_valid = false;
                continue;
            };
            if argument_id != &expected_argument {
                errors.push(format!(
                    "MIR call '{}' arg {index} is not exact source operand '{}'",
                    callsite.key.node.0, source.actual_node.0
                ));
                arguments_valid = false;
                continue;
            }
            let (Some(argument_value), Some(parameter_value)) = (
                caller.values.get(argument_id),
                callee.values.get(parameter_id),
            ) else {
                errors.push(format!(
                    "MIR call '{}' arg/formal {index} lacks its value-table row",
                    callsite.key.node.0
                ));
                arguments_valid = false;
                continue;
            };
            if argument_value.ty != source.actual_type.source_id
                || parameter_value.ty != source.parameter_type.source_id
                || argument_value.ty != parameter_value.ty
            {
                errors.push(format!(
                    "MIR call '{}' arg {index} TypeDesc differs from exact Checker actual/formal",
                    callsite.key.node.0
                ));
                arguments_valid = false;
                continue;
            }
            let parameter_permission = callee
                .parameter_permissions
                .as_ref()
                .and_then(|permissions| permissions.get(index))
                .copied()
                .flatten();
            if parameter_permission != source.permission {
                errors.push(format!(
                    "MIR call '{}' formal {index} permission differs from exact Checker source",
                    callsite.key.node.0
                ));
                arguments_valid = false;
                continue;
            }
            if source.any_paths.is_empty() && source.parameter_any_paths.is_empty() {
                continue;
            }
            if source.any_paths != source.parameter_any_paths
                || source.conversion_kind != CheckedConversionKind::Identity
                || source.permission != Some(Permission::Consume)
            {
                errors.push(format!(
                    "MIR call '{}' arg {index} has non-identical Any paths/conversion/permission",
                    callsite.key.node.0
                ));
                arguments_valid = false;
                continue;
            }
            match &source.transfer {
                ManagedAnyCallArgumentTransferV1::OwnedMoveFromTypedPlace { place }
                    if place.projections.is_empty() =>
                {
                    validate_exact_local_transfer(
                        caller,
                        argument_id,
                        &place.base,
                        true,
                        &callsite.key.node,
                        index,
                        &mut errors,
                    );
                }
                ManagedAnyCallArgumentTransferV1::SnapshotCloneOfTypedPlace { place }
                    if place.projections.is_empty() =>
                {
                    validate_exact_local_transfer(
                        caller,
                        argument_id,
                        &place.base,
                        false,
                        &callsite.key.node,
                        index,
                        &mut errors,
                    );
                }
                ManagedAnyCallArgumentTransferV1::FreshDirectHelperResult { call_node } => {
                    let child_key = ManagedAnySourceCallsiteKeyV1 {
                        caller: callsite.key.caller.clone(),
                        node: call_node.clone(),
                    };
                    let child_proven = plan.callsites.get(&child_key).is_some_and(|child| {
                        child.any_boundary
                            && !plan.uncovered_boundaries.contains_key(&child_key)
                            && child.result_type == source.actual_type
                            && child.result_paths == source.any_paths
                            && plan.summaries.get(&child.callee).is_some_and(|summary| {
                                summary.state == ManagedAnySourceFunctionSummaryStateV1::Proven
                            })
                    });
                    let child_id = format!("inst:call:{}", call_node.0);
                    let child_output = caller.blocks.values().flat_map(|block| &block.instructions).any(|producer| {
                        producer.id.as_str() == child_id
                            && matches!(&producer.kind, crate::core::mir::MirInstructionKind::Call {
                                result: Some(value), callee: ResolvedCallee::Function(_), ..
                            } if value == argument_id)
                    });
                    if call_node != &source.actual_node || !child_proven || !child_output {
                        errors.push(format!("MIR call '{}' arg {index} is not the exact result of a separately sealed helper call", callsite.key.node.0));
                        arguments_valid = false;
                    }
                }
                ManagedAnyCallArgumentTransferV1::UnsupportedAnyFreshExpression {
                    node,
                    reason,
                } => {
                    errors.push(format!(
                        "Any call '{}' arg '{}' is unsupported: {reason}",
                        callsite.key.node.0, node.0
                    ));
                    arguments_valid = false;
                }
                _ => {
                    errors.push(format!(
                        "Any call '{}' arg {index} has no exact move/clone/helper-result transfer",
                        callsite.key.node.0
                    ));
                    arguments_valid = false;
                }
            }
            if caller.values.get(argument_id).is_none_or(|value| {
                type_catalog.get(&value.ty).is_none_or(|descriptor| {
                    !descriptor.ownership.needs_drop()
                        || !type_catalog.contains_dynamic_or_managed_any(&value.ty)
                })
            }) {
                errors.push(format!(
                    "MIR call '{}' arg {index} lacks an exact recursive Any owner/drop TypeDesc",
                    callsite.key.node.0
                ));
                arguments_valid = false;
            }
        }
        let expected_result = MirValueId::new(format!("expr:{}", callsite.key.node.0)).ok();
        let result_valid = result.as_ref() == expected_result.as_ref()
            && result.as_ref().is_some_and(|result| {
                caller.values.get(result).is_some_and(|value| {
                    value.ty == callsite.result_type.source_id && value.ty == callee.result
                })
            });
        if !result_valid {
            errors.push(format!(
                "MIR call '{}' result differs from exact Checker result identity",
                callsite.key.node.0
            ));
            arguments_valid = false;
        }
        if arguments_valid
            && bindings
                .insert((caller_owner, instruction_id), callsite.key.clone())
                .is_some()
        {
            errors.push(format!(
                "MIR instruction '{}' has duplicate Checker source call rows",
                instruction.id
            ));
        }
    }
    if errors.is_empty() {
        Ok(bindings)
    } else {
        Err(errors)
    }
}

fn exact_source_mir_owner(
    key: &ManagedAnySourceFunctionKeyV1,
    functions: &BTreeMap<NodeId, MirFunction>,
    instances: &BTreeMap<crate::core::mir::MirInstanceId, MirInstance>,
) -> Option<NodeId> {
    if key.type_arguments.is_empty() {
        return functions
            .contains_key(&key.owner)
            .then(|| key.owner.clone());
    }
    let matched = instances
        .values()
        .filter(|instance| {
            instance.template == key.owner
                && instance.arguments.len() == key.type_arguments.len()
                && instance
                    .arguments
                    .iter()
                    .zip(&key.type_arguments)
                    .all(|(raw, term)| term.source_id == *raw)
        })
        .collect::<Vec<_>>();
    (matched.len() == 1).then(|| matched[0].function.clone())
}

fn validate_exact_local_transfer(
    caller: &MirFunction,
    argument: &crate::core::mir::MirValueId,
    local: &crate::core::ir::ResolvedLocalId,
    is_move: bool,
    call_node: &NodeId,
    argument_index: usize,
    errors: &mut Vec<String>,
) {
    let Ok(local_value) = crate::core::mir::MirValueId::new(format!("local:{}", local.0 .0)) else {
        errors.push(format!(
            "Any call '{}' arg {argument_index} has an invalid Checker local identity",
            call_node.0
        ));
        return;
    };
    let producers = caller
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| match (&instruction.kind, is_move) {
            (crate::core::mir::MirInstructionKind::Move { result, .. }, true) => result == argument,
            (crate::core::mir::MirInstructionKind::Clone { result, .. }, false) => {
                result == argument
            }
            _ => false,
        })
        .collect::<Vec<_>>();
    let exact = producers.len() == 1
        && match (&producers[0].kind, is_move) {
            (crate::core::mir::MirInstructionKind::Move { source, .. }, true)
            | (crate::core::mir::MirInstructionKind::Clone { source, .. }, false) => {
                source == &local_value
            }
            _ => false,
        };
    if !exact
        || caller.values.get(&local_value).is_none_or(|value| {
            caller
                .values
                .get(argument)
                .is_none_or(|argument_value| argument_value.ty != value.ty)
        })
    {
        errors.push(format!(
            "Any call '{}' arg {argument_index} does not have its exact MIR {} from the Checker local",
            call_node.0,
            if is_move { "Move" } else { "Clone" }
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked_program(source: &str) -> CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex ordinary Any helper fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse ordinary Any helper fixture");
        crate::core::check_program(&file).expect("check ordinary Any helper fixture")
    }

    fn function_key(name: &str) -> ManagedAnySourceFunctionKeyV1 {
        ManagedAnySourceFunctionKeyV1 {
            owner: NodeId(name.to_owned()),
            type_arguments: Vec::new(),
        }
    }

    fn callsite(
        caller: &ManagedAnySourceFunctionKeyV1,
        callee: &ManagedAnySourceFunctionKeyV1,
    ) -> ManagedAnySourceCallsiteV1 {
        let key = ManagedAnySourceCallsiteKeyV1 {
            caller: caller.clone(),
            node: NodeId(format!("call:{}->{}", caller.owner.0, callee.owner.0)),
        };
        let result_type = ManagedAnySourceTypeTermV1 {
            source_id: ResolvedTypeId::synthetic("interprocedural-test-i32"),
            head: ManagedAnySourceTypeHeadV1::Primitive(crate::core::ir::PrimitiveType::I32),
            children: Vec::new(),
        };
        ManagedAnySourceCallsiteV1 {
            key,
            callee: callee.clone(),
            caller_source_seal: String::new(),
            callee_source_seal: String::new(),
            arguments: Vec::new(),
            result_type,
            result_paths: Vec::new(),
            effects_seal: String::new(),
            session_seal: String::new(),
            call_permission: None,
            has_effects: false,
            has_session: false,
            complete_source_call_seal: String::new(),
            any_boundary: false,
        }
    }

    #[test]
    fn source_type_term_equality_uses_constructor_and_children_not_intern_id() {
        let left = ManagedAnySourceTypeTermV1 {
            source_id: ResolvedTypeId::synthetic("interned-left"),
            head: ManagedAnySourceTypeHeadV1::Primitive(crate::core::ir::PrimitiveType::I32),
            children: Vec::new(),
        };
        let same_shape = ManagedAnySourceTypeTermV1 {
            source_id: ResolvedTypeId::synthetic("interned-right"),
            head: ManagedAnySourceTypeHeadV1::Primitive(crate::core::ir::PrimitiveType::I32),
            children: Vec::new(),
        };
        let different_shape = ManagedAnySourceTypeTermV1 {
            source_id: ResolvedTypeId::synthetic("interned-right"),
            head: ManagedAnySourceTypeHeadV1::Primitive(crate::core::ir::PrimitiveType::I64),
            children: Vec::new(),
        };

        assert_eq!(left, same_shape);
        assert_ne!(left, different_shape);
    }

    #[test]
    fn source_call_sccs_preserves_recursive_and_acyclic_components() {
        let a = function_key("fn:a");
        let b = function_key("fn:b");
        let c = function_key("fn:c");
        let d = function_key("fn:d");
        let keys = [a.clone(), b.clone(), c.clone(), d.clone()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let callsites = [
            callsite(&a, &b),
            callsite(&b, &a),
            callsite(&c, &c),
            callsite(&d, &a),
        ]
        .into_iter()
        .map(|callsite| (callsite.key.clone(), callsite))
        .collect::<BTreeMap<_, _>>();

        let observed = source_call_sccs(&keys, &callsites)
            .into_iter()
            .map(|component| component.into_iter().collect::<BTreeSet<_>>())
            .collect::<BTreeSet<_>>();
        let expected = [
            [a, b].into_iter().collect::<BTreeSet<_>>(),
            [c].into_iter().collect::<BTreeSet<_>>(),
            [d].into_iter().collect::<BTreeSet<_>>(),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        assert_eq!(observed, expected);
    }

    #[test]
    fn ordinary_any_helper_chain_is_bound_to_checker_source_and_mir_calls() {
        let source = r#"
func leaf(value: Any) -> Any { value }
func relay(value: Any) -> Any { leaf(value) }
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "key", 7)
    let pair = map_get(root, "key")
    drop(empty)
    drop(root)
    let payload = pair.1
    let returned = relay(payload)
    drop(pair)
    drop(payload)
    drop(returned)
    0
}
"#;
        // `Any` is retained only by stdlib declarations.  User-facing
        // functions cannot declare an erased owner after the 0.34 grammar
        // decision; this fixture therefore proves the checker rejection
        // rather than constructing an impossible MIR source plan.
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex ordinary Any helper fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse ordinary Any helper fixture");
        let errors = crate::core::check_program(&file)
            .expect_err("user-facing Any helper declarations must stay rejected");
        assert!(errors
            .iter()
            .all(|error| error.code.as_deref() == Some(crate::diagnostic::codes::E0407)));
    }

    #[test]
    fn ordinary_any_helper_chain_source_plan_rejects_unmaterialized_generic_substitute() {
        let checked = checked_program(
            r#"
func leaf<T>(value: T) -> T { value }
func relay<T>(value: T) -> T { leaf(value) }
func main() -> i32 {
    let empty = map_new()
    let root = map_set(empty, "key", 7)
    let pair = map_get(root, "key")
    drop(empty)
    drop(root)
    let payload = pair.1
    let returned = relay(payload)
    drop(pair)
    drop(payload)
    drop(returned)
    0
}
"#,
        );
        let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect_err("generic helper chains must not inherit the direct Any identity ABI");
        let text = error
            .to_diagnostics()
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect::<Vec<_>>()
            .join(";");
        assert!(
            text.contains("outside scalar contract")
                || text.contains("generic identity contract")
                || text.contains("ManagedAny"),
            "{text}"
        );
    }

    #[test]
    fn recursive_any_helper_scc_stays_unsupported_at_fixedpoint_bottom() {
        let source = r#"
func recursive_left(value: Any) -> Any { recursive_right(value) }
func recursive_right(value: Any) -> Any { recursive_left(value) }
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex recursive Any helper fixture");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse recursive Any helper fixture");
        let errors = crate::core::check_program(&file)
            .expect_err("user-facing Any helper declarations must stay rejected");
        assert!(errors
            .iter()
            .all(|error| error.code.as_deref() == Some(crate::diagnostic::codes::E0407)));
    }

    #[test]
    fn recursive_generic_helper_scc_is_unreached_without_instantiation() {
        let checked = checked_program(
            r#"
func recursive_left<T>(value: T) -> T { recursive_right(value) }
func recursive_right<T>(value: T) -> T { recursive_left(value) }
func main() -> i32 { 0 }
"#,
        );
        let type_catalog = crate::core::mir::types::MirTypeCatalog::from_checked_program(&checked)
            .expect("Checker-derived type catalog for recursive Any helpers");
        let plan = ManagedAnyInterproceduralSourcePlanV1::derive(
            &checked,
            &BTreeMap::new(),
            &type_catalog,
            None,
        )
        .expect("recursive source calls have an exact typed call census");

        let recursive = plan
            .summaries
            .values()
            .filter(|summary| summary.key.owner.0.contains("recursive_"))
            .collect::<Vec<_>>();
        assert!(
            recursive.is_empty(),
            "unreachable generic templates must not be materialized"
        );
        assert!(!plan
            .instance_sccs
            .iter()
            .any(|component| component.len() == 2));
    }
}
