//! Checker-derived source receipts for mutable `while` loop induction.
//!
//! A receipt is only a typed source census. It is not an invariant proof and
//! it is not permission to assume an invariant. Resolved and MIR consumers
//! must independently prove initialization, every backedge, and each exit.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::core::cfg::{
    BasicBlock, BasicBlockId, CallableCfg, CfgEdge, EdgeId, EdgeKind, Terminator,
};
use crate::core::{
    CanonicalResourceAction, CheckedConversion, Loan, NodeId, ResolvedBody, ResolvedCallable,
    ResolvedCallee, ResolvedExpr, ResolvedExprKind, ResolvedLocalId, ResolvedPattern,
    ResolvedPatternKind, ResolvedPlace, ResolvedProjection, ResolvedStmt, ResolvedStmtKind,
    ResolvedTypeId, ResourceFact, ResourceId,
};

use super::{TypedLoopInvariantPoint, TypedProofPointId, TypedProofScopeFrame};

pub const TYPED_LOOP_PROOF_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypedLoopExpressionRole {
    Condition,
    Invariant,
    WriteValue,
    BranchCondition,
    ReturnValue,
    BreakValue,
    Math,
    Eval,
    BindInitializer,
    PlaceInput,
}

/// Exact source-expression site bound by a private receipt witness. The two
/// fingerprints separately bind the checked Resolved expression tree and
/// the canonical path-to-TypeId map consumed by MIR's term encoder.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TypedLoopExpressionSite {
    pub owner: NodeId,
    pub loop_node: NodeId,
    pub role: TypedLoopExpressionRole,
    pub point: NodeId,
    pub expression: NodeId,
    pub branch_expression: Option<NodeId>,
    pub ordinal: u32,
    pub scope: Vec<TypedProofScopeFrame>,
    pub ty: ResolvedTypeId,
    pub effects: Vec<crate::core::EffectId>,
    pub source_fingerprint: [u8; 32],
    pub type_path_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypedLoopEdgeKind {
    BodyFallthrough,
    Continue,
    Break,
    Return,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TypedLoopBranchGuard {
    /// Typed worklist branch identity (the enclosing statement, when any).
    pub statement: NodeId,
    /// Resolved CFG branch source identity (the exact `If` expression node).
    pub expression: NodeId,
    pub condition: NodeId,
    pub then_arm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedLoopEdgeSite {
    pub site: NodeId,
    pub kind: TypedLoopEdgeKind,
    /// Exact lexical loop receiving a source `break` or `continue`. The
    /// checker only admits these terminal edges for the current loop when no
    /// nested loop is present; MIR independently binds the CFG target.
    pub target_loop: Option<NodeId>,
    pub guards: Vec<TypedLoopBranchGuard>,
    /// Exact CFG terminal for this source-level edge occurrence. This binds
    /// the lexical guard path to the real break/continue/return/backedge
    /// block instead of merely checking that the named edges exist.
    pub cfg_terminal_block: BasicBlockId,
    pub cfg_terminal_edge: Option<EdgeId>,
    pub reachable: bool,
    /// The source return/break operand identity, when the terminal carries
    /// one. The typed worklist separately carries its full expression and
    /// conversion receipt.
    pub terminal_value: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScannedLoopEdge {
    site: NodeId,
    kind: TypedLoopEdgeKind,
    target_loop: Option<NodeId>,
    guards: Vec<TypedLoopBranchGuard>,
    terminal_value: Option<NodeId>,
}

/// Narrow source profile for a loop that contains exactly one by-value scalar
/// FFI call on a straight-line path which terminates at one exact break. Any
/// other body path may break or continue before the call. The exact guard
/// chain reaching the call is separate from the call point's lexical guards:
/// a branch before a later sibling statement affects execution without
/// enclosing that statement lexically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedLoopFfiCallPath {
    pub loop_node: NodeId,
    pub condition_node: NodeId,
    pub call_statement: NodeId,
    pub call_node: NodeId,
    pub call_guards: Vec<TypedLoopBranchGuard>,
    pub terminal_break: NodeId,
    pub terminal_block: BasicBlockId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedLoopStateLocal {
    pub local: ResolvedLocalId,
    pub ty: ResolvedTypeId,
    /// True when at least one source assignment in the loop targets this
    /// whole local. Read-only state locals are still threaded through the
    /// header so consumers use the current preheader SSA value.
    pub written_in_loop: bool,
    /// Source-order assignment node identities, all targeting this whole
    /// scalar local. Projected writes are retained in `writes` but do not
    /// become a scalar induction variable.
    pub write_sites: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedLoopWriteSite {
    pub statement: NodeId,
    pub target: ResolvedPlace,
    pub value: NodeId,
    pub conversion: CheckedConversion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedLoopResourceEdgeState {
    pub edge: EdgeId,
    pub from_input: Option<BTreeMap<ResourceId, ResourceFact>>,
    pub from_output: Option<BTreeMap<ResourceId, ResourceFact>>,
    pub to_input: Option<BTreeMap<ResourceId, ResourceFact>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedLoopResourceBlockState {
    pub block: BasicBlockId,
    pub input: Option<BTreeMap<ResourceId, ResourceFact>>,
    pub output: Option<BTreeMap<ResourceId, ResourceFact>>,
}

/// Complete source-side induction census for one checker-resolved `while`.
/// `cfg_*` and `resource_*` fields bind the receipt to the exact
/// `CallableCfg`/`ResourceAnalysis`; consumers must compare/rederive them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedLoopProofReceipt {
    pub schema_version: u32,
    pub owner: NodeId,
    pub loop_node: NodeId,
    pub scope: Vec<TypedProofScopeFrame>,
    pub body_node: NodeId,
    pub condition_node: NodeId,
    pub invariant_points: Vec<TypedProofPointId>,
    pub state_locals: Vec<TypedLoopStateLocal>,
    pub writes: Vec<TypedLoopWriteSite>,
    /// One source edge for each possible loop-body fallthrough/continue/break/
    /// return path, with its complete lexical branch-guard path.
    pub body_edges: Vec<TypedLoopEdgeSite>,
    /// Caller-entry locals used by `old`; loop-header remapping must never
    /// replace these symbols with current-iteration values.
    pub old_snapshot: Vec<ResolvedLocalId>,
    /// Every syntactic call evaluated by the loop condition/body, including
    /// calls with an empty declared effect list. A consumer requires a
    /// matching ordered CallProofPoint and may not infer purity from `[]`.
    pub call_nodes: Vec<NodeId>,
    /// Present only for the closed, immutable, at-most-once FFI path profile
    /// above. The general call census remains complete regardless of whether
    /// this profile can be issued.
    pub ffi_call_path: Option<TypedLoopFfiCallPath>,
    /// Every expression carrying checker effects, in evaluation traversal
    /// order. This list is independent of whether the expression is a call.
    pub effect_nodes: Vec<NodeId>,
    /// Exact source CFG subgraph rooted at the loop's entry edge and bounded
    /// by its normal exit, backedges, continue edges, and break edges.
    pub cfg_blocks: Vec<BasicBlock>,
    pub cfg_edges: Vec<CfgEdge>,
    pub cfg_reachable_blocks: BTreeSet<BasicBlockId>,
    /// Exact in/out ownership snapshots for every edge in `cfg_edges`.
    pub resource_edges: Vec<TypedLoopResourceEdgeState>,
    /// Exact in/out ownership snapshots for loop-executed CFG blocks, including
    /// terminal Return blocks. The post-loop normal-exit target is excluded;
    /// its incoming ownership is bound through `resource_edges.to_input`.
    pub resource_blocks: Vec<TypedLoopResourceBlockState>,
    /// Canonical ownership actions whose source block is in the loop slice.
    /// Consumers must compare this complete rederived vector and reject
    /// unmodeled actions; equal block snapshots alone do not prove that a
    /// resource was not moved out and back within one block.
    pub resource_actions: Vec<CanonicalResourceAction>,
    /// Complete callable loan census, including loans that begin before and
    /// end after this loop but are live across one of its edges.
    pub loans: Vec<Loan>,
    /// Source-ordered, role-tagged expression roots used by proof operations,
    /// CFG guards, terminal values, and all nested body expression trees.
    pub expression_sites: Vec<TypedLoopExpressionSite>,
    private_source_witness: [u8; 32],
}

impl TypedLoopProofReceipt {
    fn compute_private_source_witness(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        macro_rules! absorb {
            ($value:expr) => {{
                hasher.update(format!("{:?}", $value).as_bytes());
                hasher.update(&[0]);
            }};
        }
        absorb!(self.schema_version);
        absorb!(self.owner);
        absorb!(self.loop_node);
        absorb!(self.scope);
        absorb!(self.body_node);
        absorb!(self.condition_node);
        absorb!(self.invariant_points);
        absorb!(self.state_locals);
        absorb!(self.writes);
        absorb!(self.body_edges);
        absorb!(self.old_snapshot);
        absorb!(self.call_nodes);
        absorb!(self.ffi_call_path);
        absorb!(self.effect_nodes);
        absorb!(self.cfg_blocks);
        absorb!(self.cfg_edges);
        absorb!(self.cfg_reachable_blocks);
        absorb!(self.resource_edges);
        absorb!(self.resource_blocks);
        absorb!(self.resource_actions);
        absorb!(self.loans);
        absorb!(self.expression_sites);
        *hasher.finalize().as_bytes()
    }

    /// Public MIR consumers use this to detect mutation of any public receipt
    /// field after checker construction. The witness is not itself a proof:
    /// expression-site equality and all induction VCs are still mandatory.
    pub fn has_valid_source_witness(&self) -> bool {
        self.private_source_witness == self.compute_private_source_witness()
    }
}

pub fn typed_loop_expression_fingerprint(expression: &ResolvedExpr) -> [u8; 32] {
    *blake3::hash(format!("{:#?}", expression).as_bytes()).as_bytes()
}

pub fn type_path_fingerprint(types: &BTreeMap<Vec<u8>, ResolvedTypeId>) -> [u8; 32] {
    *blake3::hash(format!("{types:?}").as_bytes()).as_bytes()
}

pub fn source_expression_type_path_fingerprint(
    expression: &ResolvedExpr,
    body: &ResolvedBody,
) -> Result<[u8; 32], String> {
    fn collect(
        expression: &ResolvedExpr,
        body: &ResolvedBody,
        path: &mut Vec<u8>,
        types: &mut BTreeMap<Vec<u8>, ResolvedTypeId>,
    ) -> Result<(), String> {
        use crate::core::ir::ResolvedUnaryOp;
        use crate::core::ResolvedLiteral;

        types.insert(path.clone(), expression.ty.clone());
        if let ResolvedExprKind::Unary {
            op: ResolvedUnaryOp::Negate,
            operand,
        } = &expression.kind
        {
            if expression.ty != operand.ty
                && matches!(&operand.kind, ResolvedExprKind::Literal(ResolvedLiteral::Int(value)) if value.checked_neg().is_some())
            {
                return Ok(());
            }
        }
        match &expression.kind {
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::Project { value: operand, .. } => {
                path.push(0);
                collect(operand, body, path, types)?;
                path.pop();
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                for (index, operand) in [left, right].into_iter().enumerate() {
                    path.push(index as u8);
                    collect(operand, body, path, types)?;
                    path.pop();
                }
            }
            ResolvedExprKind::Load(place) => {
                collect_place_types(place, body, path, types)?;
            }
            ResolvedExprKind::Old(inner) => {
                let ResolvedExprKind::Load(place) = &inner.kind else {
                    return Err("old() requires a direct callable parameter load".into());
                };
                collect_place_types(place, body, path, types)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn collect_place_types(
        place: &ResolvedPlace,
        body: &ResolvedBody,
        path: &mut Vec<u8>,
        types: &mut BTreeMap<Vec<u8>, ResolvedTypeId>,
    ) -> Result<(), String> {
        let base = body.locals.get(&place.base).ok_or_else(|| {
            "contract place type is absent from the checked local catalog".to_string()
        })?;
        let original_length = path.len();
        for count in (0..place.projections.len()).rev() {
            path.push(0);
            let ty = count
                .checked_sub(1)
                .map(|index| place.projections[index].ty())
                .unwrap_or(&base.ty);
            types.insert(path.clone(), ty.clone());
        }
        path.truncate(original_length);
        Ok(())
    }

    let mut types = BTreeMap::new();
    collect(expression, body, &mut Vec::new(), &mut types)?;
    Ok(type_path_fingerprint(&types))
}

fn collect_typed_loop_expression_sites(
    callable: &ResolvedCallable,
    loop_node: &NodeId,
    enclosing_scope: &[TypedProofScopeFrame],
    condition: &ResolvedExpr,
    body: &crate::core::ir::ResolvedBlock,
    invariants: &[TypedLoopInvariantPoint],
) -> Result<Vec<TypedLoopExpressionSite>, String> {
    fn record_expression(
        callable: &ResolvedCallable,
        loop_node: &NodeId,
        role: TypedLoopExpressionRole,
        point: &NodeId,
        branch_expression: Option<&NodeId>,
        ordinal: u32,
        scope: &[TypedProofScopeFrame],
        expression: &ResolvedExpr,
        output: &mut Vec<TypedLoopExpressionSite>,
    ) -> Result<(), String> {
        let site = TypedLoopExpressionSite {
            owner: callable.owner.clone(),
            loop_node: loop_node.clone(),
            role,
            point: point.clone(),
            expression: expression.node_id.clone(),
            branch_expression: branch_expression.cloned(),
            ordinal,
            scope: scope.to_vec(),
            ty: expression.ty.clone(),
            effects: expression.effects.clone(),
            source_fingerprint: typed_loop_expression_fingerprint(expression),
            type_path_fingerprint: source_expression_type_path_fingerprint(
                expression,
                &callable.body,
            )?,
        };
        if let Some(existing) = output.iter().find(|existing| {
            existing.owner == site.owner
                && existing.loop_node == site.loop_node
                && existing.role == site.role
                && existing.point == site.point
                && existing.expression == site.expression
                && existing.branch_expression == site.branch_expression
                && existing.ordinal == site.ordinal
                && existing.scope == site.scope
        }) {
            if existing != &site {
                return Err(format!(
                    "loop expression site '{}' has conflicting source seals",
                    expression.node_id.0
                ));
            }
        } else {
            output.push(site);
        }
        Ok(())
    }

    fn visit_expression(
        callable: &ResolvedCallable,
        loop_node: &NodeId,
        role: TypedLoopExpressionRole,
        point: &NodeId,
        branch_expression: Option<&NodeId>,
        ordinal: u32,
        scope: &[TypedProofScopeFrame],
        invariants: &[TypedLoopInvariantPoint],
        expression: &ResolvedExpr,
        output: &mut Vec<TypedLoopExpressionSite>,
    ) -> Result<(), String> {
        use ResolvedExprKind as E;

        record_expression(
            callable,
            loop_node,
            role,
            point,
            branch_expression,
            ordinal,
            scope,
            expression,
            output,
        )?;
        let recurse = |child: &ResolvedExpr,
                       child_role: TypedLoopExpressionRole,
                       child_point: &NodeId,
                       child_branch: Option<&NodeId>,
                       child_ordinal: u32,
                       child_scope: &[TypedProofScopeFrame],
                       output: &mut Vec<TypedLoopExpressionSite>| {
            visit_expression(
                callable,
                loop_node,
                child_role,
                child_point,
                child_branch,
                child_ordinal,
                child_scope,
                invariants,
                child,
                output,
            )
        };
        match &expression.kind {
            E::Project { value, projection } => {
                recurse(
                    value,
                    role,
                    point,
                    branch_expression,
                    ordinal,
                    scope,
                    output,
                )?;
                if let crate::core::ir::ResolvedValueProjection::Index(index) = projection {
                    recurse(
                        index,
                        role,
                        point,
                        branch_expression,
                        ordinal,
                        scope,
                        output,
                    )?;
                }
            }
            E::Binary { left, right, .. }
            | E::Range {
                start: left,
                end: right,
            } => {
                recurse(left, role, point, branch_expression, ordinal, scope, output)?;
                recurse(
                    right,
                    role,
                    point,
                    branch_expression,
                    ordinal,
                    scope,
                    output,
                )?;
            }
            E::Unary { operand, .. }
            | E::OptionalChain {
                receiver: operand, ..
            }
            | E::TypeOf(operand)
            | E::Old(operand)
            | E::Try { value: operand, .. }
            | E::Cast { value: operand, .. }
            | E::Spawn(operand)
            | E::Await(operand) => {
                recurse(
                    operand,
                    role,
                    point,
                    branch_expression,
                    ordinal,
                    scope,
                    output,
                )?;
            }
            E::Call(call) => {
                for argument in &call.arguments {
                    recurse(
                        &argument.value,
                        role,
                        point,
                        branch_expression,
                        ordinal,
                        scope,
                        output,
                    )?;
                }
            }
            E::Tuple(values) | E::List(values) | E::Set(values) => {
                for value in values {
                    recurse(
                        value,
                        role,
                        point,
                        branch_expression,
                        ordinal,
                        scope,
                        output,
                    )?;
                }
            }
            E::Map(entries) => {
                for (key, value) in entries {
                    recurse(key, role, point, branch_expression, ordinal, scope, output)?;
                    recurse(
                        value,
                        role,
                        point,
                        branch_expression,
                        ordinal,
                        scope,
                        output,
                    )?;
                }
            }
            E::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                recurse(
                    iterable,
                    role,
                    point,
                    branch_expression,
                    ordinal,
                    scope,
                    output,
                )?;
                recurse(
                    value,
                    role,
                    point,
                    branch_expression,
                    ordinal,
                    scope,
                    output,
                )?;
                if let Some(guard) = guard {
                    recurse(
                        guard,
                        role,
                        point,
                        branch_expression,
                        ordinal,
                        scope,
                        output,
                    )?;
                }
            }
            E::Record { fields, rest, .. } => {
                for field in fields {
                    recurse(
                        &field.value,
                        role,
                        point,
                        branch_expression,
                        ordinal,
                        scope,
                        output,
                    )?;
                }
                if let Some(rest) = rest {
                    recurse(rest, role, point, branch_expression, ordinal, scope, output)?;
                }
            }
            E::Block(block)
            | E::Scope { body: block, .. }
            | E::Comptime(block)
            | E::Quote(block) => {
                visit_block(callable, loop_node, block, scope, invariants, output)?;
            }
            E::If {
                condition,
                then_block,
                else_block,
            } => {
                recurse(
                    condition,
                    TypedLoopExpressionRole::BranchCondition,
                    point,
                    Some(&expression.node_id),
                    0,
                    scope,
                    output,
                )?;
                for (then_arm, branch_block) in [(true, then_block), (false, else_block)] {
                    let mut branch_scope = scope.to_vec();
                    branch_scope.push(TypedProofScopeFrame::Branch {
                        statement: point.clone(),
                        expression: expression.node_id.clone(),
                        then_arm,
                    });
                    visit_block(
                        callable,
                        loop_node,
                        branch_block,
                        &branch_scope,
                        invariants,
                        output,
                    )?;
                }
            }
            E::Match { scrutinee, arms } => {
                recurse(
                    scrutinee,
                    role,
                    point,
                    branch_expression,
                    ordinal,
                    scope,
                    output,
                )?;
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        recurse(
                            guard,
                            role,
                            &arm.node_id,
                            branch_expression,
                            ordinal,
                            scope,
                            output,
                        )?;
                    }
                    recurse(
                        &arm.body,
                        role,
                        &arm.node_id,
                        branch_expression,
                        ordinal,
                        scope,
                        output,
                    )?;
                }
            }
            E::Slice { target, start, end } => {
                recurse(
                    target,
                    role,
                    point,
                    branch_expression,
                    ordinal,
                    scope,
                    output,
                )?;
                if let Some(start) = start {
                    recurse(
                        start,
                        role,
                        point,
                        branch_expression,
                        ordinal,
                        scope,
                        output,
                    )?;
                }
                if let Some(end) = end {
                    recurse(end, role, point, branch_expression, ordinal, scope, output)?;
                }
            }
            E::FString(parts) => {
                for part in parts {
                    if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                        recurse(
                            value,
                            role,
                            point,
                            branch_expression,
                            ordinal,
                            scope,
                            output,
                        )?;
                    }
                }
            }
            E::Lambda(lambda) => {
                visit_block(callable, loop_node, &lambda.body, scope, invariants, output)?;
            }
            E::Load(place) => {
                visit_place_inputs(callable, loop_node, point, scope, invariants, place, output)?
            }
            E::Literal(_)
            | E::Constant(_)
            | E::Callable(_)
            | E::DefaultArgument { .. }
            | E::ComptimeValue(_)
            | E::TypeValue(_) => {}
        }
        Ok(())
    }

    fn visit_place_inputs(
        callable: &ResolvedCallable,
        loop_node: &NodeId,
        point: &NodeId,
        scope: &[TypedProofScopeFrame],
        invariants: &[TypedLoopInvariantPoint],
        place: &ResolvedPlace,
        output: &mut Vec<TypedLoopExpressionSite>,
    ) -> Result<(), String> {
        for (ordinal, projection) in place.projections.iter().enumerate() {
            if let ResolvedProjection::Index {
                index: crate::core::ResolvedIndex::Dynamic(node),
                ..
            } = projection
            {
                let expression = callable.body.place_inputs.get(node).ok_or_else(|| {
                    format!(
                        "dynamic place input '{}' is absent from ResolvedBody",
                        node.0
                    )
                })?;
                visit_expression(
                    callable,
                    loop_node,
                    TypedLoopExpressionRole::PlaceInput,
                    point,
                    None,
                    ordinal as u32,
                    scope,
                    invariants,
                    expression,
                    output,
                )?;
            }
        }
        Ok(())
    }

    fn visit_statement(
        callable: &ResolvedCallable,
        loop_node: &NodeId,
        statement: &ResolvedStmt,
        scope: &[TypedProofScopeFrame],
        invariants: &[TypedLoopInvariantPoint],
        output: &mut Vec<TypedLoopExpressionSite>,
    ) -> Result<(), String> {
        use ResolvedStmtKind as S;
        let point = &statement.node_id;
        match &statement.kind {
            S::Bind {
                initializer: Some(expression),
                ..
            } => visit_expression(
                callable,
                loop_node,
                TypedLoopExpressionRole::BindInitializer,
                point,
                None,
                0,
                scope,
                invariants,
                expression,
                output,
            )?,
            S::Bind {
                initializer: None, ..
            }
            | S::Continue
            | S::NestedCallable(_) => {}
            S::Assign { target, value, .. } => {
                visit_expression(
                    callable,
                    loop_node,
                    TypedLoopExpressionRole::WriteValue,
                    point,
                    None,
                    0,
                    scope,
                    invariants,
                    value,
                    output,
                )?;
                visit_place_inputs(
                    callable, loop_node, point, scope, invariants, target, output,
                )?;
            }
            S::Return {
                value: Some(expression),
                ..
            } => visit_expression(
                callable,
                loop_node,
                TypedLoopExpressionRole::ReturnValue,
                point,
                None,
                0,
                scope,
                invariants,
                expression,
                output,
            )?,
            S::Return { value: None, .. } => {}
            S::Break(Some(expression)) => visit_expression(
                callable,
                loop_node,
                TypedLoopExpressionRole::BreakValue,
                point,
                None,
                0,
                scope,
                invariants,
                expression,
                output,
            )?,
            S::Break(None) => {}
            S::Expr(expression) => visit_expression(
                callable,
                loop_node,
                TypedLoopExpressionRole::Eval,
                point,
                None,
                0,
                scope,
                invariants,
                expression,
                output,
            )?,
            S::Contract { kind, condition } => {
                if let Some(invariant) = invariants
                    .iter()
                    .find(|invariant| invariant.id.statement == *point)
                {
                    visit_expression(
                        callable,
                        loop_node,
                        TypedLoopExpressionRole::Invariant,
                        point,
                        None,
                        invariant.id.ordinal,
                        scope,
                        invariants,
                        condition,
                        output,
                    )?;
                } else {
                    let role = if matches!(kind, crate::core::ir::ContractKind::Invariant) {
                        TypedLoopExpressionRole::Invariant
                    } else {
                        TypedLoopExpressionRole::Eval
                    };
                    visit_expression(
                        callable, loop_node, role, point, None, 0, scope, invariants, condition,
                        output,
                    )?;
                }
            }
            S::Math(expressions) => {
                for (ordinal, expression) in expressions.iter().enumerate() {
                    visit_expression(
                        callable,
                        loop_node,
                        TypedLoopExpressionRole::Math,
                        point,
                        None,
                        ordinal as u32,
                        scope,
                        invariants,
                        expression,
                        output,
                    )?;
                }
            }
            S::Scope { body, .. } | S::Loop(body) => {
                visit_block(callable, loop_node, body, scope, invariants, output)?;
            }
            S::While { condition, body } => {
                visit_expression(
                    callable,
                    loop_node,
                    TypedLoopExpressionRole::Condition,
                    point,
                    None,
                    0,
                    scope,
                    invariants,
                    condition,
                    output,
                )?;
                let mut nested = scope.to_vec();
                nested.push(TypedProofScopeFrame::Loop(point.clone()));
                visit_block(callable, loop_node, body, &nested, invariants, output)?;
            }
            S::WhileLet {
                initializer, body, ..
            }
            | S::For {
                iterable: initializer,
                body,
                ..
            } => {
                visit_expression(
                    callable,
                    loop_node,
                    TypedLoopExpressionRole::Eval,
                    point,
                    None,
                    0,
                    scope,
                    invariants,
                    initializer,
                    output,
                )?;
                let mut nested = scope.to_vec();
                nested.push(TypedProofScopeFrame::Loop(point.clone()));
                visit_block(callable, loop_node, body, &nested, invariants, output)?;
            }
            S::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                visit_expression(
                    callable,
                    loop_node,
                    TypedLoopExpressionRole::Eval,
                    point,
                    None,
                    0,
                    scope,
                    invariants,
                    initializer,
                    output,
                )?;
                visit_block(callable, loop_node, then_block, scope, invariants, output)?;
                if let Some(block) = else_block {
                    visit_block(callable, loop_node, block, scope, invariants, output)?;
                }
            }
            S::Drop(places) => {
                for place in places {
                    visit_place_inputs(
                        callable, loop_node, point, scope, invariants, place, output,
                    )?;
                }
            }
            S::Pinned { value, body, .. } => {
                visit_expression(
                    callable,
                    loop_node,
                    TypedLoopExpressionRole::Eval,
                    point,
                    None,
                    0,
                    scope,
                    invariants,
                    value,
                    output,
                )?;
                visit_block(callable, loop_node, body, scope, invariants, output)?;
            }
        }
        Ok(())
    }

    fn visit_block(
        callable: &ResolvedCallable,
        loop_node: &NodeId,
        block: &crate::core::ir::ResolvedBlock,
        parent_scope: &[TypedProofScopeFrame],
        invariants: &[TypedLoopInvariantPoint],
        output: &mut Vec<TypedLoopExpressionSite>,
    ) -> Result<(), String> {
        let mut scope = parent_scope.to_vec();
        scope.push(TypedProofScopeFrame::Block(block.node_id.clone()));
        for statement in &block.statements {
            visit_statement(callable, loop_node, statement, &scope, invariants, output)?;
        }
        if let Some(result) = &block.result {
            visit_expression(
                callable,
                loop_node,
                TypedLoopExpressionRole::Eval,
                &block.node_id,
                None,
                0,
                &scope,
                invariants,
                result,
                output,
            )?;
        }
        Ok(())
    }

    let mut output = Vec::new();
    let mut loop_scope = enclosing_scope.to_vec();
    loop_scope.push(TypedProofScopeFrame::Loop(loop_node.clone()));
    visit_expression(
        callable,
        loop_node,
        TypedLoopExpressionRole::Condition,
        loop_node,
        None,
        0,
        &loop_scope,
        invariants,
        condition,
        &mut output,
    )?;
    visit_block(
        callable,
        loop_node,
        body,
        &loop_scope,
        invariants,
        &mut output,
    )?;
    output.sort();
    if output.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(format!(
            "loop '{}' expression-site census contains a duplicate identity",
            loop_node.0
        ));
    }
    Ok(output)
}

pub fn validate_loop_expression_site(
    receipt: &TypedLoopProofReceipt,
    role: TypedLoopExpressionRole,
    point: &NodeId,
    expression: &NodeId,
    branch_expression: Option<&NodeId>,
    ordinal: u32,
    scope: &[TypedProofScopeFrame],
    ty: &ResolvedTypeId,
    effects: &[crate::core::EffectId],
    source_fingerprint: &[u8; 32],
    type_path_fingerprint: &[u8; 32],
) -> bool {
    if !receipt.has_valid_source_witness() {
        return false;
    }
    let mut matches = receipt.expression_sites.iter().filter(|site| {
        site.owner == receipt.owner
            && site.loop_node == receipt.loop_node
            && site.role == role
            && &site.point == point
            && &site.expression == expression
            && site.branch_expression.as_ref() == branch_expression
            && site.ordinal == ordinal
            && site.scope == scope
    });
    matches.next().is_some_and(|site| {
        matches.next().is_none()
            && &site.ty == ty
            && site.effects == effects
            && &site.source_fingerprint == source_fingerprint
            && &site.type_path_fingerprint == type_path_fingerprint
    })
}

/// Build the source receipt from an already-checked callable and exact while
/// statement. This operation performs no proof and does not assume invariants.
pub fn derive_typed_loop_proof_receipt(
    callable: &ResolvedCallable,
    loop_node: &NodeId,
    scope: &[TypedProofScopeFrame],
    invariants: &[TypedLoopInvariantPoint],
) -> Result<TypedLoopProofReceipt, String> {
    if callable.owner != callable.body.owner
        || callable.owner != callable.cfg.owner
        || callable.owner != callable.resources.owner
    {
        return Err("callable, body, CFG, and ownership analysis owners disagree".into());
    }
    // Do not call `proof_point_worklist()` here. The FFI-owned worklist
    // builder invokes this factory while it is constructing that worklist;
    // re-entering it would recurse forever. Consumers pass the already
    // checker-derived scope/invariants and this function verifies those
    // identities against the canonical body below.
    let statement = find_unique_while_statement(&callable.body.root, loop_node)?;
    let ResolvedStmtKind::While { condition, body } = &statement.kind else {
        return Err("loop receipt target is not a typed while statement".into());
    };

    let mut invariant_points = Vec::new();
    let mut still_leading = true;
    for nested in &body.statements {
        match &nested.kind {
            ResolvedStmtKind::Contract {
                kind: crate::core::ir::ContractKind::Invariant,
                ..
            } if still_leading => {
                invariant_points.push(TypedProofPointId {
                    owner: callable.owner.clone(),
                    statement: nested.node_id.clone(),
                    ordinal: invariant_points.len() as u32,
                });
            }
            _ => still_leading = false,
        }
    }

    let expected_scope = scope
        .iter()
        .cloned()
        .chain([
            TypedProofScopeFrame::Loop(loop_node.clone()),
            TypedProofScopeFrame::Block(body.node_id.clone()),
        ])
        .collect::<Vec<_>>();
    let expected_invariants = body
        .statements
        .iter()
        .take_while(|nested| {
            matches!(
                &nested.kind,
                ResolvedStmtKind::Contract {
                    kind: crate::core::ir::ContractKind::Invariant,
                    ..
                }
            )
        })
        .collect::<Vec<_>>();
    if invariants.len() != invariant_points.len()
        || invariants.iter().zip(&expected_invariants).enumerate().any(
            |(ordinal, (point, statement))| {
                point.id.owner != callable.owner
                    || point.id.statement != statement.node_id
                    || point.id.ordinal != ordinal as u32
                    || point.scope != expected_scope
                    || !matches!(
                        &statement.kind,
                        ResolvedStmtKind::Contract { condition, .. }
                            if point.condition.node_id == condition.node_id
                    )
            },
        )
    {
        return Err(format!(
            "loop '{}' invariant receipts disagree with the checked leading invariant statements",
            loop_node.0
        ));
    }

    let mut scan = SourceLoopScan::new(&callable.body, loop_node);
    // The condition is evaluated on the header before every body execution.
    // Its calls/effects are part of this loop receipt even when the body is
    // empty or has no proof points.
    scan.expression(condition)?;
    let fallthrough = scan.block(body, &[])?;
    for guards in fallthrough {
        scan.edges.push(ScannedLoopEdge {
            site: body.node_id.clone(),
            kind: TypedLoopEdgeKind::BodyFallthrough,
            target_loop: None,
            guards,
            terminal_value: None,
        });
    }

    let mut writes = scan.writes;
    writes.sort_by(|left, right| left.statement.cmp(&right.statement));
    writes.dedup_by(|left, right| left == right);
    if let Some(projected) = writes
        .iter()
        .find(|write| !write.target.projections.is_empty())
    {
        return Err(format!(
            "loop write '{}' mutates a projected place outside the scalar carry profile",
            projected.statement.0
        ));
    }
    if let Some(local) = writes
        .iter()
        .map(|write| &write.target.base)
        .find(|local| scan.loop_local_bindings.contains(*local))
    {
        return Err(format!(
            "loop-assigned local '{}' is introduced inside the loop body and has no entry carry",
            local.0 .0
        ));
    }
    let mut writes_by_local = BTreeMap::<ResolvedLocalId, Vec<NodeId>>::new();
    for write in &writes {
        if write.target.projections.is_empty() {
            let Some(local) = callable.body.locals.get(&write.target.base) else {
                return Err(format!(
                    "loop write '{}' targets a local absent from the resolved body",
                    write.statement.0
                ));
            };
            if !local.mutable {
                return Err(format!(
                    "loop write '{}' targets a non-mutable local",
                    write.statement.0
                ));
            }
            writes_by_local
                .entry(write.target.base.clone())
                .or_default()
                .push(write.statement.clone());
        }
    }
    let written_locals = writes_by_local
        .into_iter()
        .map(|(local, mut write_sites)| {
            write_sites.sort();
            write_sites.dedup();
            (local, write_sites)
        })
        .collect::<BTreeMap<_, _>>();

    scan.edges.sort_by(|left, right| {
        (&left.site, left.kind, &left.guards).cmp(&(&right.site, right.kind, &right.guards))
    });
    dedup_preserving_order(&mut scan.call_nodes);
    dedup_preserving_order(&mut scan.effect_nodes);

    let (cfg_blocks, cfg_edges, cfg_reachable_blocks) = loop_cfg_slice(&callable.cfg, loop_node)?;
    let body_edges = bind_source_edges_to_cfg(&callable.cfg, loop_node, scan.edges)?;
    validate_loop_cfg_sites(
        &callable.cfg,
        loop_node,
        &body.node_id,
        &condition.node_id,
        &body_edges,
        &cfg_blocks,
        &cfg_edges,
    )?;
    let resource_edges = cfg_edges
        .iter()
        .map(|edge| {
            let from_input = callable.resources.in_states.get(&edge.from).cloned();
            let from_output = callable.resources.out_states.get(&edge.from).cloned();
            let to_input = callable.resources.in_states.get(&edge.to).cloned();
            if callable.cfg.reachable.contains(&edge.from)
                && (from_input.is_none() || from_output.is_none())
            {
                return Err(format!(
                    "reachable loop CFG edge '{}' has no complete ownership input/output snapshot at source block",
                    edge.id.0 .0
                ));
            }
            if callable.cfg.reachable.contains(&edge.to) && to_input.is_none() {
                return Err(format!(
                    "reachable loop CFG edge '{}' has no ownership input snapshot at target block",
                    edge.id.0 .0
                ));
            }
            Ok(TypedLoopResourceEdgeState {
                edge: edge.id.clone(),
                from_input,
                from_output,
                to_input,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let normal_exit_block = cfg_edges
        .iter()
        .find(|edge| edge.kind == EdgeKind::LoopExit && edge.source.node == *loop_node)
        .map(|edge| edge.to.clone())
        .ok_or_else(|| format!("loop '{}' has no exact normal-exit CFG edge", loop_node.0))?;
    let resource_blocks = cfg_blocks
        .iter()
        // This is the first block after the loop. Its input is already
        // covered by the condition-false/break edge snapshots; its output
        // and resource actions belong to subsequent source statements.
        .filter(|block| block.id != normal_exit_block)
        .map(|block| {
            let input = callable.resources.in_states.get(&block.id).cloned();
            let output = callable.resources.out_states.get(&block.id).cloned();
            if callable.cfg.reachable.contains(&block.id) && (input.is_none() || output.is_none()) {
                return Err(format!(
                    "reachable loop CFG block '{}' has no complete ownership input/output snapshot",
                    block.id.0 .0
                ));
            }
            Ok(TypedLoopResourceBlockState {
                block: block.id.clone(),
                input,
                output,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let loop_block_ids = resource_blocks
        .iter()
        .map(|state| state.block.clone())
        .collect::<BTreeSet<_>>();
    let mut resource_actions = callable
        .resources
        .actions
        .iter()
        .filter(|action| loop_block_ids.contains(&action.location.block))
        .cloned()
        .collect::<Vec<_>>();
    resource_actions.sort_by(|left, right| {
        fn action_rank(kind: crate::core::ownership::CanonicalActionKind) -> u8 {
            use crate::core::ownership::CanonicalActionKind as K;
            match kind {
                K::Read => 0,
                K::Write => 1,
                K::Introduce => 2,
                K::Move => 3,
                K::Drop => 4,
                K::Return => 5,
                K::TransferSession => 6,
                K::TransferChild => 7,
                K::BorrowShared => 8,
                K::BorrowMut => 9,
                K::BorrowEnd => 10,
            }
        }
        (
            &left.location.block,
            &left.location.point,
            &left.location.edge,
            action_rank(left.kind),
            &left.resource,
        )
            .cmp(&(
                &right.location.block,
                &right.location.point,
                &right.location.edge,
                action_rank(right.kind),
                &right.resource,
            ))
    });
    // Keep the full callable loan census. A loan can be live across this loop
    // even when both its start and end lie outside the loop CFG slice.
    let mut loans = callable.resources.loans.clone();
    loans.sort_by(|left, right| left.id.cmp(&right.id));

    let expression_sites = collect_typed_loop_expression_sites(
        callable, loop_node, scope, condition, body, invariants,
    )?;
    let mut receipt = TypedLoopProofReceipt {
        schema_version: TYPED_LOOP_PROOF_SCHEMA_VERSION,
        owner: callable.owner.clone(),
        loop_node: loop_node.clone(),
        scope: scope.to_vec(),
        body_node: body.node_id.clone(),
        condition_node: condition.node_id.clone(),
        invariant_points,
        state_locals: {
            let mut locals = BTreeSet::new();
            locals.extend(scan.state_reads.iter().cloned());
            locals.extend(written_locals.keys().cloned());
            locals.retain(|local| !scan.loop_local_bindings.contains(local));
            locals
                .into_iter()
                .map(|local| {
                    let info = callable.body.locals.get(&local).ok_or_else(|| {
                        format!(
                            "loop state local '{}' is absent from ResolvedBody",
                            local.0 .0
                        )
                    })?;
                    let write_sites = written_locals.get(&local).cloned().unwrap_or_default();
                    Ok(TypedLoopStateLocal {
                        local,
                        ty: info.ty.clone(),
                        written_in_loop: !write_sites.is_empty(),
                        write_sites,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?
        },
        writes,
        body_edges,
        old_snapshot: callable
            .body
            .parameters
            .iter()
            .chain(&callable.body.captures)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        call_nodes: scan.call_nodes,
        ffi_call_path: None,
        effect_nodes: scan.effect_nodes,
        cfg_blocks,
        cfg_edges,
        cfg_reachable_blocks,
        resource_edges,
        resource_blocks,
        resource_actions,
        loans,
        expression_sites,
        private_source_witness: [0; 32],
    };
    receipt.ffi_call_path = derive_typed_loop_ffi_call_path(callable, &receipt);
    receipt.private_source_witness = receipt.compute_private_source_witness();
    Ok(receipt)
}

/// Recompute and compare every receipt field against the typed body, source
/// CFG, and ownership analysis. The receipt alone never authorizes a proof.
pub fn validate_typed_loop_proof_receipt(
    receipt: &TypedLoopProofReceipt,
    callable: &ResolvedCallable,
    loop_node: &NodeId,
    scope: &[TypedProofScopeFrame],
    invariants: &[TypedLoopInvariantPoint],
) -> Result<(), String> {
    if !receipt.has_valid_source_witness() {
        return Err(format!(
            "typed loop receipt for '{}' has an invalid private source witness",
            loop_node.0
        ));
    }
    let expected = derive_typed_loop_proof_receipt(callable, loop_node, scope, invariants)?;
    if receipt != &expected {
        return Err(format!(
            "typed loop receipt for '{}' disagrees with checked body/CFG/ownership census",
            loop_node.0
        ));
    }
    Ok(())
}

fn loop_profile_scalar_type(ty: &ResolvedTypeId) -> bool {
    use crate::core::ir::PrimitiveType as P;

    [P::I32, P::I64, P::Bool]
        .into_iter()
        .any(|primitive| *ty == super::primitive_type_id(primitive))
}

/// Values used to steer a certified loop path must be immutable local scalar
/// reads and total scalar operations. In particular, this rejects globals,
/// projections, calls, references, and aggregate/string state that an
/// unknown FFI effect could influence between iterations.
fn loop_profile_stable_scalar_expression(body: &ResolvedBody, expression: &ResolvedExpr) -> bool {
    use ResolvedExprKind as E;

    if !expression.effects.is_empty() || !loop_profile_scalar_type(&expression.ty) {
        return false;
    }
    match &expression.kind {
        E::Literal(_) | E::Constant(_) => true,
        E::Load(place) => {
            place.projections.is_empty()
                && body
                    .locals
                    .get(&place.base)
                    .is_some_and(|local| !local.mutable && local.ty == expression.ty)
        }
        E::Unary { op, operand } => {
            matches!(op, super::ResolvedUnaryOp::Not)
                && loop_profile_stable_scalar_expression(body, operand)
        }
        E::Binary { op, left, right } => {
            matches!(
                op,
                super::ResolvedBinaryOp::Equal
                    | super::ResolvedBinaryOp::NotEqual
                    | super::ResolvedBinaryOp::Less
                    | super::ResolvedBinaryOp::Greater
                    | super::ResolvedBinaryOp::LessEqual
                    | super::ResolvedBinaryOp::GreaterEqual
                    | super::ResolvedBinaryOp::LogicalAnd
                    | super::ResolvedBinaryOp::LogicalOr
            ) && loop_profile_stable_scalar_expression(body, left)
                && loop_profile_stable_scalar_expression(body, right)
        }
        E::Cast { value, conversion } => {
            matches!(
                conversion.kind,
                crate::core::ir::CheckedConversionKind::Identity
                    | crate::core::ir::CheckedConversionKind::NumericWiden
            ) && loop_profile_stable_scalar_expression(body, value)
        }
        _ => false,
    }
}

/// A branch block is accepted as a loop-prefix selector only when it either
/// falls through empty or consists of exactly one direct break/continue.
/// `None` means it contains any unsupported work.
fn loop_profile_branch_falls_through(block: &crate::core::ir::ResolvedBlock) -> Option<bool> {
    if block.result.is_some() {
        return None;
    }
    match block.statements.as_slice() {
        [] => Some(true),
        [statement] => match &statement.kind {
            ResolvedStmtKind::Break(None) | ResolvedStmtKind::Continue => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Derive the only loop-call profile currently admitted by the FFI path
/// checker: one direct scalar extern call, reached through a pure immutable
/// guard prefix, followed in the same CFG block by an unconditional break.
/// Other paths may break or continue before that call. The source receipt and
/// CFG provide the exact condition, guards, terminal target, and call identity;
/// this profile never claims that the loop body executes.
pub fn derive_typed_loop_ffi_call_path(
    callable: &ResolvedCallable,
    receipt: &TypedLoopProofReceipt,
) -> Option<TypedLoopFfiCallPath> {
    use crate::core::ir::PrimitiveType;

    let statement = find_unique_while_statement(&callable.body.root, &receipt.loop_node).ok()?;
    let ResolvedStmtKind::While { condition, body } = &statement.kind else {
        return None;
    };
    if condition.node_id != receipt.condition_node
        || body.node_id != receipt.body_node
        || !receipt.invariant_points.is_empty()
        || !receipt.writes.is_empty()
        || !receipt.effect_nodes.is_empty()
        || !receipt.loans.is_empty()
        || receipt
            .resource_actions
            .iter()
            .any(|action| action.kind != crate::core::ownership::CanonicalActionKind::Read)
        || !loop_profile_stable_scalar_expression(&callable.body, condition)
        || condition.ty != super::primitive_type_id(PrimitiveType::Bool)
        || receipt.body_edges.is_empty()
        || receipt.body_edges.iter().any(|edge| {
            !edge.reachable
                || edge.target_loop.as_ref() != Some(&receipt.loop_node)
                || !matches!(
                    edge.kind,
                    TypedLoopEdgeKind::Break | TypedLoopEdgeKind::Continue
                )
        })
    {
        return None;
    }

    let mut call_guards = Vec::new();
    let mut call_statement = None;
    let mut call_node = None;
    let mut terminal_break = None;
    let mut saw_call = false;
    let mut saw_terminal_break = false;
    let mut prefix_branch_count = 0usize;
    for (index, nested) in body.statements.iter().enumerate() {
        if !saw_call {
            if let ResolvedStmtKind::Expr(expression) = &nested.kind {
                if let ResolvedExprKind::If {
                    condition: branch_condition,
                    then_block,
                    else_block,
                } = &expression.kind
                {
                    prefix_branch_count += 1;
                    if prefix_branch_count > 1 {
                        return None;
                    }
                    if !loop_profile_stable_scalar_expression(&callable.body, branch_condition)
                        || branch_condition.ty != super::primitive_type_id(PrimitiveType::Bool)
                    {
                        return None;
                    }
                    let then_falls_through = loop_profile_branch_falls_through(then_block)?;
                    let else_falls_through = loop_profile_branch_falls_through(else_block)?;
                    if then_falls_through == else_falls_through {
                        return None;
                    }
                    call_guards.push(TypedLoopBranchGuard {
                        statement: nested.node_id.clone(),
                        expression: expression.node_id.clone(),
                        condition: branch_condition.node_id.clone(),
                        then_arm: then_falls_through,
                    });
                    continue;
                }
                if let ResolvedExprKind::Call(call) = &expression.kind {
                    if !matches!(call.callee, ResolvedCallee::Extern(_))
                        || !call.type_arguments.is_empty()
                        || call.result != expression.ty
                        || !loop_profile_scalar_type(&call.result)
                        || !expression.effects.is_empty()
                        || !call.effects.is_empty()
                        || call.arguments.iter().any(|argument| {
                            !loop_profile_stable_scalar_expression(&callable.body, &argument.value)
                                || !loop_profile_scalar_type(&argument.conversion.from)
                                || !loop_profile_scalar_type(&argument.conversion.to)
                                || !matches!(
                                    argument.conversion.kind,
                                    crate::core::ir::CheckedConversionKind::Identity
                                        | crate::core::ir::CheckedConversionKind::NumericWiden
                                )
                        })
                    {
                        return None;
                    }
                    saw_call = true;
                    call_statement = Some(nested.node_id.clone());
                    call_node = Some(expression.node_id.clone());
                    continue;
                }
            }
            return None;
        }

        if saw_terminal_break || index + 1 != body.statements.len() {
            return None;
        }
        if matches!(&nested.kind, ResolvedStmtKind::Break(None)) {
            saw_terminal_break = true;
            terminal_break = Some(nested.node_id.clone());
        } else {
            return None;
        }
    }
    let call_node_ref = call_node.as_ref()?;
    if !saw_call
        || !saw_terminal_break
        || body.result.is_some()
        || receipt.call_nodes.as_slice() != std::slice::from_ref(call_node_ref)
    {
        return None;
    }

    let call_statement = call_statement?;
    let call_node = call_node?;
    let terminal_break = terminal_break?;
    if call_guards.len() != prefix_branch_count || call_guards.len() > 1 {
        return None;
    }
    let expected_body_edge_count = 1 + usize::from(!call_guards.is_empty());
    if receipt.body_edges.len() != expected_body_edge_count {
        return None;
    }
    let terminal_break_edges = receipt
        .body_edges
        .iter()
        .filter(|edge| {
            edge.site == terminal_break
                && edge.kind == TypedLoopEdgeKind::Break
                && edge.target_loop.as_ref() == Some(&receipt.loop_node)
                && edge.guards == call_guards
        })
        .collect::<Vec<_>>();
    if terminal_break_edges.len() != 1 {
        return None;
    }
    if let Some(call_guard) = call_guards.first() {
        let early_terminal_edges = receipt
            .body_edges
            .iter()
            .filter(|edge| edge.site != terminal_break)
            .collect::<Vec<_>>();
        let [early_terminal] = early_terminal_edges.as_slice() else {
            return None;
        };
        let mut complementary_guard = call_guard.clone();
        complementary_guard.then_arm = !complementary_guard.then_arm;
        if early_terminal.guards != [complementary_guard]
            || early_terminal.target_loop.as_ref() != Some(&receipt.loop_node)
            || !matches!(
                early_terminal.kind,
                TypedLoopEdgeKind::Break | TypedLoopEdgeKind::Continue
            )
        {
            return None;
        }
    }
    let call_blocks = receipt
        .cfg_blocks
        .iter()
        .filter(|block| {
            block
                .points
                .iter()
                .any(|point| point.source.node == call_node)
        })
        .collect::<Vec<_>>();
    let [call_block] = call_blocks.as_slice() else {
        return None;
    };
    let break_edge = terminal_break_edges[0];
    let call_point_occurrences = receipt
        .cfg_blocks
        .iter()
        .flat_map(|block| block.points.iter())
        .filter(|point| point.source.node == call_node)
        .count();
    let break_point_occurrences = receipt
        .cfg_blocks
        .iter()
        .flat_map(|block| block.points.iter())
        .filter(|point| point.source.node == terminal_break)
        .count();
    if call_point_occurrences != 1 || break_point_occurrences != 1 {
        return None;
    }
    let body_entry = receipt
        .cfg_edges
        .iter()
        .find(|edge| edge.id.0 .0 == format!("{}/cfg:edge:while-body", receipt.loop_node.0))?
        .to
        .clone();
    if call_block.id != break_edge.cfg_terminal_block
        || !receipt.cfg_reachable_blocks.contains(&call_block.id)
        || !cfg_guard_path_reaches(
            &callable.cfg,
            &body_entry,
            &call_block.id,
            &receipt.loop_node,
            &call_guards,
        )
        .ok()?
    {
        return None;
    }
    let call_point_index = call_block
        .points
        .iter()
        .position(|point| point.source.node == call_node)?;
    let break_point_index = call_block
        .points
        .iter()
        .position(|point| point.source.node == terminal_break)?;
    if call_point_index >= break_point_index {
        return None;
    }

    Some(TypedLoopFfiCallPath {
        loop_node: receipt.loop_node.clone(),
        condition_node: receipt.condition_node.clone(),
        call_statement,
        call_node,
        call_guards,
        terminal_break,
        terminal_block: call_block.id.clone(),
    })
}

struct SourceLoopScan<'a> {
    body: &'a ResolvedBody,
    loop_node: NodeId,
    writes: Vec<TypedLoopWriteSite>,
    edges: Vec<ScannedLoopEdge>,
    call_nodes: Vec<NodeId>,
    effect_nodes: Vec<NodeId>,
    state_reads: BTreeSet<ResolvedLocalId>,
    loop_local_bindings: BTreeSet<ResolvedLocalId>,
}

type GuardPath = Vec<TypedLoopBranchGuard>;

impl<'a> SourceLoopScan<'a> {
    fn new(body: &'a ResolvedBody, loop_node: &NodeId) -> Self {
        Self {
            body,
            loop_node: loop_node.clone(),
            writes: Vec::new(),
            edges: Vec::new(),
            call_nodes: Vec::new(),
            effect_nodes: Vec::new(),
            state_reads: BTreeSet::new(),
            loop_local_bindings: BTreeSet::new(),
        }
    }

    /// Returns every normal fallthrough path; terminal break/continue/return
    /// paths are recorded in `edges` and never rejoined into the next stmt.
    fn block(
        &mut self,
        block: &crate::core::ir::ResolvedBlock,
        guards: &[TypedLoopBranchGuard],
    ) -> Result<Vec<GuardPath>, String> {
        let mut fallthrough = vec![guards.to_vec()];
        for statement in &block.statements {
            if fallthrough.is_empty() {
                // Dormant expressions remain in the source census. We refuse
                // a loop receipt if the dormant subtree introduces a control
                // construct whose proof/effect behavior is not modeled.
                self.scan_statement_expressions(statement)?;
                if statement_has_unsupported_nested_control(statement) {
                    return Err(format!(
                        "loop body '{}' contains dormant unsupported control at '{}'",
                        block.node_id.0, statement.node_id.0
                    ));
                }
                continue;
            }
            let mut next = Vec::new();
            for path in fallthrough {
                match &statement.kind {
                    ResolvedStmtKind::Break(value) => {
                        if let Some(value) = value {
                            self.expression(value)?;
                        }
                        self.edges.push(ScannedLoopEdge {
                            site: statement.node_id.clone(),
                            kind: TypedLoopEdgeKind::Break,
                            target_loop: Some(self.loop_node.clone()),
                            guards: path,
                            terminal_value: value.as_ref().map(|value| value.node_id.clone()),
                        });
                    }
                    ResolvedStmtKind::Continue => self.edges.push(ScannedLoopEdge {
                        site: statement.node_id.clone(),
                        kind: TypedLoopEdgeKind::Continue,
                        target_loop: Some(self.loop_node.clone()),
                        guards: path,
                        terminal_value: None,
                    }),
                    ResolvedStmtKind::Return { value, .. } => {
                        if let Some(value) = value {
                            self.expression(value)?;
                        }
                        self.edges.push(ScannedLoopEdge {
                            site: statement.node_id.clone(),
                            kind: TypedLoopEdgeKind::Return,
                            target_loop: None,
                            guards: path,
                            terminal_value: value.as_ref().map(|value| value.node_id.clone()),
                        });
                    }
                    ResolvedStmtKind::Assign {
                        target,
                        value,
                        conversion,
                    } => {
                        // Canonical CFG lowering evaluates the RHS before
                        // dynamic target-place inputs; preserve that order.
                        self.expression(value)?;
                        self.place_inputs(target)?;
                        self.writes.push(TypedLoopWriteSite {
                            statement: statement.node_id.clone(),
                            target: target.clone(),
                            value: value.node_id.clone(),
                            conversion: conversion.clone(),
                        });
                        next.push(path);
                    }
                    ResolvedStmtKind::Bind {
                        pattern,
                        initializer,
                    } => {
                        collect_pattern_locals(pattern, &mut self.loop_local_bindings);
                        if let Some(initializer) = initializer {
                            self.expression(initializer)?;
                        }
                        next.push(path);
                    }
                    ResolvedStmtKind::Expr(expression) => {
                        next.extend(self.expression_flow(expression, &statement.node_id, path)?);
                    }
                    ResolvedStmtKind::Scope { body, .. } => {
                        next.extend(self.block(body, &path)?);
                    }
                    ResolvedStmtKind::Contract { condition, .. } => {
                        self.expression(condition)?;
                        next.push(path);
                    }
                    ResolvedStmtKind::Math(expressions) => {
                        for expression in expressions {
                            self.expression(expression)?;
                        }
                        next.push(path);
                    }
                    ResolvedStmtKind::While { .. }
                    | ResolvedStmtKind::WhileLet { .. }
                    | ResolvedStmtKind::Loop(_)
                    | ResolvedStmtKind::For { .. }
                    | ResolvedStmtKind::IfLet { .. }
                    | ResolvedStmtKind::Pinned { .. }
                    | ResolvedStmtKind::Drop(_)
                    | ResolvedStmtKind::NestedCallable(_) => {
                        self.scan_statement_expressions(statement)?;
                        return Err(format!(
                            "loop body '{}' contains unsupported nested control, ownership, or callable statement at '{}'",
                            block.node_id.0, statement.node_id.0
                        ));
                    }
                }
            }
            fallthrough = next;
        }
        if let Some(result) = &block.result {
            let mut next = Vec::new();
            for path in fallthrough {
                // The typed proof worklist records a block-tail expression
                // under its enclosing block NodeId, while `expression` is
                // retained separately as the dual source branch identity.
                next.extend(self.expression_flow(result, &block.node_id, path)?);
            }
            Ok(next)
        } else {
            Ok(fallthrough)
        }
    }

    fn expression_flow(
        &mut self,
        expression: &ResolvedExpr,
        worklist_statement: &NodeId,
        guards: GuardPath,
    ) -> Result<Vec<GuardPath>, String> {
        match &expression.kind {
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expression(condition)?;
                let then_path = guards
                    .iter()
                    .cloned()
                    .chain([TypedLoopBranchGuard {
                        statement: worklist_statement.clone(),
                        expression: expression.node_id.clone(),
                        condition: condition.node_id.clone(),
                        then_arm: true,
                    }])
                    .collect::<GuardPath>();
                let else_path = guards
                    .iter()
                    .cloned()
                    .chain([TypedLoopBranchGuard {
                        statement: worklist_statement.clone(),
                        expression: expression.node_id.clone(),
                        condition: condition.node_id.clone(),
                        then_arm: false,
                    }])
                    .collect::<GuardPath>();
                let mut result = self.block(then_block, &then_path)?;
                result.extend(self.block(else_block, &else_path)?);
                Ok(result)
            }
            ResolvedExprKind::Block(block) | ResolvedExprKind::Scope { body: block, .. } => {
                self.block(block, &guards)
            }
            _ => {
                self.expression(expression)?;
                Ok(vec![guards])
            }
        }
    }

    fn scan_statement_expressions(&mut self, statement: &ResolvedStmt) -> Result<(), String> {
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                collect_pattern_locals(pattern, &mut self.loop_local_bindings);
                if let Some(value) = initializer {
                    self.expression_census(value)?;
                }
            }
            ResolvedStmtKind::Assign {
                target,
                value,
                conversion,
            } => {
                self.expression_census(value)?;
                self.place_inputs_census(target)?;
                self.writes.push(TypedLoopWriteSite {
                    statement: statement.node_id.clone(),
                    target: target.clone(),
                    value: value.node_id.clone(),
                    conversion: conversion.clone(),
                });
            }
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    self.expression_census(value)?;
                }
            }
            ResolvedStmtKind::Expr(expression)
            | ResolvedStmtKind::Contract {
                condition: expression,
                ..
            } => self.expression_census(expression)?,
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    self.expression_census(expression)?;
                }
            }
            ResolvedStmtKind::While { condition, body } => {
                self.expression_census(condition)?;
                self.scan_block_census(body)?;
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                self.expression_census(initializer)?;
                self.scan_block_census(body)?;
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                self.expression_census(initializer)?;
                self.scan_block_census(then_block)?;
                if let Some(block) = else_block {
                    self.scan_block_census(block)?;
                }
            }
            ResolvedStmtKind::Scope { body, .. } | ResolvedStmtKind::Loop(body) => {
                self.scan_block_census(body)?;
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                self.expression_census(value)?;
                self.scan_block_census(body)?;
            }
            ResolvedStmtKind::Drop(places) => {
                for place in places {
                    self.place_inputs_census(place)?;
                }
            }
            ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {}
        }
        Ok(())
    }

    fn scan_block_census(&mut self, block: &crate::core::ir::ResolvedBlock) -> Result<(), String> {
        for statement in &block.statements {
            self.scan_statement_expressions(statement)?;
        }
        if let Some(result) = &block.result {
            self.expression_census(result)?;
        }
        Ok(())
    }

    fn place_inputs(&mut self, place: &ResolvedPlace) -> Result<(), String> {
        for projection in &place.projections {
            if let ResolvedProjection::Index {
                index: crate::core::ResolvedIndex::Dynamic(node),
                ..
            } = projection
            {
                let input = self.body.place_inputs.get(node).ok_or_else(|| {
                    format!(
                        "dynamic loop place input '{}' is absent from ResolvedBody",
                        node.0
                    )
                })?;
                self.expression(input)?;
            }
        }
        Ok(())
    }

    fn place_inputs_census(&mut self, place: &ResolvedPlace) -> Result<(), String> {
        for projection in &place.projections {
            if let ResolvedProjection::Index {
                index: crate::core::ResolvedIndex::Dynamic(node),
                ..
            } = projection
            {
                let input = self.body.place_inputs.get(node).ok_or_else(|| {
                    format!(
                        "dynamic loop place input '{}' is absent from ResolvedBody",
                        node.0
                    )
                })?;
                self.expression_census(input)?;
            }
        }
        Ok(())
    }

    /// Evaluation-order traversal used for executable source nodes.
    fn expression(&mut self, expression: &ResolvedExpr) -> Result<(), String> {
        if !expression.effects.is_empty() {
            self.effect_nodes.push(expression.node_id.clone());
        }
        match &expression.kind {
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    self.expression(&argument.value)?;
                }
                if !call.effects.is_empty() {
                    self.effect_nodes.push(expression.node_id.clone());
                }
                // Calls remain effectful unknowns even when both checked
                // effect vectors are empty; consumers require a matching
                // checker-sealed call receipt and conservative havoc.
                self.call_nodes.push(expression.node_id.clone());
            }
            ResolvedExprKind::Project { value, projection } => {
                self.expression(value)?;
                if let super::ResolvedValueProjection::Index(index) = projection {
                    self.expression(index)?;
                }
            }
            ResolvedExprKind::Binary { left, right, .. }
            | ResolvedExprKind::Range {
                start: left,
                end: right,
            } => {
                self.expression(left)?;
                self.expression(right)?;
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Cast { value: operand, .. } => self.expression(operand)?,
            ResolvedExprKind::Old(operand) => {
                let ResolvedExprKind::Load(place) = &operand.kind else {
                    // Preserve whole-root call/effect census even when a
                    // malformed Old shape is rejected from this loop profile.
                    self.expression_census(operand)?;
                    return Err(format!(
                        "old() in loop '{}' must name a direct callable-entry parameter",
                        expression.node_id.0
                    ));
                };
                if !self.body.parameters.contains(&place.base) {
                    return Err(format!(
                        "old() in loop '{}' does not name a callable-entry parameter",
                        expression.node_id.0
                    ));
                }
                // Old reads the immutable callable-entry snapshot. It must
                // not add the parameter to the mutable loop-header state map.
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => {
                self.expression(receiver)?;
                return Err(format!(
                    "optional-chain expression '{}' has an unmodeled conditional evaluation path",
                    expression.node_id.0
                ));
            }
            ResolvedExprKind::Try { value, .. } => {
                self.expression(value)?;
                return Err(format!(
                    "try expression '{}' has an unmodeled early-return path",
                    expression.node_id.0
                ));
            }
            ResolvedExprKind::Spawn(value) | ResolvedExprKind::Await(value) => {
                self.expression(value)?;
                return Err(format!(
                    "spawn/await expression '{}' has an unmodeled scheduler or effect path",
                    expression.node_id.0
                ));
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.expression(value)?;
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.expression(key)?;
                    self.expression(value)?;
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.expression(iterable)?;
                self.expression(value)?;
                if let Some(guard) = guard {
                    self.expression(guard)?;
                }
                return Err(format!(
                    "comprehension '{}' has repeated/conditional evaluation outside the scalar loop profile",
                    expression.node_id.0
                ));
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.expression(&field.value)?;
                }
                if let Some(rest) = rest {
                    self.expression(rest)?;
                }
            }
            ResolvedExprKind::Block(_)
            | ResolvedExprKind::Scope { .. }
            | ResolvedExprKind::Comptime(_)
            | ResolvedExprKind::Quote(_)
            | ResolvedExprKind::If { .. }
            | ResolvedExprKind::Match { .. } => {
                return Err(format!(
                    "nested control expression '{}' is outside the checked loop-expression profile",
                    expression.node_id.0
                ));
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.expression(target)?;
                if let Some(start) = start {
                    self.expression(start)?;
                }
                if let Some(end) = end {
                    self.expression(end)?;
                }
            }
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                        self.expression(value)?;
                    }
                }
            }
            ResolvedExprKind::Lambda(_) => {
                return Err(format!(
                    "lambda expression '{}' creates a nested callable inside a mutable loop",
                    expression.node_id.0
                ));
            }
            ResolvedExprKind::Load(place) => {
                self.state_reads.insert(place.base.clone());
                self.place_inputs(place)?;
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
        Ok(())
    }

    /// Census-only traversal for dormant/unreachable syntax: retain nested
    /// calls/effects even though this receipt will normally reject hidden
    /// control before it can authorize any loop proof.
    fn expression_census(&mut self, expression: &ResolvedExpr) -> Result<(), String> {
        if !expression.effects.is_empty() {
            self.effect_nodes.push(expression.node_id.clone());
        }
        match &expression.kind {
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    self.expression_census(&argument.value)?;
                }
                if !call.effects.is_empty() {
                    self.effect_nodes.push(expression.node_id.clone());
                }
                self.call_nodes.push(expression.node_id.clone());
            }
            ResolvedExprKind::Project { value, projection } => {
                self.expression_census(value)?;
                if let super::ResolvedValueProjection::Index(index) = projection {
                    self.expression_census(index)?;
                }
            }
            ResolvedExprKind::Binary { left, right, .. }
            | ResolvedExprKind::Range {
                start: left,
                end: right,
            } => {
                self.expression_census(left)?;
                self.expression_census(right)?;
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::TypeOf(operand)
            | ResolvedExprKind::Try { value: operand, .. }
            | ResolvedExprKind::Cast { value: operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.expression_census(operand)?,
            ResolvedExprKind::Old(operand) => {
                if let ResolvedExprKind::Load(place) = &operand.kind {
                    if !self.body.parameters.contains(&place.base) {
                        return Err(format!(
                            "old() in loop expression '{}' does not name a callable-entry parameter",
                            expression.node_id.0
                        ));
                    }
                    // The entry snapshot is sealed separately; do not treat
                    // this as a current-state read in the loop body.
                } else {
                    // Census nested nodes before rejecting an unsupported
                    // Old shape so a hidden call cannot vanish from the root
                    // call/effect inventory.
                    self.expression_census(operand)?;
                    return Err(format!(
                        "old() in loop expression '{}' must name a direct callable-entry parameter",
                        expression.node_id.0
                    ));
                }
            }
            ResolvedExprKind::OptionalChain { receiver, .. } => {
                self.expression_census(receiver)?;
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.expression_census(value)?;
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.expression_census(key)?;
                    self.expression_census(value)?;
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                self.expression_census(iterable)?;
                self.expression_census(value)?;
                if let Some(guard) = guard {
                    self.expression_census(guard)?;
                }
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    self.expression_census(&field.value)?;
                }
                if let Some(rest) = rest {
                    self.expression_census(rest)?;
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => self.scan_block_census(block)?,
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expression_census(condition)?;
                self.scan_block_census(then_block)?;
                self.scan_block_census(else_block)?;
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.expression_census(scrutinee)?;
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.expression_census(guard)?;
                    }
                    self.expression_census(&arm.body)?;
                }
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.expression_census(target)?;
                if let Some(start) = start {
                    self.expression_census(start)?;
                }
                if let Some(end) = end {
                    self.expression_census(end)?;
                }
            }
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                        self.expression_census(value)?;
                    }
                }
            }
            ResolvedExprKind::Lambda(_) => {}
            ResolvedExprKind::Load(place) => {
                self.state_reads.insert(place.base.clone());
                self.place_inputs_census(place)?;
            }
            ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
        Ok(())
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

fn statement_has_unsupported_nested_control(statement: &ResolvedStmt) -> bool {
    matches!(
        &statement.kind,
        ResolvedStmtKind::While { .. }
            | ResolvedStmtKind::WhileLet { .. }
            | ResolvedStmtKind::Loop(_)
            | ResolvedStmtKind::For { .. }
            | ResolvedStmtKind::IfLet { .. }
            | ResolvedStmtKind::Pinned { .. }
            | ResolvedStmtKind::Drop(_)
            | ResolvedStmtKind::NestedCallable(_)
    )
}

fn dedup_preserving_order(nodes: &mut Vec<NodeId>) {
    let mut seen = BTreeSet::new();
    nodes.retain(|node| seen.insert(node.clone()));
}

fn find_unique_while_statement<'a>(
    root: &'a crate::core::ir::ResolvedBlock,
    target: &NodeId,
) -> Result<&'a ResolvedStmt, String> {
    fn visit_expr<'a>(
        expression: &'a ResolvedExpr,
        target: &NodeId,
        out: &mut Vec<&'a ResolvedStmt>,
    ) {
        match &expression.kind {
            ResolvedExprKind::Call(call) => {
                for argument in &call.arguments {
                    visit_expr(&argument.value, target, out);
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                visit_expr(value, target, out);
                if let super::ResolvedValueProjection::Index(index) = projection {
                    visit_expr(index, target, out);
                }
            }
            ResolvedExprKind::Binary { left, right, .. }
            | ResolvedExprKind::Range {
                start: left,
                end: right,
            } => {
                visit_expr(left, target, out);
                visit_expr(right, target, out);
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
            | ResolvedExprKind::Await(operand) => visit_expr(operand, target, out),
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    visit_expr(value, target, out);
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    visit_expr(key, target, out);
                    visit_expr(value, target, out);
                }
            }
            ResolvedExprKind::Comprehension {
                value,
                iterable,
                guard,
                ..
            } => {
                visit_expr(iterable, target, out);
                visit_expr(value, target, out);
                if let Some(guard) = guard {
                    visit_expr(guard, target, out);
                }
            }
            ResolvedExprKind::Record { fields, rest, .. } => {
                for field in fields {
                    visit_expr(&field.value, target, out);
                }
                if let Some(rest) = rest {
                    visit_expr(rest, target, out);
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Scope { body: block, .. }
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => visit_block(block, target, out),
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                visit_expr(condition, target, out);
                visit_block(then_block, target, out);
                visit_block(else_block, target, out);
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                visit_expr(scrutinee, target, out);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit_expr(guard, target, out);
                    }
                    visit_expr(&arm.body, target, out);
                }
            }
            ResolvedExprKind::Slice {
                target: value,
                start,
                end,
            } => {
                visit_expr(value, target, out);
                if let Some(start) = start {
                    visit_expr(start, target, out);
                }
                if let Some(end) = end {
                    visit_expr(end, target, out);
                }
            }
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let crate::core::ir::ResolvedFStringPart::Interpolation(value) = part {
                        visit_expr(value, target, out);
                    }
                }
            }
            // Lambda bodies have their own callable owner and are not
            // statements in this callable's loop tree.
            ResolvedExprKind::Lambda(_)
            | ResolvedExprKind::Load(_)
            | ResolvedExprKind::Literal(_)
            | ResolvedExprKind::Constant(_)
            | ResolvedExprKind::Callable(_)
            | ResolvedExprKind::DefaultArgument { .. }
            | ResolvedExprKind::ComptimeValue(_)
            | ResolvedExprKind::TypeValue(_) => {}
        }
    }

    fn visit_block<'a>(
        block: &'a crate::core::ir::ResolvedBlock,
        target: &NodeId,
        out: &mut Vec<&'a ResolvedStmt>,
    ) {
        for statement in &block.statements {
            visit_stmt(statement, target, out);
        }
        if let Some(result) = &block.result {
            visit_expr(result, target, out);
        }
    }

    fn visit_stmt<'a>(
        statement: &'a ResolvedStmt,
        target: &NodeId,
        out: &mut Vec<&'a ResolvedStmt>,
    ) {
        if statement.node_id == *target && matches!(&statement.kind, ResolvedStmtKind::While { .. })
        {
            out.push(statement);
        }
        match &statement.kind {
            ResolvedStmtKind::Bind { initializer, .. } => {
                if let Some(value) = initializer {
                    visit_expr(value, target, out);
                }
            }
            ResolvedStmtKind::Assign { value, .. } => visit_expr(value, target, out),
            ResolvedStmtKind::Return { value, .. } | ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    visit_expr(value, target, out);
                }
            }
            ResolvedStmtKind::Continue => {}
            ResolvedStmtKind::Expr(value)
            | ResolvedStmtKind::Contract {
                condition: value, ..
            } => {
                visit_expr(value, target, out);
            }
            ResolvedStmtKind::While { condition, body } => {
                visit_expr(condition, target, out);
                visit_block(body, target, out);
            }
            ResolvedStmtKind::WhileLet {
                initializer, body, ..
            }
            | ResolvedStmtKind::For {
                iterable: initializer,
                body,
                ..
            } => {
                visit_expr(initializer, target, out);
                visit_block(body, target, out);
            }
            ResolvedStmtKind::IfLet {
                initializer,
                then_block,
                else_block,
                ..
            } => {
                visit_expr(initializer, target, out);
                visit_block(then_block, target, out);
                if let Some(block) = else_block {
                    visit_block(block, target, out);
                }
            }
            ResolvedStmtKind::Loop(body) | ResolvedStmtKind::Scope { body, .. } => {
                visit_block(body, target, out);
            }
            ResolvedStmtKind::Pinned { value, body, .. } => {
                visit_expr(value, target, out);
                visit_block(body, target, out);
            }
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    visit_expr(expression, target, out);
                }
            }
            ResolvedStmtKind::Drop(_) | ResolvedStmtKind::NestedCallable(_) => {}
        }
    }

    let mut matches = Vec::new();
    visit_block(root, target, &mut matches);
    match matches.as_slice() {
        [statement] => Ok(*statement),
        [] => Err(format!(
            "loop '{}' is absent from the exact ResolvedBody",
            target.0
        )),
        _ => Err(format!(
            "loop '{}' has duplicate source identities",
            target.0
        )),
    }
}

