use super::{
    derive_typed_loop_proof_receipt, ContractKind, EffectId, Permission, ResolvedBlock,
    ResolvedBody, ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedLiteral, ResolvedLocalId,
    ResolvedParameterId, ResolvedPatternKind, ResolvedStmtKind, ResolvedTypeId, ResolvedTypeTable,
    TypedLoopFfiCallPath, TypedLoopProofReceipt,
};
use crate::core::{
    cfg::{BasicBlockId, CallableCfg, EdgeId, EdgeKind, Terminator},
    NodeId, Origin, ResourceAnalysis,
};
use std::collections::{BTreeMap, BTreeSet};

fn exact_result_domain_literals(
    expression: &ResolvedExpr,
    result_local: &ResolvedLocalId,
) -> Option<Vec<i64>> {
    match &expression.kind {
        ResolvedExprKind::Binary {
            op: super::ResolvedBinaryOp::LogicalOr,
            left,
            right,
        } => {
            let mut values = exact_result_domain_literals(left, result_local)?;
            values.extend(exact_result_domain_literals(right, result_local)?);
            Some(values)
        }
        ResolvedExprKind::Binary {
            op: super::ResolvedBinaryOp::Equal,
            left,
            right,
        } => {
            let (result, literal) = match (&left.kind, &right.kind) {
                (
                    ResolvedExprKind::Load(place),
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(value)),
                )
                | (
                    ResolvedExprKind::Literal(ResolvedLiteral::Int(value)),
                    ResolvedExprKind::Load(place),
                ) => (place, *value),
                _ => return None,
            };
            (result.base == *result_local && result.projections.is_empty()).then_some(vec![literal])
        }
        _ => None,
    }
}

pub(crate) fn set_get_any_i32_contract_is_direct_result_domain(
    condition: &ResolvedExpr,
    owner: &NodeId,
    query_key: &str,
) -> bool {
    let result_local = ResolvedLocalId(NodeId(format!("{}/contract-result/local", owner.0)));
    let Some(values) = exact_result_domain_literals(condition, &result_local) else {
        return false;
    };
    let observed = values.iter().copied().collect::<BTreeSet<_>>();
    if observed.len() != values.len() {
        return false;
    }
    let hit = query_key == "answer";
    let accepted = if hit {
        [
            BTreeSet::from([42, 2, 3]),
            BTreeSet::from([0, 2, 3]),
            BTreeSet::from([42]),
        ]
    } else if query_key == "absent" {
        [
            BTreeSet::from([0, 2, 3]),
            BTreeSet::from([42, 2, 3]),
            BTreeSet::from([0]),
        ]
    } else {
        return false;
    };
    accepted.iter().any(|domain| domain == &observed)
}

/// Validate the Result-valued GetAny verifier slice's direct postcondition
/// shape without deciding its truth. The caller must be a finite disjunction
/// of distinct `result == status` comparisons drawn from the route's observed
/// i32 outcomes; the verifier then proves or disproves the actual claim.
pub(crate) fn set_get_any_result_i32_contract_is_direct_result_domain(
    condition: &ResolvedExpr,
    owner: &NodeId,
) -> bool {
    let result_local = ResolvedLocalId(NodeId(format!("{}/contract-result/local", owner.0)));
    let Some(values) = exact_result_domain_literals(condition, &result_local) else {
        return false;
    };
    let observed = values.iter().copied().collect::<BTreeSet<_>>();
    !observed.is_empty()
        && observed.len() == values.len()
        && observed
            .iter()
            .all(|value| matches!(*value, 0 | 2 | 3 | 4 | 42))
}

/// Validate the nested fallible New/Set Values route's direct postcondition
/// shape. The route's reachable result domain is the selected payload 41,
/// Set failure 2, or New failure 3. A proper subset remains a well-formed
/// claim for the verifier to prove or disprove; values outside this closed
/// domain are rejected before verification.
pub(crate) fn values_local_index_i32_result_branch_contract_is_direct_result_domain(
    condition: &ResolvedExpr,
    owner: &NodeId,
) -> bool {
    let result_local = ResolvedLocalId(NodeId(format!("{}/contract-result/local", owner.0)));
    let Some(values) = exact_result_domain_literals(condition, &result_local) else {
        return false;
    };
    let observed = values.iter().copied().collect::<BTreeSet<_>>();
    !observed.is_empty()
        && observed.len() == values.len()
        && observed.iter().all(|value| matches!(*value, 2 | 3 | 41))
}

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
        let contracts = collect_proof_roots(&body).contracts;
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

/// Complete typed obligation census, including dormant expression subtrees.
/// A closed NoContracts consumer must inspect this census before trimming its
/// execution graph. Nested named callables retain their separate owner census.
impl ResolvedCallable {
    pub fn has_proof_obligations(&self) -> bool {
        !self.contracts.is_empty() || self.body.has_proof_obligations()
    }

    /// The String Set/Get verifier slice keeps its three calls in the typed
    /// call census, while the Result-match arms do not yet have executable
    /// proof operations.  This narrowly authenticates the source shape whose
    /// unmatched arm calls may be retained as proof-only boundary events.
    /// The MIR consumer must still bind each call to its canonical operation
    /// and preserve the complete owner census before accepting the worklist.
    pub(crate) fn has_exact_string_set_get_match_worklist(
        &self,
        worklist: &TypedProofPointWorklist,
    ) -> bool {
        let Some(receipt) = self
            .resources
            .ordinary_map_owned_any_v2_set_get_string_lineage_receipt
            .as_ref()
        else {
            return false;
        };
        if !worklist.validate_against(self)
            || worklist.owner != receipt.owner
            || self.owner != receipt.owner
            || receipt.proof_ensures.is_none()
            || receipt.proof_ensures_expression.is_none()
            || !matches!(
                self.body.root.result.as_deref(),
                Some(ResolvedExpr {
                    node_id,
                    kind: ResolvedExprKind::Match { .. },
                    ..
                }) if node_id == &receipt.outer_match
            )
        {
            return false;
        }

        let expected_calls = BTreeSet::from([
            receipt.new_operation.clone(),
            receipt.set_operation.clone(),
            receipt.get_operation.clone(),
        ]);
        let actual_calls = worklist
            .calls
            .iter()
            .map(|call| call.call_node.clone())
            .collect::<BTreeSet<_>>();
        if worklist.calls.len() != expected_calls.len()
            || actual_calls != expected_calls
            || !worklist.points.is_empty()
            || !worklist.loop_invariant_points().is_empty()
            || !worklist.deferred_calls.is_empty()
            || worklist.default_roots.iter().any(|root| {
                !root.points.is_empty() || !root.calls.is_empty() || !root.boundaries.is_empty()
            })
        {
            return false;
        }

        let direct_contracts = self
            .body
            .root
            .statements
            .iter()
            .filter(|statement| matches!(&statement.kind, ResolvedStmtKind::Contract { .. }))
            .collect::<Vec<_>>();
        let Some(ensures) = direct_contracts
            .first()
            .copied()
            .filter(|_| direct_contracts.len() == 1)
        else {
            return false;
        };
        let ResolvedStmtKind::Contract {
            kind: ContractKind::Ensures,
            condition,
        } = &ensures.kind
        else {
            return false;
        };
        if receipt.proof_ensures.as_ref() != Some(&ensures.node_id)
            || receipt.proof_ensures_expression.as_ref() != Some(&condition.node_id)
            || !set_get_any_i32_contract_is_direct_result_domain(
                condition,
                &self.owner,
                &receipt.get_key_value,
            )
        {
            return false;
        }

        let allowed_boundary_reasons = [
            "proof root or effect is nested in an unsupported expression form",
            "block result call/effect needs a typed return-value receipt",
        ];
        let boundary_reasons = worklist
            .boundaries
            .iter()
            .filter(|boundary| boundary.point == receipt.outer_match)
            .map(|boundary| boundary.reason.as_str())
            .collect::<BTreeSet<_>>();
        worklist.boundaries.len() <= allowed_boundary_reasons.len()
            && boundary_reasons.len() == worklist.boundaries.len()
            && worklist
                .boundaries
                .iter()
                .all(|boundary| allowed_boundary_reasons.contains(&boundary.reason.as_str()))
            && matches!(
                worklist.root.operations.as_slice(),
                [TypedProofOperation::Call(call)]
                    if call.call_node == receipt.new_operation
            )
    }

    /// The fallible New/Set plus generic GetAny i32-unpack profile has two
    /// Result matches and three exact effect calls. Its Checker worklist may
    /// fully model those effects or retain only the narrowly admitted result
    /// match boundaries; it does not authorize other Result matches or
    /// arbitrary nested effect expressions.
    pub(crate) fn has_exact_set_get_any_i32_match_worklist(
        &self,
        worklist: &TypedProofPointWorklist,
    ) -> bool {
        let Some(receipt) = self
            .resources
            .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
            .as_ref()
        else {
            return false;
        };
        if !worklist.validate_against(self)
            || worklist.owner != receipt.owner
            || self.owner != receipt.owner
            || !matches!(
                self.body.root.result.as_deref(),
                Some(ResolvedExpr {
                    node_id,
                    kind: ResolvedExprKind::Match { .. },
                    ..
                }) if node_id == &receipt.outer_match
            )
        {
            return false;
        }
        let expected_calls = BTreeSet::from([
            receipt.new_operation.clone(),
            receipt.set_operation.clone(),
            receipt.get_operation.clone(),
        ]);
        let actual_calls = worklist
            .calls
            .iter()
            .map(|call| call.call_node.clone())
            .collect::<BTreeSet<_>>();
        if worklist.calls.len() != expected_calls.len()
            || actual_calls != expected_calls
            || !worklist.points.is_empty()
            || !worklist.loop_invariant_points().is_empty()
            || !worklist.deferred_calls.is_empty()
            || worklist.default_roots.iter().any(|root| {
                !root.points.is_empty() || !root.calls.is_empty() || !root.boundaries.is_empty()
            })
        {
            return false;
        }
        let contracts = self
            .body
            .root
            .statements
            .iter()
            .filter(|statement| matches!(&statement.kind, ResolvedStmtKind::Contract { .. }))
            .collect::<Vec<_>>();
        if contracts.len() != 1
            || !matches!(
                contracts[0].kind,
                ResolvedStmtKind::Contract {
                    kind: ContractKind::Ensures,
                    ..
                }
            )
        {
            return false;
        }
        let allowed_boundary_reasons = [
            "proof root or effect is nested in an unsupported expression form",
            "block result call/effect needs a typed return-value receipt",
        ];
        worklist.boundaries.len() <= allowed_boundary_reasons.len() * 2
            && worklist.boundaries.iter().all(|boundary| {
                (boundary.point == receipt.outer_match || boundary.point == receipt.set_match)
                    && allowed_boundary_reasons.contains(&boundary.reason.as_str())
            })
    }

    /// The Result-valued `map_get_any` slice may retain only the exact three
    /// fallible Map call points and the three source Result-match boundaries
    /// named by its independent Checker receipt. This does not reuse the
    /// tuple-valued GetAny receipt or authorize any additional Map call.
    pub(crate) fn has_exact_set_get_any_result_i32_match_worklist(
        &self,
        worklist: &TypedProofPointWorklist,
    ) -> bool {
        let Some(receipt) = self
            .resources
            .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt
            .as_ref()
        else {
            return false;
        };
        if !worklist.validate_against(self)
            || worklist.owner != receipt.owner
            || self.owner != receipt.owner
            || receipt.profile_version
                != crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageProfileVersion::V1
            || receipt.get_api != crate::core::ownership::OrdinaryMapOwnedAnyV2GetApi::MapGetAnyResult
            || self
                .resources
                .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
                .is_some()
            || receipt.new_operation == receipt.set_operation
            || receipt.new_operation == receipt.get_operation
            || receipt.set_operation == receipt.get_operation
            || receipt.operations
                != BTreeSet::from([
                    crate::core::ownership::OrdinaryMapOwnedAnyV2Attempt {
                        kind: crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::New,
                        point: receipt.new_operation.clone(),
                        arguments: Vec::new(),
                        receiver_local: None,
                        result_local: Some(receipt.new_result_local.clone()),
                    },
                    crate::core::ownership::OrdinaryMapOwnedAnyV2Attempt {
                        kind: crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::Set,
                        point: receipt.set_operation.clone(),
                        arguments: vec![
                            receipt.set_receiver.clone(),
                            receipt.set_key.clone(),
                            receipt.set_value.clone(),
                        ],
                        receiver_local: Some(receipt.initial_root_local.clone()),
                        result_local: Some(receipt.set_result_local.clone()),
                    },
                    crate::core::ownership::OrdinaryMapOwnedAnyV2Attempt {
                        kind: crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::GetAny,
                        point: receipt.get_operation.clone(),
                        arguments: vec![
                            receipt.get_receiver.clone(),
                            receipt.get_key.clone(),
                        ],
                        receiver_local: Some(receipt.updated_root_local.clone()),
                        result_local: Some(receipt.get_result_local.clone()),
                    },
                ])
            || !matches!(
                self.body.root.result.as_deref(),
                Some(ResolvedExpr {
                    node_id,
                    kind: ResolvedExprKind::Match { .. },
                    ..
                }) if node_id == &receipt.new_match
            )
        {
            return false;
        }

        let expected_calls = BTreeSet::from([
            receipt.new_operation.clone(),
            receipt.set_operation.clone(),
            receipt.get_operation.clone(),
        ]);
        let actual_calls = worklist
            .calls
            .iter()
            .map(|call| call.call_node.clone())
            .collect::<BTreeSet<_>>();
        if worklist.calls.len() != 3
            || expected_calls.len() != 3
            || actual_calls != expected_calls
            || actual_calls.len() != 3
            || !worklist.points.is_empty()
            || !worklist.loop_invariant_points().is_empty()
            || !worklist.deferred_calls.is_empty()
            || worklist.default_roots.iter().any(|root| {
                !root.points.is_empty() || !root.calls.is_empty() || !root.boundaries.is_empty()
            })
        {
            return false;
        }

        let direct_contracts = self
            .body
            .root
            .statements
            .iter()
            .filter(|statement| matches!(&statement.kind, ResolvedStmtKind::Contract { .. }))
            .collect::<Vec<_>>();
        let [ensures] = direct_contracts.as_slice() else {
            return false;
        };
        let ResolvedStmtKind::Contract {
            kind: ContractKind::Ensures,
            condition,
        } = &ensures.kind
        else {
            return false;
        };
        if receipt.proof_ensures.as_ref() != Some(&ensures.node_id)
            || receipt.proof_ensures_expression.as_ref() != Some(&condition.node_id)
            || !set_get_any_result_i32_contract_is_direct_result_domain(condition, &self.owner)
        {
            return false;
        }

        let allowed_points = BTreeSet::from([
            receipt.new_match.clone(),
            receipt.set_match.clone(),
            receipt.get_match.clone(),
        ]);
        let allowed_reasons = [
            "proof root or effect is nested in an unsupported expression form",
            "block result call/effect needs a typed return-value receipt",
        ];
        let retained = worklist
            .boundaries
            .iter()
            .map(|boundary| (boundary.point.clone(), boundary.reason.clone()))
            .collect::<BTreeSet<_>>();
        retained.len() == worklist.boundaries.len()
            && retained.len() <= allowed_points.len() * allowed_reasons.len()
            && worklist.boundaries.iter().all(|boundary| {
                allowed_points.contains(&boundary.point)
                    && allowed_reasons.contains(&boundary.reason.as_str())
            })
    }

