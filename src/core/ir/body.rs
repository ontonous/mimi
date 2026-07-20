use super::{NominalTypeId, ResolvedType, ResolvedTypeId, ResolvedTypeTable};
use crate::core::{NodeId, Origin, TransitionId};
use std::collections::{BTreeMap, BTreeSet};

macro_rules! semantic_string_id {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ResolvedBodyError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(ResolvedBodyError::new(
                        NodeId("resolved-body:schema".into()),
                        concat!($kind, " identity must not be empty"),
                    ));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

semantic_string_id!(BuiltinId, "builtin");
semantic_string_id!(MethodId, "method");
semantic_string_id!(EffectId, "effect");
semantic_string_id!(SessionResidualId, "session residual");

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResolvedLocalId(pub NodeId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResolvedParameterId(pub NodeId);

#[derive(Debug, Clone)]
pub struct ResolvedLocal {
    pub id: ResolvedLocalId,
    pub display_name: String,
    pub ty: ResolvedTypeId,
    pub mutable: bool,
    pub origin: Origin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedIndex {
    Constant(i64),
    Dynamic(NodeId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedProjection {
    Field {
        field: NodeId,
        name: String,
        ty: ResolvedTypeId,
    },
    Tuple {
        index: usize,
        ty: ResolvedTypeId,
    },
    Index {
        index: ResolvedIndex,
        ty: ResolvedTypeId,
    },
    Deref {
        ty: ResolvedTypeId,
    },
}

impl ResolvedProjection {
    pub fn ty(&self) -> &ResolvedTypeId {
        match self {
            Self::Field { ty, .. }
            | Self::Tuple { ty, .. }
            | Self::Index { ty, .. }
            | Self::Deref { ty } => ty,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPlace {
    pub base: ResolvedLocalId,
    pub projections: Vec<ResolvedProjection>,
}

impl ResolvedPlace {
    pub fn root(base: ResolvedLocalId) -> Self {
        Self {
            base,
            projections: Vec::new(),
        }
    }

    pub fn projected_type<'a>(&'a self, local: &'a ResolvedLocal) -> &'a ResolvedTypeId {
        self.projections
            .last()
            .map(ResolvedProjection::ty)
            .unwrap_or(&local.ty)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckedConversionKind {
    Identity,
    NumericWiden,
    NumericNarrowChecked,
    TraitUpcast,
    DynamicPack,
    DynamicDowncastChecked,
    AliasWrap,
    AliasUnwrap,
    NewtypeWrap,
    NewtypeUnwrap,
    OwnershipWrap,
    OwnershipDowngrade,
    OwnershipRead,
    /// A checked slice expression reuses the source sequence ABI while
    /// narrowing its visible bounds.
    SliceView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedConversion {
    pub kind: CheckedConversionKind,
    pub from: ResolvedTypeId,
    pub to: ResolvedTypeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    View,
    Mutate,
    Consume,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedCallee {
    Function(NodeId),
    Constructor(NodeId),
    Extern(NodeId),
    Builtin(BuiltinId),
    LocalClosure(ResolvedLocalId),
    ActorMethod { actor: NodeId, method: MethodId },
    ProtocolMethod { protocol: NodeId, method: MethodId },
    Transition(TransitionId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendRequirement {
    pub requirement_id: String,
    pub capability: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTransition {
    pub endpoint: ResolvedLocalId,
    pub before: SessionResidualId,
    pub after: SessionResidualId,
}

#[derive(Debug, Clone)]
pub struct ResolvedArgument {
    pub parameter: ResolvedParameterId,
    pub value: ResolvedExpr,
    pub conversion: CheckedConversion,
}

#[derive(Debug, Clone)]
pub struct ResolvedCall {
    pub callee: ResolvedCallee,
    /// Result type after generic/overload resolution.
    pub result: ResolvedTypeId,
    /// Explicit or checker-inferred generic arguments in binder order.
    pub type_arguments: Vec<ResolvedTypeId>,
    /// Checker-sorted parameter order. Surface named/default arguments are gone.
    pub arguments: Vec<ResolvedArgument>,
    pub permission: Option<Permission>,
    pub effects: Vec<EffectId>,
    pub session: Vec<SessionTransition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedLiteral {
    Int(i64),
    FloatBits(u64),
    Bool(bool),
    String(String),
    Unit,
}

#[derive(Debug, Clone)]
pub enum ResolvedFStringPart {
    Text(String),
    Interpolation(ResolvedExpr),
}

impl ResolvedLiteral {
    pub fn float(value: f64) -> Self {
        Self::FloatBits(value.to_bits())
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::FloatBits(bits) => Some(f64::from_bits(*bits)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedBinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Power,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    LogicalAnd,
    LogicalOr,
    BitAnd,
    BitOr,
    BitXor,
    ShiftLeft,
    ShiftRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedUnaryOp {
    Negate,
    Not,
    BorrowShared,
    BorrowMutable,
    Dereference,
}

#[derive(Debug, Clone)]
pub struct ResolvedRecordField {
    pub field: NodeId,
    pub value: ResolvedExpr,
    pub conversion: CheckedConversion,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub node_id: NodeId,
    pub origin: Origin,
    pub pattern: ResolvedPattern,
    pub guard: Option<ResolvedExpr>,
    pub body: ResolvedExpr,
}

#[derive(Debug, Clone)]
pub struct ResolvedExpr {
    pub node_id: NodeId,
    pub origin: Origin,
    pub ty: ResolvedTypeId,
    pub effects: Vec<EffectId>,
    pub backend_requirements: Vec<BackendRequirement>,
    pub kind: ResolvedExprKind,
}

#[derive(Debug, Clone)]
pub struct ResolvedLambda {
    pub owner: NodeId,
    pub parameters: Vec<ResolvedLocalId>,
    pub captures: Vec<ResolvedLocalId>,
    pub body: ResolvedBlock,
}

#[derive(Debug, Clone)]
pub enum ResolvedValueProjection {
    Field(NodeId),
    Tuple(usize),
    Index(Box<ResolvedExpr>),
    Dereference,
}

#[derive(Debug, Clone)]
pub enum ResolvedExprKind {
    Literal(ResolvedLiteral),
    FString(Vec<ResolvedFStringPart>),
    Load(ResolvedPlace),
    Constant(NodeId),
    /// A callable used as a first-class value. Its declaration identity is
    /// closed here; subsequent calls through a binding use `LocalClosure`.
    Callable(ResolvedCallee),
    /// Projection from an rvalue aggregate. Lvalue projections remain a
    /// `Load(ResolvedPlace)` and therefore always have a stable local base.
    Project {
        value: Box<ResolvedExpr>,
        projection: ResolvedValueProjection,
    },
    DefaultArgument {
        callable: NodeId,
        parameter: ResolvedParameterId,
    },
    Binary {
        op: ResolvedBinaryOp,
        left: Box<ResolvedExpr>,
        right: Box<ResolvedExpr>,
    },
    Unary {
        op: ResolvedUnaryOp,
        operand: Box<ResolvedExpr>,
    },
    Call(ResolvedCall),
    Tuple(Vec<ResolvedExpr>),
    List(Vec<ResolvedExpr>),
    Map(Vec<(ResolvedExpr, ResolvedExpr)>),
    Set(Vec<ResolvedExpr>),
    Comprehension {
        pattern: ResolvedPattern,
        value: Box<ResolvedExpr>,
        iterable: Box<ResolvedExpr>,
        guard: Option<Box<ResolvedExpr>>,
    },
    OptionalChain {
        receiver: Box<ResolvedExpr>,
        field: NodeId,
        field_type: ResolvedTypeId,
    },
    TypeOf(Box<ResolvedExpr>),
    Old(Box<ResolvedExpr>),
    Record {
        nominal: NominalTypeId,
        fields: Vec<ResolvedRecordField>,
    },
    Block(Box<ResolvedBlock>),
    Scope {
        kind: ResolvedScopeKind,
        body: Box<ResolvedBlock>,
    },
    Comptime(Box<ResolvedBlock>),
    If {
        condition: Box<ResolvedExpr>,
        then_block: Box<ResolvedBlock>,
        else_block: Box<ResolvedBlock>,
    },
    Match {
        scrutinee: Box<ResolvedExpr>,
        arms: Vec<MatchArm>,
    },
    Try {
        value: Box<ResolvedExpr>,
        propagation_target: NodeId,
    },
    Range {
        start: Box<ResolvedExpr>,
        end: Box<ResolvedExpr>,
    },
    Slice {
        target: Box<ResolvedExpr>,
        start: Option<Box<ResolvedExpr>>,
        end: Option<Box<ResolvedExpr>>,
    },
    Cast {
        value: Box<ResolvedExpr>,
        conversion: CheckedConversion,
    },
    Spawn(Box<ResolvedExpr>),
    Await(Box<ResolvedExpr>),
    Lambda(Box<ResolvedLambda>),
    ComptimeValue(NodeId),
    Quote(Box<ResolvedBlock>),
    TypeValue(ResolvedTypeId),
}

#[derive(Debug, Clone)]
pub struct ResolvedPattern {
    pub node_id: NodeId,
    pub origin: Origin,
    pub ty: ResolvedTypeId,
    pub kind: ResolvedPatternKind,
}

#[derive(Debug, Clone)]
pub enum ResolvedPatternKind {
    Wildcard,
    Binding {
        local: ResolvedLocalId,
        by_reference: Option<Permission>,
    },
    Literal(ResolvedLiteral),
    Constructor {
        variant: NodeId,
        fields: Vec<(NodeId, ResolvedPattern)>,
    },
    Tuple(Vec<ResolvedPattern>),
    Array(Vec<ResolvedPattern>),
    Slice {
        prefix: Vec<ResolvedPattern>,
        rest: Option<Box<ResolvedPattern>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractKind {
    Requires,
    Ensures,
    Invariant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocatorKind {
    System,
    Arena,
    Bump,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedScopeKind {
    Lexical,
    Unsafe,
    FailureGuard,
    Arena,
    Allocator(AllocatorKind),
    Parallel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegateTarget {
    Local(ResolvedLocalId),
    Callable(NodeId),
}

#[derive(Debug, Clone)]
pub struct ResolvedStmt {
    pub node_id: NodeId,
    pub origin: Origin,
    pub ty: ResolvedTypeId,
    pub backend_requirements: Vec<BackendRequirement>,
    pub kind: ResolvedStmtKind,
}

#[derive(Debug, Clone)]
pub enum ResolvedStmtKind {
    Bind {
        pattern: ResolvedPattern,
        initializer: Option<ResolvedExpr>,
    },
    Assign {
        target: ResolvedPlace,
        value: ResolvedExpr,
        conversion: CheckedConversion,
    },
    Return {
        value: Option<ResolvedExpr>,
        conversion: Option<CheckedConversion>,
    },
    Break(Option<ResolvedExpr>),
    Continue,
    Expr(ResolvedExpr),
    While {
        condition: ResolvedExpr,
        body: ResolvedBlock,
    },
    WhileLet {
        pattern: ResolvedPattern,
        initializer: ResolvedExpr,
        body: ResolvedBlock,
    },
    Loop(ResolvedBlock),
    For {
        pattern: ResolvedPattern,
        iterable: ResolvedExpr,
        body: ResolvedBlock,
    },
    Drop(Vec<ResolvedPlace>),
    Contract {
        kind: ContractKind,
        condition: ResolvedExpr,
    },
    Math(Vec<ResolvedExpr>),
    Scope {
        kind: ResolvedScopeKind,
        body: ResolvedBlock,
    },
    Delegate {
        permission: Permission,
        source: ResolvedPlace,
        target: DelegateTarget,
    },
    Pinned {
        value: ResolvedExpr,
        timeout: Option<ResolvedExpr>,
        binding: Option<ResolvedLocalId>,
        body: ResolvedBlock,
    },
    NestedCallable(NodeId),
}

#[derive(Debug, Clone)]
pub struct ResolvedBlock {
    pub node_id: NodeId,
    pub origin: Origin,
    pub ty: ResolvedTypeId,
    pub statements: Vec<ResolvedStmt>,
    pub result: Option<Box<ResolvedExpr>>,
}

#[derive(Debug, Clone)]
pub struct ResolvedBody {
    pub owner: NodeId,
    pub locals: BTreeMap<ResolvedLocalId, ResolvedLocal>,
    /// Lexically captured locals owned by enclosing callable bodies.
    pub captures: Vec<ResolvedLocalId>,
    /// Typed expressions evaluated to compute structured places. A place
    /// references these nodes by stable NodeId instead of retaining raw AST.
    pub place_inputs: BTreeMap<NodeId, ResolvedExpr>,
    /// Declaration-owned typed default expressions keyed by parameter identity.
    pub default_values: BTreeMap<ResolvedParameterId, ResolvedExpr>,
    pub root: ResolvedBlock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBodyError {
    pub node_id: NodeId,
    pub message: String,
}

impl ResolvedBodyError {
    pub(crate) fn new(node_id: NodeId, message: impl Into<String>) -> Self {
        Self {
            node_id,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ResolvedBodyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "resolved body node '{}': {}",
            self.node_id.0, self.message
        )
    }
}

impl std::error::Error for ResolvedBodyError {}

impl ResolvedBody {
    pub fn validate(&self, types: &ResolvedTypeTable) -> Result<(), Vec<ResolvedBodyError>> {
        let mut validator = BodyValidator {
            body: self,
            types,
            nodes: BTreeSet::new(),
            pending_nodes: Vec::new(),
            errors: Vec::new(),
        };
        if self.owner.0.trim().is_empty() {
            validator.error(&self.owner, "callable owner identity is empty");
        }
        let mut captures = BTreeSet::new();
        for capture in &self.captures {
            if !captures.insert(capture) {
                validator.error(&capture.0, "capture identity is duplicated");
            }
            if !self.locals.contains_key(capture) {
                validator.error(&capture.0, "capture is absent from the local catalog");
            }
        }
        for (key, local) in &self.locals {
            if key != &local.id {
                validator.error(&local.id.0, "local table key disagrees with local identity");
            }
            validator.register_node(&local.id.0);
            validator.require_type(&local.id.0, &local.ty);
            validator.validate_origin(&local.id.0, &local.origin);
            if local.display_name.trim().is_empty() {
                validator.error(&local.id.0, "local display name is empty");
            }
        }
        for (key, input) in &self.place_inputs {
            if key != &input.node_id {
                validator.error(key, "place input key disagrees with expression identity");
            }
            validator.visit_expr(input);
        }
        for (parameter, value) in &self.default_values {
            if parameter.0 .0.trim().is_empty() {
                validator.error(&self.owner, "default value has an empty parameter identity");
            }
            validator.visit_expr(value);
        }
        validator.visit_block(&self.root);
        for (owner, target, role) in validator.pending_nodes.clone() {
            if !validator.nodes.contains(&target) && target != self.owner {
                validator.error(
                    &owner,
                    format!("{role} references missing body node '{}'", target.0),
                );
            }
        }
        if validator.errors.is_empty() {
            Ok(())
        } else {
            Err(validator.errors)
        }
    }
}

struct BodyValidator<'a> {
    body: &'a ResolvedBody,
    types: &'a ResolvedTypeTable,
    nodes: BTreeSet<NodeId>,
    pending_nodes: Vec<(NodeId, NodeId, &'static str)>,
    errors: Vec<ResolvedBodyError>,
}

impl BodyValidator<'_> {
    fn error(&mut self, node_id: &NodeId, message: impl Into<String>) {
        self.errors
            .push(ResolvedBodyError::new(node_id.clone(), message));
    }

    fn register_node(&mut self, node_id: &NodeId) {
        if node_id.0.trim().is_empty() {
            self.error(node_id, "semantic NodeId is empty");
        } else if !self.nodes.insert(node_id.clone()) {
            self.error(node_id, "semantic NodeId occurs more than once in body");
        }
    }

    fn require_type(&mut self, node_id: &NodeId, ty: &ResolvedTypeId) {
        if self.types.get(ty).is_none() {
            self.error(
                node_id,
                format!("references missing canonical type '{}'", ty.as_str()),
            );
        }
    }

    fn validate_origin(&mut self, node_id: &NodeId, origin: &Origin) {
        if let Some(rule) = origin.rule() {
            if rule.trim().is_empty() {
                self.error(node_id, "generated Origin rule is empty");
            }
        }
    }

    fn validate_requirements(&mut self, node_id: &NodeId, values: &[BackendRequirement]) {
        let mut seen = BTreeSet::new();
        for value in values {
            if value.requirement_id.trim().is_empty() || value.capability.trim().is_empty() {
                self.error(
                    node_id,
                    "backend requirement must have requirement and capability identities",
                );
            }
            if !seen.insert((&value.requirement_id, &value.capability)) {
                self.error(node_id, "duplicate node-local backend requirement");
            }
        }
    }

    fn visit_block(&mut self, block: &ResolvedBlock) {
        self.register_node(&block.node_id);
        self.require_type(&block.node_id, &block.ty);
        self.validate_origin(&block.node_id, &block.origin);
        for statement in &block.statements {
            self.visit_stmt(statement);
        }
        if let Some(result) = &block.result {
            self.visit_expr(result);
            if result.ty != block.ty {
                self.error(
                    &block.node_id,
                    "block result type disagrees with block resolved type",
                );
            }
        }
    }

    fn visit_stmt(&mut self, statement: &ResolvedStmt) {
        self.register_node(&statement.node_id);
        self.require_type(&statement.node_id, &statement.ty);
        self.validate_origin(&statement.node_id, &statement.origin);
        self.validate_requirements(&statement.node_id, &statement.backend_requirements);
        match &statement.kind {
            ResolvedStmtKind::Bind {
                pattern,
                initializer,
            } => {
                self.visit_pattern(pattern);
                if let Some(initializer) = initializer {
                    self.visit_expr(initializer);
                }
            }
            ResolvedStmtKind::Assign {
                target,
                value,
                conversion,
            } => {
                self.visit_place(&statement.node_id, target);
                self.visit_expr(value);
                self.visit_conversion(&statement.node_id, conversion, Some(&value.ty));
                if let Some(target_ty) = self.place_type(&statement.node_id, target) {
                    if &conversion.to != target_ty {
                        self.error(
                            &statement.node_id,
                            "assignment conversion target disagrees with place type",
                        );
                    }
                }
            }
            ResolvedStmtKind::Return { value, conversion } => match (value, conversion) {
                (Some(value), Some(conversion)) => {
                    self.visit_expr(value);
                    self.visit_conversion(&statement.node_id, conversion, Some(&value.ty));
                }
                (None, None) => {}
                _ => self.error(
                    &statement.node_id,
                    "return value and conversion must either both exist or both be absent",
                ),
            },
            ResolvedStmtKind::Break(value) => {
                if let Some(value) = value {
                    self.visit_expr(value);
                }
            }
            ResolvedStmtKind::Continue | ResolvedStmtKind::NestedCallable(_) => {}
            ResolvedStmtKind::Expr(expression) => self.visit_expr(expression),
            ResolvedStmtKind::While { condition, body } => {
                self.visit_expr(condition);
                self.visit_block(body);
            }
            ResolvedStmtKind::WhileLet {
                pattern,
                initializer,
                body,
            } => {
                self.visit_pattern(pattern);
                self.visit_expr(initializer);
                if pattern.ty != initializer.ty {
                    self.error(
                        &statement.node_id,
                        "while-let pattern type disagrees with initializer type",
                    );
                }
                self.visit_block(body);
            }
            ResolvedStmtKind::Loop(body) => self.visit_block(body),
            ResolvedStmtKind::For {
                pattern,
                iterable,
                body,
            } => {
                self.visit_pattern(pattern);
                self.visit_expr(iterable);
                self.visit_block(body);
            }
            ResolvedStmtKind::Drop(places) => {
                if places.is_empty() {
                    self.error(&statement.node_id, "drop has no resolved places");
                }
                for place in places {
                    self.visit_place(&statement.node_id, place);
                }
            }
            ResolvedStmtKind::Contract { condition, .. } => self.visit_expr(condition),
            ResolvedStmtKind::Math(expressions) => {
                for expression in expressions {
                    self.visit_expr(expression);
                }
            }
            ResolvedStmtKind::Scope { body, .. } => self.visit_block(body),
            ResolvedStmtKind::Delegate { source, target, .. } => {
                self.visit_place(&statement.node_id, source);
                if let DelegateTarget::Local(local) = target {
                    self.require_local(&statement.node_id, local);
                }
            }
            ResolvedStmtKind::Pinned {
                value,
                timeout,
                binding,
                body,
            } => {
                self.visit_expr(value);
                if let Some(timeout) = timeout {
                    self.visit_expr(timeout);
                }
                if let Some(binding) = binding {
                    self.require_local(&statement.node_id, binding);
                }
                self.visit_block(body);
            }
        }
    }

    fn visit_pattern(&mut self, pattern: &ResolvedPattern) {
        self.register_node(&pattern.node_id);
        self.require_type(&pattern.node_id, &pattern.ty);
        self.validate_origin(&pattern.node_id, &pattern.origin);
        match &pattern.kind {
            ResolvedPatternKind::Wildcard | ResolvedPatternKind::Literal(_) => {}
            ResolvedPatternKind::Binding { local, .. } => {
                self.require_local(&pattern.node_id, local);
                if let Some(local) = self.body.locals.get(local) {
                    if local.ty != pattern.ty {
                        self.error(
                            &pattern.node_id,
                            "binding pattern type disagrees with local type",
                        );
                    }
                }
            }
            ResolvedPatternKind::Constructor { variant, fields } => {
                if variant.0.trim().is_empty() {
                    self.error(&pattern.node_id, "constructor variant identity is empty");
                }
                for (field, pattern) in fields {
                    if field.0.trim().is_empty() {
                        self.error(&pattern.node_id, "constructor field identity is empty");
                    }
                    self.visit_pattern(pattern);
                }
            }
            ResolvedPatternKind::Tuple(patterns) | ResolvedPatternKind::Array(patterns) => {
                for pattern in patterns {
                    self.visit_pattern(pattern);
                }
            }
            ResolvedPatternKind::Slice { prefix, rest } => {
                for pattern in prefix {
                    self.visit_pattern(pattern);
                }
                if let Some(rest) = rest {
                    self.visit_pattern(rest);
                }
            }
        }
    }

    fn visit_expr(&mut self, expression: &ResolvedExpr) {
        self.register_node(&expression.node_id);
        self.require_type(&expression.node_id, &expression.ty);
        self.validate_origin(&expression.node_id, &expression.origin);
        self.validate_requirements(&expression.node_id, &expression.backend_requirements);
        let mut effects = BTreeSet::new();
        for effect in &expression.effects {
            if !effects.insert(effect) {
                self.error(&expression.node_id, "duplicate expression effect identity");
            }
        }
        match &expression.kind {
            ResolvedExprKind::Literal(_) => {}
            ResolvedExprKind::FString(parts) => {
                for part in parts {
                    if let ResolvedFStringPart::Interpolation(expression) = part {
                        self.visit_expr(expression);
                    }
                }
            }
            ResolvedExprKind::Load(place) => {
                self.visit_place(&expression.node_id, place);
                if self
                    .place_type(&expression.node_id, place)
                    .is_some_and(|place_type| place_type != &expression.ty)
                {
                    self.error(
                        &expression.node_id,
                        "load type disagrees with its canonical place type",
                    );
                }
            }
            ResolvedExprKind::Constant(item) | ResolvedExprKind::ComptimeValue(item) => {
                if item.0.trim().is_empty() {
                    self.error(&expression.node_id, "resolved item identity is empty");
                }
            }
            ResolvedExprKind::Callable(callee) => {
                self.validate_callee(&expression.node_id, callee);
                if !matches!(
                    self.types.get(&expression.ty),
                    Some(ResolvedType::Function { .. })
                ) {
                    self.error(
                        &expression.node_id,
                        "callable value does not have a canonical function type",
                    );
                }
            }
            ResolvedExprKind::Project { value, projection } => {
                self.visit_expr(value);
                match projection {
                    ResolvedValueProjection::Field(field) if field.0.trim().is_empty() => {
                        self.error(&expression.node_id, "rvalue field identity is empty");
                    }
                    ResolvedValueProjection::Index(index) => self.visit_expr(index),
                    ResolvedValueProjection::Field(_)
                    | ResolvedValueProjection::Tuple(_)
                    | ResolvedValueProjection::Dereference => {}
                }
            }
            ResolvedExprKind::Lambda(lambda) => {
                self.register_node(&lambda.owner);
                let mut parameters = BTreeSet::new();
                for parameter in &lambda.parameters {
                    if !parameters.insert(parameter) {
                        self.error(
                            &expression.node_id,
                            "lambda parameter identity is duplicated",
                        );
                    }
                    self.require_local(&expression.node_id, parameter);
                }
                let mut captures = BTreeSet::new();
                for capture in &lambda.captures {
                    if !captures.insert(capture) || parameters.contains(capture) {
                        self.error(&expression.node_id, "lambda capture identity is invalid");
                    }
                    self.require_local(&expression.node_id, capture);
                }
                self.visit_block(&lambda.body);
            }
            ResolvedExprKind::DefaultArgument {
                callable,
                parameter,
            } => {
                if callable.0.trim().is_empty() || parameter.0 .0.trim().is_empty() {
                    self.error(
                        &expression.node_id,
                        "default argument contains an empty semantic identity",
                    );
                }
            }
            ResolvedExprKind::Binary { left, right, .. } => {
                self.visit_expr(left);
                self.visit_expr(right);
            }
            ResolvedExprKind::Unary { operand, .. }
            | ResolvedExprKind::Spawn(operand)
            | ResolvedExprKind::Await(operand) => self.visit_expr(operand),
            ResolvedExprKind::Call(call) => {
                self.visit_call(&expression.node_id, call);
                if call.result != expression.ty {
                    self.error(
                        &expression.node_id,
                        "call result disagrees with expression type",
                    );
                }
            }
            ResolvedExprKind::Tuple(values)
            | ResolvedExprKind::List(values)
            | ResolvedExprKind::Set(values) => {
                for value in values {
                    self.visit_expr(value);
                }
            }
            ResolvedExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.visit_expr(key);
                    self.visit_expr(value);
                }
            }
            ResolvedExprKind::Comprehension {
                pattern,
                value,
                iterable,
                guard,
            } => {
                self.visit_pattern(pattern);
                self.visit_expr(value);
                self.visit_expr(iterable);
                if let Some(guard) = guard {
                    self.visit_expr(guard);
                }
            }
            ResolvedExprKind::OptionalChain {
                receiver,
                field,
                field_type,
            } => {
                self.visit_expr(receiver);
                if field.0.trim().is_empty() {
                    self.error(
                        &expression.node_id,
                        "optional-chain field identity is empty",
                    );
                }
                self.require_type(&expression.node_id, field_type);
            }
            ResolvedExprKind::TypeOf(value) | ResolvedExprKind::Old(value) => {
                self.visit_expr(value);
            }
            ResolvedExprKind::Record { nominal, fields } => {
                if nominal.as_str().trim().is_empty() {
                    self.error(&expression.node_id, "record nominal identity is empty");
                }
                let mut seen = BTreeSet::new();
                for field in fields {
                    if !seen.insert(&field.field) {
                        self.error(&expression.node_id, "duplicate resolved record field");
                    }
                    self.visit_expr(&field.value);
                    self.visit_conversion(
                        &expression.node_id,
                        &field.conversion,
                        Some(&field.value.ty),
                    );
                }
            }
            ResolvedExprKind::Block(block)
            | ResolvedExprKind::Comptime(block)
            | ResolvedExprKind::Quote(block) => {
                self.visit_block(block);
            }
            ResolvedExprKind::Scope { body, .. } => {
                self.visit_block(body);
                if body.ty != expression.ty {
                    self.error(
                        &expression.node_id,
                        "expression scope body type disagrees with expression type",
                    );
                }
            }
            ResolvedExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.visit_expr(condition);
                self.visit_block(then_block);
                self.visit_block(else_block);
                if then_block.ty != expression.ty || else_block.ty != expression.ty {
                    self.error(
                        &expression.node_id,
                        "if branch type disagrees with expression type",
                    );
                }
            }
            ResolvedExprKind::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee);
                for arm in arms {
                    self.register_node(&arm.node_id);
                    self.validate_origin(&arm.node_id, &arm.origin);
                    self.visit_pattern(&arm.pattern);
                    if let Some(guard) = &arm.guard {
                        self.visit_expr(guard);
                    }
                    self.visit_expr(&arm.body);
                    if arm.body.ty != expression.ty {
                        self.error(
                            &arm.node_id,
                            "match arm type disagrees with match expression type",
                        );
                    }
                }
            }
            ResolvedExprKind::Try {
                value,
                propagation_target,
            } => {
                self.visit_expr(value);
                if propagation_target.0.trim().is_empty() {
                    self.error(&expression.node_id, "error propagation target is empty");
                }
            }
            ResolvedExprKind::Range { start, end } => {
                self.visit_expr(start);
                self.visit_expr(end);
            }
            ResolvedExprKind::Slice { target, start, end } => {
                self.visit_expr(target);
                if let Some(start) = start {
                    self.visit_expr(start);
                }
                if let Some(end) = end {
                    self.visit_expr(end);
                }
            }
            ResolvedExprKind::Cast { value, conversion } => {
                self.visit_expr(value);
                self.visit_conversion(&expression.node_id, conversion, Some(&value.ty));
                if conversion.to != expression.ty {
                    self.error(
                        &expression.node_id,
                        "cast conversion target disagrees with expression type",
                    );
                }
            }
            ResolvedExprKind::TypeValue(ty) => self.require_type(&expression.node_id, ty),
        }
    }

    fn visit_call(&mut self, owner: &NodeId, call: &ResolvedCall) {
        self.validate_callee(owner, &call.callee);
        self.require_type(owner, &call.result);
        for argument in &call.type_arguments {
            self.require_type(owner, argument);
        }
        let mut parameters = BTreeSet::new();
        for argument in &call.arguments {
            if !parameters.insert(&argument.parameter) {
                self.error(owner, "call contains a duplicate resolved parameter");
            }
            self.visit_expr(&argument.value);
            self.visit_conversion(owner, &argument.conversion, Some(&argument.value.ty));
        }
        let mut effects = BTreeSet::new();
        for effect in &call.effects {
            if !effects.insert(effect) {
                self.error(owner, "call contains a duplicate resolved effect");
            }
        }
        let mut endpoints = BTreeSet::new();
        for session in &call.session {
            self.require_local(owner, &session.endpoint);
            if !endpoints.insert(&session.endpoint) {
                self.error(owner, "call advances one session endpoint more than once");
            }
            if session.before == session.after {
                self.error(owner, "session action does not advance its residual state");
            }
        }
    }

    fn validate_callee(&mut self, owner: &NodeId, callee: &ResolvedCallee) {
        let valid = match callee {
            ResolvedCallee::Function(item)
            | ResolvedCallee::Constructor(item)
            | ResolvedCallee::Extern(item) => !item.0.trim().is_empty(),
            ResolvedCallee::Builtin(item) => !item.as_str().trim().is_empty(),
            ResolvedCallee::LocalClosure(local) => {
                self.require_local(owner, local);
                let callable = self.body.locals.get(local).is_some_and(|local| {
                    matches!(
                        self.types.get(&local.ty),
                        Some(ResolvedType::Function { .. })
                    )
                });
                if !callable && self.body.locals.contains_key(local) {
                    self.error(
                        owner,
                        "local callee does not have a canonical function type",
                    );
                }
                !local.0 .0.trim().is_empty() && callable
            }
            ResolvedCallee::ActorMethod { actor, method } => {
                !actor.0.trim().is_empty() && !method.as_str().trim().is_empty()
            }
            ResolvedCallee::ProtocolMethod { protocol, method } => {
                !protocol.0.trim().is_empty() && !method.as_str().trim().is_empty()
            }
            ResolvedCallee::Transition(transition) => {
                !transition.flow.0.trim().is_empty()
                    && !transition.event.trim().is_empty()
                    && !transition.source.name.trim().is_empty()
            }
        };
        if !valid {
            self.error(owner, "callee contains an empty semantic identity");
        }
    }

    fn visit_conversion(
        &mut self,
        owner: &NodeId,
        conversion: &CheckedConversion,
        actual_from: Option<&ResolvedTypeId>,
    ) {
        self.require_type(owner, &conversion.from);
        self.require_type(owner, &conversion.to);
        if let Some(actual_from) = actual_from {
            if actual_from != &conversion.from {
                self.error(owner, "conversion source disagrees with value type");
            }
        }
        if conversion.kind == CheckedConversionKind::Identity && conversion.from != conversion.to {
            self.error(owner, "identity conversion changes the canonical type");
        }
    }

    fn visit_place(&mut self, owner: &NodeId, place: &ResolvedPlace) {
        self.require_local(owner, &place.base);
        for projection in &place.projections {
            self.require_type(owner, projection.ty());
            match projection {
                ResolvedProjection::Field { field, name, .. }
                    if field.0.trim().is_empty() || name.trim().is_empty() =>
                {
                    self.error(owner, "place field identity or display name is empty");
                }
                ResolvedProjection::Index {
                    index: ResolvedIndex::Dynamic(node),
                    ..
                } => self
                    .pending_nodes
                    .push((owner.clone(), node.clone(), "dynamic index")),
                _ => {}
            }
        }
    }

    fn place_type<'a>(
        &'a mut self,
        owner: &NodeId,
        place: &'a ResolvedPlace,
    ) -> Option<&'a ResolvedTypeId> {
        let local = self.body.locals.get(&place.base);
        if local.is_none() {
            self.require_local(owner, &place.base);
        }
        local.map(|local| place.projected_type(local))
    }

    fn require_local(&mut self, owner: &NodeId, local: &ResolvedLocalId) {
        if !self.body.locals.contains_key(local) {
            self.error(
                owner,
                format!("references missing resolved local '{}'", local.0 .0),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Type;
    use crate::core::ir::{ResolvedTypeCapabilities, ResolvedTypeName};
    use crate::core::phase::ZonkedTy;
    use crate::span::Span;

    fn node(value: &str) -> NodeId {
        NodeId(format!("test:{value}"))
    }

    fn origin() -> Origin {
        Origin::User(Span::UNKNOWN)
    }

    fn types() -> (ResolvedTypeTable, ResolvedTypeId, ResolvedTypeId) {
        let mut types = ResolvedTypeTable::new();
        let mut resolve = |name: &str| ResolvedTypeName::primitive(name);
        let i32_ty = types
            .intern_zonked(
                &ZonkedTy::from_resolved(Type::Name("i32".into(), Vec::new())).unwrap(),
                &ResolvedTypeCapabilities::default(),
                &mut resolve,
            )
            .unwrap();
        let unit_ty = types
            .intern_zonked(
                &ZonkedTy::from_resolved(Type::Name("unit".into(), Vec::new())).unwrap(),
                &ResolvedTypeCapabilities::default(),
                &mut resolve,
            )
            .unwrap();
        (types, i32_ty, unit_ty)
    }

    fn literal(id: &str, ty: &ResolvedTypeId, value: i64) -> ResolvedExpr {
        ResolvedExpr {
            node_id: node(id),
            origin: origin(),
            ty: ty.clone(),
            effects: Vec::new(),
            backend_requirements: Vec::new(),
            kind: ResolvedExprKind::Literal(ResolvedLiteral::Int(value)),
        }
    }

    fn valid_body(i32_ty: &ResolvedTypeId, unit_ty: &ResolvedTypeId) -> ResolvedBody {
        let local_id = ResolvedLocalId(node("local.x"));
        let local = ResolvedLocal {
            id: local_id.clone(),
            display_name: "x".into(),
            ty: i32_ty.clone(),
            mutable: false,
            origin: origin(),
        };
        let pattern = ResolvedPattern {
            node_id: node("pattern.x"),
            origin: origin(),
            ty: i32_ty.clone(),
            kind: ResolvedPatternKind::Binding {
                local: local_id.clone(),
                by_reference: None,
            },
        };
        let bind = ResolvedStmt {
            node_id: node("stmt.bind"),
            origin: origin(),
            ty: unit_ty.clone(),
            backend_requirements: Vec::new(),
            kind: ResolvedStmtKind::Bind {
                pattern,
                initializer: Some(literal("expr.one", i32_ty, 1)),
            },
        };
        let result = ResolvedExpr {
            node_id: node("expr.load"),
            origin: origin(),
            ty: i32_ty.clone(),
            effects: Vec::new(),
            backend_requirements: Vec::new(),
            kind: ResolvedExprKind::Load(ResolvedPlace::root(local_id)),
        };
        ResolvedBody {
            owner: node("func.main"),
            locals: BTreeMap::from([(local.id.clone(), local)]),
            captures: Vec::new(),
            place_inputs: BTreeMap::new(),
            default_values: BTreeMap::new(),
            root: ResolvedBlock {
                node_id: node("block.root"),
                origin: origin(),
                ty: i32_ty.clone(),
                statements: vec![bind],
                result: Some(Box::new(result)),
            },
        }
    }

    #[test]
    fn structured_body_validates_without_surface_ast() {
        let (types, i32_ty, unit_ty) = types();
        let body = valid_body(&i32_ty, &unit_ty);
        assert!(body.validate(&types).is_ok());
    }

    #[test]
    fn validator_rejects_duplicate_node_identity() {
        let (types, i32_ty, unit_ty) = types();
        let mut body = valid_body(&i32_ty, &unit_ty);
        body.root.result.as_mut().unwrap().node_id = body.root.node_id.clone();
        let errors = body.validate(&types).unwrap_err();
        assert!(errors
            .iter()
            .any(|error| error.message.contains("more than once")));
    }

    #[test]
    fn validator_rejects_conversion_source_mismatch() {
        let (types, i32_ty, unit_ty) = types();
        let mut body = valid_body(&i32_ty, &unit_ty);
        body.root.statements.push(ResolvedStmt {
            node_id: node("stmt.return"),
            origin: origin(),
            ty: unit_ty.clone(),
            backend_requirements: Vec::new(),
            kind: ResolvedStmtKind::Return {
                value: Some(literal("expr.return", &i32_ty, 1)),
                conversion: Some(CheckedConversion {
                    kind: CheckedConversionKind::Identity,
                    from: unit_ty,
                    to: i32_ty,
                }),
            },
        });
        let errors = body.validate(&types).unwrap_err();
        assert!(errors
            .iter()
            .any(|error| error.message.contains("conversion source disagrees")));
    }

    #[test]
    fn validator_rejects_dynamic_index_without_body_node() {
        let (types, i32_ty, unit_ty) = types();
        let mut body = valid_body(&i32_ty, &unit_ty);
        let result = body.root.result.as_mut().unwrap();
        let ResolvedExprKind::Load(place) = &mut result.kind else {
            panic!("fixture result must be a load");
        };
        place.projections.push(ResolvedProjection::Index {
            index: ResolvedIndex::Dynamic(node("expr.missing-index")),
            ty: i32_ty,
        });
        let errors = body.validate(&types).unwrap_err();
        assert!(errors
            .iter()
            .any(|error| error.message.contains("dynamic index references missing")));
    }

    #[test]
    fn validator_accepts_dynamic_index_from_typed_place_input_catalog() {
        let (types, i32_ty, unit_ty) = types();
        let mut body = valid_body(&i32_ty, &unit_ty);
        let index = literal("expr.index", &i32_ty, 0);
        body.place_inputs
            .insert(index.node_id.clone(), index.clone());
        let result = body.root.result.as_mut().unwrap();
        let ResolvedExprKind::Load(place) = &mut result.kind else {
            panic!("fixture result must be a load");
        };
        place.projections.push(ResolvedProjection::Index {
            index: ResolvedIndex::Dynamic(index.node_id),
            ty: i32_ty,
        });
        body.validate(&types).expect("dynamic index is owned");
    }

    #[test]
    fn validator_rejects_duplicate_call_parameter_identity() {
        let (types, i32_ty, unit_ty) = types();
        let mut body = valid_body(&i32_ty, &unit_ty);
        let parameter = ResolvedParameterId(node("param.value"));
        let argument = |id: &str| ResolvedArgument {
            parameter: parameter.clone(),
            value: literal(id, &i32_ty, 1),
            conversion: CheckedConversion {
                kind: CheckedConversionKind::Identity,
                from: i32_ty.clone(),
                to: i32_ty.clone(),
            },
        };
        body.root.statements.push(ResolvedStmt {
            node_id: node("stmt.call"),
            origin: origin(),
            ty: unit_ty,
            backend_requirements: Vec::new(),
            kind: ResolvedStmtKind::Expr(ResolvedExpr {
                node_id: node("expr.call"),
                origin: origin(),
                ty: i32_ty.clone(),
                effects: Vec::new(),
                backend_requirements: Vec::new(),
                kind: ResolvedExprKind::Call(ResolvedCall {
                    callee: ResolvedCallee::Builtin(BuiltinId::new("test.identity").unwrap()),
                    result: i32_ty.clone(),
                    type_arguments: Vec::new(),
                    arguments: vec![argument("expr.arg-a"), argument("expr.arg-b")],
                    permission: None,
                    effects: Vec::new(),
                    session: Vec::new(),
                }),
            }),
        });
        let errors = body.validate(&types).unwrap_err();
        assert!(errors
            .iter()
            .any(|error| error.message.contains("duplicate resolved parameter")));
    }
}