fn cfg_edge_id(source: &NodeId, role: &str) -> EdgeId {
    EdgeId(NodeId(format!("{}/cfg:edge:{role}", source.0)))
}

fn bind_source_edges_to_cfg(
    cfg: &CallableCfg,
    loop_node: &NodeId,
    scanned: Vec<ScannedLoopEdge>,
) -> Result<Vec<TypedLoopEdgeSite>, String> {
    let mut result = Vec::with_capacity(scanned.len());
    for source in scanned {
        let (terminal_block, terminal_edge) = match source.kind {
            TypedLoopEdgeKind::BodyFallthrough => {
                let edge_id = cfg_edge_id(loop_node, "while-backedge");
                let edge = cfg.edges.get(&edge_id).ok_or_else(|| {
                    format!(
                        "loop '{}' has no exact body-fallthrough backedge",
                        loop_node.0
                    )
                })?;
                (edge.from.clone(), Some(edge_id))
            }
            TypedLoopEdgeKind::Continue => {
                let edge_id = cfg_edge_id(&source.site, "continue");
                let edge = cfg.edges.get(&edge_id).ok_or_else(|| {
                    format!(
                        "loop '{}' has no CFG continue edge for '{}'",
                        loop_node.0, source.site.0
                    )
                })?;
                (edge.from.clone(), Some(edge_id))
            }
            TypedLoopEdgeKind::Break => {
                let edge_id = cfg_edge_id(&source.site, "break");
                let edge = cfg.edges.get(&edge_id).ok_or_else(|| {
                    format!(
                        "loop '{}' has no CFG break edge for '{}'",
                        loop_node.0, source.site.0
                    )
                })?;
                (edge.from.clone(), Some(edge_id))
            }
            TypedLoopEdgeKind::Return => {
                let mut matches = cfg.blocks.values().filter(|block| {
                    block
                        .points
                        .iter()
                        .any(|point| point.source.node == source.site)
                        && matches!(
                            &block.terminator,
                            Terminator::Return { value, implicit: false }
                                if value == &source.terminal_value
                        )
                });
                let block = matches.next().ok_or_else(|| {
                    format!(
                        "loop return '{}' has no exact CFG Return terminator",
                        source.site.0
                    )
                })?;
                if matches.next().is_some() {
                    return Err(format!(
                        "loop return '{}' matches multiple CFG Return terminators",
                        source.site.0
                    ));
                }
                (block.id.clone(), None)
            }
        };
        result.push(TypedLoopEdgeSite {
            site: source.site,
            kind: source.kind,
            target_loop: source.target_loop,
            guards: source.guards,
            cfg_terminal_block: terminal_block.clone(),
            cfg_terminal_edge: terminal_edge,
            reachable: cfg.reachable.contains(&terminal_block),
            terminal_value: source.terminal_value,
        });
    }
    Ok(result)
}