    /// These roots are a proof census, not unconditional function assumptions.
    /// Nested contracts and math are supported only when the typed worklist
    /// gives them an ordered program point and lexical control path.
    pub fn has_unmodeled_proof_roots(&self) -> bool {
        let roots = collect_proof_roots(&self.body);
        if roots.contracts.is_empty() && !roots.has_math {
            return false;
        }
        let worklist = self.proof_point_worklist();
        let exact_new_result_boundary = self
            .resources
            .ordinary_map_owned_any_v2_new_result_receipt
            .as_ref()
            .is_some_and(|receipt| {
                matches!(
                    worklist.boundaries.as_slice(),
                    [boundary]
                        if boundary.point == receipt.match_node
                            && boundary.reason
                                == "proof root or effect is nested in an unsupported expression form"
                )
            });
        let exact_string_set_get_match_worklist =
            self.has_exact_string_set_get_match_worklist(&worklist);
        let exact_set_get_any_result_i32_match_worklist =
            self.has_exact_set_get_any_result_i32_match_worklist(&worklist);
        let direct = self
            .body
            .root
            .statements
            .iter()
            .filter(|statement| {
                matches!(
                    statement.kind,
                    ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_)
                )
            })
            .map(|statement| &statement.node_id)
            .collect::<BTreeSet<_>>();
        let loop_invariants = worklist
            .loop_invariant_nodes()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let math_points = worklist
            .points
            .iter()
            .map(|point| point.id.statement.clone())
            .collect::<BTreeSet<_>>();
        let default_math_points = worklist
            .default_roots
            .iter()
            .filter(|root| root.supports_local_only_profile(self))
            .flat_map(|root| root.points.iter().map(|point| point.id.statement.clone()))
            .collect::<BTreeSet<_>>();
        roots.contracts.iter().any(|contract| {
            !direct.contains(&contract.node_id) && !loop_invariants.contains(&contract.node_id)
        }) || roots
            .math_nodes
            .iter()
            .any(|node| !math_points.contains(node) && !default_math_points.contains(node))
            || (!worklist.boundaries.is_empty()
                && !exact_new_result_boundary
                && !exact_string_set_get_match_worklist
                && !exact_set_get_any_result_i32_match_worklist)
            || worklist.default_roots.iter().any(|root| {
                !root.boundaries.is_empty()
                    || (!root.points.is_empty() && !root.supports_local_only_profile(self))
            })
    }

    /// Build a checker-owned, ordered proof worklist.  This is deliberately a
    /// separate representation from the flattened `contracts` census: a
    /// point contains its statement order and structured path, so nested
    /// Requires can never acquire callable-entry scope by accident.
    pub fn proof_point_worklist(&self) -> TypedProofPointWorklist {
        let root_contracts = self
            .body
            .root
            .statements
            .iter()
            .filter(|statement| matches!(&statement.kind, ResolvedStmtKind::Contract { .. }))
            .map(|statement| statement.node_id.clone())
            .collect::<BTreeSet<_>>();
        let roots = collect_proof_roots(&self.body);
        let proof_requested = roots.has_math
            || roots
                .contracts
                .iter()
                .any(|contract| !root_contracts.contains(&contract.node_id));
        let mut builder = ProofPointBuilder {
            callable: self,
            owner: &self.owner,
            body_fingerprint: resolved_body_fingerprint(&self.body),
            boundaries: Vec::new(),
            points: Vec::new(),
            calls: Vec::new(),
            match_path: Vec::new(),
            scope: Vec::new(),
            guards: Vec::new(),
            active_loop_call_paths: Vec::new(),
            next_order: 0,
            root_contracts,
            proof_requested,
        };
        // Place-input calls are sequenced by the statement that consumes the
        // place, not by declaration/body traversal order. Census them in a
        // separate builder below, outside the executable body timeline.
        let root = builder.block(&self.body.root, &BTreeSet::new());

        let mut place_builder = ProofPointBuilder {
            callable: self,
            owner: &self.owner,
            body_fingerprint: resolved_body_fingerprint(&self.body),
            boundaries: Vec::new(),
            points: Vec::new(),
            calls: Vec::new(),
            match_path: Vec::new(),
            scope: Vec::new(),
            guards: Vec::new(),
            active_loop_call_paths: Vec::new(),
            next_order: 0,
            root_contracts: BTreeSet::new(),
            proof_requested: false,
        };
        for (input_node, value) in &self.body.place_inputs {
            place_builder
                .scope
                .push(TypedProofScopeFrame::PlaceInput(input_node.clone()));
            place_builder.expression_calls(
                &self.owner,
                input_node,
                value,
                Some("dynamic place input has no statement evaluation receipt"),
            );
            place_builder.scope.pop();
        }
        let default_roots = self.typed_default_proof_roots();
        let body_calls = builder.calls;
        let deferred_calls = place_builder.calls;
        let all_call_points = body_calls
            .iter()
            .chain(&deferred_calls)
            .chain(default_roots.iter().flat_map(|root| root.calls.iter()));
        let mut default_uses = Vec::new();
        for point in all_call_points {
            let ResolvedExprKind::Call(call) = &point.expression.kind else {
                continue;
            };
            for (parameter_index, argument) in call.arguments.iter().enumerate() {
                let ResolvedExprKind::DefaultArgument {
                    callable,
                    parameter,
                } = &argument.value.kind
                else {
                    continue;
                };
                if let Some(usage) = TypedDefaultProofUse::from_call_argument(
                    point,
                    callable,
                    parameter,
                    &argument.value.node_id,
                    parameter_index,
                ) {
                    default_uses.push(usage);
                }
            }
        }
        let mut worklist = TypedProofPointWorklist {
            owner: self.owner.clone(),
            root,
            points: builder.points,
            calls: body_calls,
            default_roots,
            default_uses,
            deferred_calls,
            boundaries: builder.boundaries,
            source_body_fingerprint: resolved_body_fingerprint(&self.body),
            private_witness: blake3::hash(b"unsealed-typed-proof-worklist"),
        };
        worklist.private_witness = worklist.compute_witness();
        worklist
    }

    /// Checker-owned per-parameter default proof programs. They are separate
    /// from `body.root`; a caller may consume one only through an exact
    /// `DefaultArgument` call-site receipt.
    pub fn typed_default_proof_roots(&self) -> Vec<TypedDefaultProofRoot> {
        self.body
            .default_values
            .keys()
            .filter_map(|parameter| self.typed_default_proof_root(parameter))
            .collect()
    }

    pub fn typed_default_proof_root(
        &self,
        parameter: &ResolvedParameterId,
    ) -> Option<TypedDefaultProofRoot> {
        let expression = self.body.default_values.get(parameter)?;
        let mut builder = ProofPointBuilder {
            callable: self,
            owner: &self.owner,
            body_fingerprint: resolved_body_fingerprint(&self.body),
            boundaries: Vec::new(),
            points: Vec::new(),
            calls: Vec::new(),
            match_path: Vec::new(),
            scope: vec![TypedProofScopeFrame::Default {
                parameter: parameter.clone(),
                expression: expression.node_id.clone(),
            }],
            guards: Vec::new(),
            active_loop_call_paths: Vec::new(),
            next_order: 0,
            root_contracts: BTreeSet::new(),
            proof_requested: true,
        };
        let mut root = TypedProofBlock {
            id: expression.node_id.clone(),
            operations: Vec::new(),
        };
        if let Some(operation) = builder.expression(expression, &expression.node_id) {
            root.operations.push(operation);
        } else if proof_expression_is_pure(expression) {
            root.operations.push(TypedProofOperation::Eval {
                point: expression.node_id.clone(),
                expression: expression.clone(),
            });
        }
        if !builder.calls.is_empty() {
            builder.defer(
                &expression.node_id,
                "default expression call requires its exact omitted-argument expansion/effect receipt",
            );
        }
        let mut proof_root = TypedDefaultProofRoot {
            owner: self.owner.clone(),
            parameter: parameter.clone(),
            expression: expression.node_id.clone(),
            root,
            points: builder.points,
            calls: builder.calls,
            boundaries: builder.boundaries,
            source_body_fingerprint: resolved_body_fingerprint(&self.body),
            private_witness: blake3::hash(b"unsealed-typed-default-proof-root"),
        };
        proof_root.private_witness = proof_root.compute_witness();
        Some(proof_root)
    }
}

/// Stable identity for one individual expression in a `math: { ... }`
/// statement.  The statement NodeId alone is insufficient because a math
/// block may contain several ordered claims.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TypedProofPointId {
    pub owner: NodeId,
    pub statement: NodeId,
    pub ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypedProofScopeFrame {
    Block(NodeId),
    Branch {
        /// Enclosing resolved statement used by the worklist event timeline.
        statement: NodeId,
        /// Exact source `if` expression used by the checker CFG and loop-edge
        /// guards. These identities can differ for an expression nested in a
        /// binding, argument, return value, or block tail.
        expression: NodeId,
        then_arm: bool,
    },
    Loop(NodeId),
    Default {
        parameter: ResolvedParameterId,
        expression: NodeId,
    },
    PlaceInput(NodeId),
}

/// One arm in the checker-authenticated source selection for a narrow scalar
/// integer match.  The complete ordered arm vector is carried with each
/// selected literal arm so MIR consumers can compare the entire dispatch,
/// including the final wildcard, instead of trusting one matching edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedMatchArmSelection {
    pub arm: NodeId,
    pub cfg_edge: EdgeId,
    pub cfg_target: BasicBlockId,
    pub case: TypedMatchCase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedMatchCase {
    Integer(i64),
    Wildcard,
}

/// Source/CFG receipt for one exact literal match arm containing a call.
/// This carries no solver fact: the verifier must still execute the matching
/// MIR `Switch` and derive the literal equality from that selected edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedMatchPathReceipt {
    /// Exact call expression for which the Checker issued this selection
    /// receipt. The same match arm may contain several calls, so an arm-only
    /// receipt must never be reusable at a neighboring callsite.
    pub call_node: Option<NodeId>,
    pub match_expression: NodeId,
    pub scrutinee_expression: NodeId,
    pub scrutinee_local: super::ResolvedLocalId,
    pub scrutinee_type: ResolvedTypeId,
    pub cfg_dispatch: BasicBlockId,
    pub arms: Vec<TypedMatchArmSelection>,
    pub selected_arm: NodeId,
    /// Commitment to the identical ownership ledger at the dispatch exit and
    /// every arm-entry edge.  A path receipt is withheld if selection itself
    /// changes ownership state or performs an edge-associated action.
    pub ownership_state_fingerprint: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct TypedMathProofPoint {
    pub id: TypedProofPointId,
    /// Exact source span of the enclosing `math: { ... }` statement.
    pub statement_span: crate::span::Span,
    /// Monotone expression/statement evaluation event order within `owner`.
    /// Call points share this counter, so consumers can split proven facts at
    /// the exact call boundary instead of treating a body as one environment.
    pub order: u64,
    pub scope: Vec<TypedProofScopeFrame>,
    pub condition: ResolvedExpr,
}

/// One checker-lowered call at a specific owner-body evaluation point.
/// `expression` is the source of truth: its Call variant contains the selected
/// callee, canonical ordered actuals, conversions, result type and effects.
/// Consumers must bind this exact value back to the owner's ResolvedBody
/// before using it; the NodeIds alone are not an authorization receipt.
#[derive(Debug, Clone)]
pub struct TypedCallProofPoint {
    /// Owning callable for the worklist that contains this event. This differs
    /// from `owner` only for a nested lambda body, whose calls are always
    /// Deferred until that closure has a checked execution receipt.
    pub body_owner: NodeId,
    pub owner: NodeId,
    pub statement: NodeId,
    pub call_node: NodeId,
    /// Shared monotone event order with `TypedMathProofPoint::order`.
    pub order: u64,
    pub scope: Vec<TypedProofScopeFrame>,
    /// Boolean path conditions for `if` arms and short-circuit RHS terms.
    /// `Deferred` points retain their guard/census but cannot authorize proof.
    pub guards: Vec<TypedCallGuard>,
    /// Exact nested scalar Match selections enclosing this call. Wildcard
    /// calls and unsupported patterns deliberately carry no selection receipt
    /// and remain Deferred.
    pub match_path: Vec<TypedMatchPathReceipt>,
    pub disposition: TypedCallExecutionDisposition,
    pub expression: ResolvedExpr,
    /// Every call defaults to unknown host/state mutation. An empty source
    /// effect vector is not evidence of purity or no-write behavior.
    pub effect: TypedCallEffectReceipt,
    source_body_fingerprint: String,
    private_witness: blake3::Hash,
}

#[derive(Debug, Clone)]
pub struct TypedCallGuard {
    pub condition: ResolvedExpr,
    pub when: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedCallExecutionDisposition {
    Unconditional,
    Guarded,
    Deferred(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedCallEffectReceipt {
    pub call_node: NodeId,
    pub callee: ResolvedCallee,
    pub declared_effects: Vec<EffectId>,
    pub authority: TypedCallEffectAuthority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedCallEffectAuthority {
    /// No checked read/write summary is currently attached to this call.
    /// Consumers must invalidate every mutable/captured state term they may
    /// observe after the call. This is never a no-write certificate.
    UnknownMayReadWrite,
}

#[derive(Debug, Clone)]
pub struct TypedLoopInvariantPoint {
    pub id: TypedProofPointId,
    pub scope: Vec<TypedProofScopeFrame>,
    pub condition: ResolvedExpr,
}

/// A structured, ordered proof plan shared as input by the independent
/// Resolved and MIR encoders.  The solvers and expression encoders remain
/// independent; this value carries only checker-owned identities, typed
/// expressions, and lexical control-flow structure.
#[derive(Debug, Clone)]
pub struct TypedProofPointWorklist {
    pub owner: NodeId,
    pub root: TypedProofBlock,
    pub points: Vec<TypedMathProofPoint>,
    /// Complete lexical census for calls in this body. Ordered executable
    /// body calls can be consumed alongside Math points; calls in declaration
    /// defaults, place inputs, proof-only terms, latent lambdas or unsupported
    /// repeated/control contexts remain present with `Deferred` disposition.
    pub calls: Vec<TypedCallProofPoint>,
    /// Proof programs for declaration-owned default expressions. Their facts
    /// are isolated from the callable body and become executable only through
    /// an exact `TypedDefaultProofUse` at an omitted-argument call site.
    pub default_roots: Vec<TypedDefaultProofRoot>,
    pub default_uses: Vec<TypedDefaultProofUse>,
    /// Calls in checker-owned place-input expressions do not yet have a
    /// statement evaluation receipt. They remain in this exact census.
    pub deferred_calls: Vec<TypedCallProofPoint>,
    pub boundaries: Vec<TypedProofBoundary>,
    source_body_fingerprint: String,
    private_witness: blake3::Hash,
}

#[derive(Debug, Clone)]
pub enum TypedProofOrderedEvent {
    Math(TypedMathProofPoint),
    Call(TypedCallProofPoint),
}

#[derive(Debug, Clone)]
pub struct TypedProofBoundary {
    pub point: NodeId,
    pub reason: String,
}

/// Isolated proof worklist for one exact parameter default expression.
/// It carries no callable Requires assumptions and cannot contribute facts to
/// the target body after the argument has been evaluated.
#[derive(Debug, Clone)]
pub struct TypedDefaultProofRoot {
    pub owner: NodeId,
    pub parameter: ResolvedParameterId,
    pub expression: NodeId,
    pub root: TypedProofBlock,
    pub points: Vec<TypedMathProofPoint>,
    pub calls: Vec<TypedCallProofPoint>,
    pub boundaries: Vec<TypedProofBoundary>,
    source_body_fingerprint: String,
    private_witness: blake3::Hash,
}

/// Checker-issued link from an actual canonical DefaultArgument slot back to
/// the exact target declaration default. A root NodeId alone cannot prove that
/// the source call omitted the corresponding argument.
#[derive(Debug, Clone)]
pub struct TypedDefaultProofUse {
    pub caller_owner: NodeId,
    pub call_node: NodeId,
    pub callee_owner: NodeId,
    pub parameter: ResolvedParameterId,
    pub argument_node: NodeId,
    pub parameter_index: usize,
    pub order: u64,
    pub scope: Vec<TypedProofScopeFrame>,
    pub guards: Vec<TypedCallGuard>,
    pub disposition: TypedCallExecutionDisposition,
    call_point_witness: blake3::Hash,
    source_body_fingerprint: String,
    private_witness: blake3::Hash,
}

#[derive(Debug, Clone)]
pub struct TypedProofBlock {
    pub id: NodeId,
    pub operations: Vec<TypedProofOperation>,
}

#[derive(Debug, Clone)]
pub enum TypedProofOperation {
    Bind {
        point: NodeId,
        local: ResolvedLocalId,
        initializer: ResolvedExpr,
    },
    /// A checker-censused call boundary, including explicit Deferred events.
    /// Consumers may discharge Requires/apply effects only for a validated,
    /// executable disposition; UnknownMayReadWrite invalidates mutable state
    /// after the call. Deferred entries preserve source order without
    /// authorizing any proof.
    Call(TypedCallProofPoint),
    /// Evaluate a checked value whose result is unused at this point. Keeping
    /// it explicit lets consumers discharge overflow, division and other
    /// definedness obligations before a later assertion or loop edge.
    Eval {
        point: NodeId,
        expression: ResolvedExpr,
    },
    Assign {
        point: NodeId,
        target: super::ResolvedPlace,
        value: ResolvedExpr,
        conversion: super::CheckedConversion,
    },
    Math(TypedMathProofPoint),
    Branch {
        point: NodeId,
        /// Exact source if-expression NodeId corresponding to CFG guard IDs.
        expression: NodeId,
        condition: ResolvedExpr,
        then_block: TypedProofBlock,
        else_block: TypedProofBlock,
    },
    Scope(TypedProofBlock),
    Loop {
        point: NodeId,
        condition: ResolvedExpr,
        /// Ordered calls evaluated while computing the repeated loop header
        /// condition. They may be used only when each has a validated loop
        /// receipt and executable disposition; currently unknown loop effects
        /// remain Deferred.
        condition_calls: Vec<TypedCallProofPoint>,
        invariants: Vec<TypedLoopInvariantPoint>,
        body: TypedProofBlock,
        stationary: bool,
        /// Exact checker-derived CFG/resource/source receipt. Consumers must
        /// rederive it and prove each invariant; `None` is a fail-closed
        /// boundary, never authorization to assume an invariant.
        receipt: Option<TypedLoopProofReceipt>,
    },
    Break {
        point: NodeId,
        target_loop: Option<NodeId>,
        value: Option<ResolvedExpr>,
    },
    Continue {
        point: NodeId,
        target_loop: Option<NodeId>,
    },
    Return {
        point: NodeId,
        value: Option<ResolvedExpr>,
        conversion: Option<super::CheckedConversion>,
    },
    /// Stops only the current lexical path. The enclosing branch/loop
    /// interpreter decides whether later sibling work remains reachable.
    Exit,
}

impl TypedProofPointWorklist {
    /// Validate this public worklist against its exact checker-owned callable.
    /// Re-derivation protects ordering, guards, source call contents and the
    /// complete census; private seals prevent edited/cloned payloads from
    /// retaining authority merely because a NodeId still matches.
    pub fn validate_against(&self, callable: &ResolvedCallable) -> bool {
        if self.owner != callable.owner
            || self.private_witness != self.compute_witness()
            || self.source_body_fingerprint != resolved_body_fingerprint(&callable.body)
            || self
                .calls
                .iter()
                .any(|point| !point.validate_against(callable))
            || self
                .deferred_calls
                .iter()
                .any(|point| !point.validate_against(callable))
            || self
                .default_roots
                .iter()
                .any(|root| !root.validate_against(callable))
        {
            return false;
        }
        callable.proof_point_worklist().private_witness == self.private_witness
    }

    /// Validate in the exact CheckedProgram that also owns the typed FFI
    /// declaration directory. Consumers that combine call points with FFI
    /// descriptors must bind both inputs to the same checked program.
    pub fn validate_in_checked_program(&self, program: &crate::core::CheckedProgram) -> bool {
        program
            .callable(&self.owner)
            .is_some_and(|callable| self.validate_against(callable))
    }

    fn compute_witness(&self) -> blake3::Hash {
        let material = format!(
            "typed-proof-worklist-v4\nowner={:?}\nbody={}\nroot={:#?}\npoints={:#?}\ncalls={:#?}\ndefault_roots={:#?}\ndefault_uses={:#?}\ndeferred_calls={:#?}\nboundaries={:#?}",
            self.owner,
            self.source_body_fingerprint,
            self.root,
            self.points,
            self.calls,
            self.default_roots,
            self.default_uses,
            self.deferred_calls,
            self.boundaries
        );
        blake3::hash(material.as_bytes())
    }

    /// Return proof roots and call boundaries in the checker's exact
    /// expression/statement event order. Structured scope and call guards
    /// stay attached to each event; this flat ordering is not permission to
    /// merge mutually exclusive paths.
    pub fn ordered_events(&self) -> Vec<TypedProofOrderedEvent> {
        let mut events = self
            .points
            .iter()
            .cloned()
            .map(TypedProofOrderedEvent::Math)
            .chain(self.calls.iter().cloned().map(TypedProofOrderedEvent::Call))
            .collect::<Vec<_>>();
        events.sort_by_key(|event| match event {
            TypedProofOrderedEvent::Math(point) => point.order,
            TypedProofOrderedEvent::Call(point) => point.order,
        });
        events
    }

    pub fn call_nodes(&self) -> Vec<NodeId> {
        self.calls
            .iter()
            .chain(&self.deferred_calls)
            .chain(self.default_roots.iter().flat_map(|root| root.calls.iter()))
            .map(|point| point.call_node.clone())
            .collect()
    }

    /// Calls that have a modeled, once-only point in this execution stream.
    /// Deferred calls remain in `calls` and `call_nodes()` for complete census.
    pub fn executable_calls(&self) -> impl Iterator<Item = &TypedCallProofPoint> {
        self.calls.iter().filter(|point| {
            matches!(
                &point.disposition,
                TypedCallExecutionDisposition::Unconditional
                    | TypedCallExecutionDisposition::Guarded
            )
        })
    }

    pub fn math_nodes(&self) -> Vec<NodeId> {
        self.points
            .iter()
            .chain(
                self.default_roots
                    .iter()
                    .flat_map(|root| root.points.iter()),
            )
            .map(|point| point.id.statement.clone())
            .collect()
    }

    pub fn loop_invariant_nodes(&self) -> Vec<NodeId> {
        self.loop_invariant_points()
            .into_iter()
            .map(|point| point.id.statement)
            .collect()
    }

    /// Checker-owned invariant census, kept independent of the later MIR
    /// lowering so its expected identities can validate the emitted receipt.
    pub fn loop_invariant_points(&self) -> Vec<TypedLoopInvariantPoint> {
        fn walk(block: &TypedProofBlock, out: &mut Vec<TypedLoopInvariantPoint>) {
            for operation in &block.operations {
                match operation {
                    TypedProofOperation::Branch {
                        then_block,
                        else_block,
                        ..
                    } => {
                        walk(then_block, out);
                        walk(else_block, out);
                    }
                    TypedProofOperation::Scope(block) => walk(block, out),
                    TypedProofOperation::Loop {
                        invariants, body, ..
                    } => {
                        out.extend(invariants.iter().cloned());
                        walk(body, out);
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    pub fn loop_nodes(&self) -> Vec<NodeId> {
        fn walk(block: &TypedProofBlock, out: &mut Vec<NodeId>) {
            for operation in &block.operations {
                match operation {
                    TypedProofOperation::Branch {
                        then_block,
                        else_block,
                        ..
                    } => {
                        walk(then_block, out);
                        walk(else_block, out);
                    }
                    TypedProofOperation::Scope(inner) => walk(inner, out),
                    TypedProofOperation::Loop { point, body, .. } => {
                        out.push(point.clone());
                        walk(body, out);
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }
}

impl TypedCallProofPoint {
    /// Revalidate a call event against the owning checker body before a
    /// consumer uses its precondition or effect. The private witness binds
    /// every public field, including canonical actual terms and event order.
    pub fn validate_against(&self, callable: &ResolvedCallable) -> bool {
        self.body_owner == callable.owner
            && self.source_body_fingerprint == resolved_body_fingerprint(&callable.body)
            && self.call_node == self.expression.node_id
            && matches!(&self.expression.kind, ResolvedExprKind::Call(_))
            && self.effect.call_node == self.call_node
            && self.private_witness == self.compute_witness()
    }

    /// Require the exact event to occur in this CheckedProgram's complete,
    /// rederived call census. This keeps a same-NodeId body/descriptor pair
    /// from another checked program from authorizing proof or effects.
    pub fn validate_in_checked_program(&self, program: &crate::core::CheckedProgram) -> bool {
        let Some(callable) = program.callable(&self.body_owner) else {
            return false;
        };
        let worklist = callable.proof_point_worklist();
        worklist.validate_against(callable)
            && worklist
                .calls
                .iter()
                .chain(&worklist.deferred_calls)
                .chain(
                    worklist
                        .default_roots
                        .iter()
                        .flat_map(|root| root.calls.iter()),
                )
                .any(|candidate| candidate.private_witness == self.private_witness)
    }

    fn compute_witness(&self) -> blake3::Hash {
        let material = format!(
            "typed-call-proof-point-v3\nbody_owner={:?}\nowner={:?}\nstatement={:?}\ncall_node={:?}\norder={}\nscope={:?}\nguards={:#?}\nmatch_path={:#?}\ndisposition={:?}\nexpression={:#?}\neffect={:#?}\nbody={}",
            self.body_owner,
            self.owner,
            self.statement,
            self.call_node,
            self.order,
            self.scope,
            self.guards,
            self.match_path,
            self.disposition,
            self.expression,
            self.effect,
            self.source_body_fingerprint,
        );
        blake3::hash(material.as_bytes())
    }
}

impl TypedDefaultProofRoot {
    pub fn validate_against(&self, callable: &ResolvedCallable) -> bool {
        if self.owner != callable.owner
            || self.source_body_fingerprint != resolved_body_fingerprint(&callable.body)
            || callable
                .body
                .default_values
                .get(&self.parameter)
                .is_none_or(|expression| expression.node_id != self.expression)
            || self.private_witness != self.compute_witness()
        {
            return false;
        }
        callable
            .typed_default_proof_root(&self.parameter)
            .is_some_and(|expected| expected.private_witness == self.private_witness)
    }

    pub fn validate_in_checked_program(&self, program: &crate::core::CheckedProgram) -> bool {
        program
            .callable(&self.owner)
            .is_some_and(|callable| self.validate_against(callable))
    }

    /// Conservative first executable profile: straight-line scalar Math over
    /// immutable locals bound from literals in this same default expression.
    /// Callers provide no Requires facts and values never escape to the body.
    pub fn supports_local_only_profile(&self, callable: &ResolvedCallable) -> bool {
        if !self.validate_against(callable) || !self.boundaries.is_empty() || !self.calls.is_empty()
        {
            return false;
        }
        let mut bound = BTreeSet::new();
        default_block_is_local_only(&self.root, &callable.body, &mut bound)
    }

    fn compute_witness(&self) -> blake3::Hash {
        let material = format!(
            "typed-default-proof-root-v1\nowner={:?}\nparameter={:?}\nexpression={:?}\nbody={}\nroot={:#?}\npoints={:#?}\ncalls={:#?}\nboundaries={:#?}",
            self.owner,
            self.parameter,
            self.expression,
            self.source_body_fingerprint,
            self.root,
            self.points,
            self.calls,
            self.boundaries,
        );
        blake3::hash(material.as_bytes())
    }
}

impl TypedDefaultProofUse {
    fn from_call_argument(
        point: &TypedCallProofPoint,
        callee_owner: &NodeId,
        parameter: &ResolvedParameterId,
        argument_node: &NodeId,
        parameter_index: usize,
    ) -> Option<Self> {
        let mut usage = Self {
            caller_owner: point.body_owner.clone(),
            call_node: point.call_node.clone(),
            callee_owner: callee_owner.clone(),
            parameter: parameter.clone(),
            argument_node: argument_node.clone(),
            parameter_index,
            order: point.order,
            scope: point.scope.clone(),
            guards: point.guards.clone(),
            disposition: point.disposition.clone(),
            call_point_witness: point.private_witness,
            source_body_fingerprint: point.source_body_fingerprint.clone(),
            private_witness: blake3::hash(b"unsealed-typed-default-proof-use"),
        };
        usage.private_witness = usage.compute_witness();
        Some(usage)
    }

    /// Authenticate both the caller's omitted-argument marker and the target's
    /// exact declaration default. A same-name call, matching NodeId or raw
    /// argc cannot substitute for the canonical DefaultArgument slot.
    pub fn validate_against(&self, caller: &ResolvedCallable, target: &ResolvedCallable) -> bool {
        if self.private_witness != self.compute_witness()
            || self.caller_owner != caller.owner
            || self.source_body_fingerprint != resolved_body_fingerprint(&caller.body)
            || self.callee_owner != target.owner
            || target.signature.owner != target.owner
        {
            return false;
        }
        let caller_worklist = caller.proof_point_worklist();
        let Some(point) = caller_worklist
            .calls
            .iter()
            .chain(&caller_worklist.deferred_calls)
            .chain(
                caller_worklist
                    .default_roots
                    .iter()
                    .flat_map(|root| root.calls.iter()),
            )
            .find(|point| point.private_witness == self.call_point_witness)
        else {
            return false;
        };
        if !self.validate_for_point(caller, point) {
            return false;
        }
        let ResolvedExprKind::Call(call) = &point.expression.kind else {
            return false;
        };
        if call.callee != ResolvedCallee::Function(target.owner.clone()) {
            return false;
        }
        let Some(argument) = call.arguments.get(self.parameter_index) else {
            return false;
        };
        let Some(parameter) = target.signature.parameters.get(self.parameter_index) else {
            return false;
        };
        let ResolvedExprKind::DefaultArgument {
            callable,
            parameter: marker_parameter,
        } = &argument.value.kind
        else {
            return false;
        };
        let marker_origin_matches = matches!(
            &argument.value.origin,
            crate::core::Origin::Desugared { parent, rule, .. }
                if parent == &point.call_node && rule == "resolved_body.default_argument"
        );
        callable == &target.owner
            && marker_parameter == &self.parameter
            && parameter.id == self.parameter
            && argument.parameter == self.parameter
            && argument.value.node_id == self.argument_node
            && argument.value.ty == parameter.ty
            && argument.conversion.from == argument.value.ty
            && argument.conversion.to == parameter.ty
            && parameter.has_default
            && marker_origin_matches
            && target
                .typed_default_proof_root(&self.parameter)
                .is_some_and(|root| {
                    target
                        .body
                        .default_values
                        .get(&self.parameter)
                        .is_some_and(|expression| root.expression == expression.node_id)
                })
    }

    /// Resolve both the call owner and declaration owner through one checked
    /// directory before authorizing default-root proof or substitution.
    pub fn validate_in_checked_program(&self, program: &crate::core::CheckedProgram) -> bool {
        let Some(caller) = program.callable(&self.caller_owner) else {
            return false;
        };
        let Some(target) = program.callable(&self.callee_owner) else {
            return false;
        };
        self.validate_against(caller, target)
    }

    fn validate_for_point(&self, caller: &ResolvedCallable, point: &TypedCallProofPoint) -> bool {
        if !point.validate_against(caller)
            || point.body_owner != self.caller_owner
            || point.call_node != self.call_node
            || point.order != self.order
            || point.scope != self.scope
            || debug_fingerprint(&point.guards) != debug_fingerprint(&self.guards)
            || point.disposition != self.disposition
            || point.private_witness != self.call_point_witness
        {
            return false;
        }
        let ResolvedExprKind::Call(call) = &point.expression.kind else {
            return false;
        };
        call.arguments
            .get(self.parameter_index)
            .is_some_and(|argument| {
                argument.value.node_id == self.argument_node
                    && matches!(
                        &argument.value.kind,
                        ResolvedExprKind::DefaultArgument { callable, parameter }
                            if callable == &self.callee_owner && parameter == &self.parameter
                    )
            })
    }

    fn compute_witness(&self) -> blake3::Hash {
        let material = format!(
            "typed-default-proof-use-v1\ncaller={:?}\ncall={:?}\ncallee={:?}\nparameter={:?}\nargument={:?}\nindex={}\norder={}\nscope={:?}\nguards={:#?}\ndisposition={:?}\npoint={:?}\nbody={}",
            self.caller_owner,
            self.call_node,
            self.callee_owner,
            self.parameter,
            self.argument_node,
            self.parameter_index,
            self.order,
            self.scope,
            self.guards,
            self.disposition,
            self.call_point_witness,
            self.source_body_fingerprint,
        );
        blake3::hash(material.as_bytes())
    }
}

struct ProofPointBuilder<'a> {
    callable: &'a ResolvedCallable,
    owner: &'a NodeId,
    body_fingerprint: String,
    boundaries: Vec<TypedProofBoundary>,
    points: Vec<TypedMathProofPoint>,
    calls: Vec<TypedCallProofPoint>,
    match_path: Vec<TypedMatchPathReceipt>,
    scope: Vec<TypedProofScopeFrame>,
    guards: Vec<TypedCallGuard>,
    /// A loop-local call can be executable only when its exact checker loop
    /// receipt carries the narrow immutable at-most-once FFI call profile.
    active_loop_call_paths: Vec<(NodeId, Option<TypedLoopFfiCallPath>)>,
    next_order: u64,
    root_contracts: BTreeSet<NodeId>,
    /// Once a callable has a proof root, every executable statement that can
    /// mutate a value or call an unmodeled effect must be accounted for. A
    /// proof worklist may not skip an earlier assignment/call and then reuse
    /// the stale parameter environment at a later assertion.
    proof_requested: bool,
}

impl ProofPointBuilder<'_> {
    fn exact_new_result_match(&self, expression: &ResolvedExpr) -> bool {
        matches!(&expression.kind, ResolvedExprKind::Match { .. })
            && self
                .callable
                .resources
                .ordinary_map_owned_any_v2_new_result_receipt
                .as_ref()
                .is_some_and(|receipt| receipt.match_node == expression.node_id)
    }

    fn defer(&mut self, point: &NodeId, reason: &str) {
        self.boundaries.push(TypedProofBoundary {
            point: point.clone(),
            reason: reason.into(),
        });
    }

    fn next_event_order(&mut self) -> u64 {
        let order = self.next_order;
        self.next_order = self
            .next_order
            .checked_add(1)
            .expect("a finite checked body cannot contain u64::MAX proof events");
        order
    }

    fn record_call(
        &mut self,
        owner: &NodeId,
        statement: &NodeId,
        expression: &ResolvedExpr,
        deferred: Option<&str>,
    ) {
        let ResolvedExprKind::Call(call) = &expression.kind else {
            return;
        };
        let loop_call_supported = {
            let active_loops = self
                .scope
                .iter()
                .filter_map(|frame| match frame {
                    TypedProofScopeFrame::Loop(loop_node) => Some(loop_node),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if let [loop_node] = active_loops.as_slice() {
                self.active_loop_call_paths
                    .last()
                    .filter(|(active_loop, _)| active_loop == *loop_node)
                    .and_then(|(_, profile)| profile.as_ref())
                    .is_some_and(|profile| {
                        profile.loop_node == **loop_node
                            && profile.call_statement == *statement
                            && profile.call_node == expression.node_id
                    })
            } else {
                false
            }
        };
        let disposition = if let Some(reason) = deferred {
            TypedCallExecutionDisposition::Deferred(reason.to_owned())
        } else if self
            .scope
            .iter()
            .any(|frame| matches!(frame, TypedProofScopeFrame::Default { .. }))
        {
            let reason = if self
                .scope
                .iter()
                .any(|frame| matches!(frame, TypedProofScopeFrame::Block(_)))
            {
                // A block embedded in a declaration default has a lexical
                // DefaultArgument receipt, but no transfer timeline for its
                // internal execution calls. Keep those calls in the census
                // without allowing an active default use to execute them.
                "nested expression block has no transfer timeline"
            } else {
                "default expression call requires exact omitted-argument expansion"
            };
            TypedCallExecutionDisposition::Deferred(reason.into())
        } else if self
            .scope
            .iter()
            .any(|frame| matches!(frame, TypedProofScopeFrame::Loop(_)))
        {
            if loop_call_supported {
                // A loop call is conditional on the real condition-true CFG
                // edge even if its lexical `if` guard list is empty.
                TypedCallExecutionDisposition::Guarded
            } else {
                TypedCallExecutionDisposition::Deferred(
                    "loop call point lacks an exact immutable at-most-once FFI path receipt".into(),
                )
            }
        } else if self.guards.is_empty() && self.match_path.is_empty() {
            TypedCallExecutionDisposition::Unconditional
        } else {
            TypedCallExecutionDisposition::Guarded
        };
        let event_order = self.next_event_order();
        let match_path = self
            .match_path
            .iter()
            .cloned()
            .map(|mut receipt| {
                receipt.call_node = Some(expression.node_id.clone());
                receipt
            })
            .collect::<Vec<_>>();
        let mut point = TypedCallProofPoint {
            body_owner: self.owner.clone(),
            owner: owner.clone(),
            statement: statement.clone(),
            call_node: expression.node_id.clone(),
            order: event_order,
            scope: self.scope.clone(),
            guards: self.guards.clone(),
            match_path,
            disposition,
            expression: expression.clone(),
            effect: TypedCallEffectReceipt {
                call_node: expression.node_id.clone(),
                callee: call.callee.clone(),
                declared_effects: call.effects.clone(),
                authority: TypedCallEffectAuthority::UnknownMayReadWrite,
            },
            source_body_fingerprint: self.body_fingerprint.clone(),
            private_witness: blake3::hash(b"unsealed-typed-call-proof-point"),
        };
        point.private_witness = point.compute_witness();
        self.calls.push(point);
    }

    /// Mint a Checker receipt only for a complete signed-i32/i64 scalar
    /// dispatch.  The CFG edge and resource-state checks happen here, while
    /// the matching MIR Switch and call reachability are independently
    /// checked by the MIR/FFI receipt validator.
    fn literal_match_path_receipts(
        &self,
        expression: &ResolvedExpr,
        scrutinee: &ResolvedExpr,
        arms: &[super::MatchArm],
    ) -> Option<Vec<TypedMatchPathReceipt>> {
        if arms.len() < 2 || arms.iter().any(|arm| arm.guard.is_some()) {
            return None;
        }
        let (local_id, scrutinee_type) = match &scrutinee.kind {
            ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                (place.base.clone(), scrutinee.ty.clone())
            }
            _ => return None,
        };
        let local = self.callable.body.locals.get(&local_id)?;
        if local.ty != scrutinee_type {
            return None;
        }
        let is_i32 = scrutinee_type == super::primitive_type_id(super::PrimitiveType::I32);
        let is_i64 = scrutinee_type == super::primitive_type_id(super::PrimitiveType::I64);
        if !is_i32 && !is_i64 {
            return None;
        }

        let mut seen_literals = BTreeSet::new();
        let mut cases = Vec::with_capacity(arms.len());
        for (index, arm) in arms.iter().enumerate() {
            match &arm.pattern.kind {
                ResolvedPatternKind::Literal(ResolvedLiteral::Int(value))
                    if index + 1 < arms.len()
                        && (!is_i32 || i32::try_from(*value).is_ok())
                        && seen_literals.insert(*value) =>
                {
                    cases.push(TypedMatchCase::Integer(*value));
                }
                ResolvedPatternKind::Wildcard if index + 1 == arms.len() => {
                    cases.push(TypedMatchCase::Wildcard);
                }
                _ => return None,
            }
        }
        if cases.last() != Some(&TypedMatchCase::Wildcard)
            || cases[..cases.len() - 1]
                .iter()
                .any(|case| !matches!(case, TypedMatchCase::Integer(_)))
        {
            return None;
        }

        let mut dispatches = self.callable.cfg.blocks.values().filter(|block| {
            matches!(
                &block.terminator,
                Terminator::Match { scrutinee: candidate, .. }
                    if candidate == &scrutinee.node_id
            )
        });
        let dispatch = dispatches.next()?;
        if dispatches.next().is_some() || !self.callable.cfg.reachable.contains(&dispatch.id) {
            return None;
        }
        let Terminator::Match {
            scrutinee: cfg_scrutinee,
            arms: cfg_edges,
        } = &dispatch.terminator
        else {
            return None;
        };
        if cfg_scrutinee != &scrutinee.node_id || cfg_edges.len() != arms.len() {
            return None;
        }

        let dispatch_state = self.callable.resources.out_states.get(&dispatch.id)?;
        let mut selections = Vec::with_capacity(arms.len());
        let mut seen_edges = BTreeSet::new();
        let mut seen_targets = BTreeSet::new();
        for ((arm, edge_id), case) in arms.iter().zip(cfg_edges).zip(&cases) {
            if !seen_edges.insert(edge_id.clone()) {
                return None;
            }
            let edge = self.callable.cfg.edges.get(edge_id)?;
            if edge.id != *edge_id
                || edge.from != dispatch.id
                || edge.kind != EdgeKind::MatchArm
                || edge.source.node != arm.node_id
                || edge.source.origin != arm.origin
                || !self.callable.cfg.reachable.contains(&edge.to)
                || !seen_targets.insert(edge.to.clone())
                || self
                    .callable
                    .resources
                    .actions
                    .iter()
                    .any(|action| action.location.edge.as_ref() == Some(edge_id))
                || self.callable.resources.in_states.get(&edge.to)? != dispatch_state
            {
                return None;
            }
            selections.push(TypedMatchArmSelection {
                arm: arm.node_id.clone(),
                cfg_edge: edge_id.clone(),
                cfg_target: edge.to.clone(),
                case: case.clone(),
            });
        }
        if selections.len() != arms.len() {
            return None;
        }

        let mut ownership_material = format!(
            "typed-match-selection-ownership-v1\nowner={:?}\nmatch={:?}\ndispatch={:?}\nstate={:#?}",
            self.callable.owner, expression.node_id, dispatch.id, dispatch_state
        );
        for selection in &selections {
            ownership_material.push_str(&format!("\narm={selection:#?}"));
        }
        let ownership_state_fingerprint = *blake3::hash(ownership_material.as_bytes()).as_bytes();
        Some(
            selections
                .iter()
                .filter(|selection| matches!(&selection.case, TypedMatchCase::Integer(_)))
                .map(|selection| TypedMatchPathReceipt {
                    call_node: None,
                    match_expression: expression.node_id.clone(),
                    scrutinee_expression: scrutinee.node_id.clone(),
                    scrutinee_local: local_id.clone(),
                    scrutinee_type: scrutinee_type.clone(),
                    cfg_dispatch: dispatch.id.clone(),
                    arms: selections.clone(),
                    selected_arm: selection.arm.clone(),
                    ownership_state_fingerprint,
                })
                .collect(),
        )
    }

    /// Collect calls in Mimi's left-to-right expression order. The visitor
    /// visits call actuals before the containing call, and records explicit
    /// Bool guards for short-circuit and conditional expressions. Constructs
    /// whose runtime multiplicity/selection is not represented by the proof
    /// worklist remain present with a Deferred disposition.
    fn expression_calls(
        &mut self,
        owner: &NodeId,
        statement: &NodeId,
        expression: &ResolvedExpr,
        deferred: Option<&str>,
    ) {
        match &expression.kind {
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let super::ResolvedFStringPart::Interpolation(value) = part {
                        self.expression_calls(owner, statement, value, deferred);
                    }
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.expression_calls(owner, statement, value, deferred);
                if let super::ResolvedValueProjection::Index(index) = projection {
                    self.expression_calls(owner, statement, index, deferred);
                }
            }
            ResolvedExprKind::Binary { op, left, right } => {
                self.expression_calls(owner, statement, left, deferred);
                if matches!(op, super::ResolvedBinaryOp::LogicalAnd) {
                    self.guards.push(TypedCallGuard {
                        condition: *left.clone(),
                        when: true,
                    });
                    self.expression_calls(owner, statement, right, deferred);
                    self.guards.pop();
                } else if matches!(op, super::ResolvedBinaryOp::LogicalOr) {
                    self.guards.push(TypedCallGuard {
                        condition: *left.clone(),
                        when: false,
                    });
                    self.expression_calls(owner, statement, right, deferred);
                    self.guards.pop();
                } else {
                    self.expression_calls(owner, statement, right, deferred);
                }
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand)
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. } => {
                let nested_deferred = match &expression.kind {
                    ResolvedExprKind::Spawn(_) | ResolvedExprKind::Await(_) => {
                        Some("spawn/await call ordering requires a task effect receipt")
                    }
                    ResolvedExprKind::TypeOf(_) => {
                        Some("type-of operand is not an execution point")
                    }
                    _ => deferred,
                };
                self.expression_calls(owner, statement, operand, nested_deferred);
            }
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    self.expression_calls(owner, statement, &argument.value, deferred);
                }
                self.record_call(owner, statement, expression, deferred);
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.expression_calls(owner, statement, value, deferred);
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.expression_calls(owner, statement, key, deferred);
                    self.expression_calls(owner, statement, value, deferred);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.expression_calls(owner, statement, iterable, deferred);
                let repeated = deferred.or(Some(
                    "comprehension body has repeated iteration without a loop receipt",
                ));
                self.expression_calls(owner, statement, value, repeated);
                if let Some(guard) = guard {
                    self.expression_calls(owner, statement, guard, repeated);
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => {
                self.expression_calls(owner, statement, receiver, deferred);
            }
            ResolvedExprKind::Old(value) => self.expression_calls(
                owner,
                statement,
                value,
                deferred.or(Some(
                    "old(...) is a snapshot term, not a runtime call point",
                )),
            ),
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.expression_calls(owner, statement, &field.value, deferred);
                }
                if let Some(rest) = rest {
                    self.expression_calls(owner, statement, rest, deferred);
                }
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let branch_expression = expression.node_id.clone();
                self.expression_calls(owner, statement, condition, deferred);
                self.scope.push(TypedProofScopeFrame::Branch {
                    statement: statement.clone(),
                    expression: branch_expression.clone(),
                    then_arm: true,
                });
                self.guards.push(TypedCallGuard {
                    condition: *condition.clone(),
                    when: true,
                });
                self.call_block_only(
                    owner,
                    then_block,
                    deferred.or(Some("nested conditional block has no transfer timeline")),
                );
                self.guards.pop();
                self.scope.pop();
                self.scope.push(TypedProofScopeFrame::Branch {
                    statement: statement.clone(),
                    expression: branch_expression,
                    then_arm: false,
                });
                self.guards.push(TypedCallGuard {
                    condition: *condition.clone(),
                    when: false,
                });
                self.call_block_only(
                    owner,
                    else_block,
                    deferred.or(Some("nested conditional block has no transfer timeline")),
                );
                self.guards.pop();
                self.scope.pop();
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.expression_calls(owner, statement, scrutinee, deferred);
                let selection_receipts =
                    self.literal_match_path_receipts(expression, scrutinee, arms);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expression_calls(
                            owner,
                            &arm.node_id,
                            guard,
                            Some("match arm guard requires pattern-selection receipt"),
                        );
                    }
                    let selection = selection_receipts.as_ref().and_then(|receipts| {
                        receipts
                            .iter()
                            .find(|receipt| receipt.selected_arm == arm.node_id)
                            .cloned()
                    });
                    let arm_deferred = if deferred.is_some() {
                        deferred
                    } else if selection.is_some() {
                        None
                    } else {
                        Some(
                            "wildcard or unsupported match arm has no executable pattern-selection receipt",
                        )
                    };
                    let pushed_selection = selection.is_some();
                    if let Some(selection) = selection {
                        self.match_path.push(selection);
                    }
                    if let Some(call) = arm_deferred
                        .is_none()
                        .then(|| direct_scalar_ffi_match_arm_call(&arm.body))
                        .flatten()
                    {
                        // The parser represents even an expression-bodied match
                        // arm as a value-producing block. Its exact typed shape
                        // (no statements, one direct scalar FFI call as the
                        // result) is a complete transfer timeline: record the
                        // call at the arm identity, without inventing a block
                        // boundary whose effects have not been analyzed.
                        self.expression_calls(owner, &arm.node_id, call, None);
                    } else {
                        self.expression_calls(owner, &arm.node_id, &arm.body, arm_deferred);
                    }
                    if pushed_selection {
                        self.match_path.pop();
                    }
                }
            }
            ResolvedExprKind::Range { start, end } => {
                self.expression_calls(owner, statement, start, deferred);
                self.expression_calls(owner, statement, end, deferred);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.expression_calls(owner, statement, target, deferred);
                if let Some(start) = start {
                    self.expression_calls(owner, statement, start, deferred);
                }
                if let Some(end) = end {
                    self.expression_calls(owner, statement, end, deferred);
                }
            }
            ResolvedExprKind::Lambda(lambda) => self.call_block_only(
                &lambda.owner,
                &lambda.body,
                Some("lambda body is latent until a checked invocation path exists"),
            ),
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => self
                .call_block_only(
                    owner,
                    block,
                    deferred.or(Some("nested expression block has no transfer timeline")),
                ),
            ResolvedExprKind::Comptime(block) | ResolvedExprKind::Quote(block) => self
                .call_block_only(
                    owner,
                    block,
                    Some("comptime/quote body is not a runtime call point"),
                ),
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }

    fn call_block_only(&mut self, owner: &NodeId, block: &ResolvedBlock, deferred: Option<&str>) {
        self.scope
            .push(TypedProofScopeFrame::Block(block.node_id.clone()));
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Bind { initializer, .. } => {
                    if let Some(initializer) = initializer {
                        self.expression_calls(owner, &statement.node_id, initializer, deferred);
                    }
                }
                ResolvedStmtKind::Assign { value, .. }
                | ResolvedStmtKind::Expr(value)
                | ResolvedStmtKind::Contract {
                    condition: value, ..
                } => self.expression_calls(
                    owner,
                    &statement.node_id,
                    value,
                    if matches!(&statement.kind, ResolvedStmtKind::Contract { .. }) {
                        Some("contract expression is an obligation, not a runtime call point")
                    } else {
                        deferred
                    },
                ),
                ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                    if let Some(value) = value {
                        self.expression_calls(owner, &statement.node_id, value, deferred);
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    self.expression_calls(
                        owner,
                        &statement.node_id,
                        condition,
                        Some("loop condition call requires induction/effect receipt"),
                    );
                    self.scope
                        .push(TypedProofScopeFrame::Loop(statement.node_id.clone()));
                    self.call_block_only(
                        owner,
                        body,
                        Some("loop body call requires induction/effect receipt"),
                    );
                    self.scope.pop();
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                } => {
                    self.expression_calls(
                        owner,
                        &statement.node_id,
                        initializer,
                        Some("while-let condition call requires induction/effect receipt"),
                    );
                    self.scope
                        .push(TypedProofScopeFrame::Loop(statement.node_id.clone()));
                    self.call_block_only(
                        owner,
                        body,
                        Some("while-let body call requires induction/effect receipt"),
                    );
                    self.scope.pop();
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    self.expression_calls(owner, &statement.node_id, initializer, deferred);
                    self.call_block_only(
                        owner,
                        then_block,
                        Some("if-let arm requires typed pattern-selection path receipt"),
                    );
                    if let Some(else_block) = else_block {
                        self.call_block_only(
                            owner,
                            else_block,
                            Some("if-let arm requires typed pattern-selection path receipt"),
                        );
                    }
                }
                ResolvedStmtKind::Loop(body) => {
                    self.scope
                        .push(TypedProofScopeFrame::Loop(statement.node_id.clone()));
                    self.call_block_only(
                        owner,
                        body,
                        Some("loop body call requires induction/effect receipt"),
                    );
                    self.scope.pop();
                }
                ResolvedStmtKind::For { iterable, body, .. } => {
                    self.expression_calls(owner, &statement.node_id, iterable, deferred);
                    self.scope
                        .push(TypedProofScopeFrame::Loop(statement.node_id.clone()));
                    self.call_block_only(
                        owner,
                        body,
                        Some("for body call requires iteration receipt"),
                    );
                    self.scope.pop();
                }
                ResolvedStmtKind::Scope { body, .. } => self.call_block_only(owner, body, deferred),
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    self.expression_calls(owner, &statement.node_id, value, deferred);
                    self.call_block_only(
                        owner,
                        body,
                        Some("pinned scope requires an ownership/effect receipt"),
                    );
                }
                ResolvedStmtKind::Math(expressions) => {
                    for expression in expressions {
                        self.expression_calls(
                            owner,
                            &statement.node_id,
                            expression,
                            Some("math term is a proof obligation, not a runtime call point"),
                        );
                    }
                }
                ResolvedStmtKind::Drop(_)
                | ResolvedStmtKind::Continue
                | ResolvedStmtKind::NestedCallable(_) => {}
            }
        }
        if let Some(result) = &block.result {
            self.expression_calls(owner, &block.node_id, result, deferred);
        }
        self.scope.pop();
    }

    fn block(
        &mut self,
        block: &ResolvedBlock,
        consumed_loop_invariants: &BTreeSet<NodeId>,
    ) -> TypedProofBlock {
        let mut output = TypedProofBlock {
            id: block.node_id.clone(),
            operations: Vec::new(),
        };
        self.scope
            .push(TypedProofScopeFrame::Block(block.node_id.clone()));
        self.block_statements(block, consumed_loop_invariants, &mut output);
        if let Some(result) = &block.result {
            let call_start = self.calls.len();
            let operation = self.expression(result, &block.node_id);
            let exact_new_result_match = self.exact_new_result_match(result);
            self.append_current_scope_calls(&mut output, call_start);
            if operation.is_none()
                && !exact_new_result_match
                && expression_contains_proof_root(result)
            {
                self.defer(
                    &result.node_id,
                    "block result embeds a proof root without an evaluation-order receipt",
                );
            } else if operation.is_none()
                && !exact_new_result_match
                && self.proof_requested
                && !proof_expression_is_pure(result)
                && !standalone_checked_call(result)
            {
                self.defer(
                    &result.node_id,
                    "block result call/effect needs a typed return-value receipt",
                );
            }
            if let Some(operation) = operation {
                output.operations.push(operation);
            } else if proof_expression_is_pure(result) {
                output.operations.push(TypedProofOperation::Eval {
                    point: block.node_id.clone(),
                    expression: result.as_ref().clone(),
                });
            }
        }
        self.scope.pop();
        output
    }

    fn bind_arm_block(
        &mut self,
        bind_point: &NodeId,
        local: &ResolvedLocalId,
        block: &ResolvedBlock,
    ) -> TypedProofBlock {
        let mut output = TypedProofBlock {
            id: block.node_id.clone(),
            operations: Vec::new(),
        };
        self.scope
            .push(TypedProofScopeFrame::Block(block.node_id.clone()));
        self.block_statements(block, &BTreeSet::new(), &mut output);
        if let Some(result) = &block.result {
            self.bind_initializer_operations(bind_point, local, result, &mut output);
        }
        self.scope.pop();
        output
    }

    fn build_branch_operation(
        &mut self,
        statement: &NodeId,
        branch_expression: NodeId,
        condition: &ResolvedExpr,
        then_block: &ResolvedBlock,
        else_block: &ResolvedBlock,
        mut build_arm: impl FnMut(&mut Self, &ResolvedBlock) -> TypedProofBlock,
    ) -> TypedProofOperation {
        // Direct calls inside `condition` stay at the enclosing `self.scope` so
        // callers wrapping `call_start..` append them before pushing `Branch`.
        self.expression_calls(self.owner, statement, condition, None);
        if expression_contains_proof_root(condition)
            || (self.proof_requested
                && !proof_expression_is_pure(condition)
                && !standalone_checked_call(condition))
        {
            self.defer(
                &condition.node_id,
                "branch condition embeds a proof root or effect without an evaluation receipt",
            );
        }
        self.scope.push(TypedProofScopeFrame::Branch {
            statement: statement.clone(),
            expression: branch_expression.clone(),
            then_arm: true,
        });
        self.guards.push(TypedCallGuard {
            condition: condition.clone(),
            when: true,
        });
        let then_block = build_arm(self, then_block);
        self.guards.pop();
        self.scope.pop();
        self.scope.push(TypedProofScopeFrame::Branch {
            statement: statement.clone(),
            expression: branch_expression.clone(),
            then_arm: false,
        });
        self.guards.push(TypedCallGuard {
            condition: condition.clone(),
            when: false,
        });
        let else_block = build_arm(self, else_block);
        self.guards.pop();
        self.scope.pop();
        TypedProofOperation::Branch {
            point: statement.clone(),
            expression: branch_expression,
            condition: condition.clone(),
            then_block,
            else_block,
        }
    }

    fn bind_initializer_operations(
        &mut self,
        point: &NodeId,
        local: &ResolvedLocalId,
        initializer: &ResolvedExpr,
        output: &mut TypedProofBlock,
    ) {
        match &initializer.kind {
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let call_start = self.calls.len();
                let branch = self.build_branch_operation(
                    point,
                    initializer.node_id.clone(),
                    condition,
                    then_block,
                    else_block,
                    |builder, arm| builder.bind_arm_block(point, local, arm),
                );
                self.append_current_scope_calls(output, call_start);
                output.operations.push(branch);
            }
            ResolvedExprKind::Block(inner) | ResolvedExprKind::Scope { body: inner, .. } => {
                let inner_block = self.bind_arm_block(point, local, inner);
                output
                    .operations
                    .push(TypedProofOperation::Scope(inner_block));
            }
            _ => {
                let call_start = self.calls.len();
                self.expression_calls(self.owner, point, initializer, None);
                self.append_current_scope_calls(output, call_start);
                if !expression_contains_proof_root(initializer)
                    && proof_expression_is_pure(initializer)
                {
                    output.operations.push(TypedProofOperation::Bind {
                        point: point.clone(),
                        local: local.clone(),
                        initializer: initializer.clone(),
                    });
                } else if expression_contains_proof_root(initializer)
                    || (self.proof_requested && !standalone_checked_call(initializer))
                {
                    self.defer(
                        point,
                        "binding initializer embeds a proof root or effect without a typed evaluation receipt",
                    );
                }
            }
        }
    }

    fn block_statements(
        &mut self,
        block: &ResolvedBlock,
        consumed_loop_invariants: &BTreeSet<NodeId>,
        output: &mut TypedProofBlock,
    ) {
        for statement in &block.statements {
            match &statement.kind {
                ResolvedStmtKind::Contract { kind, condition }
                    if self.root_contracts.contains(&statement.node_id) =>
                {
                    // Root contracts remain separate callable entry/return
                    // obligations. They are never replayed as statement facts.
                    self.expression_calls(
                        self.owner,
                        &statement.node_id,
                        condition,
                        Some("call inside callable contract is a proof term, not a runtime call point"),
                    );
                    let _ = kind;
                }
                ResolvedStmtKind::Contract { condition, .. }
                    if consumed_loop_invariants.contains(&statement.node_id) =>
                {
                    self.expression_calls(
                        self.owner,
                        &statement.node_id,
                        condition,
                        Some(
                            "call inside loop invariant is a proof term, not a runtime call point",
                        ),
                    );
                }
                ResolvedStmtKind::Contract { condition, .. } => {
                    self.expression_calls(
                        self.owner,
                        &statement.node_id,
                        condition,
                        Some(
                            "call inside nested contract is a proof term, not a runtime call point",
                        ),
                    );
                    self.defer(
                        &statement.node_id,
                        "nested Requires/Ensures/Invariant lacks a matching scoped proof point",
                    );
                }
                ResolvedStmtKind::Math(expressions) => {
                    for condition in expressions {
                        self.expression_calls(
                            self.owner,
                            &statement.node_id,
                            condition,
                            Some("call inside Math is a proof term, not a runtime call point"),
                        );
                    }
                    if expressions.iter().any(expression_contains_proof_root) {
                        self.defer(
                            &statement.node_id,
                            "a math expression embeds another proof root; evaluation-order receipts are required",
                        );
                    }
                    for (ordinal, condition) in expressions.iter().enumerate() {
                        let order = self.next_event_order();
                        let point = TypedMathProofPoint {
                            id: TypedProofPointId {
                                owner: self.owner.clone(),
                                statement: statement.node_id.clone(),
                                ordinal: ordinal as u32,
                            },
                            statement_span: statement.origin.user_span(),
                            order,
                            scope: self.scope.clone(),
                            condition: condition.clone(),
                        };
                        self.points.push(point.clone());
                        output.operations.push(TypedProofOperation::Math(point));
                    }
                }
                ResolvedStmtKind::Bind {
                    pattern,
                    initializer: Some(initializer),
                } => {
                    if let super::ResolvedPatternKind::Binding {
                        local,
                        by_reference: None,
                    } = &pattern.kind
                    {
                        self.bind_initializer_operations(
                            &statement.node_id,
                            local,
                            initializer,
                            output,
                        );
                    } else {
                        let call_start = self.calls.len();
                        self.expression_calls(self.owner, &statement.node_id, initializer, None);
                        self.append_current_scope_calls(output, call_start);
                        if block_has_proof_roots(block) {
                            self.defer(
                                &statement.node_id,
                                "destructuring/reference binding before a proof point needs a typed binding receipt",
                            );
                        }
                    }
                }
                ResolvedStmtKind::While { condition, body } => {
                    let enclosing_scope = self.scope.clone();
                    self.scope
                        .push(TypedProofScopeFrame::Loop(statement.node_id.clone()));
                    let condition_call_start = self.calls.len();
                    self.expression_calls(
                        self.owner,
                        &statement.node_id,
                        condition,
                        Some("loop condition call requires a validated induction/effect receipt"),
                    );
                    let condition_calls = self.calls[condition_call_start..].to_vec();
                    let (invariants, consumed) = leading_loop_invariants(
                        body,
                        &statement.node_id,
                        self.owner,
                        &enclosing_scope,
                    );
                    let stationary = stationary_loop_body(body);
                    let receipt = derive_typed_loop_proof_receipt(
                        self.callable,
                        &statement.node_id,
                        &enclosing_scope,
                        &invariants,
                    )
                    .ok();
                    // A direct callable contract still makes nested loops part
                    // of the proof obligation graph.  Do not let
                    // `proof_requested == false` (which only means that all
                    // roots are entry/exit contracts) silently materialize a
                    // loop with no leading invariant or induction receipt.
                    // The loop consumer remains fail-closed for this shape;
                    // root-only callables without loops are unaffected.
                    let loop_proof_requested =
                        self.proof_requested || !self.root_contracts.is_empty();
                    let ffi_call_path = receipt
                        .as_ref()
                        .and_then(|receipt| receipt.ffi_call_path.clone());
                    let induction_profile_missing = ffi_call_path.is_none()
                        && (invariants.is_empty()
                            || receipt.is_none()
                            || receipt.as_ref().is_some_and(|receipt| {
                                !receipt.call_nodes.is_empty() || !receipt.effect_nodes.is_empty()
                            }));
                    if loop_proof_requested
                        && (induction_profile_missing || !proof_expression_is_pure(condition))
                    {
                        self.defer(
                            &statement.node_id,
                            "loop needs either a valid induction receipt or the exact immutable at-most-once FFI call profile, with pure typed values and no unmodeled effects",
                        );
                    }
                    self.active_loop_call_paths
                        .push((statement.node_id.clone(), ffi_call_path));
                    let body_block = self.block(body, &consumed);
                    self.active_loop_call_paths.pop();
                    self.scope.pop();
                    if expression_contains_proof_root(condition) {
                        self.defer(
                            &statement.node_id,
                            "loop condition embeds a proof root without a header evaluation receipt",
                        );
                    }
                    output.operations.push(TypedProofOperation::Loop {
                        point: statement.node_id.clone(),
                        condition: condition.clone(),
                        condition_calls,
                        invariants,
                        body: body_block,
                        stationary,
                        receipt,
                    });
                }
                ResolvedStmtKind::Expr(expression) => {
                    let call_start = self.calls.len();
                    let operation = self.expression(expression, &statement.node_id);
                    let exact_new_result_match = self.exact_new_result_match(expression);
                    self.append_current_scope_calls(output, call_start);
                    if let Some(operation) = operation {
                        output.operations.push(operation);
                    } else if proof_expression_is_pure(expression) {
                        output.operations.push(TypedProofOperation::Eval {
                            point: statement.node_id.clone(),
                            expression: expression.clone(),
                        });
                    } else if self.proof_requested
                        && !proof_expression_is_pure(expression)
                        && !standalone_checked_call(expression)
                        && !exact_new_result_match
                    {
                        self.defer(
                            &statement.node_id,
                            "call/effect expression before a proof point has no scoped value/effect receipt",
                        );
                    }
                }
                ResolvedStmtKind::Scope { body, .. } => {
                    output.operations.push(TypedProofOperation::Scope(
                        self.block(body, &BTreeSet::new()),
                    ));
                }
                ResolvedStmtKind::Break(value) => {
                    let call_start = self.calls.len();
                    if let Some(value) = value {
                        self.expression_calls(self.owner, &statement.node_id, value, None);
                    }
                    self.append_current_scope_calls(output, call_start);
                    output.operations.push(TypedProofOperation::Break {
                        point: statement.node_id.clone(),
                        target_loop: self.nearest_loop(),
                        value: value.clone(),
                    });
                }
                ResolvedStmtKind::Return { value, conversion } => {
                    let call_start = self.calls.len();
                    if let Some(value) = value {
                        self.expression_calls(self.owner, &statement.node_id, value, None);
                    }
                    self.append_current_scope_calls(output, call_start);
                    output.operations.push(TypedProofOperation::Return {
                        point: statement.node_id.clone(),
                        value: value.clone(),
                        conversion: conversion.clone(),
                    });
                }
                ResolvedStmtKind::Continue => {
                    output.operations.push(TypedProofOperation::Continue {
                        point: statement.node_id.clone(),
                        target_loop: self.nearest_loop(),
                    });
                }
                ResolvedStmtKind::Assign {
                    target,
                    value,
                    conversion,
                } => {
                    let call_start = self.calls.len();
                    self.expression_calls(self.owner, &statement.node_id, value, None);
                    self.append_current_scope_calls(output, call_start);
                    output.operations.push(TypedProofOperation::Assign {
                        point: statement.node_id.clone(),
                        target: target.clone(),
                        value: value.clone(),
                        conversion: conversion.clone(),
                    });
                    if statement_contains_proof_root(statement) {
                        self.defer(
                            &statement.node_id,
                            "assignment value contains a nested proof root without a scoped evaluation receipt",
                        );
                    }
                }
                ResolvedStmtKind::WhileLet {
                    initializer, body, ..
                } => {
                    self.expression_calls(
                        self.owner,
                        &statement.node_id,
                        initializer,
                        Some("while-let header requires a validated induction/effect receipt"),
                    );
                    self.scope
                        .push(TypedProofScopeFrame::Loop(statement.node_id.clone()));
                    self.call_block_only(
                        self.owner,
                        body,
                        Some("while-let body requires a validated induction/effect receipt"),
                    );
                    self.scope.pop();
                    if self.proof_requested || statement_contains_proof_root(statement) {
                        self.defer(
                            &statement.node_id,
                            "assignment/effect/unsupported control in a proof-bearing callable needs a dedicated typed receipt",
                        );
                    }
                }
                ResolvedStmtKind::Loop(body) => {
                    self.scope
                        .push(TypedProofScopeFrame::Loop(statement.node_id.clone()));
                    self.call_block_only(
                        self.owner,
                        body,
                        Some("loop body requires a validated induction/effect receipt"),
                    );
                    self.scope.pop();
                    if self.proof_requested || statement_contains_proof_root(statement) {
                        self.defer(
                            &statement.node_id,
                            "assignment/effect/unsupported control in a proof-bearing callable needs a dedicated typed receipt",
                        );
                    }
                }
                ResolvedStmtKind::For { iterable, body, .. } => {
                    self.expression_calls(
                        self.owner,
                        &statement.node_id,
                        iterable,
                        Some("for iterable requires the checked iteration/control receipt"),
                    );
                    self.scope
                        .push(TypedProofScopeFrame::Loop(statement.node_id.clone()));
                    self.call_block_only(
                        self.owner,
                        body,
                        Some("for body requires a checked iteration receipt"),
                    );
                    self.scope.pop();
                    if self.proof_requested || statement_contains_proof_root(statement) {
                        self.defer(
                            &statement.node_id,
                            "assignment/effect/unsupported control in a proof-bearing callable needs a dedicated typed receipt",
                        );
                    }
                }
                ResolvedStmtKind::IfLet {
                    initializer,
                    then_block,
                    else_block,
                    ..
                } => {
                    self.expression_calls(
                        self.owner,
                        &statement.node_id,
                        initializer,
                        Some("if-let initializer requires a checked pattern-selection receipt"),
                    );
                    self.call_block_only(
                        self.owner,
                        then_block,
                        Some("if-let arm requires a typed pattern-selection path receipt"),
                    );
                    if let Some(else_block) = else_block {
                        self.call_block_only(
                            self.owner,
                            else_block,
                            Some("if-let arm requires a typed pattern-selection path receipt"),
                        );
                    }
                    if self.proof_requested || statement_contains_proof_root(statement) {
                        self.defer(
                            &statement.node_id,
                            "assignment/effect/unsupported control in a proof-bearing callable needs a dedicated typed receipt",
                        );
                    }
                }
                ResolvedStmtKind::Pinned { value, body, .. } => {
                    self.expression_calls(self.owner, &statement.node_id, value, None);
                    self.call_block_only(
                        self.owner,
                        body,
                        Some("pinned scope requires an ownership/effect receipt"),
                    );
                    if self.proof_requested || statement_contains_proof_root(statement) {
                        self.defer(
                            &statement.node_id,
                            "assignment/effect/unsupported control in a proof-bearing callable needs a dedicated typed receipt",
                        );
                    }
                }
                ResolvedStmtKind::Drop(_) | ResolvedStmtKind::NestedCallable(_) => {
                    if self.proof_requested || statement_contains_proof_root(statement) {
                        self.defer(
                            &statement.node_id,
                            "assignment/effect/unsupported control in a proof-bearing callable needs a dedicated typed receipt",
                        );
                    }
                }
                ResolvedStmtKind::Bind {
                    initializer: None, ..
                } => {
                    if self.proof_requested {
                        self.defer(
                            &statement.node_id,
                            "uninitialized binding in a proof-bearing callable needs a write/definition receipt",
                        );
                    }
                }
            }
        }
    }

    fn expression(
        &mut self,
        expression: &ResolvedExpr,
        statement: &NodeId,
    ) -> Option<TypedProofOperation> {
        match &expression.kind {
            ResolvedExprKind::Match { .. } if self.exact_new_result_match(expression) => {
                let call_start = self.calls.len();
                self.expression_calls(self.owner, statement, expression, None);
                if self.proof_requested {
                    self.defer(
                        &expression.node_id,
                        "proof root or effect is nested in an unsupported expression form",
                    );
                    let receipt = self
                        .callable
                        .resources
                        .ordinary_map_owned_any_v2_new_result_receipt
                        .as_ref()?;
                    let [size_call] = &self.calls[call_start..] else {
                        self.defer(
                            &expression.node_id,
                            "NewResult match proof calls do not have one exact size-call scope",
                        );
                        return None;
                    };
                    if size_call.call_node != receipt.size_operation
                        || size_call.scope.get(..self.scope.len()) != Some(self.scope.as_slice())
                    {
                        self.defer(
                            &expression.node_id,
                            "NewResult match proof call is detached from its exact checked scope",
                        );
                        return None;
                    }
                    let [TypedProofScopeFrame::Block(arm_block)] =
                        &size_call.scope[self.scope.len()..]
                    else {
                        self.defer(
                            &expression.node_id,
                            "NewResult match proof call has an unsupported nested scope shape",
                        );
                        return None;
                    };
                    return Some(TypedProofOperation::Scope(TypedProofBlock {
                        id: arm_block.clone(),
                        operations: vec![TypedProofOperation::Call(size_call.clone())],
                    }));
                }
                None
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => Some(self.build_branch_operation(
                statement,
                expression.node_id.clone(),
                condition,
                then_block,
                else_block,
                |builder, arm| builder.block(arm, &BTreeSet::new()),
            )),
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => Some(
                TypedProofOperation::Scope(self.block(block, &BTreeSet::new())),
            ),
            _ => {
                self.expression_calls(self.owner, statement, expression, None);
                if expression_contains_proof_root(expression)
                    || (self.proof_requested
                        && !proof_expression_is_pure(expression)
                        && !standalone_checked_call(expression))
                {
                    self.defer(
                        &expression.node_id,
                        "proof root or effect is nested in an unsupported expression form",
                    );
                }
                None
            }
        }
    }

    fn append_current_scope_calls(&self, block: &mut TypedProofBlock, start: usize) {
        for point in &self.calls[start..] {
            if point.scope == self.scope {
                block
                    .operations
                    .push(TypedProofOperation::Call(point.clone()));
            }
        }
    }

    fn nearest_loop(&self) -> Option<NodeId> {
        self.scope.iter().rev().find_map(|frame| match frame {
            TypedProofScopeFrame::Loop(point) => Some(point.clone()),
            _ => None,
        })
    }
}

fn standalone_checked_call(expression: &ResolvedExpr) -> bool {
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return false;
    };
    call.arguments
        .iter()
        .all(|argument| proof_expression_is_pure(&argument.value))
}

fn direct_scalar_ffi_match_arm_call(arm_body: &ResolvedExpr) -> Option<&ResolvedExpr> {
    let ResolvedExprKind::Block(block) = &arm_body.kind else {
        return None;
    };
    if !block.statements.is_empty() {
        return None;
    }
    let call_expression = block.result.as_deref()?;
    let ResolvedExprKind::Call(call) = &call_expression.kind else {
        return None;
    };
    if !matches!(&call.callee, ResolvedCallee::Extern(_))
        || !call.type_arguments.is_empty()
        || call.permission.is_some()
        || !call.session.is_empty()
        || call.arguments.len() != 1
        || !signed_scalar_type(&call.result)
        || call_expression.ty != call.result
        || block.ty != call_expression.ty
    {
        return None;
    }
    let argument = &call.arguments[0];
    if !matches!(
        &argument.value.kind,
        ResolvedExprKind::Load(place) if place.projections.is_empty()
    ) || !signed_scalar_type(&argument.value.ty)
        || argument.conversion.kind != super::CheckedConversionKind::Identity
        || argument.conversion.from != argument.value.ty
        || argument.conversion.to != argument.value.ty
    {
        return None;
    }
    Some(call_expression)
}

fn signed_scalar_type(ty: &ResolvedTypeId) -> bool {
    ty == &super::primitive_type_id(super::PrimitiveType::I32)
        || ty == &super::primitive_type_id(super::PrimitiveType::I64)
}

fn resolved_body_fingerprint(body: &ResolvedBody) -> String {
    blake3::hash(format!("{body:#?}").as_bytes())
        .to_hex()
        .to_string()
}

fn debug_fingerprint(value: &impl std::fmt::Debug) -> blake3::Hash {
    blake3::hash(format!("{value:#?}").as_bytes())
}

fn default_block_is_local_only(
    block: &TypedProofBlock,
    body: &ResolvedBody,
    available: &mut BTreeSet<ResolvedLocalId>,
) -> bool {
    for operation in &block.operations {
        match operation {
            TypedProofOperation::Bind {
                local, initializer, ..
            } => {
                let Some(local_info) = body.locals.get(local) else {
                    return false;
                };
                if local_info.mutable
                    || body.parameters.contains(local)
                    || body.captures.contains(local)
                    || !matches!(&initializer.kind, ResolvedExprKind::Literal(_))
                    || !available.insert(local.clone())
                {
                    return false;
                }
            }
            TypedProofOperation::Math(point) => {
                if !default_expr_uses_only_local_literals_and_bindings(
                    &point.condition,
                    body,
                    available,
                ) {
                    return false;
                }
            }
            TypedProofOperation::Eval { expression, .. } => {
                if !default_expr_uses_only_local_literals_and_bindings(expression, body, available)
                {
                    return false;
                }
            }
            TypedProofOperation::Scope(inner) => {
                let mut nested = available.clone();
                if !default_block_is_local_only(inner, body, &mut nested) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

fn default_expr_uses_only_local_literals_and_bindings(
    expression: &ResolvedExpr,
    body: &ResolvedBody,
    available: &BTreeSet<ResolvedLocalId>,
) -> bool {
    match &expression.kind {
        ResolvedExprKind::Literal(_) => true,
        ResolvedExprKind::Load(place) => {
            place.projections.is_empty()
                && available.contains(&place.base)
                && body
                    .locals
                    .get(&place.base)
                    .is_some_and(|local| !local.mutable)
                && !body.parameters.contains(&place.base)
                && !body.captures.contains(&place.base)
        }
        ResolvedExprKind::Unary { operand, .. } => {
            default_expr_uses_only_local_literals_and_bindings(operand, body, available)
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            default_expr_uses_only_local_literals_and_bindings(left, body, available)
                && default_expr_uses_only_local_literals_and_bindings(right, body, available)
        }
        _ => false,
    }
}

fn expression_contains_proof_root(expression: &ResolvedExpr) -> bool {
    let mut roots = ProofRoots::default();
    collect_expr_contracts(expression, &mut roots);
    !roots.contracts.is_empty() || roots.has_math
}

fn statement_contains_proof_root(statement: &super::ResolvedStmt) -> bool {
    let mut roots = ProofRoots::default();
    collect_contracts(
        &ResolvedBlock {
            node_id: statement.node_id.clone(),
            origin: statement.origin.clone(),
            ty: statement.ty.clone(),
            statements: vec![statement.clone()],
            result: None,
        },
        &mut roots,
    );
    !roots.contracts.is_empty() || roots.has_math
}

fn block_has_proof_roots(block: &ResolvedBlock) -> bool {
    let mut roots = ProofRoots::default();
    collect_contracts(block, &mut roots);
    !roots.contracts.is_empty() || roots.has_math
}

fn leading_loop_invariants(
    body: &ResolvedBlock,
    loop_node: &NodeId,
    owner: &NodeId,
    parent_scope: &[TypedProofScopeFrame],
) -> (Vec<TypedLoopInvariantPoint>, BTreeSet<NodeId>) {
    let mut invariants = Vec::new();
    let mut consumed = BTreeSet::new();
    let mut still_leading = true;
    for statement in &body.statements {
        match &statement.kind {
            ResolvedStmtKind::Contract {
                kind: ContractKind::Invariant,
                condition,
            } if still_leading => {
                let ordinal = invariants.len() as u32;
                invariants.push(TypedLoopInvariantPoint {
                    id: TypedProofPointId {
                        owner: owner.clone(),
                        statement: statement.node_id.clone(),
                        ordinal,
                    },
                    scope: parent_scope
                        .iter()
                        .cloned()
                        .chain([
                            TypedProofScopeFrame::Loop(loop_node.clone()),
                            TypedProofScopeFrame::Block(body.node_id.clone()),
                        ])
                        .collect(),
                    condition: condition.clone(),
                });
                consumed.insert(statement.node_id.clone());
            }
            ResolvedStmtKind::Contract { .. } => still_leading = false,
            _ => still_leading = false,
        }
    }
    (invariants, consumed)
}

fn stationary_loop_body(block: &ResolvedBlock) -> bool {
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Contract {
                kind: ContractKind::Invariant,
                ..
            }
            | ResolvedStmtKind::Math(_)
            | ResolvedStmtKind::Continue => {}
            ResolvedStmtKind::Scope { body, .. } if stationary_loop_body(body) => {}
            _ => return false,
        }
    }
    block.result.is_none()
}

fn proof_expression_is_pure(expression: &ResolvedExpr) -> bool {
    match &expression.kind {
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_) => true,
        ResolvedExprKind::Old(operand) => proof_expression_is_pure(operand),
        ResolvedExprKind::Project { value, projection } => {
            proof_expression_is_pure(value)
                && match projection {
                    super::ResolvedValueProjection::Index(index) => proof_expression_is_pure(index),
                    _ => true,
                }
        }
        ResolvedExprKind::Unary { operand, .. } => proof_expression_is_pure(operand),
        // Identity and numeric widening are total value conversions, so keep
        // their branch facts (for example `x > (0 as i64)`). Narrowing and
        // dynamic downcasts can trap, while ownership/dynamic packing may
        // cross runtime boundaries; those conversions remain proof boundaries.
        ResolvedExprKind::Cast { value, conversion } => {
            matches!(
                conversion.kind,
                super::CheckedConversionKind::Identity | super::CheckedConversionKind::NumericWiden
            ) && proof_expression_is_pure(value)
        }
        ResolvedExprKind::Binary { left, right, .. } => {
            proof_expression_is_pure(left) && proof_expression_is_pure(right)
        }
        ResolvedExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            proof_expression_is_pure(condition)
                && proof_block_is_pure(then_block)
                && proof_block_is_pure(else_block)
        }
        ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
            proof_block_is_pure(block)
        }
        _ => false,
    }
}

fn proof_block_is_pure(block: &ResolvedBlock) -> bool {
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Contract { .. } | ResolvedStmtKind::Math(_) => {}
            ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(initializer),
            } if matches!(
                &pattern.kind,
                super::ResolvedPatternKind::Binding {
                    by_reference: None,
                    ..
                }
            ) && proof_expression_is_pure(initializer) => {}
            ResolvedStmtKind::Expr(expression) if proof_expression_is_pure(expression) => {}
            ResolvedStmtKind::Scope { body, .. } if proof_block_is_pure(body) => {}
            ResolvedStmtKind::Return { value, .. }
                if value.as_ref().map_or(true, proof_expression_is_pure) => {}
            ResolvedStmtKind::Break(_) | ResolvedStmtKind::Continue => {}
            _ => return false,
        }
    }
    block.result.as_deref().is_none_or(proof_expression_is_pure)
}