fn validate_loop_cfg_sites(
    cfg: &CallableCfg,
    loop_node: &NodeId,
    body_node: &NodeId,
    condition_node: &NodeId,
    source_sites: &[TypedLoopEdgeSite],
    blocks: &[BasicBlock],
    edges: &[CfgEdge],
) -> Result<(), String> {
    let entry = cfg_edge_id(loop_node, "while-enter");
    let body = cfg_edge_id(loop_node, "while-body");
    let exit = cfg_edge_id(loop_node, "while-exit");
    let backedge = cfg_edge_id(loop_node, "while-backedge");
    let get = |id: &EdgeId| {
        cfg.edges.get(id).ok_or_else(|| {
            format!(
                "loop '{}' is missing exact CFG edge '{}'",
                loop_node.0, id.0 .0
            )
        })
    };
    let entry_edge = get(&entry)?;
    let body_edge = get(&body)?;
    let exit_edge = get(&exit)?;
    if entry_edge.to != body_edge.from
        || body_edge.from != exit_edge.from
        || entry_edge.source.node != *loop_node
        || body_edge.source.node != *loop_node
        || exit_edge.source.node != *loop_node
        || entry_edge.kind != EdgeKind::Fallthrough
        || body_edge.kind != EdgeKind::LoopBody
        || exit_edge.kind != EdgeKind::LoopExit
    {
        return Err(format!(
            "loop '{}' entry/body/exit CFG edge identities do not form one checked header",
            loop_node.0
        ));
    }
    let header = &entry_edge.to;
    let exit_block = &exit_edge.to;
    let header_block = cfg
        .blocks
        .get(header)
        .ok_or_else(|| format!("loop '{}' exact CFG header block is absent", loop_node.0))?;
    if !matches!(
        &header_block.terminator,
        Terminator::Branch { condition, then_edge, else_edge }
            if condition == condition_node
                && then_edge == &body
                && else_edge == &exit
    ) {
        return Err(format!(
            "loop '{}' header terminator does not bind its checked condition and exact body/exit edges",
            loop_node.0
        ));
    }

    let included_edges = edges
        .iter()
        .map(|edge| edge.id.clone())
        .collect::<BTreeSet<_>>();
    if ![entry.clone(), body.clone(), exit.clone()]
        .iter()
        .all(|id| included_edges.contains(id))
    {
        return Err(format!(
            "loop '{}' CFG receipt omits an entry/header/exit edge",
            loop_node.0
        ));
    }

    let mut mapped_terminal_edges = BTreeSet::new();
    for site in source_sites {
        let expected_target_loop = matches!(
            site.kind,
            TypedLoopEdgeKind::Break | TypedLoopEdgeKind::Continue
        )
        .then(|| loop_node.clone());
        if site.target_loop != expected_target_loop {
            return Err(format!(
                "loop edge site '{}' has an incorrect lexical loop target",
                site.site.0
            ));
        }
        if !blocks
            .iter()
            .any(|block| block.id == site.cfg_terminal_block)
        {
            return Err(format!(
                "loop edge site '{}' terminal block is outside the exact CFG slice",
                site.site.0
            ));
        }
        match site.kind {
            TypedLoopEdgeKind::BodyFallthrough => {
                let edge = get(&backedge)?;
                if edge.kind != EdgeKind::Backedge
                    || edge.from != site.cfg_terminal_block
                    || &edge.to != header
                    || edge.source.node != *loop_node
                    || site.cfg_terminal_edge.as_ref() != Some(&backedge)
                    || site.terminal_value.is_some()
                {
                    return Err(format!(
                        "loop '{}' fallthrough is not bound to its exact CFG backedge",
                        loop_node.0
                    ));
                }
                mapped_terminal_edges.insert(backedge.clone());
            }
            TypedLoopEdgeKind::Continue => {
                let id = cfg_edge_id(&site.site, "continue");
                let edge = get(&id)?;
                if edge.kind != EdgeKind::Continue
                    || edge.from != site.cfg_terminal_block
                    || &edge.to != header
                    || edge.source.node != site.site
                    || !included_edges.contains(&id)
                    || site.cfg_terminal_edge.as_ref() != Some(&id)
                    || site.terminal_value.is_some()
                {
                    return Err(format!(
                        "loop '{}' continue site '{}' disagrees with CallableCfg",
                        loop_node.0, site.site.0
                    ));
                }
                mapped_terminal_edges.insert(id);
            }
            TypedLoopEdgeKind::Break => {
                let id = cfg_edge_id(&site.site, "break");
                let edge = get(&id)?;
                if edge.kind != EdgeKind::Break
                    || edge.from != site.cfg_terminal_block
                    || &edge.to != exit_block
                    || edge.source.node != site.site
                    || !included_edges.contains(&id)
                    || site.cfg_terminal_edge.as_ref() != Some(&id)
                {
                    return Err(format!(
                        "loop '{}' break site '{}' disagrees with CallableCfg",
                        loop_node.0, site.site.0
                    ));
                }
                mapped_terminal_edges.insert(id);
            }
            TypedLoopEdgeKind::Return => {
                let block = cfg.blocks.get(&site.cfg_terminal_block).ok_or_else(|| {
                    format!("loop return '{}' terminal block is absent", site.site.0)
                })?;
                if site.cfg_terminal_edge.is_some()
                    || !block
                        .points
                        .iter()
                        .any(|point| point.source.node == site.site)
                    || !matches!(
                        &block.terminator,
                        Terminator::Return { value, implicit: false }
                            if value == &site.terminal_value
                    )
                {
                    return Err(format!(
                        "loop '{}' return site '{}' does not bind one exact CFG return terminator",
                        loop_node.0, site.site.0
                    ));
                }
            }
        }
        for guard in &site.guards {
            let then_id = cfg_edge_id(&guard.expression, "if-then");
            let else_id = cfg_edge_id(&guard.expression, "if-else");
            let then_edge = get(&then_id)?;
            let else_edge = get(&else_id)?;
            let branch = cfg.blocks.get(&then_edge.from).ok_or_else(|| {
                format!(
                    "loop branch '{}' has no CFG source block",
                    guard.statement.0
                )
            })?;
            if then_edge.from != else_edge.from
                || then_edge.source.node != guard.expression
                || else_edge.source.node != guard.expression
                || then_edge.kind != EdgeKind::Then
                || else_edge.kind != EdgeKind::Else
                || !matches!(
                    &branch.terminator,
                    Terminator::Branch { condition, then_edge: actual_then, else_edge: actual_else }
                        if condition == &guard.condition
                            && actual_then == &then_id
                            && actual_else == &else_id
                )
                || !included_edges.contains(&then_id)
                || !included_edges.contains(&else_id)
            {
                return Err(format!(
                    "loop branch guard expression '{}' (worklist statement '{}') is not bound to its exact ordered CFG branch",
                    guard.expression.0, guard.statement.0
                ));
            }
        }
        if site.reachable
            && !cfg_guard_path_reaches(
                cfg,
                &body_edge.to,
                &site.cfg_terminal_block,
                loop_node,
                &site.guards,
            )?
        {
            return Err(format!(
                "loop edge site '{}' is not reachable along its exact ordered source guard path",
                site.site.0
            ));
        }
    }

    let reachable_backedges = edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Backedge && edge.to == *header)
        .filter(|edge| cfg.reachable.contains(&edge.from))
        .map(|edge| edge.id.clone())
        .collect::<BTreeSet<_>>();
    let mapped_reachable_backedges = source_sites
        .iter()
        .filter(|site| site.kind == TypedLoopEdgeKind::BodyFallthrough && site.reachable)
        .filter_map(|site| site.cfg_terminal_edge.clone())
        .collect::<BTreeSet<_>>();
    if reachable_backedges != mapped_reachable_backedges {
        return Err(format!(
            "loop '{}' source fallthrough census and reachable CFG backedges disagree",
            loop_node.0
        ));
    }
    let reachable_continue_break = edges
        .iter()
        .filter(|edge| {
            matches!(edge.kind, EdgeKind::Continue | EdgeKind::Break)
                && cfg.reachable.contains(&edge.from)
        })
        .map(|edge| edge.id.clone())
        .collect::<BTreeSet<_>>();
    let mapped_reachable_continue_break = source_sites
        .iter()
        .filter(|site| {
            site.reachable
                && matches!(
                    site.kind,
                    TypedLoopEdgeKind::Continue | TypedLoopEdgeKind::Break
                )
        })
        .filter_map(|site| site.cfg_terminal_edge.clone())
        .collect::<BTreeSet<_>>();
    if reachable_continue_break != mapped_reachable_continue_break {
        return Err(format!(
            "loop '{}' source break/continue census and reachable CFG exits disagree",
            loop_node.0
        ));
    }
    let reachable_return_blocks = blocks
        .iter()
        .filter(|block| {
            cfg.reachable.contains(&block.id)
                && block.id != *header
                && block.id != *exit_block
                && matches!(
                    &block.terminator,
                    Terminator::Return {
                        implicit: false,
                        ..
                    }
                )
        })
        .map(|block| block.id.clone())
        .collect::<BTreeSet<_>>();
    let mapped_reachable_return_blocks = source_sites
        .iter()
        .filter(|site| site.kind == TypedLoopEdgeKind::Return && site.reachable)
        .map(|site| site.cfg_terminal_block.clone())
        .collect::<BTreeSet<_>>();
    if reachable_return_blocks != mapped_reachable_return_blocks {
        return Err(format!(
            "loop '{}' source return census and reachable CFG Return terminators disagree",
            loop_node.0
        ));
    }
    if mapped_terminal_edges
        .iter()
        .any(|id| !included_edges.contains(id))
    {
        return Err(format!(
            "loop '{}' terminal edge binding is absent from its CFG receipt",
            loop_node.0
        ));
    }
    if body_node.0.is_empty() {
        return Err(format!("loop '{}' has an empty body identity", loop_node.0));
    }
    Ok(())
}