impl ResolvedBody {
    pub fn has_proof_obligations(&self) -> bool {
        let roots = collect_proof_roots(self);
        !roots.contracts.is_empty() || roots.has_math
    }

    /// Return every checker-owned Math statement root, including roots inside
    /// declaration defaults and structured expression bodies. This census is
    /// independent of whether a proof-point worklist can encode the root.
    pub fn math_proof_roots(&self) -> Vec<(NodeId, Origin)> {
        collect_proof_roots(self).math_origins.into_iter().collect()
    }

    /// Return every checker-owned contract root in this body and its typed
    /// declaration-expression inputs, preserving the exact node and origin.
    pub fn contract_proof_roots(&self) -> Vec<ResolvedContract> {
        collect_proof_roots(self).contracts
    }
}

#[derive(Default)]
struct ProofRoots {
    contracts: Vec<ResolvedContract>,
    has_math: bool,
    math_nodes: BTreeSet<NodeId>,
    math_origins: BTreeMap<NodeId, Origin>,
    seen_expressions: BTreeSet<NodeId>,
    seen_contracts: BTreeSet<NodeId>,
}

fn collect_proof_roots(body: &ResolvedBody) -> ProofRoots {
    let mut roots = ProofRoots::default();
    collect_contracts(&body.root, &mut roots);
    // Loads and assignment targets refer to these typed inputs by identity.
    // Defaults are also declaration-owned executable expression roots.
    for value in body
        .place_inputs
        .values()
        .chain(body.default_values.values())
    {
        collect_expr_contracts(value, &mut roots);
    }
    roots
}

fn collect_contracts(block: &ResolvedBlock, out: &mut ProofRoots) {
    for statement in &block.statements {
        match &statement.kind {
            ResolvedStmtKind::Contract { kind, condition } => {
                if out.seen_contracts.insert(statement.node_id.clone()) {
                    out.contracts.push(ResolvedContract {
                        node_id: statement.node_id.clone(),
                        kind: *kind,
                        condition: condition.clone(),
                        origin: statement.origin.clone(),
                    });
                }
                collect_expr_contracts(condition, out);
            }
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
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                collect_expr_contracts(initializer, out);
                collect_contracts(then_block, out);
                if let Some(else_block) = else_block {
                    collect_contracts(else_block, out);
                }
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                collect_contracts(body, out)
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                collect_expr_contracts(value, out);
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
                out.has_math = true;
                out.math_nodes.insert(statement.node_id.clone());
                out.math_origins
                    .entry(statement.node_id.clone())
                    .or_insert_with(|| statement.origin.clone());
                for expression in expressions {
                    collect_expr_contracts(expression, out);
                }
            }
            ResolvedStmtKind::Return { value: None, .. }
            | ResolvedStmtKind::Break(None)
            | ResolvedStmtKind::Continue
            | ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::NestedCallable(_) => {}
        }
    }
    if let Some(result) = &block.result {
        collect_expr_contracts(result, out);
    }
}