fn cfg_guard_path_reaches(
    cfg: &CallableCfg,
    start: &BasicBlockId,
    terminal: &BasicBlockId,
    loop_node: &NodeId,
    guards: &[TypedLoopBranchGuard],
) -> Result<bool, String> {
    let mut current = start.clone();
    let guarded_branches = guards
        .iter()
        .map(|guard| guard.expression.clone())
        .collect::<BTreeSet<_>>();
    for guard in guards {
        let then_id = cfg_edge_id(&guard.expression, "if-then");
        let else_id = cfg_edge_id(&guard.expression, "if-else");
        let then_edge = cfg
            .edges
            .get(&then_id)
            .ok_or_else(|| format!("guard '{}' is missing exact then edge", guard.expression.0))?;
        let else_edge = cfg
            .edges
            .get(&else_id)
            .ok_or_else(|| format!("guard '{}' is missing exact else edge", guard.expression.0))?;
        if then_edge.from != else_edge.from {
            return Ok(false);
        }
        if !cfg_path_to_block(cfg, &current, &then_edge.from, loop_node, &guarded_branches) {
            return Ok(false);
        }
        let selected = if guard.then_arm { then_edge } else { else_edge };
        current = selected.to.clone();
    }
    Ok(cfg_path_to_block(
        cfg,
        &current,
        terminal,
        loop_node,
        &guarded_branches,
    ))
}