fn collect_expr_contracts(expression: &ResolvedExpr, out: &mut ProofRoots) {
    if !out.seen_expressions.insert(expression.node_id.clone()) {
        return;
    }
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
        ResolvedExprKind::Call(call) => {
            for argument in &call.arguments {
                collect_expr_contracts(&argument.value, out);
            }
        }
        ResolvedExprKind::FString(parts) => {
            for part in parts {
                if let super::ResolvedFStringPart::Interpolation(value) = part {
                    collect_expr_contracts(value, out);
                }
            }
        }
        ResolvedExprKind::Project { value, projection } => {
            collect_expr_contracts(value, out);
            if let super::ResolvedValueProjection::Index(index) = projection {
                collect_expr_contracts(index, out);
            }
        }
        ResolvedExprKind::Binary { left, right, .. }
        | ResolvedExprKind::Range {
            start: left,
            end: right,
        } => {
            collect_expr_contracts(left, out);
            collect_expr_contracts(right, out);
        }
        ResolvedExprKind::Unary { operand, .. }
        | ResolvedExprKind::OptionalChain {
            receiver: operand, ..
        }
        | ResolvedExprKind::TypeOf(operand)
        | ResolvedExprKind::Old(operand)
        | ResolvedExprKind::Try { value: operand, .. }
        | ResolvedExprKind::Cast { value: operand, .. }
        | ResolvedExprKind::Spawn(operand)
        | ResolvedExprKind::Await(operand) => collect_expr_contracts(operand, out),
        ResolvedExprKind::Tuple(values)
        | ResolvedExprKind::List(values)
        | ResolvedExprKind::Set(values) => {
            for value in values {
                collect_expr_contracts(value, out);
            }
        }
        ResolvedExprKind::Map(entries) => {
            for (key, value) in entries {
                collect_expr_contracts(key, out);
                collect_expr_contracts(value, out);
            }
        }
        ResolvedExprKind::Comprehension {
            value,
            iterable,
            guard,
            ..
        } => {
            collect_expr_contracts(iterable, out);
            if let Some(guard) = guard {
                collect_expr_contracts(guard, out);
            }
            collect_expr_contracts(value, out);
        }
        ResolvedExprKind::Record { fields, rest, .. } => {
            for field in fields {
                collect_expr_contracts(&field.value, out);
            }
            if let Some(rest) = rest {
                collect_expr_contracts(rest, out);
            }
        }
        ResolvedExprKind::Slice { target, start, end } => {
            collect_expr_contracts(target, out);
            if let Some(start) = start {
                collect_expr_contracts(start, out);
            }
            if let Some(end) = end {
                collect_expr_contracts(end, out);
            }
        }
        ResolvedExprKind::Lambda(lambda) => collect_contracts(&lambda.body, out),
        ResolvedExprKind::Literal(_)
        | ResolvedExprKind::Load(_)
        | ResolvedExprKind::Constant(_)
        | ResolvedExprKind::Callable(_)
        | ResolvedExprKind::DefaultArgument { .. }
        | ResolvedExprKind::ComptimeValue(_)
        | ResolvedExprKind::TypeValue(_) => {}
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

    fn checked_program(source: &str) -> crate::core::CheckedProgram {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("tokenize match path fixture");
        let file = crate::parser::Parser::new_memory(
            tokens,
            "typed-match-path-test",
            "typed-match-path-test",
            source,
        )
        .expect("create match path parser")
        .parse_file()
        .expect("parse match path fixture");
        crate::core::check_program(&file).expect("check match path fixture")
    }

    fn body_call_points(
        checked: &crate::core::CheckedProgram,
        function: &str,
    ) -> (ResolvedCallable, TypedProofPointWorklist) {
        let owner = NodeId(format!("function:{function}"));
        let callable = checked
            .callable(&owner)
            .expect("checked function callable")
            .clone();
        let worklist = callable.proof_point_worklist();
        (callable, worklist)
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

    #[test]
    fn literal_match_call_receipts_are_callsite_bound_and_unsupported_shapes_defer() {
        let source = r#"
            extern "C" { func foreign(value: i32) -> i32; }
            func literal_and_wildcard(value: i32) -> i32 {
                match value {
                    7 => foreign(value),
                    _ => foreign(value)
                }
            }
            func guarded(value: i32) -> i32 {
                match value {
                    7 if value > 0 => foreign(value),
                    _ => 0
                }
            }
            func overlapping(value: i32) -> i32 {
                match value {
                    7 => foreign(value),
                    7 => foreign(value),
                    _ => 0
                }
            }
            func prefixed_assignment(value: i32) -> i32 {
                match value {
                    7 => {
                        let mut candidate = value;
                        candidate = candidate + 1;
                        foreign(candidate)
                    },
                    _ => 0
                }
            }
            func prefixed_effect(value: i32) -> i32 {
                match value {
                    7 => { foreign(value); foreign(value) },
                    _ => 0
                }
            }
            func main() -> i64 { 0 }
        "#;
        let checked = checked_program(source);

        let (literal_callable, literal_worklist) =
            body_call_points(&checked, "literal_and_wildcard");
        assert!(literal_worklist.validate_in_checked_program(&checked));
        assert_eq!(literal_worklist.calls.len(), 2);
        let literal_call = &literal_worklist.calls[0];
        assert_eq!(
            literal_call.disposition,
            TypedCallExecutionDisposition::Guarded
        );
        assert_eq!(literal_call.match_path.len(), 1);
        assert_eq!(
            literal_call.match_path[0].call_node.as_ref(),
            Some(&literal_call.call_node)
        );
        assert_eq!(
            literal_call.match_path[0].selected_arm,
            literal_call.match_path[0].arms[0].arm
        );
        assert_eq!(
            literal_call.match_path[0].arms[0].case,
            TypedMatchCase::Integer(7)
        );
        assert_eq!(
            literal_call.match_path[0].arms[0].cfg_edge.0 .0,
            format!(
                "{}/cfg:edge:match-arm",
                literal_call.match_path[0].arms[0].arm.0
            )
        );
        assert_eq!(
            literal_call.match_path[0].arms[0].cfg_target.0 .0,
            format!(
                "{}/cfg:block:match-arm",
                literal_call.match_path[0].arms[0].arm.0
            )
        );
        assert_eq!(
            literal_call.statement,
            literal_call.match_path[0].selected_arm
        );
        assert_eq!(
            literal_worklist
                .root
                .operations
                .iter()
                .filter(|operation| matches!(
                    operation,
                    TypedProofOperation::Call(point) if point.call_node == literal_call.call_node
                ))
                .count(),
            1,
            "the selected direct call must occupy one exact typed transfer event"
        );
        let wildcard_call = &literal_worklist.calls[1];
        assert!(matches!(
            &wildcard_call.disposition,
            TypedCallExecutionDisposition::Deferred(_)
        ));
        assert!(wildcard_call.match_path.is_empty());

        let mut forged_worklist = literal_worklist.clone();
        forged_worklist.calls[0].match_path[0].arms[0].case = TypedMatchCase::Integer(8);
        assert!(
            !forged_worklist.validate_in_checked_program(&checked),
            "a stale or forged literal-case receipt must not survive checker rederivation"
        );
        assert!(literal_callable
            .proof_point_worklist()
            .validate_against(&literal_callable));

        for function in ["guarded", "overlapping"] {
            let (_, worklist) = body_call_points(&checked, function);
            assert!(
                !worklist.calls.is_empty(),
                "the unsupported {function} call remains in the checker census"
            );
            assert!(
                worklist.calls.iter().all(|call| {
                    matches!(
                        &call.disposition,
                        TypedCallExecutionDisposition::Deferred(_)
                    ) && call.match_path.is_empty()
                }),
                "unsupported {function} pattern shape must not gain a match receipt"
            );
        }

        for function in ["prefixed_assignment", "prefixed_effect"] {
            let (_, worklist) = body_call_points(&checked, function);
            assert!(!worklist.calls.is_empty());
            assert!(
                worklist.calls.iter().all(|call| {
                    matches!(
                        &call.disposition,
                        TypedCallExecutionDisposition::Deferred(_)
                    )
                }),
                "a multi-event {function} arm must stay fail-closed"
            );
        }
    }
}