fn cfg_path_to_block(
    cfg: &CallableCfg,
    start: &BasicBlockId,
    target: &BasicBlockId,
    loop_node: &NodeId,
    guarded_branches: &BTreeSet<NodeId>,
) -> bool {
    let mut queue = VecDeque::from([start.clone()]);
    let mut seen = BTreeSet::new();
    while let Some(block_id) = queue.pop_front() {
        if block_id == *target {
            return true;
        }
        if !seen.insert(block_id.clone()) {
            continue;
        }
        for edge in cfg.successors(&block_id) {
            if matches!(
                edge.kind,
                EdgeKind::Backedge | EdgeKind::Continue | EdgeKind::Break | EdgeKind::LoopExit
            ) {
                continue;
            }
            if edge.kind == EdgeKind::LoopBody && edge.source.node != *loop_node {
                continue;
            }
            if guarded_branches.contains(&edge.source.node) {
                continue;
            }
            queue.push_back(edge.to.clone());
        }
    }
    false
}

fn loop_cfg_slice(
    cfg: &CallableCfg,
    loop_node: &NodeId,
) -> Result<(Vec<BasicBlock>, Vec<CfgEdge>, BTreeSet<BasicBlockId>), String> {
    let entry_id = EdgeId(NodeId(format!("{}/cfg:edge:while-enter", loop_node.0)));
    let entry = cfg.edges.get(&entry_id).ok_or_else(|| {
        format!(
            "while '{}' has no exact CallableCfg entry edge",
            loop_node.0
        )
    })?;
    if entry.kind != EdgeKind::Fallthrough || entry.source.node != *loop_node {
        return Err(format!(
            "while '{}' CallableCfg entry edge identity drifted",
            loop_node.0
        ));
    }
    let body_edges = cfg
        .edges
        .values()
        .filter(|edge| edge.kind == EdgeKind::LoopBody && edge.source.node == *loop_node)
        .collect::<Vec<_>>();
    let exit_edges = cfg
        .edges
        .values()
        .filter(|edge| edge.kind == EdgeKind::LoopExit && edge.source.node == *loop_node)
        .collect::<Vec<_>>();
    let [body_edge] = body_edges.as_slice() else {
        return Err(format!(
            "while '{}' must have one exact LoopBody edge",
            loop_node.0
        ));
    };
    let [exit_edge] = exit_edges.as_slice() else {
        return Err(format!(
            "while '{}' must have one exact LoopExit edge",
            loop_node.0
        ));
    };
    if entry.to != body_edge.from || body_edge.from != exit_edge.from {
        return Err(format!(
            "while '{}' entry/condition CFG does not join the exact loop header",
            loop_node.0
        ));
    }
    let header = entry.to.clone();
    let exit = exit_edge.to.clone();
    let mut queue = VecDeque::from([header.clone(), body_edge.to.clone(), exit.clone()]);
    let mut block_ids = BTreeSet::new();
    let mut edge_ids = BTreeSet::from([entry.id.clone()]);
    while let Some(block_id) = queue.pop_front() {
        if !block_ids.insert(block_id.clone()) || block_id == header && block_ids.len() > 1 {
            continue;
        }
        let block = cfg.blocks.get(&block_id).ok_or_else(|| {
            format!(
                "while '{}' references absent CFG block '{}'",
                loop_node.0, block_id.0 .0
            )
        })?;
        if block_id == exit {
            continue;
        }
        for edge in cfg.successors(&block_id) {
            edge_ids.insert(edge.id.clone());
            match edge.kind {
                EdgeKind::Backedge | EdgeKind::Continue if edge.to == header => {}
                EdgeKind::Break | EdgeKind::LoopExit if edge.to == exit => {
                    queue.push_back(edge.to.clone());
                }
                EdgeKind::LoopBody if edge.source.node == *loop_node => {
                    queue.push_back(edge.to.clone());
                }
                EdgeKind::LoopBody | EdgeKind::LoopExit => {
                    return Err(format!(
                        "while '{}' contains nested loop CFG before its induction receipt is supported",
                        loop_node.0
                    ));
                }
                _ => queue.push_back(edge.to.clone()),
            }
        }
        if matches!(&block.terminator, Terminator::Return { .. }) {
            continue;
        }
    }
    let cfg_blocks = block_ids
        .iter()
        .filter_map(|id| cfg.blocks.get(id).cloned())
        .collect::<Vec<_>>();
    let cfg_edges = edge_ids
        .iter()
        .filter_map(|id| cfg.edges.get(id).cloned())
        .collect::<Vec<_>>();
    let reachable = block_ids
        .iter()
        .filter(|id| cfg.reachable.contains(*id))
        .cloned()
        .collect();
    Ok((cfg_blocks, cfg_edges, reachable))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked_loop_receipt(
        checked: &crate::core::CheckedProgram,
        name: &str,
    ) -> (
        NodeId,
        crate::core::ir::ResolvedCallable,
        TypedLoopProofReceipt,
    ) {
        let owner = NodeId(format!("function:{name}"));
        let callable = checked
            .callables()
            .get(&owner)
            .expect("checked loop callable")
            .clone();
        let statement = callable
            .body
            .root
            .statements
            .iter()
            .find(|statement| matches!(&statement.kind, ResolvedStmtKind::While { .. }))
            .expect("while statement");
        let scope = vec![TypedProofScopeFrame::Block(
            callable.body.root.node_id.clone(),
        )];
        let receipt = derive_typed_loop_proof_receipt(&callable, &statement.node_id, &scope, &[])
            .expect("loop source receipt");
        (statement.node_id.clone(), callable, receipt)
    }

    #[test]
    fn typed_loop_receipt_binds_condition_calls_and_exact_cfg_ownership() {
        let source = r#"
func positive(x: i32) -> bool { x > 0 }
func count(limit: i32) -> i32 {
    requires: limit >= 0 && limit <= 10
    let mut i = 0
    while positive(i) {
        invariant: i >= 0 && i <= limit
        i = i + 1
    }
    i
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("checked source");
        let owner = NodeId("function:count".into());
        let callable = checked.callables().get(&owner).expect("typed callable");
        let statement = callable
            .body
            .root
            .statements
            .iter()
            .find(|statement| matches!(&statement.kind, ResolvedStmtKind::While { .. }))
            .expect("while statement");
        let scope = vec![TypedProofScopeFrame::Block(
            callable.body.root.node_id.clone(),
        )];
        let ResolvedStmtKind::While { body, .. } = &statement.kind else {
            unreachable!();
        };
        let invariant_scope = scope
            .iter()
            .cloned()
            .chain([
                TypedProofScopeFrame::Loop(statement.node_id.clone()),
                TypedProofScopeFrame::Block(body.node_id.clone()),
            ])
            .collect::<Vec<_>>();
        let invariants = body
            .statements
            .iter()
            .take_while(|nested| {
                matches!(
                    &nested.kind,
                    ResolvedStmtKind::Contract {
                        kind: crate::core::ir::ContractKind::Invariant,
                        ..
                    }
                )
            })
            .enumerate()
            .map(|(ordinal, nested)| {
                let ResolvedStmtKind::Contract { condition, .. } = &nested.kind else {
                    unreachable!();
                };
                TypedLoopInvariantPoint {
                    id: TypedProofPointId {
                        owner: callable.owner.clone(),
                        statement: nested.node_id.clone(),
                        ordinal: ordinal as u32,
                    },
                    scope: invariant_scope.clone(),
                    condition: condition.clone(),
                }
            })
            .collect::<Vec<_>>();
        let receipt =
            derive_typed_loop_proof_receipt(callable, &statement.node_id, &scope, &invariants)
                .expect("loop source receipt");
        let ResolvedStmtKind::While { condition, .. } = &statement.kind else {
            unreachable!();
        };
        assert_eq!(receipt.call_nodes, vec![condition.node_id.clone()]);
        assert!(receipt.has_valid_source_witness());
        let condition_site = receipt
            .expression_sites
            .iter()
            .find(|site| {
                site.role == TypedLoopExpressionRole::Condition
                    && site.point == statement.node_id
                    && site.expression == condition.node_id
                    && site.branch_expression.is_none()
                    && site.ordinal == 0
            })
            .expect("exact loop-condition expression site");
        assert!(validate_loop_expression_site(
            &receipt,
            TypedLoopExpressionRole::Condition,
            &statement.node_id,
            &condition.node_id,
            None,
            0,
            &condition_site.scope,
            &condition_site.ty,
            &condition_site.effects,
            &condition_site.source_fingerprint,
            &condition_site.type_path_fingerprint,
        ));
        assert!(receipt.expression_sites.iter().any(|site| {
            site.role == TypedLoopExpressionRole::Invariant
                && site.point == invariants[0].id.statement
                && site.expression == invariants[0].condition.node_id
                && site.ordinal == invariants[0].id.ordinal
        }));
        let write = receipt.writes.first().expect("loop assignment source site");
        assert!(receipt.expression_sites.iter().any(|site| {
            site.role == TypedLoopExpressionRole::WriteValue
                && site.point == write.statement
                && site.expression == write.value
        }));
        assert!(receipt.effect_nodes.is_empty());
        assert_eq!(receipt.body_edges.len(), 1);
        assert_eq!(
            receipt.body_edges[0].kind,
            TypedLoopEdgeKind::BodyFallthrough
        );
        assert_eq!(receipt.state_locals.len(), 2);
        assert!(receipt
            .state_locals
            .iter()
            .any(|local| local.written_in_loop));
        assert_eq!(
            receipt.body_edges[0]
                .cfg_terminal_edge
                .as_ref()
                .unwrap()
                .0
                 .0,
            format!("{}/cfg:edge:while-backedge", statement.node_id.0)
        );
        assert!(receipt
            .old_snapshot
            .iter()
            .any(|local| callable.body.locals[local].display_name == "limit"));
        validate_typed_loop_proof_receipt(
            &receipt,
            callable,
            &statement.node_id,
            &scope,
            &invariants,
        )
        .expect("exact rederived receipt");

        let mut tampered = receipt.clone();
        tampered.call_nodes.clear();
        assert!(!tampered.has_valid_source_witness());
        assert!(validate_typed_loop_proof_receipt(
            &tampered,
            callable,
            &statement.node_id,
            &scope,
            &invariants,
        )
        .is_err());

        let mut forged_cfg = callable.clone();
        let entry_edge = cfg_edge_id(&statement.node_id, "while-enter");
        let header = forged_cfg.cfg.edges[&entry_edge].to.clone();
        forged_cfg.cfg.blocks.get_mut(&header).unwrap().terminator = Terminator::Unreachable;
        assert!(derive_typed_loop_proof_receipt(
            &forged_cfg,
            &statement.node_id,
            &scope,
            &invariants,
        )
        .is_err());

        let mut missing_resources = callable.clone();
        let reachable_source = receipt
            .cfg_edges
            .iter()
            .find(|edge| callable.cfg.reachable.contains(&edge.from))
            .expect("loop has a reachable CFG edge")
            .from
            .clone();
        missing_resources
            .resources
            .in_states
            .remove(&reachable_source);
        assert!(derive_typed_loop_proof_receipt(
            &missing_resources,
            &statement.node_id,
            &scope,
            &invariants,
        )
        .is_err());
    }

    #[test]
    fn typed_loop_ffi_profile_binds_only_one_immutable_call_reaching_path() {
        let source = r#"
extern "C" {
    func positive(value: i32) -> i32 requires: value > 0;
    func unknown_guard(value: i32) -> bool;
}
func while_positive(value: i32) -> i32 {
    while value >= 0 { positive(value); break; }
    0
}
func while_continue_positive(value: i32) -> i32 {
    while value >= 0 {
        if value == 0 { continue; }
        positive(value);
        break;
    }
    0
}
func while_break_positive(value: i32) -> i32 {
    while value >= 0 {
        if value == 0 { break; }
        positive(value);
        break;
    }
    0
}
func while_mutates(value: i32) -> i32 {
    let mut current = value
    while current > 0 {
        current = current - 1
        positive(current);
        break;
    }
    0
}
func while_extra_call(value: i32) -> i32 {
    while value > 0 {
        positive(value);
        positive(value);
        break;
    }
    0
}
func while_effectful_guard(value: i32) -> i32 {
    while value > 0 {
        if unknown_guard(value) { break; }
        positive(value);
        break;
    }
    0
}
func while_multiple_prefixes(value: i32) -> i32 {
    while value >= 0 {
        if value == 0 { continue; }
        if value == 1 { break; }
        positive(value);
        break;
    }
    0
}
func while_nested(value: i32) -> i32 {
    while value > 0 {
        while value > 0 { break; }
        positive(value);
        break;
    }
    0
}
func main() -> i32 { 0 }
"#;
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("checked loop fixtures");

        for name in [
            "while_positive",
            "while_continue_positive",
            "while_break_positive",
        ] {
            let (loop_node, callable, receipt) = checked_loop_receipt(&checked, name);
            let profile = receipt
                .ffi_call_path
                .as_ref()
                .unwrap_or_else(|| panic!("{name} should have an exact FFI loop profile"));
            assert_eq!(profile.loop_node, loop_node, "{name} loop identity");
            assert_eq!(
                profile.condition_node, receipt.condition_node,
                "{name} guard"
            );
            assert_eq!(receipt.call_nodes, vec![profile.call_node.clone()]);
            assert_eq!(
                profile.call_guards.len(),
                if name == "while_positive" { 0 } else { 1 }
            );
            assert_eq!(receipt.body_edges.len(), 1 + profile.call_guards.len());
            assert_eq!(
                receipt
                    .body_edges
                    .iter()
                    .filter(|edge| edge.site == profile.terminal_break
                        && edge.kind == TypedLoopEdgeKind::Break
                        && edge.guards == profile.call_guards)
                    .count(),
                1,
                "{name} call path must terminate at its exact break"
            );
            for edge in &receipt.body_edges {
                assert!(
                    edge.reachable,
                    "{name} has no unreachable call alternatives"
                );
                assert_eq!(edge.target_loop, Some(loop_node.clone()));
                assert!(matches!(
                    edge.kind,
                    TypedLoopEdgeKind::Break | TypedLoopEdgeKind::Continue
                ));
            }
            if name == "while_continue_positive" {
                let guard = &profile.call_guards[0];
                assert!(!guard.then_arm, "the call follows the value!=0 edge");
                let continue_edge = receipt
                    .body_edges
                    .iter()
                    .find(|edge| edge.kind == TypedLoopEdgeKind::Continue)
                    .expect("the value==0 path continues without calling");
                assert!(continue_edge.guards[0].then_arm);
                assert_ne!(continue_edge.guards, profile.call_guards);
            }
            validate_typed_loop_proof_receipt(
                &receipt,
                &callable,
                &loop_node,
                &[TypedProofScopeFrame::Block(
                    callable.body.root.node_id.clone(),
                )],
                &[],
            )
            .expect("exact source/CFG/ownership rederivation");
        }

        for name in [
            "while_mutates",
            "while_extra_call",
            "while_effectful_guard",
            "while_multiple_prefixes",
        ] {
            let (_, _, receipt) = checked_loop_receipt(&checked, name);
            assert!(
                receipt.ffi_call_path.is_none(),
                "{name} is outside the exact immutable at-most-once profile"
            );
        }
        let nested_owner = NodeId("function:while_nested".into());
        let nested = checked
            .callables()
            .get(&nested_owner)
            .expect("nested callable");
        let nested_loop = nested
            .body
            .root
            .statements
            .iter()
            .find(|statement| matches!(&statement.kind, ResolvedStmtKind::While { .. }))
            .expect("outer while");
        let nested_scope = vec![TypedProofScopeFrame::Block(
            nested.body.root.node_id.clone(),
        )];
        assert!(
            derive_typed_loop_proof_receipt(nested, &nested_loop.node_id, &nested_scope, &[])
                .is_err(),
            "nested loop paths remain outside the source receipt profile"
        );

        let (loop_node, callable, receipt) =
            checked_loop_receipt(&checked, "while_continue_positive");
        let mut forged = receipt.clone();
        forged
            .ffi_call_path
            .as_mut()
            .expect("exact loop profile")
            .terminal_break = NodeId("forged:break".into());
        assert!(!forged.has_valid_source_witness());
        assert!(validate_typed_loop_proof_receipt(
            &forged,
            &callable,
            &loop_node,
            &[TypedProofScopeFrame::Block(
                callable.body.root.node_id.clone()
            )],
            &[],
        )
        .is_err());

        let profile = receipt.ffi_call_path.as_ref().expect("exact loop profile");
        let break_cfg_edge = cfg_edge_id(&profile.terminal_break, "break");
        let mut forged_cfg = callable.clone();
        let header = forged_cfg
            .cfg
            .edges
            .get(&cfg_edge_id(&loop_node, "while-enter"))
            .expect("loop entry edge")
            .to
            .clone();
        forged_cfg
            .cfg
            .edges
            .get_mut(&break_cfg_edge)
            .expect("exact call-path break edge")
            .to = header;
        assert!(
            derive_typed_loop_proof_receipt(
                &forged_cfg,
                &loop_node,
                &[TypedProofScopeFrame::Block(
                    callable.body.root.node_id.clone()
                )],
                &[],
            )
            .is_err(),
            "a call-site receipt cannot survive a changed source break target"
        );
    }
}
