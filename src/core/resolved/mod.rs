use crate::ast::{
    AstNodeMeta, AstOrigin, AstParentHint, Expr, FStringPart, File, FlowDef, Item, Pattern,
    PatternKind, Stmt, Type,
};
use crate::core::checker::flow::FlowAcc;
use crate::core::phase::{TypeScheme, ZonkedTy};
use crate::diagnostic::Diagnostic;
use crate::span::{SourceRegistry, Span};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[cfg(test)]
thread_local! {
    /// Test-only trace of legacy codegen body entry points.  This is kept
    /// separate from the checked-program boundary: already-emitted canonical
    /// callables must never enter the legacy emitter.
    static TEST_LEGACY_CODEGEN_FUNCTION_CALLS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub const RESOLVED_IR_VERSION: &str = "mimi-resolved-ir-1";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub String);

/// The single canonical builder for anonymous semantic node identities.
///
/// `SourceId` is deliberately absent from the emitted identity: it is a dense,
/// session-local allocation index and therefore changes when import discovery
/// order changes.  User nodes are anchored by their stable `SourceKey`, source
/// range, syntax kind, and semantic role.  Nodes without an honest source
/// range use a controlled role discriminator supplied by the walker.
pub(crate) struct NodeIdBuilder<'a> {
    sources: &'a SourceRegistry,
}

impl<'a> NodeIdBuilder<'a> {
    pub(crate) fn new(sources: &'a SourceRegistry) -> Self {
        Self { sources }
    }

    pub(crate) fn anonymous(
        &self,
        owner: &NodeId,
        kind: &str,
        role: &str,
        span: Option<Span>,
        origin: AstOrigin,
        errors: &mut Vec<Diagnostic>,
    ) -> NodeId {
        if origin == AstOrigin::User {
            if let Some(span) = span.filter(|span| span.start_line > 0 && span.start_col > 0) {
                let source = if span.source_id.is_known() {
                    match self.sources.key(span.source_id) {
                        Some(key) => stable_id_fragment(key.as_str()),
                        None => {
                            errors.push(Diagnostic::error(
                                format!(
                                "TOOL-RESOLUTION-001: source id {} has no SourceRegistry record",
                                span.source_id.raw()
                            ),
                                span,
                            ));
                            "unregistered-source".to_string()
                        }
                    }
                } else {
                    "unknown-source".to_string()
                };
                NodeId(format!(
                    "{}/node:{}@{}:{}:{}-{}:{}",
                    owner.0,
                    stable_id_fragment(kind),
                    source,
                    span.start_line,
                    span.start_col,
                    span.end_line,
                    span.end_col
                ))
            } else {
                NodeId(format!(
                    "{}/fallback:{}:{}",
                    owner.0,
                    stable_id_fragment(kind),
                    stable_id_fragment(role)
                ))
            }
        } else {
            NodeId(format!(
                "{}/generated:{}:{}:{}",
                owner.0,
                stable_id_fragment(kind),
                stable_id_fragment(origin.rule().unwrap_or("missing-rule")),
                stable_id_fragment(role)
            ))
        }
    }
}

pub(crate) fn stable_id_fragment(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':') {
            escaped.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(escaped, "%{byte:02x}");
        }
    }
    escaped
}

pub(crate) fn builtin_record_schema(
    owner: &str,
) -> Option<&'static [(&'static str, &'static str)]> {
    match owner {
        "builtin:type:MemoryDump" => Some(&[("fields", "string"), ("count", "i32")]),
        "builtin:type:PanicPayload" => Some(&[
            ("error_type", "string"),
            ("file", "string"),
            ("line", "i32"),
            ("stack", "string"),
        ]),
        "builtin:type:PeerFault" => Some(&[("peer_id", "string"), ("reason", "string")]),
        "builtin:type:SystemTrace" => Some(&[
            ("last_state_name", "string"),
            ("unexpected_event", "string"),
            ("snapshot", "string"),
            ("memory_dump", "MemoryDump"),
            ("panic_payload", "PanicPayload"),
        ]),
        // v0.34.19 (CHECKER-GAP): ExecResult/StatResult are builtin records but had
        // no field schema, so `r.exit_code` / `s.is_file` field access failed the
        // resolved-IR field-catalog lookup. Field sets mirror the codegen/runtime
        // construction (io.rs compile_exec_safe, misc.rs file_stat).
        "builtin:type:ExecResult" => Some(&[
            ("exit_code", "i32"),
            ("stdout", "string"),
            ("stderr", "string"),
        ]),
        "builtin:type:StatResult" => Some(&[
            ("size", "i64"),
            ("modified", "i64"),
            ("is_file", "bool"),
            ("is_dir", "bool"),
        ]),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedItemKind {
    Function,
    Type,
    Constant,
    Capability,
    Trait,
    Impl,
    ExternBlock,
    Actor,
    Flow,
    Session,
}

#[derive(Debug, Clone)]
pub struct ResolvedItem {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub kind: ResolvedItemKind,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanPrecision {
    Exact,
    /// A real source range belonging to a child/token anchors the node, but
    /// the AST representation cannot yet express the node's full range.
    SourceAnchor,
    DeclarationFallback,
}

#[derive(Debug, Clone)]
pub struct NodeMeta {
    pub node_id: NodeId,
    pub origin: Origin,
    pub precision: SpanPrecision,
    /// Ephemeral checker correlation key. Cleared before `CheckedProgram`
    /// crosses its construction boundary and never used as semantic identity.
    expression_key: Option<ExpressionTypeKey>,
    /// Ephemeral shared-binding kind and initializer correlation key used to
    /// materialize the binding's canonical ownership type.
    shared_binding: Option<(crate::ast::SharedKind, ExpressionTypeKey)>,
    /// Ephemeral reference-binding initializer correlation key used to
    /// materialize the binding's canonical Reference type even when the ref
    /// local is never used (V-1).
    ref_binding: Option<ExpressionTypeKey>,
    /// Ephemeral explicit type operand consumed while constructing canonical IR.
    type_operand: Option<Type>,
    /// Ephemeral ordered generic arguments consumed while constructing canonical IR.
    type_arguments: Vec<Type>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ExpressionTypeKey {
    source_id: u32,
    start_line: usize,
    start_col: usize,
    end_line: usize,
    end_col: usize,
    origin_kind: &'static str,
    origin_rule: Option<&'static str>,
    expression_kind: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedCallKind {
    Function,
    Extern,
    Builtin,
    Method,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ResolvedCallSite {
    pub node_id: NodeId,
    pub owner: String,
    pub callee: String,
    /// Stable Checker node identity of a syntactic method receiver, when the
    /// call is written as `receiver.method(...)`. Consumers that enforce
    /// receiver-type boundaries must use this identity instead of parsing
    /// the lossy display spelling in `callee`.
    pub method_receiver: Option<NodeId>,
    pub argc: usize,
    /// Expected arity from function/extern directories when known.
    pub expected_argc: Option<usize>,
    /// Effects from callee function directory when known.
    pub effects: Vec<String>,
    /// Return type display from callee function directory when known.
    pub ret: Option<String>,
    pub kind: ResolvedCallKind,
    /// Qualified-import target selected from the loader identity graph.  The
    /// surface collector keeps the historical `alias.field` spelling, while
    /// this optional node identity lets body lowering dispatch duplicate
    /// exports without guessing by bare name.
    pub target_function: Option<NodeId>,
    /// Canonical callable identity recovered from a typed callable body or a
    /// Checker-issued declaration-root expression. Surface call-site
    /// collection intentionally records only syntax facts; typed lowering
    /// closes the target before backend use.
    pub resolved_callee: Option<crate::core::ir::ResolvedCallee>,
    pub origin: Origin,
}

impl ResolvedCallSite {
    pub fn arity_matches(&self) -> bool {
        self.expected_argc
            .map(|expected| expected == self.argc)
            .unwrap_or(true)
    }

    /// Check a declaration-root FFI call against its exact Checker-owned
    /// declaration. Variadic tails are accepted here only as source facts for
    /// the non-executable root census; callable-body and executable consumers
    /// continue to use `arity_matches` and the existing MIR admission gates.
    pub(crate) fn checked_ffi_source_arity_matches(
        &self,
        typed_argument_count: usize,
        declaration: &crate::core::ir::ResolvedFfiDeclarationContract,
    ) -> bool {
        let fixed_argument_count = declaration.fixed_parameter_ids.len();
        if self.kind != ResolvedCallKind::Extern
            || self.argc != typed_argument_count
            || self.expected_argc != Some(fixed_argument_count)
        {
            return false;
        }
        if declaration.invocation.variadic {
            self.argc >= fixed_argument_count
        } else {
            self.argc == fixed_argument_count && self.arity_matches()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FlowId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StateId {
    pub flow: FlowId,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedState {
    pub node_id: NodeId,
    pub id: StateId,
    pub payload: Vec<(String, Type)>,
    pub field_ids: BTreeMap<String, NodeId>,
    pub origin: Origin,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransitionId {
    pub flow: FlowId,
    pub event: String,
    pub source: StateId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    User(Span),
    Desugared {
        parent: NodeId,
        rule: String,
        span: Span,
    },
    PrototypeFallback {
        parent: NodeId,
        rule: String,
        span: Span,
    },
    RuntimeSystem {
        parent: NodeId,
        rule: String,
        span: Span,
    },
}

impl Origin {
    pub fn user_span(&self) -> Span {
        match self {
            Self::User(span)
            | Self::Desugared { span, .. }
            | Self::PrototypeFallback { span, .. }
            | Self::RuntimeSystem { span, .. } => *span,
        }
    }

    pub fn rule(&self) -> Option<&str> {
        match self {
            Self::User(_) => None,
            Self::Desugared { rule, .. }
            | Self::PrototypeFallback { rule, .. }
            | Self::RuntimeSystem { rule, .. } => Some(rule),
        }
    }
}

#[derive(Default)]
struct OriginCatalog {
    entries: HashMap<NodeId, Origin>,
}

impl OriginCatalog {
    fn register(&mut self, node_id: &NodeId, origin: &Origin, errors: &mut Vec<Diagnostic>) {
        if let Some(existing) = self.entries.get(node_id) {
            if existing != origin {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: NodeId '{}' has conflicting Origin records",
                        node_id.0
                    ),
                    origin.user_span(),
                ));
            }
            return;
        }
        self.entries.insert(node_id.clone(), origin.clone());
    }

    fn validate(&self, errors: &mut Vec<Diagnostic>) {
        for (node_id, origin) in &self.entries {
            let mut current_id = node_id;
            let mut current = origin;
            let mut seen = std::collections::HashSet::new();
            loop {
                match current {
                    Origin::User(_) => break,
                    Origin::Desugared { parent, rule, span }
                    | Origin::PrototypeFallback { parent, rule, span }
                    | Origin::RuntimeSystem { parent, rule, span } => {
                        if rule.trim().is_empty() {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: generated NodeId '{}' has an empty Origin rule",
                                    current_id.0
                                ),
                                *span,
                            ));
                            break;
                        }
                        if parent == current_id {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: generated NodeId '{}' has a self-referential Origin",
                                    current_id.0
                                ),
                                *span,
                            ));
                            break;
                        }
                        if !seen.insert(current_id.clone()) || seen.contains(parent) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: Origin cycle reaches NodeId '{}'",
                                    parent.0
                                ),
                                *span,
                            ));
                            break;
                        }
                        let Some(parent_origin) = self.entries.get(parent) else {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: generated NodeId '{}' references missing Origin parent '{}'",
                                    current_id.0, parent.0
                                ),
                                *span,
                            ));
                            break;
                        };
                        current_id = parent;
                        current = parent_origin;
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedTransition {
    pub node_id: NodeId,
    pub id: TransitionId,
    pub targets: Vec<StateId>,
    pub source_parameter_id: NodeId,
    pub params: Vec<(String, Type)>,
    pub parameter_ids: Vec<NodeId>,
    pub is_fallback: bool,
    pub is_ffi_pinned: bool,
    /// 追加 A / 0.38.46 Phase C: Silent Transition — true if this
    /// transition has no cross-boundary operations (Channel send, FFI call,
    /// Actor event emit). When true, lowering **strips** `TransitionEpoch`
    /// (amendment clause 5.1 silent stay / local self-loop, no atomic tax).
    /// Crossing Channel / FFI / mailbox must pack the epoch; mismatch is a
    /// typed boundary error. v0.34.11: renamed from silent_stay — the
    /// property is about boundary ops, not the terminal form (ADR-001
    /// removed become/stay).
    pub silent_transition: bool,
    pub origin: Origin,
    pub span: Span,
    pub fails: Option<Type>,
}

#[derive(Debug, Clone)]
pub struct ResolvedFlow {
    pub node_id: NodeId,
    pub id: FlowId,
    /// Stable Flow-level type binders, in source declaration order.
    pub generic_parameters: Vec<(String, NodeId)>,
    pub states: HashMap<String, ResolvedState>,
    pub transitions: Vec<TransitionId>,
    pub max_children: Option<usize>,
    pub mailbox_depth: Option<usize>,
    pub persistent_fields: Vec<String>,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendProfile {
    Interpreter,
    Native,
    Verifier,
    Component,
}

#[derive(Debug, Clone)]
pub struct CapabilityRequirement {
    pub requirement_id: &'static str,
    pub capability: &'static str,
    pub flow: FlowId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ResolvedFunction {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub params: Vec<(String, Type)>,
    pub param_decls: Vec<crate::ast::Param>,
    pub ret: Type,
    pub effects: Vec<String>,
    /// Direct checker effect sources before interprocedural propagation.
    /// Route gates use these provenance facts to distinguish a checked
    /// scalar-FFI boundary from unrelated unknown operations in the same
    /// call graph; an `unknown` summary alone is never permission to proceed.
    pub(crate) direct_effect_sources: BTreeMap<crate::core::effects::EffectKind, BTreeSet<String>>,
    pub pub_: bool,
    pub is_comptime: bool,
    pub is_async: bool,
    pub extern_abi: Option<String>,
    pub generics: Vec<crate::ast::GenericParam>,
    /// Canonical generic binders visible to this callable, including binders
    /// inherited from an enclosing impl or callable.
    pub generic_binders: Vec<(String, NodeId)>,
    pub where_clause: Vec<crate::ast::WhereClause>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedSession {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub body: crate::ast::SessionType,
    /// Pretty-printed residual session type for directory consumers.
    pub body_display: String,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedActorMethod {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub ret: String,
    pub effects: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedActor {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub fields: Vec<(String, Type, bool)>,
    /// Stable declaration identity for every actor field, keyed by source name.
    pub field_ids: BTreeMap<String, NodeId>,
    pub methods: Vec<String>,
    pub method_signatures: Vec<ResolvedActorMethod>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedCapability {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub combined_with: Option<String>,
    /// Checker-owned component names in the historical construction order.
    /// A plain declaration has one component; `cap A + B` has A then B.
    pub components: Vec<String>,
    pub origin: Origin,
}

fn capability_components(name: &str, combined: Option<&str>) -> Vec<String> {
    match combined {
        Some(combined) => {
            let parts = combined
                .split(" + ")
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>();
            if parts.len() > 1 {
                parts
            } else {
                vec![name.to_string(), combined.trim().to_string()]
            }
        }
        None => vec![name.to_string()],
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedConstValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Unit,
    /// Non-literal or non-materializable initializer (fail-closed: value not folded).
    Complex,
}

#[derive(Debug, Clone)]
pub struct ResolvedConstant {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub ty: Option<String>,
    pub value: ResolvedConstValue,
    /// ABI tag retained for constants declared as native data exports.
    pub extern_abi: Option<String>,
    /// Validated scalar C data-symbol descriptor, populated during checked
    /// program construction and consumed by native emitters.
    pub native_data_export: Option<crate::core::native_data_export::CheckedNativeDataExport>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedMethodSig {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub ret: String,
    pub effects: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedTrait {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub methods: Vec<String>,
    /// Stable declaration owners in the same source order as `methods`.
    pub method_owners: Vec<NodeId>,
    pub method_signatures: Vec<ResolvedMethodSig>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedImpl {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub trait_name: String,
    pub type_name: String,
    /// Checked generic declarations owned by this implementation. These
    /// binders are distinct from method-level binders, while each method's
    /// canonical signature carries the combined ordered binder identity.
    pub generics: Vec<crate::ast::GenericParam>,
    pub generic_parameters: Vec<(String, NodeId)>,
    pub methods: Vec<String>,
    /// Exact callable identities in the same declaration order as `methods`.
    /// Compatibility display signatures below must never be used to recover
    /// these owners or their checker-finalized ABI types.
    pub method_owners: Vec<NodeId>,
    pub method_signatures: Vec<ResolvedMethodSig>,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedTypeKind {
    Alias,
    Newtype,
    Record,
    Enum,
    Union,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedVariantShape {
    Unit,
    Tuple,
    Record,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVariantMember {
    pub node_id: NodeId,
    pub name: String,
    pub ty: crate::core::ResolvedTypeId,
}

/// Checker-owned schema for one enum variant.
///
/// The schema contains stable declaration identities and canonical member
/// types only. Consumers must not recover payload structure from the retained
/// surface declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVariantSchema {
    pub node_id: NodeId,
    pub owner: NodeId,
    pub name: String,
    pub shape: ResolvedVariantShape,
    pub members: Vec<ResolvedVariantMember>,
}

#[derive(Debug, Clone)]
pub struct ResolvedTypeDef {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub kind: ResolvedTypeKind,
    /// Alias/newtype target type display when applicable.
    pub alias_of: Option<String>,
    /// Record/union fields: (name, type display).
    pub fields: Vec<(String, String)>,
    /// Stable record/union field identities keyed by their display names.
    pub field_ids: BTreeMap<String, NodeId>,
    /// Enum variants: (name, optional payload display).
    pub variants: Vec<(String, Option<String>)>,
    /// Stable enum variant identities keyed by their display names.
    pub variant_ids: BTreeMap<String, NodeId>,
    /// Stable generic binder identities in declaration order.
    pub generic_parameters: Vec<(String, NodeId)>,
    /// Complete checked declaration snapshot for declaration-only consumers.
    pub declaration: crate::ast::TypeDef,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedExternFunc {
    pub node_id: NodeId,
    pub name: String,
    pub span: Span,
    pub params: Vec<(String, String)>,
    pub typed_params: Vec<(String, Type, Option<crate::ast::CapMode>)>,
    pub parameter_ids: Vec<NodeId>,
    pub ret: String,
    pub ret_type: Option<Type>,
    pub requires: Option<Expr>,
    pub ensures: Option<Expr>,
    pub variadic: bool,
    /// Effective flag after the enclosing block default is applied.
    pub no_panic: bool,
    /// Effective flag after the enclosing block default is applied.
    pub returns_errno: bool,
    /// Whether this declaration itself carries `#[errno]`; unlike
    /// `returns_errno`, this excludes an inherited block-level default.
    pub declared_returns_errno: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedExternBlock {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub abi: String,
    pub funcs: Vec<String>,
    pub signatures: Vec<ResolvedExternFunc>,
    pub no_panic: bool,
    pub unsafe_: bool,
    /// SD-3: block-level errno checking default.
    pub returns_errno: bool,
    pub origin: Origin,
}

/// Pre-computed transition lookup tables shared by interpreter and codegen.
/// Built once from `CheckedProgram::transitions()` via `build_transition_tables()`,
/// eliminating ~90 lines of duplicated construction in each backend (AD-6).
#[derive(Debug, Clone)]
pub struct TransitionTables {
    /// (flow, event, source) → target state names.
    pub resolved: HashMap<(String, String, String), Vec<String>>,
    /// Fallback/matrix-injected transition keys.
    pub fallbacks: std::collections::HashSet<(String, String, String)>,
    /// FFI-pinned system transition keys.
    pub pinned: std::collections::HashSet<(String, String, String)>,
    /// (flow, event, source) → event parameter arity.
    pub param_arity: HashMap<(String, String, String), usize>,
    /// (flow, event, source) → [(param_name, type_display)].
    pub param_lists: HashMap<(String, String, String), Vec<(String, String)>>,
    /// flow → [(event, source, targets_joined, is_fallback, is_pinned, param_count)].
    pub by_flow: HashMap<String, Vec<(String, String, String, bool, bool, usize)>>,
    /// event → [(flow, source, targets_joined, is_fallback, is_pinned, param_count)].
    pub by_event: HashMap<String, Vec<(String, String, String, bool, bool, usize)>>,
}

#[derive(Debug)]
pub struct CheckedProgram {
    /// Whether the checked source was assembled from one or more imports.
    /// This checker-owned route fact lets consumers distinguish imported
    /// programs without retaining or reopening the surface AST.
    has_imports: bool,
    /// Checker-owned source provenance for route and diagnostic gates.
    source_registry: SourceRegistry,
    items: HashMap<NodeId, ResolvedItem>,
    node_meta: HashMap<NodeId, NodeMeta>,
    call_sites: HashMap<NodeId, ResolvedCallSite>,
    flows: HashMap<FlowId, ResolvedFlow>,
    transitions: HashMap<TransitionId, ResolvedTransition>,
    functions: HashMap<NodeId, ResolvedFunction>,
    sessions: HashMap<NodeId, ResolvedSession>,
    actors: HashMap<NodeId, ResolvedActor>,
    actor_descriptors: BTreeMap<NodeId, crate::core::ir::ResolvedActorDescriptor>,
    capabilities: HashMap<NodeId, ResolvedCapability>,
    constants: HashMap<NodeId, ResolvedConstant>,
    traits: HashMap<NodeId, ResolvedTrait>,
    impls: HashMap<NodeId, ResolvedImpl>,
    type_defs: HashMap<NodeId, ResolvedTypeDef>,
    /// Canonical declaration and native ABI facts produced after the type
    /// table, field identities, aliases, and variant schemas are finalized.
    native_declarations: crate::core::ir::ResolvedNativeDeclarations,
    extern_blocks: HashMap<NodeId, ResolvedExternBlock>,
    backend_requirements: Vec<CapabilityRequirement>,
    type_schemes: HashMap<NodeId, TypeScheme>,
    zonked_function_types: HashMap<NodeId, (Vec<ZonkedTy>, ZonkedTy)>,
    resolved_types: crate::core::ResolvedTypeTable,
    resolved_signatures: BTreeMap<NodeId, crate::core::ResolvedSignature>,
    /// Canonical ABI of trait declarations, separate from executable callable
    /// signatures because trait methods have no body/callable CFG of their own.
    resolved_trait_signatures: BTreeMap<NodeId, crate::core::ResolvedSignature>,
    resolved_node_types: BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    resolved_type_operands: BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    resolved_type_arguments: BTreeMap<NodeId, Vec<crate::core::ResolvedTypeId>>,
    resolved_field_types: BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    resolved_state_field_types: BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    resolved_variants: BTreeMap<NodeId, ResolvedVariantSchema>,
    resolved_type_targets: BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    resolved_session_actions: BTreeMap<NodeId, crate::core::ResolvedSessionAction>,
    resolved_bodies: BTreeMap<NodeId, crate::core::ResolvedBody>,
    declaration_expression_bodies:
        BTreeMap<NodeId, crate::core::ir::lower::ResolvedDeclarationExpressionBody>,
    /// Checker-issued authority records for declaration-owned FFI source
    /// roots. This includes unused declaration defaults; default-use
    /// occurrences are separate call-site facts.
    ffi_source_roots: Vec<crate::core::ir::lower::CheckedFfiSourceRoot>,
    ffi_declarations: BTreeMap<NodeId, crate::core::ir::ResolvedFfiDeclarationContract>,
    callable_cfgs: BTreeMap<NodeId, crate::core::cfg::CallableCfg>,
    resource_analyses: BTreeMap<NodeId, crate::core::ResourceAnalysis>,
    callables: BTreeMap<NodeId, crate::core::ResolvedCallable>,
    /// 0.36.48: method-level generic names per (trait, method) (see flow.rs).
    pub trait_method_generics: HashMap<(String, String), Vec<String>>,
}

/// Outcome of resolving a resolved function's checker-finalized signature.
#[derive(Debug, Clone)]
pub(crate) enum ZonkedResolution {
    /// Authoritative hit (node-id for nested callables, or exact
    /// qualified-name match). Always safe to apply.
    Exact((Vec<ZonkedTy>, ZonkedTy)),
    /// Non-authoritative suffix fallback hit (taken only when unambiguous
    /// and type-consistent; see `resolve_zonked_signature`).
    Suffix((Vec<ZonkedTy>, ZonkedTy)),
    /// No checker-finalized signature found; the function keeps its
    /// AST-derived snapshot.
    None,
}

/// core_resolved F1 (audit 2026-08-20, reworked 0.1.8): resolve the
/// checker-finalized (`zonked`) signature for a resolved function.
///
/// The checker and resolved catalogs agree on module-qualified keys for
/// every callable kind, so the exact qualified name is authoritative in
/// well-formed programs. The suffix fallback exists only as a compatibility
/// shim for the rare case where the exact key is genuinely absent; it is
/// taken when *exactly one* suffix candidate exists. Multiple distinct
/// suffix candidates are rejected because the old `find_map` over suffixes
/// would silently last-wins misbind a same-suffix, same-arity,
/// different-typed signature (a soundness hole). The caller additionally
/// enforces return-type consistency for suffix hits.
pub(crate) fn resolve_zonked_signature(
    qualified_name: &str,
    node_id: &crate::core::NodeId,
    zonked_func_types: &HashMap<String, (Vec<ZonkedTy>, ZonkedTy)>,
    zonked_nested_func_types: &HashMap<crate::core::NodeId, (Vec<ZonkedTy>, ZonkedTy)>,
) -> Result<ZonkedResolution, String> {
    if let Some(sig) = zonked_nested_func_types.get(node_id) {
        return Ok(ZonkedResolution::Exact(sig.clone()));
    }
    if let Some(sig) = zonked_func_types.get(qualified_name) {
        return Ok(ZonkedResolution::Exact(sig.clone()));
    }
    // Suffix fallback: collect every candidate suffix key actually present.
    let mut suffixes: Vec<String> = {
        let mut v = vec![qualified_name.to_string()];
        let mut cursor = qualified_name;
        while let Some((_, rest)) = cursor.split_once("::") {
            v.push(rest.to_string());
            cursor = rest;
        }
        v.into_iter()
            .skip(1)
            .filter(|k| zonked_func_types.contains_key(k))
            .collect()
    };
    suffixes.sort();
    suffixes.dedup();
    if suffixes.len() > 1 {
        return Err(format!(
            "ambiguous across {} suffix candidates (same suffix, different functions); refusing to guess",
            suffixes.len()
        ));
    }
    match suffixes
        .into_iter()
        .next()
        .and_then(|key| zonked_func_types.get(&key).cloned())
    {
        Some(sig) => Ok(ZonkedResolution::Suffix(sig)),
        // The suffix list is filtered by `contains_key` above, so absence
        // here is structurally unreachable; degrading to `None` keeps that
        // invariant check local instead of panicking on a broken catalog.
        None => Ok(ZonkedResolution::None),
    }
}

/// Resolve one checker-owned extern declaration for a call-site name from a
/// resolved extern block directory. Keep the map-level implementation here
/// so semantic body lowering and `CheckedProgram` consumers share identical
/// zero/one/many cardinality and deterministic identity diagnostics.
pub(crate) fn resolve_extern_func_signature_for_call<'a>(
    extern_blocks: &'a HashMap<NodeId, ResolvedExternBlock>,
    name: &str,
    argc: usize,
) -> Result<Option<&'a ResolvedExternFunc>, String> {
    let mut matches = extern_blocks
        .values()
        .flat_map(|block| {
            block
                .signatures
                .iter()
                .filter(|signature| signature.name == name)
                .map(move |signature| (block, signature))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(left_block, left), (right_block, right)| {
        left_block
            .node_id
            .cmp(&right_block.node_id)
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    match matches.as_slice() {
        [] => Ok(None),
        [(_, signature)] => Ok(Some(*signature)),
        _ => {
            let identities = matches
                .iter()
                .map(|(block, signature)| format!("{}:{}", block.node_id.0, signature.node_id.0))
                .collect::<Vec<_>>();
            Err(format!(
                "extern call '{}' ({} arguments) has ambiguous checker-owned declaration identity: {:?}",
                name, argc, identities
            ))
        }
    }
}

/// Resolve one checker-owned extern signature identity from a resolved block
/// directory. A malformed merged directory can duplicate a signature id in
/// one or several blocks; expose that cardinality instead of letting a
/// consumer's first-match lookup choose an ABI or symbol nondeterministically.
pub(crate) fn resolve_extern_func_for_signature<'a>(
    extern_blocks: &'a HashMap<NodeId, ResolvedExternBlock>,
    signature_id: &NodeId,
) -> Result<Option<&'a ResolvedExternFunc>, String> {
    let mut matches = extern_blocks
        .values()
        .flat_map(|block| {
            block
                .signatures
                .iter()
                .filter(|signature| signature.node_id == *signature_id)
                .map(move |signature| (block, signature))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(left_block, left), (right_block, right)| {
        left_block
            .node_id
            .cmp(&right_block.node_id)
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    match matches.as_slice() {
        [] => Ok(None),
        [(_, signature)] => Ok(Some(*signature)),
        _ => {
            let identities = matches
                .iter()
                .map(|(block, signature)| format!("{}:{}", block.node_id.0, signature.node_id.0))
                .collect::<Vec<_>>();
            Err(format!(
                "extern declaration '{}' has ambiguous checker-owned signature identity: {:?}",
                signature_id.0, identities
            ))
        }
    }
}

impl CheckedProgram {
    /// 0.36.48: number of method-level generic params declared by a trait
    /// method (0 for plain methods like `is_empty`, 1 for `map<U>`).
    pub fn trait_method_generic_count(&self, trait_name: &str, method_name: &str) -> usize {
        self.trait_method_generics
            .get(&(trait_name.to_string(), method_name.to_string()))
            .map(|g| g.len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn from_checked_file(file: &File) -> Result<Self, Vec<Diagnostic>> {
        Self::from_checked_file_base(file)
    }

    /// Construct `CheckedProgram` from checker-finalized typed artifacts.
    /// Canonical ownership is derived later from `ResolvedBody` and CFG.
    /// Uses checker-resolved function types for ResolvedFunction when available,
    /// falling back to AST clone for items the checker didn't process.
    pub(crate) fn from_flow_acc(file: &File, acc: FlowAcc) -> Result<Self, Vec<Diagnostic>> {
        let FlowAcc {
            schemes,
            zonked_func_types,
            zonked_nested_func_types,
            zonked_expr_types,
            ordinary_map_owned_any_v2_bool_cast_candidates,
            ordinary_map_owned_any_v2_nonprimitive_cast_candidates,
            ordinary_map_owned_any_v2_checked_unpack_candidates,
            ordinary_map_owned_any_v2_values_crosscall_string_candidates,
            session_actions,
            trait_method_generics,
            effect_summaries,
            ..
        } = acc;
        let mut program = Self::from_checked_file_base(file)?;
        let mut errors = Vec::new();
        let mut zonked_by_node = HashMap::new();

        // Install the checker-owned inferred summary before canonical
        // signatures and typed call sites are lowered.  This keeps the old
        // surface `FuncDef::effects` storage as a compatibility input while
        // making resolved/MIR/verifier consumers observe the same propagated
        // finite set.
        for function in program.functions.values_mut() {
            if let Some(summary) = effect_summaries
                .get(&function.node_id.0)
                .cloned()
                .or_else(|| effect_summary_for_name(&effect_summaries, &function.qualified_name))
            {
                function.effects = summary.inferred.to_vec();
                function.direct_effect_sources = summary.sources.clone();
            }
        }
        for site in program.call_sites.values_mut() {
            // Surface collection records Option/Result builtin methods as
            // `Unknown` because their receiver type is not available to the
            // syntax-only directory pass.  The checker has already resolved
            // these closed extraction/observation methods; restore their
            // finite (empty) effect summary before the MIR route gate sees
            // the call site.
            let method_name = site
                .callee
                .rsplit('.')
                .next()
                .unwrap_or(site.callee.as_str());
            if crate::core::effects::is_pure_builtin_method(method_name)
                && matches!(
                    site.kind,
                    ResolvedCallKind::Unknown | ResolvedCallKind::Method
                )
            {
                site.effects.clear();
                // Keep method identity for the semantic lowerer.  Marking
                // this as `Builtin` would make the surface spelling
                // (`snapshot.unwrap`) look like a free builtin name during
                // MIR materialization; `Method` lets typed lowering use its
                // resolved builtin-method identity.
                if matches!(site.kind, ResolvedCallKind::Unknown) {
                    site.kind = ResolvedCallKind::Method;
                }
            } else if let Some(summary) = effect_summary_for_name(&effect_summaries, &site.callee) {
                site.effects = summary.inferred.to_vec();
            } else if !crate::core::effects::builtin_effects(&site.callee).is_empty() {
                site.effects = crate::core::effects::builtin_effects(&site.callee).to_vec();
            } else if matches!(
                site.kind,
                ResolvedCallKind::Extern | ResolvedCallKind::Unknown
            ) && crate::core::effects::builtin_effects(&site.callee).is_empty()
            {
                // Extern and dynamic call sites are retained as an explicit
                // unknown effect.  The checker has already rejected truly
                // undefined names, but the resolved boundary must still keep
                // opaque FFI/dynamic calls from looking pure.
                site.effects = vec!["unknown".into()];
            }
        }

        // Override declaration snapshots only with mandatory-finalized checker
        // artifacts. Raw checker types are not a backend input.
        for func in program.functions.values_mut() {
            // core_resolved F1 (audit 2026-08-20, reworked 0.1.8): the
            // checker/resolved catalogs now agree on module-qualified keys for
            // every callable kind, so the exact qualified name is authoritative
            // in well-formed programs. The suffix fallback below is only a
            // compatibility shim for type constructors (newtype/enum ctors),
            // which the checker registers under a *bare* name. The old code
            // used `find_map` over progressively shorter suffixes with *only a
            // parameter-count guard*, so a same-suffix, same-arity,
            // different-typed signature could be silently bound (a soundness
            // hole — the wrong function's signature flows into the resolved IR
            // and downstream codegen/interp). The fallback is now defensive:
            //   * exact qualified-name hit  -> authoritative, applied silently;
            //   * node-id hit (nested)       -> authoritative, applied silently;
            //   * suffix hit only when exactly ONE candidate exists AND it is
            //     type-consistent with the declaration -> applied;
            //   * ambiguous (multiple suffix candidates) -> fail-closed
            //     (TOOL-RESOLUTION-001) instead of last-wins misbinding;
            //   * single suffix hit that disagrees on a concrete return type ->
            //     fail-closed (TOOL-RESOLUTION-001) instead of misbinding.
            let resolution = match resolve_zonked_signature(
                &func.qualified_name,
                &func.node_id,
                &zonked_func_types,
                &zonked_nested_func_types,
            ) {
                Ok(res) => res,
                Err(msg) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: zonked signature for '{}': {}",
                            func.qualified_name, msg
                        ),
                        func.origin.user_span(),
                    ));
                    continue;
                }
            };
            let (resolved_params, resolved_ret, is_exact) = match resolution {
                ZonkedResolution::None => continue,
                ZonkedResolution::Exact(sig) => (sig.0, sig.1, true),
                ZonkedResolution::Suffix(sig) => (sig.0, sig.1, false),
            };
            if resolved_params.len() != func.params.len() {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: zonked signature for '{}' has {} parameters, declaration has {}",
                        func.qualified_name,
                        resolved_params.len(),
                        func.params.len()
                    ),
                    func.origin.user_span(),
                ));
                continue;
            }
            // Defensive: a non-exact (suffix) hit must agree with the declared
            // return type when both are concrete; otherwise it is a misbind to
            // an unrelated same-suffix function (core_resolved F1).
            if !is_exact
                && !contains_unresolved_type(&func.ret)
                && !contains_unresolved_type(resolved_ret.as_type())
                && crate::core::fmt_type(&func.ret) != crate::core::fmt_type(resolved_ret.as_type())
            {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: zonked signature for '{}' (suffix fallback) has return type '{}' but the declaration declares '{}'; refusing to misbind",
                        func.qualified_name,
                        crate::core::fmt_type(resolved_ret.as_type()),
                        crate::core::fmt_type(&func.ret)
                    ),
                    func.origin.user_span(),
                ));
                continue;
            }
            func.params = func
                .params
                .iter()
                .zip(resolved_params.iter())
                .map(|((name, _), resolved)| (name.clone(), resolved.as_type().clone()))
                .collect();
            func.ret = resolved_ret.as_type().clone();
            zonked_by_node.insert(
                func.node_id.clone(),
                (resolved_params.clone(), resolved_ret.clone()),
            );
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let stable_expression_types = stabilize_expression_types(&program, &zonked_expr_types)?;
        let stable_bool_cast_candidates = stabilize_candidate_expression_nodes(
            &program,
            &ordinary_map_owned_any_v2_bool_cast_candidates,
        )?;
        let stable_nonprimitive_cast_candidates = stabilize_candidate_expression_nodes(
            &program,
            &ordinary_map_owned_any_v2_nonprimitive_cast_candidates,
        )?;
        let stable_checked_unpack_candidates = stabilize_candidate_expression_nodes(
            &program,
            &ordinary_map_owned_any_v2_checked_unpack_candidates,
        )?;
        let stable_values_crosscall_string_candidates = stabilize_candidate_expression_nodes(
            &program,
            &ordinary_map_owned_any_v2_values_crosscall_string_candidates,
        )?;
        program.resolved_session_actions = stabilize_session_actions(&program, &session_actions)?;
        program.type_schemes = schemes;
        program.zonked_function_types = zonked_by_node;
        let (
            resolved_types,
            resolved_signatures,
            resolved_node_types,
            resolved_type_operands,
            resolved_field_types,
            resolved_variants,
            resolved_type_targets,
            resolved_type_arguments,
            resolved_state_field_types,
            resolved_trait_signatures,
        ) = build_canonical_function_signatures(&program, &stable_expression_types, file)?;
        for meta in program.node_meta.values_mut() {
            meta.expression_key = None;
            meta.shared_binding = None;
            meta.ref_binding = None;
            meta.type_operand = None;
            meta.type_arguments.clear();
        }
        program.resolved_types = resolved_types;
        program.resolved_signatures = resolved_signatures;
        program.resolved_trait_signatures = resolved_trait_signatures;
        program.resolved_node_types = resolved_node_types;
        program.resolved_type_operands = resolved_type_operands;
        program.resolved_field_types = resolved_field_types;
        program.resolved_state_field_types = resolved_state_field_types;
        program.resolved_variants = resolved_variants;
        program.resolved_type_targets = resolved_type_targets;
        program.resolved_type_arguments = resolved_type_arguments;
        validate_resolved_variant_schemas(&program)?;
        program.resolved_bodies =
            match crate::core::ir::lower::lower_checked_callable_bodies_with_checked_unpack_candidates(
                file,
                &program,
                &stable_checked_unpack_candidates,
            ) {
                Ok(bodies) => bodies,
                Err(body_errors) => {
                    return Err(body_errors
                        .into_iter()
                        .map(|error| {
                            let span = program
                                .node_meta
                                .get(&error.node_id)
                                .map(|meta| meta.origin.user_span())
                                .or_else(|| {
                                    // CO-H2 (dx-backlog #7): generated
                                    // lowering nodes (e.g.
                                    // `function:f/generated:body.block:...`)
                                    // are not registered in the metadata
                                    // catalog. Fall back to the owning
                                    // callable's anchor (`function:f`) so
                                    // the diagnostic still names a source
                                    // location instead of Span::UNKNOWN.
                                    error
                                        .node_id
                                        .0
                                        .split('/')
                                        .next()
                                        .filter(|part| part.starts_with("function:"))
                                        .and_then(|owner| {
                                            program
                                                .node_meta
                                                .get(&NodeId(owner.to_string()))
                                                .map(|meta| meta.origin.user_span())
                                        })
                                })
                                .unwrap_or(Span::UNKNOWN);
                            // wave1-review §5.16 (closed 2026-08-07): E0830
                            // used to live only inside the message text with
                            // Diagnostic.code = None, so tooling routing on
                            // codes never saw it. Bridge embedded codes into
                            // the structured field.
                            // CO-H2 / §5.2-2 diagnostic self-healing: errors that
                            // embed their own `[E083x]` marker already carry a
                            // structured code, so do NOT prefix them with the
                            // internal `TOOL-RESOLUTION-001` jargon — leaking that
                            // token to the user violates the no-internal-identifier
                            // diagnostic contract (error_co_h2).
                            let message = if error.message.contains("[E083") {
                                error.to_string()
                            } else {
                                format!("TOOL-RESOLUTION-001: {error}")
                            };
                            let diagnostic = Diagnostic::error(message, span);
                            if error.message.contains("[E0830]") {
                                diagnostic.with_code(crate::diagnostic::codes::E0830)
                            } else {
                                diagnostic
                            }
                        })
                        .collect());
                }
            };
        for body in program.resolved_bodies.values() {
            for (call_node, callee) in &body.resolved_calls {
                // The syntax-only method collector may have selected a bare
                // method signature before the receiver's canonical Actor
                // identity was available. Once lowering resolves an
                // ActorMethod, replace that snapshot with the checker-final
                // effect summary for its exact callable owner. Do not resolve
                // effects by short method name: separate Actors may reuse it.
                let actor_method_effects = match callee {
                    crate::core::ir::ResolvedCallee::ActorMethod { actor, method } => {
                        let actor_def = program.actors.get(actor);
                        let method_owner = NodeId(method.as_str().to_owned());
                        let method_def = program.functions.get(&method_owner);
                        match (actor_def, method_def) {
                            (Some(actor_def), Some(method_def))
                                if method_def
                                    .qualified_name
                                    .strip_prefix(&format!("{}::", actor_def.qualified_name))
                                    .is_some() =>
                            {
                                // Actor mailbox dispatch has a dedicated MIR
                                // operation receipt. Its source effect set is
                                // therefore the exact checked method summary;
                                // do not inject a generic `unknown` that would
                                // diverge from the method receipt and disable
                                // finite-effect Actor routes.
                                Some(method_def.effects.clone())
                            }
                            _ => {
                                errors.push(Diagnostic::error(
                                    format!(
                                        "TOOL-RESOLUTION-001: typed Actor method '{}' has no matching canonical effect summary for '{}'",
                                        method.as_str(), actor.0
                                    ),
                                    Span::UNKNOWN,
                                ));
                                continue;
                            }
                        }
                    }
                    _ => None,
                };
                let Some(site) = program.call_sites.get_mut(call_node) else {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: typed call '{}' has no call-site record",
                            call_node.0
                        ),
                        Span::UNKNOWN,
                    ));
                    continue;
                };
                if site.owner != body.owner.0 {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: typed call '{}' belongs to '{}' but body owner is '{}'",
                            call_node.0, site.owner, body.owner.0
                        ),
                        site.origin.user_span(),
                    ));
                    continue;
                }
                if site.resolved_callee.replace(callee.clone()).is_some() {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: typed call '{}' has more than one canonical target",
                            call_node.0
                        ),
                        site.origin.user_span(),
                    ));
                }
                if let crate::core::ir::ResolvedCallee::Function(target) = &callee {
                    if !program.functions.contains_key(target) {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: typed function call '{}' has no exact callable directory entry",
                                target.0
                            ),
                            site.origin.user_span(),
                        ));
                        continue;
                    }
                    let Some(summary) = effect_summaries.get(&target.0) else {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: typed function call '{}' has no exact checker effect summary",
                                target.0
                            ),
                            site.origin.user_span(),
                        ));
                        continue;
                    };
                    if site
                        .target_function
                        .as_ref()
                        .is_some_and(|existing| existing != target)
                    {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: typed function call '{}' disagrees with syntax target '{}'",
                                target.0,
                                site.target_function
                                    .as_ref()
                                    .expect("target was just checked")
                                    .0
                            ),
                            site.origin.user_span(),
                        ));
                        continue;
                    }
                    // The syntax-only collector cannot see lexical nested
                    // declarations, so a source call such as `scaled(x)` may
                    // start as Unknown. Once the typed body supplies the exact
                    // checker Function owner and summary, canonicalize the
                    // call-site kind from that identity. Keep the route gate
                    // fail-closed for unresolved calls and require this exact
                    // summary rather than guessing from the short spelling.
                    site.kind = ResolvedCallKind::Function;
                    site.target_function = Some(target.clone());
                    site.effects = summary.inferred.to_vec();
                }
                if let Some(effects) = actor_method_effects {
                    site.effects = effects;
                }
            }
        }
        // A checked-unpack source call is lowered to a typed Cast in the
        // ResolvedBody, so it is intentionally absent from `resolved_calls`
        // (that index is validated against actual Call nodes). Preserve its
        // Checker-resolved builtin identity only in the source call-site
        // catalog, keyed by the same stable Call NodeId candidate.
        for (owner, calls) in &stable_checked_unpack_candidates {
            for call_node in calls {
                let Some(site) = program.call_sites.get_mut(call_node) else {
                    errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0200,
                        "checked Map/Any unpack has no exact source call-site identity",
                        program
                            .node_meta
                            .get(call_node)
                            .map(|meta| meta.origin.user_span())
                            .unwrap_or(Span::UNKNOWN),
                    ));
                    continue;
                };
                if site.owner != owner.0
                    || site.kind != ResolvedCallKind::Builtin
                    || site.argc != 1
                    || !site.arity_matches()
                    || !matches!(site.callee.as_str(), "unpack_i32" | "unpack_string")
                {
                    errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0200,
                        "checked Map/Any unpack source call differs from its Checker-owned builtin identity",
                        site.origin.user_span(),
                    ));
                    continue;
                }
                let builtin = match crate::core::ir::BuiltinId::new(site.callee.clone()) {
                    Ok(builtin) => builtin,
                    Err(error) => {
                        errors.push(Diagnostic::error(
                            format!("TOOL-RESOLUTION-001: {error}"),
                            site.origin.user_span(),
                        ));
                        continue;
                    }
                };
                if site.resolved_callee.as_ref().is_some_and(|existing| {
                    existing != &crate::core::ir::ResolvedCallee::Builtin(builtin.clone())
                }) {
                    errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0200,
                        "checked Map/Any unpack call identity conflicts with its typed source resolution",
                        site.origin.user_span(),
                    ));
                    continue;
                }
                site.resolved_callee = Some(crate::core::ir::ResolvedCallee::Builtin(builtin));
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let (declaration_expression_bodies, ffi_source_root_seeds) =
            crate::core::ir::lower::lower_checked_declaration_expression_bodies(file, &program);
        program.declaration_expression_bodies = declaration_expression_bodies;
        program.ffi_declarations = crate::core::ir::build_checked_ffi_declarations(&program)?;
        program.ffi_source_roots =
            crate::core::ir::lower::build_checked_ffi_source_roots(&program, ffi_source_root_seeds);
        errors.extend(bind_checked_ffi_source_call_sites(&mut program));
        validate_resolved_callable_bodies(&program)?;
        program.callable_cfgs = crate::core::cfg::lower_resolved_bodies(&program.resolved_bodies)?;
        // RECORD-LIN-001 (0.1.9, Phase B): user records/unions with linear
        // fields must be tracked as linear. Compute the set of such qualified
        // names once and hand it to the CFG's is_linear.
        let linear_record_names = compute_linear_record_names(&program);
        let checked_record_field_types = program
            .type_defs
            .values()
            .filter(|definition| definition.kind == ResolvedTypeKind::Record)
            .filter_map(|definition| {
                let fields = definition
                    .fields
                    .iter()
                    .map(|(name, _)| {
                        let field = definition.field_ids.get(name)?;
                        let ty = program.resolved_field_types.get(field)?;
                        Some((name.clone(), ty.clone()))
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some((definition.node_id.clone(), fields))
            })
            .collect::<BTreeMap<_, _>>();
        let checked_record_field_ids = program
            .type_defs
            .values()
            .filter(|definition| definition.kind == ResolvedTypeKind::Record)
            .map(|definition| (definition.node_id.clone(), definition.field_ids.clone()))
            .collect::<BTreeMap<_, _>>();
        program.resource_analyses =
            crate::core::cfg::analyze_resolved_bodies_with_declaration_expressions(
                &program.callable_cfgs,
                &program.resolved_bodies,
                &program.resolved_signatures,
                &program.resolved_types,
                &program.constants,
                &checked_record_field_types,
                &checked_record_field_ids,
                &linear_record_names,
                &program.declaration_expression_bodies,
            )?;
        attach_ordinary_map_owned_any_v2_get_or_default_i32_receipt(&mut program);
        attach_ordinary_map_owned_any_v2_get_or_default_string_receipt(&mut program);
        validate_ordinary_map_owned_any_v2_get_or_default_i32_receipt(&program)?;
        validate_ordinary_map_owned_any_v2_get_or_default_string_receipt(&program)?;
        validate_ordinary_map_owned_any_v2_values_local_cast_receipts(&program)?;
        validate_ordinary_map_owned_any_v2_bool_cast_receipts(
            &program,
            &stable_bool_cast_candidates,
        )?;
        validate_ordinary_map_owned_any_v2_nonprimitive_cast_receipts(
            &program,
            &stable_nonprimitive_cast_candidates,
        )?;
        validate_ordinary_map_owned_any_v2_nonprimitive_unpack_boundary(
            &program,
            &stable_nonprimitive_cast_candidates,
        )?;
        validate_ordinary_map_owned_any_v2_checked_unpack_receipts(
            &program,
            &stable_checked_unpack_candidates,
        )?;
        validate_ordinary_map_owned_any_v2_values_crosscall_string_receipts(
            &program,
            &stable_values_crosscall_string_candidates,
        )?;
        let mut callables = BTreeMap::new();
        for (owner, body) in &program.resolved_bodies {
            let Some(signature) = program.resolved_signatures.get(owner).cloned() else {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: callable '{}' has no canonical signature",
                        owner.0
                    ),
                    body.root.origin.user_span(),
                ));
                continue;
            };
            let Some(cfg) = program.callable_cfgs.get(owner).cloned() else {
                errors.push(Diagnostic::error(
                    format!("TOOL-RESOLUTION-001: callable '{}' has no CFG", owner.0),
                    body.root.origin.user_span(),
                ));
                continue;
            };
            let Some(resources) = program.resource_analyses.get(owner).cloned() else {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: callable '{}' has no resource analysis",
                        owner.0
                    ),
                    body.root.origin.user_span(),
                ));
                continue;
            };
            match crate::core::ResolvedCallable::assemble(signature, body.clone(), cfg, resources) {
                Ok(callable) => {
                    callables.insert(owner.clone(), callable);
                }
                Err(error) => errors.push(Diagnostic::error(
                    format!("TOOL-RESOLUTION-001: {error}"),
                    body.root.origin.user_span(),
                )),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        program.callables = callables;
        program.actor_descriptors =
            crate::core::ir::build_checked_actor_descriptors(file, &program)?;
        for descriptor in program.actor_descriptors.values() {
            if let Err(error) = descriptor.validate_against(&program) {
                errors.push(Diagnostic::error(
                    format!("TOOL-RESOLUTION-001: {error}"),
                    program
                        .actors
                        .get(&descriptor.actor)
                        .map(|actor| actor.origin.user_span())
                        .unwrap_or(Span::UNKNOWN),
                ));
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        // The complete declaration catalog includes checker-owned Actor
        // descriptors, so it is assembled only after their signatures and
        // ordered state schemas have been finalized.
        program.native_declarations =
            match crate::core::ir::build_checked_native_declarations(file, &program) {
                Ok(catalog) => catalog,
                Err(declaration_errors) => return Err(declaration_errors),
            };

        let data_exports =
            crate::core::native_data_export::build_checked_native_data_exports(file, &program)?;
        for export in data_exports {
            let Some(constant) = program.constants.get_mut(&export.owner) else {
                return Err(vec![Diagnostic::error_code(
                    crate::core::native_data_export::NATIVE_DATA_EXPORT_ERROR_CODE,
                    format!("checked constant descriptor for exported symbol '{}' disappeared during construction", export.symbol),
                    export.span,
                )]);
            };
            constant.native_data_export = Some(export);
        }
        crate::core::native_data_export::validate_checked_native_data_exports(&program)?;

        program.trait_method_generics = trait_method_generics;

        // ── Zonk Leak Detector (追加 F, architecture-amendment-1.0.md) ──
        // Final safety net: scan all function signatures for residual inference
        // artifacts (TypeVar, Infer, ForAll, "_", "unknown"). If any leak through
        // the per-signature zonk gate, abort compilation instead of passing dirty
        // types to backends (which would cause LLVM ICE / interpreter panic).
        {
            use crate::core::unification::scan_residual;
            let mut leaks = Vec::new();
            for (node_id, func) in &program.functions {
                for (param_name, param_ty) in &func.params {
                    if let Err(e) = scan_residual(param_ty) {
                        leaks.push(Diagnostic::error(
                            format!(
                                "ZONK-LEAK-001: unresolved type in parameter '{}' of '{}': {}",
                                param_name, func.qualified_name, e
                            ),
                            func.origin.user_span(),
                        ));
                    }
                }
                if let Err(e) = scan_residual(&func.ret) {
                    leaks.push(Diagnostic::error(
                        format!(
                            "ZONK-LEAK-001: unresolved return type in '{}': {}",
                            func.qualified_name, e
                        ),
                        func.origin.user_span(),
                    ));
                }
                let _ = node_id; // used for iteration context
            }
            if !leaks.is_empty() {
                return Err(leaks);
            }
        }

        Ok(program)
    }

    fn from_checked_file_base(file: &File) -> Result<Self, Vec<Diagnostic>> {
        let mut transitions = HashMap::new();
        let mut flows = HashMap::new();
        let mut items = HashMap::new();
        let mut node_meta = HashMap::new();
        let mut call_sites = HashMap::new();
        let mut functions = HashMap::new();
        let mut sessions = HashMap::new();
        let mut actors = HashMap::new();
        let mut capabilities = HashMap::new();
        let mut constants = HashMap::new();
        let mut traits = HashMap::new();
        let mut impls = HashMap::new();
        let mut type_defs = HashMap::new();
        let mut extern_blocks = HashMap::new();
        let mut backend_requirements = Vec::new();
        let mut errors = Vec::new();
        let ids = NodeIdBuilder::new(&file.sources);
        let compilation_root = NodeId(COMPILATION_ROOT_NODE_ID.to_string());
        for import in &file.imports {
            let key = format!(
                "{}:as:{}",
                import.path.join("::"),
                import.alias.as_deref().unwrap_or("_")
            );
            insert_child_meta(
                import.meta,
                &compilation_root,
                "decl.import",
                &format!("import.{}", stable_id_fragment(&key)),
                import.meta.span,
                &ids,
                &mut node_meta,
                &mut errors,
            );
        }
        collect_items(
            &file.items,
            &file.sources,
            &mut items,
            &mut node_meta,
            &mut functions,
            &mut sessions,
            &mut actors,
            &mut capabilities,
            &mut constants,
            &mut traits,
            &mut impls,
            &mut type_defs,
            &mut extern_blocks,
            &mut flows,
            &mut transitions,
            &mut backend_requirements,
            &mut errors,
        );
        if !errors.is_empty() {
            return Err(errors);
        }
        collect_program_call_sites(
            file,
            &functions,
            &extern_blocks,
            &actors,
            &impls,
            &transitions,
            &mut call_sites,
            &mut errors,
        );
        install_qualified_import_targets(file, &functions, &mut call_sites, &mut errors);
        let mut origin_catalog = OriginCatalog::default();
        origin_catalog.register(
            &NodeId(COMPILATION_ROOT_NODE_ID.to_string()),
            &Origin::User(Span::UNKNOWN),
            &mut errors,
        );
        for item in items.values() {
            origin_catalog.register(&item.node_id, &item.origin, &mut errors);
        }
        for meta in node_meta.values() {
            origin_catalog.register(&meta.node_id, &meta.origin, &mut errors);
        }
        let mut call_entries = call_sites.values().collect::<Vec<_>>();
        call_entries.sort_by(|left, right| {
            left.node_id
                .cmp(&right.node_id)
                .then_with(|| left.owner.cmp(&right.owner))
                .then_with(|| left.callee.cmp(&right.callee))
                .then_with(|| left.argc.cmp(&right.argc))
        });
        for call in call_entries {
            origin_catalog.register(&call.node_id, &call.origin, &mut errors);
        }
        for flow in flows.values() {
            origin_catalog.register(&flow.node_id, &flow.origin, &mut errors);
            for state in flow.states.values() {
                origin_catalog.register(&state.node_id, &state.origin, &mut errors);
            }
        }
        for transition in transitions.values() {
            origin_catalog.register(&transition.node_id, &transition.origin, &mut errors);
        }
        for function in functions.values() {
            origin_catalog.register(&function.node_id, &function.origin, &mut errors);
        }
        for session in sessions.values() {
            origin_catalog.register(&session.node_id, &session.origin, &mut errors);
        }
        for actor in actors.values() {
            origin_catalog.register(&actor.node_id, &actor.origin, &mut errors);
        }
        for capability in capabilities.values() {
            origin_catalog.register(&capability.node_id, &capability.origin, &mut errors);
        }
        for constant in constants.values() {
            origin_catalog.register(&constant.node_id, &constant.origin, &mut errors);
        }
        for trait_def in traits.values() {
            origin_catalog.register(&trait_def.node_id, &trait_def.origin, &mut errors);
        }
        for impl_def in impls.values() {
            origin_catalog.register(&impl_def.node_id, &impl_def.origin, &mut errors);
        }
        for type_def in type_defs.values() {
            origin_catalog.register(&type_def.node_id, &type_def.origin, &mut errors);
        }
        for extern_block in extern_blocks.values() {
            origin_catalog.register(&extern_block.node_id, &extern_block.origin, &mut errors);
        }
        origin_catalog.validate(&mut errors);
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Self {
            has_imports: !file.imports.is_empty(),
            source_registry: file.sources.clone(),
            items,
            node_meta,
            call_sites,
            flows,
            transitions,
            functions,
            sessions,
            actors,
            actor_descriptors: BTreeMap::new(),
            capabilities,
            constants,
            traits,
            impls,
            type_defs,
            native_declarations: crate::core::ir::ResolvedNativeDeclarations::default(),
            extern_blocks,
            backend_requirements,
            type_schemes: HashMap::new(),
            zonked_function_types: HashMap::new(),
            resolved_types: crate::core::ResolvedTypeTable::new(),
            resolved_signatures: BTreeMap::new(),
            resolved_trait_signatures: BTreeMap::new(),
            resolved_node_types: BTreeMap::new(),
            resolved_type_operands: BTreeMap::new(),
            resolved_type_arguments: BTreeMap::new(),
            resolved_field_types: BTreeMap::new(),
            resolved_state_field_types: BTreeMap::new(),
            resolved_variants: BTreeMap::new(),
            resolved_type_targets: BTreeMap::new(),
            resolved_session_actions: BTreeMap::new(),
            resolved_bodies: BTreeMap::new(),
            declaration_expression_bodies: BTreeMap::new(),
            ffi_source_roots: Vec::new(),
            ffi_declarations: BTreeMap::new(),
            callable_cfgs: BTreeMap::new(),
            resource_analyses: BTreeMap::new(),
            callables: BTreeMap::new(),
            trait_method_generics: HashMap::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn reset_test_legacy_codegen_function_calls() {
        TEST_LEGACY_CODEGEN_FUNCTION_CALLS.with(|calls| calls.borrow_mut().clear());
    }

    #[cfg(test)]
    pub(crate) fn test_legacy_codegen_function_calls() -> Vec<String> {
        TEST_LEGACY_CODEGEN_FUNCTION_CALLS.with(|calls| calls.borrow().clone())
    }

    /// Whether this checked program contains imported modules.  Migration
    /// gates use this fact to distinguish the switched import-free production
    /// island from unresolved module/stdlib compatibility units.
    pub(crate) fn has_imports(&self) -> bool {
        self.has_imports
    }

    /// Checker-owned source provenance for consumers that need to classify a
    /// callable's origin or module path. This accessor must not be replaced
    /// with a raw-AST lookup.
    pub(crate) fn source_registry(&self) -> &SourceRegistry {
        &self.source_registry
    }

    pub fn transitions(&self) -> &HashMap<TransitionId, ResolvedTransition> {
        &self.transitions
    }

    /// Build pre-computed transition lookup tables for backend consumption.
    /// Eliminates ~90 lines of duplicated construction in interp and codegen (AD-6).
    pub fn build_transition_tables(&self) -> TransitionTables {
        let mut resolved = HashMap::new();
        let mut fallbacks = std::collections::HashSet::new();
        let mut pinned = std::collections::HashSet::new();
        let mut param_arity = HashMap::new();
        let mut param_lists = HashMap::new();
        for (id, transition) in &self.transitions {
            let key = (id.flow.0.clone(), id.event.clone(), id.source.name.clone());
            let targets = transition
                .targets
                .iter()
                .map(|state| state.name.clone())
                .collect();
            if transition.is_fallback {
                fallbacks.insert(key.clone());
            }
            if transition.is_ffi_pinned {
                pinned.insert(key.clone());
            }
            param_arity.insert(key.clone(), transition.params.len());
            param_lists.insert(
                key.clone(),
                transition
                    .params
                    .iter()
                    .map(|(name, ty)| (name.clone(), crate::core::fmt_type(ty)))
                    .collect(),
            );
            resolved.insert(key, targets);
        }

        let mut by_flow: HashMap<String, Vec<(String, String, String, bool, bool, usize)>> =
            HashMap::new();
        for transition in self.transitions.values() {
            let flow = transition.id.flow.0.clone();
            let event = transition.id.event.clone();
            let source = transition.id.source.name.clone();
            let targets = transition
                .targets
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join("|");
            by_flow.entry(flow).or_default().push((
                event,
                source,
                targets,
                transition.is_fallback,
                transition.is_ffi_pinned,
                transition.params.len(),
            ));
        }
        for list in by_flow.values_mut() {
            list.sort();
        }

        let mut by_event: HashMap<String, Vec<(String, String, String, bool, bool, usize)>> =
            HashMap::new();
        for transition in self.transitions.values() {
            let flow = transition.id.flow.0.clone();
            let event = transition.id.event.clone();
            let source = transition.id.source.name.clone();
            let targets = transition
                .targets
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join("|");
            by_event.entry(event).or_default().push((
                flow,
                source,
                targets,
                transition.is_fallback,
                transition.is_ffi_pinned,
                transition.params.len(),
            ));
        }
        for list in by_event.values_mut() {
            list.sort();
        }

        TransitionTables {
            resolved,
            fallbacks,
            pinned,
            param_arity,
            param_lists,
            by_flow,
            by_event,
        }
    }

    pub fn flows(&self) -> &HashMap<FlowId, ResolvedFlow> {
        &self.flows
    }

    pub fn items(&self) -> &HashMap<NodeId, ResolvedItem> {
        &self.items
    }

    pub fn functions(&self) -> &HashMap<NodeId, ResolvedFunction> {
        &self.functions
    }

    pub fn function(&self, qualified_name: &str) -> Option<&ResolvedFunction> {
        self.functions
            .values()
            .find(|function| function.qualified_name == qualified_name)
    }

    pub fn sessions(&self) -> &HashMap<NodeId, ResolvedSession> {
        &self.sessions
    }

    pub fn session(&self, qualified_name: &str) -> Option<&ResolvedSession> {
        self.sessions
            .values()
            .find(|session| session.qualified_name == qualified_name)
    }

    pub fn session_body_display(&self, qualified_name: &str) -> Option<&str> {
        self.session(qualified_name)
            .map(|session| session.body_display.as_str())
    }

    pub fn actors(&self) -> &HashMap<NodeId, ResolvedActor> {
        &self.actors
    }

    /// Canonical actor declarations with typed initializers and exact method owners.
    /// Executable consumers do not need the compatibility ActorDef snapshots.
    pub fn actor_descriptors(&self) -> &BTreeMap<NodeId, crate::core::ir::ResolvedActorDescriptor> {
        &self.actor_descriptors
    }

    pub fn actor_descriptor(
        &self,
        actor: &NodeId,
    ) -> Option<&crate::core::ir::ResolvedActorDescriptor> {
        self.actor_descriptors.get(actor)
    }

    pub fn actor(&self, qualified_name: &str) -> Option<&ResolvedActor> {
        self.actors
            .values()
            .find(|actor| actor.qualified_name == qualified_name)
    }

    pub fn actor_method_signature(
        &self,
        actor_name: &str,
        method_name: &str,
    ) -> Option<&ResolvedActorMethod> {
        self.actor(actor_name).and_then(|actor| {
            actor
                .method_signatures
                .iter()
                .find(|method| method.name == method_name)
        })
    }

    pub fn impl_method_signature(
        &self,
        trait_name: &str,
        type_name: &str,
        method_name: &str,
    ) -> Option<&ResolvedMethodSig> {
        self.impls.values().find_map(|impl_def| {
            if impl_def.trait_name == trait_name && impl_def.type_name == type_name {
                impl_def
                    .method_signatures
                    .iter()
                    .find(|method| method.name == method_name)
            } else {
                None
            }
        })
    }

    pub fn capabilities(&self) -> &HashMap<NodeId, ResolvedCapability> {
        &self.capabilities
    }

    pub fn capability(&self, qualified_name: &str) -> Option<&ResolvedCapability> {
        self.capabilities
            .values()
            .find(|capability| capability.qualified_name == qualified_name)
    }

    pub fn constants(&self) -> &HashMap<NodeId, ResolvedConstant> {
        &self.constants
    }

    pub fn constant(&self, qualified_name: &str) -> Option<&ResolvedConstant> {
        self.constants
            .values()
            .find(|constant| constant.qualified_name == qualified_name)
    }

    pub fn traits(&self) -> &HashMap<NodeId, ResolvedTrait> {
        &self.traits
    }

    pub fn trait_def(&self, qualified_name: &str) -> Option<&ResolvedTrait> {
        self.traits
            .values()
            .find(|trait_def| trait_def.qualified_name == qualified_name)
    }

    pub fn trait_method_signature(
        &self,
        trait_name: &str,
        method_name: &str,
    ) -> Option<&ResolvedMethodSig> {
        self.trait_def(trait_name).and_then(|trait_def| {
            trait_def
                .method_signatures
                .iter()
                .find(|method| method.name == method_name)
        })
    }

    pub fn impls(&self) -> &HashMap<NodeId, ResolvedImpl> {
        &self.impls
    }

    pub fn type_defs(&self) -> &HashMap<NodeId, ResolvedTypeDef> {
        &self.type_defs
    }

    /// Checker-owned native declaration metadata. This catalog carries
    /// declaration order, stable member/binder identities, canonical type
    /// edges, and explicit constructor/drop ABI recipes without storing a
    /// surface `TypeDef` or `FlowDef`.
    pub fn native_declarations(&self) -> &crate::core::ir::ResolvedNativeDeclarations {
        &self.native_declarations
    }

    pub fn type_def(&self, qualified_name: &str) -> Option<&ResolvedTypeDef> {
        self.type_defs
            .values()
            .find(|type_def| type_def.qualified_name == qualified_name)
    }

    pub fn type_def_fields(&self, qualified_name: &str) -> Option<&[(String, String)]> {
        self.type_def(qualified_name)
            .map(|type_def| type_def.fields.as_slice())
    }

    pub fn type_def_variants(&self, qualified_name: &str) -> Option<&[(String, Option<String>)]> {
        self.type_def(qualified_name)
            .map(|type_def| type_def.variants.as_slice())
    }

    pub fn type_def_alias_of(&self, qualified_name: &str) -> Option<&str> {
        self.type_def(qualified_name)
            .and_then(|type_def| type_def.alias_of.as_deref())
    }

    pub fn extern_blocks(&self) -> &HashMap<NodeId, ResolvedExternBlock> {
        &self.extern_blocks
    }

    /// Return checker-owned extern blocks in canonical identity order.
    ///
    /// The backing directory is a `HashMap` because node identity is the
    /// lookup key, but compatibility consumers also materialize name-keyed
    /// views from it.  Those views must never inherit process-dependent hash
    /// iteration order: a malformed merged directory with duplicate names
    /// should at least choose the same deterministic entry everywhere while
    /// the identity-aware consumers report the structural ambiguity.
    pub fn extern_blocks_sorted(&self) -> Vec<&ResolvedExternBlock> {
        let mut blocks = self.extern_blocks.iter().collect::<Vec<_>>();
        blocks.sort_by(|(left_key, left), (right_key, right)| {
            left.node_id
                .cmp(&right.node_id)
                .then_with(|| left.qualified_name.cmp(&right.qualified_name))
                .then_with(|| left_key.cmp(right_key))
        });
        blocks.into_iter().map(|(_, block)| block).collect()
    }

    /// Return signatures inside one checker-owned extern block in canonical
    /// identity order.  Normal checked output preserves declaration order;
    /// sorting here also keeps compatibility projections deterministic for a
    /// hand-built or merged `CheckedProgram` whose vector was rearranged.
    pub fn extern_signatures_sorted(block: &ResolvedExternBlock) -> Vec<&ResolvedExternFunc> {
        let mut signatures = block.signatures.iter().collect::<Vec<_>>();
        signatures.sort_by(|left, right| {
            left.node_id
                .cmp(&right.node_id)
                .then_with(|| left.name.cmp(&right.name))
        });
        signatures
    }

    /// Resolve the checker-owned ABI block for one extern signature identity.
    ///
    /// A well-formed checked program has exactly one block containing a given
    /// signature node.  Keep that cardinality explicit at the semantic
    /// boundary: a malformed merged program must not let `HashMap` iteration
    /// order choose one of several blocks and thereby alter route admission,
    /// receipt materialization, or diagnostics.
    pub fn extern_block_for_signature(
        &self,
        signature_id: &NodeId,
    ) -> Result<Option<&ResolvedExternBlock>, String> {
        let mut matches = self
            .extern_blocks
            .values()
            .filter(|block| {
                block
                    .signatures
                    .iter()
                    .any(|signature| signature.node_id == *signature_id)
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        match matches.as_slice() {
            [] => Ok(None),
            [block] => Ok(Some(*block)),
            _ => {
                let block_ids = matches
                    .iter()
                    .map(|block| block.node_id.0.as_str())
                    .collect::<Vec<_>>();
                Err(format!(
                    "extern declaration '{}' has ambiguous checker-owned ABI blocks: {:?}",
                    signature_id.0, block_ids
                ))
            }
        }
    }

    pub fn backend_requirements(&self) -> &[CapabilityRequirement] {
        &self.backend_requirements
    }

    pub fn requires_capability(&self, capability: &str) -> bool {
        self.backend_requirements
            .iter()
            .any(|requirement| requirement.capability == capability)
    }

    pub fn node_meta(&self) -> &HashMap<NodeId, NodeMeta> {
        &self.node_meta
    }

    pub fn call_sites(&self) -> &HashMap<NodeId, ResolvedCallSite> {
        &self.call_sites
    }

    /// Return checker-owned call sites in canonical identity order.
    ///
    /// Call-site consumers often fold over the directory to select a route or
    /// report the first structural boundary.  Exposing a sorted view keeps
    /// those decisions independent of `HashMap` iteration order; the map key
    /// is the final tie-break for malformed entries whose embedded identity
    /// fields are equal.
    pub fn call_sites_sorted(&self) -> Vec<&ResolvedCallSite> {
        let mut sites = self.call_sites.iter().collect::<Vec<_>>();
        sites.sort_by(|(left_key, left), (right_key, right)| {
            left.node_id
                .cmp(&right.node_id)
                .then_with(|| left.owner.cmp(&right.owner))
                .then_with(|| left.callee.cmp(&right.callee))
                .then_with(|| left.argc.cmp(&right.argc))
                .then_with(|| left_key.cmp(right_key))
        });
        sites.into_iter().map(|(_, site)| site).collect()
    }

    /// Return the stable boundary reason for any method call whose Checker
    /// receiver type is a dynamic trait object.  This is shared by route
    /// selection and direct consumers so a caller cannot reach Bytecode,
    /// native codegen, or verifier compatibility by skipping the CLI router.
    pub fn dynamic_protocol_method_boundary_reason(&self) -> Option<String> {
        let has_dynamic_method = self.call_sites.values().any(|site| {
            site.method_receiver
                .as_ref()
                .and_then(|receiver| self.resolved_node_type(receiver))
                .and_then(|ty| self.resolved_types.get(ty))
                .is_some_and(|ty| {
                    matches!(
                        ty,
                        crate::core::ResolvedType::Trait {
                            kind: crate::core::ir::TraitTypeKind::Dynamic,
                            ..
                        }
                    )
                })
        });
        has_dynamic_method.then(|| {
            format!(
                "{}: dynamic Protocol method dispatch through a `dyn Trait` receiver is removed by language-spec §3.9; default consumers cannot use Bytecode, LLVM, or verifier compatibility fallback",
                crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE
            )
        })
    }

    pub fn extern_func_signature(&self, name: &str) -> Option<&ResolvedExternFunc> {
        self.extern_blocks_sorted()
            .into_iter()
            .flat_map(Self::extern_signatures_sorted)
            .find(|sig| sig.name == name)
    }

    /// Resolve the checker-owned extern declaration for one call-site name.
    ///
    /// The checker rejects duplicate extern symbols in a well-formed source
    /// file, but merged or hand-built `CheckedProgram` values can still carry
    /// more than one declaration with the same surface name.  A call-site
    /// consumer must not let a sorted catalog turn that malformed state into a
    /// silently selected ABI.  Keep the cardinality explicit and report every
    /// candidate identity in stable order.  `argc` is part of the API because
    /// callers are resolving a call, but it is intentionally not used to
    /// overload extern symbols: duplicate names are ambiguous even when their
    /// arities differ.
    pub fn extern_func_signature_for_call(
        &self,
        name: &str,
        argc: usize,
    ) -> Result<Option<&ResolvedExternFunc>, String> {
        resolve_extern_func_signature_for_call(&self.extern_blocks, name, argc)
    }

    /// Resolve one checker-owned extern signature identity with explicit
    /// zero/one/many cardinality. Consumers must use this when starting from
    /// a canonical `ResolvedCallee::Extern` node id.
    pub fn extern_func_for_signature(
        &self,
        signature_id: &NodeId,
    ) -> Result<Option<&ResolvedExternFunc>, String> {
        resolve_extern_func_for_signature(&self.extern_blocks, signature_id)
    }

    pub fn type_schemes(&self) -> &HashMap<NodeId, TypeScheme> {
        &self.type_schemes
    }

    pub fn zonked_function_types(&self) -> &HashMap<NodeId, (Vec<ZonkedTy>, ZonkedTy)> {
        &self.zonked_function_types
    }

    pub fn zonked_function_type(&self, function: &NodeId) -> Option<&(Vec<ZonkedTy>, ZonkedTy)> {
        self.zonked_function_types.get(function)
    }

    pub fn resolved_types(&self) -> &crate::core::ResolvedTypeTable {
        &self.resolved_types
    }

    pub fn resolved_signatures(&self) -> &BTreeMap<NodeId, crate::core::ResolvedSignature> {
        &self.resolved_signatures
    }

    pub fn resolved_signature(&self, owner: &NodeId) -> Option<&crate::core::ResolvedSignature> {
        self.resolved_signatures.get(owner)
    }

    pub fn resolved_trait_signatures(&self) -> &BTreeMap<NodeId, crate::core::ResolvedSignature> {
        &self.resolved_trait_signatures
    }

    pub fn resolved_trait_signature(
        &self,
        owner: &NodeId,
    ) -> Option<&crate::core::ResolvedSignature> {
        self.resolved_trait_signatures.get(owner)
    }

    pub fn resolved_node_types(&self) -> &BTreeMap<NodeId, crate::core::ResolvedTypeId> {
        &self.resolved_node_types
    }

    pub fn resolved_node_type(&self, node: &NodeId) -> Option<&crate::core::ResolvedTypeId> {
        self.resolved_node_types.get(node)
    }

    pub fn resolved_type_operands(&self) -> &BTreeMap<NodeId, crate::core::ResolvedTypeId> {
        &self.resolved_type_operands
    }

    pub fn resolved_type_operand(&self, node: &NodeId) -> Option<&crate::core::ResolvedTypeId> {
        self.resolved_type_operands.get(node)
    }

    pub fn resolved_type_arguments(&self) -> &BTreeMap<NodeId, Vec<crate::core::ResolvedTypeId>> {
        &self.resolved_type_arguments
    }

    pub fn resolved_type_arguments_at(
        &self,
        node: &NodeId,
    ) -> Option<&[crate::core::ResolvedTypeId]> {
        self.resolved_type_arguments.get(node).map(Vec::as_slice)
    }

    pub fn resolved_field_types(&self) -> &BTreeMap<NodeId, crate::core::ResolvedTypeId> {
        &self.resolved_field_types
    }

    pub fn resolved_field_type(&self, field: &NodeId) -> Option<&crate::core::ResolvedTypeId> {
        self.resolved_field_types.get(field)
    }

    /// Canonical payload types for flow states targeted by multi-target
    /// transitions, keyed by the state payload field identity. Empty unless a
    /// union-return receipt needs it.
    pub fn resolved_state_field_types(&self) -> &BTreeMap<NodeId, crate::core::ResolvedTypeId> {
        &self.resolved_state_field_types
    }

    pub fn resolved_variants(&self) -> &BTreeMap<NodeId, ResolvedVariantSchema> {
        &self.resolved_variants
    }

    pub fn resolved_variant(&self, variant: &NodeId) -> Option<&ResolvedVariantSchema> {
        self.resolved_variants.get(variant)
    }

    pub fn resolved_variant_named(
        &self,
        owner: &NodeId,
        name: &str,
    ) -> Option<&ResolvedVariantSchema> {
        let variant = self.type_defs.get(owner)?.variant_ids.get(name)?;
        self.resolved_variants.get(variant)
    }

    /// Return the checker-owned display name for a declaration member without
    /// consulting its retained surface declaration. Consumers dispatch by the
    /// `NodeId`; the name is only runtime/debug presentation metadata.
    pub fn resolved_member_name<'a>(&'a self, member: &NodeId) -> Option<&'a str> {
        for definition in self.type_defs.values() {
            if let Some((name, _)) = definition
                .field_ids
                .iter()
                .find(|(_, identity)| *identity == member)
                .or_else(|| {
                    definition
                        .variant_ids
                        .iter()
                        .find(|(_, identity)| *identity == member)
                })
            {
                return Some(name.as_str());
            }
        }
        for actor in self.actors.values() {
            if let Some((name, _)) = actor
                .field_ids
                .iter()
                .find(|(_, identity)| *identity == member)
            {
                return Some(name.as_str());
            }
        }
        for flow in self.flows.values() {
            for state in flow.states.values() {
                if let Some((name, _)) = state
                    .field_ids
                    .iter()
                    .find(|(_, identity)| *identity == member)
                {
                    return Some(name.as_str());
                }
            }
        }
        match member.0.as_str() {
            "builtin:variant:Option::Some" => Some("Some"),
            "builtin:variant:Option::None" => Some("None"),
            "builtin:variant:Result::Ok" => Some("Ok"),
            "builtin:variant:Result::Err" => Some("Err"),
            identity => builtin_record_schema(
                identity
                    .split_once("/field:")
                    .map(|(owner, _)| owner)
                    .unwrap_or_default(),
            )
            .and_then(|schema| {
                schema
                    .iter()
                    .find(|(name, _)| identity.ends_with(&format!("/field:{name}")))
                    .map(|(name, _)| *name)
            }),
        }
    }

    pub fn resolved_type_targets(&self) -> &BTreeMap<NodeId, crate::core::ResolvedTypeId> {
        &self.resolved_type_targets
    }

    pub fn resolved_type_target(
        &self,
        definition: &NodeId,
    ) -> Option<&crate::core::ResolvedTypeId> {
        self.resolved_type_targets.get(definition)
    }

    pub fn resolved_session_actions(
        &self,
    ) -> &BTreeMap<NodeId, crate::core::ResolvedSessionAction> {
        &self.resolved_session_actions
    }

    pub fn resolved_session_action(
        &self,
        call: &NodeId,
    ) -> Option<&crate::core::ResolvedSessionAction> {
        self.resolved_session_actions.get(call)
    }

    /// Checker-owned declaration expression roots, including exact Deferred
    /// lowering boundaries. This directory carries no retained surface body.
    pub fn declaration_expression_bodies(
        &self,
    ) -> &BTreeMap<NodeId, crate::core::ir::lower::ResolvedDeclarationExpressionBody> {
        &self.declaration_expression_bodies
    }

    /// Complete checker-owned census of constant, actor-field, trait, and
    /// callable parameter-default roots that may contain direct extern calls.
    /// Roots remain present when their declaration default has no call use.
    pub fn checked_ffi_source_roots(&self) -> &[crate::core::ir::lower::CheckedFfiSourceRoot] {
        &self.ffi_source_roots
    }

    /// Resolve a source-root handle to its canonical typed body. This API
    /// takes no caller-supplied body and rejects missing, duplicated, moved,
    /// or changed roots.
    pub fn checked_ffi_source_root_body<'a>(
        &'a self,
        handle: &crate::core::ir::lower::CheckedFfiSourceRoot,
    ) -> Result<&'a crate::core::ir::ResolvedBody, Vec<crate::core::ir::ResolvedBodyError>> {
        use crate::core::ir::lower::{
            checked_ffi_source_body_fingerprint, ResolvedDeclarationExpressionKind as Kind,
            ResolvedDeclarationRootIdentity as Identity,
        };
        let fail = |node: &NodeId, reason: &str| {
            vec![crate::core::ir::ResolvedBodyError::new(
                node.clone(),
                format!("TOOL-RESOLUTION-001: checked FFI source root {reason}"),
            )]
        };
        let matching = self
            .ffi_source_roots
            .iter()
            .filter(|root| root.root_node() == handle.root_node())
            .collect::<Vec<_>>();
        let [canonical] = matching.as_slice() else {
            return Err(fail(
                handle.root_node(),
                if matching.is_empty() {
                    "is absent from this CheckedProgram"
                } else {
                    "NodeId is ambiguous across declaration roots"
                },
            ));
        };
        if !handle.has_valid_witness()
            || !canonical.same_handle(handle)
            || !canonical.has_valid_call_census()
            || !self.ffi_source_root_identity_is_canonical(canonical)
        {
            return Err(fail(
                handle.root_node(),
                "handle does not match its checker-issued owner/declaration/body witness",
            ));
        }
        let (body, expression, fingerprint) = match (handle.kind(), handle.identity()) {
            (
                Kind::CallableParameterDefault,
                Some(Identity::CallableParameterDefault {
                    callable,
                    parameter,
                }),
            ) => {
                let body = self
                    .resolved_bodies
                    .get(callable)
                    .ok_or_else(|| fail(handle.root_node(), "callable default body is missing"))?;
                let expression = body.default_values.get(parameter).ok_or_else(|| {
                    fail(handle.root_node(), "canonical parameter default is missing")
                })?;
                if body.owner != *callable
                    || expression.node_id != *handle.root_node()
                    || *callable != *handle.owner()
                {
                    return Err(fail(
                        handle.root_node(),
                        "callable/default parameter identity does not match the source root",
                    ));
                }
                (
                    body,
                    expression,
                    checked_ffi_source_body_fingerprint(body, expression, None),
                )
            }
            (Kind::ConstantInitializer, Some(Identity::Constant { .. }))
            | (Kind::ActorFieldInitializer, Some(Identity::ActorField { .. }))
            | (Kind::TraitParameterDefault, Some(Identity::TraitParameterDefault { .. })) => {
                let root = self
                    .declaration_expression_bodies
                    .get(handle.root_node())
                    .ok_or_else(|| fail(handle.root_node(), "typed body is missing"))?;
                if root.node_id != *handle.root_node()
                    || root.owner != *handle.owner()
                    || root.kind != handle.kind()
                {
                    return Err(fail(
                        handle.root_node(),
                        "typed declaration body does not match the source root",
                    ));
                }
                let body = root.body.as_ref().map_err(Clone::clone)?;
                let expression = body.root.result.as_deref().ok_or_else(|| {
                    fail(
                        handle.root_node(),
                        "typed declaration root expression is missing",
                    )
                })?;
                (
                    body,
                    expression,
                    checked_ffi_source_body_fingerprint(body, expression, Some(root)),
                )
            }
            _ => {
                return Err(fail(
                    handle.root_node(),
                    "source root kind/identity is invalid",
                ))
            }
        };
        let root_origin = handle
            .origin()
            .ok_or_else(|| fail(handle.root_node(), "source root origin is missing"))?;
        if fingerprint != handle.body_fingerprint() || &expression.origin != root_origin {
            return Err(fail(
                handle.root_node(),
                "typed body origin disagrees with its source root handle",
            ));
        }
        Ok(body)
    }

    /// Resolve an exact source call handle through its source root, canonical
    /// typed body, and checked extern declaration. Callers cannot substitute
    /// a body, call expression, or extern descriptor.
    pub fn checked_ffi_source_call_context<'a>(
        &'a self,
        root_handle: &crate::core::ir::lower::CheckedFfiSourceRoot,
        call_handle: &crate::core::ir::lower::CheckedFfiSourceCall,
    ) -> Result<
        crate::core::ir::lower::CheckedFfiSourceCallContext<'a>,
        Vec<crate::core::ir::ResolvedBodyError>,
    > {
        use crate::core::ir::lower::CheckedFfiSourceCallContext;
        let fail = |node: &NodeId, reason: &str| {
            vec![crate::core::ir::ResolvedBodyError::new(
                node.clone(),
                format!("TOOL-RESOLUTION-001: checked FFI source call {reason}"),
            )]
        };
        let body = self.checked_ffi_source_root_body(root_handle)?;
        let canonical_root = self
            .ffi_source_roots
            .iter()
            .find(|root| {
                root.root_node() == root_handle.root_node() && root.same_handle(root_handle)
            })
            .ok_or_else(|| fail(call_handle.call_node(), "root handle is absent"))?;
        let matching_calls = canonical_root
            .calls()
            .iter()
            .filter(|call| call.call_node() == call_handle.call_node())
            .collect::<Vec<_>>();
        let [canonical_call] = matching_calls.as_slice() else {
            return Err(fail(
                call_handle.call_node(),
                if matching_calls.is_empty() {
                    "is absent from the selected source root"
                } else {
                    "NodeId is ambiguous inside the selected source root"
                },
            ));
        };
        if !canonical_call.same_handle(call_handle) || !canonical_call.belongs_to(canonical_root) {
            return Err(fail(
                call_handle.call_node(),
                "handle does not match its checker-issued root/call witness",
            ));
        }
        let expressions =
            body.call_expressions(self.resolved_types(), self.resolved_type_targets())?;
        let expression = expressions
            .get(call_handle.call_node())
            .ok_or_else(|| fail(call_handle.call_node(), "typed call node is missing"))?;
        if !canonical_call.matches_typed_expression(expression) {
            return Err(fail(
                call_handle.call_node(),
                "typed expression/origin/extern declaration changed after checker issuance",
            ));
        }
        let declaration = self
            .ffi_declarations
            .get(call_handle.external_declaration())
            .ok_or_else(|| {
                fail(
                    call_handle.external_declaration(),
                    "extern declaration is missing",
                )
            })?;
        if declaration.declaration != *call_handle.external_declaration() {
            return Err(fail(
                call_handle.external_declaration(),
                "extern declaration identity changed",
            ));
        }
        declaration.validate(self)?;
        Ok(CheckedFfiSourceCallContext {
            root: canonical_root,
            body,
            call: expression.clone(),
            declaration,
        })
    }

    fn ffi_source_root_identity_is_canonical(
        &self,
        root: &crate::core::ir::lower::CheckedFfiSourceRoot,
    ) -> bool {
        use crate::core::ir::lower::{
            ResolvedDeclarationExpressionKind as Kind, ResolvedDeclarationRootIdentity as Identity,
        };
        match (root.kind(), root.identity()) {
            (Kind::ConstantInitializer, Some(Identity::Constant { declaration })) => {
                root.owner() == declaration && self.constants.contains_key(declaration)
            }
            (Kind::ActorFieldInitializer, Some(Identity::ActorField { actor, field })) => {
                root.owner() == actor
                    && self
                        .actors
                        .get(actor)
                        .is_some_and(|resolved| resolved.field_ids.values().any(|id| id == field))
            }
            (
                Kind::TraitParameterDefault,
                Some(Identity::TraitParameterDefault { method, parameter }),
            ) => {
                root.owner() == method
                    && self
                        .resolved_trait_signature(method)
                        .is_some_and(|signature| {
                            signature.parameters.iter().any(|candidate| {
                                &candidate.id == parameter && candidate.has_default
                            })
                        })
            }
            (
                Kind::CallableParameterDefault,
                Some(Identity::CallableParameterDefault {
                    callable,
                    parameter,
                }),
            ) => {
                root.owner() == callable
                    && self.resolved_signature(callable).is_some_and(|signature| {
                        signature
                            .parameters
                            .iter()
                            .any(|candidate| &candidate.id == parameter && candidate.has_default)
                    })
                    && self
                        .resolved_body(callable)
                        .and_then(|body| body.default_values.get(parameter))
                        .is_some_and(|expression| expression.node_id == *root.root_node())
            }
            _ => false,
        }
    }

    pub fn resolved_bodies(&self) -> &BTreeMap<NodeId, crate::core::ResolvedBody> {
        &self.resolved_bodies
    }

    pub fn resolved_body(&self, owner: &NodeId) -> Option<&crate::core::ResolvedBody> {
        self.resolved_bodies.get(owner)
    }

    pub fn callable_cfgs(&self) -> &BTreeMap<NodeId, crate::core::cfg::CallableCfg> {
        &self.callable_cfgs
    }

    pub fn callable_cfg(&self, owner: &NodeId) -> Option<&crate::core::cfg::CallableCfg> {
        self.callable_cfgs.get(owner)
    }

    pub fn resource_analyses(&self) -> &BTreeMap<NodeId, crate::core::ResourceAnalysis> {
        &self.resource_analyses
    }

    pub fn resource_analysis(&self, owner: &NodeId) -> Option<&crate::core::ResourceAnalysis> {
        self.resource_analyses.get(owner)
    }

    /// Complete declaration facts, including dormant and Deferred extern contracts.
    /// Consumers validate stable roots and canonical types instead of reopening AST.
    pub fn ffi_declarations(
        &self,
    ) -> &BTreeMap<NodeId, crate::core::ir::ResolvedFfiDeclarationContract> {
        &self.ffi_declarations
    }

    pub fn callables(&self) -> &BTreeMap<NodeId, crate::core::ResolvedCallable> {
        &self.callables
    }

    pub fn callable(&self, owner: &NodeId) -> Option<&crate::core::ResolvedCallable> {
        self.callables.get(owner)
    }

    pub fn entry_span(&self) -> Option<Span> {
        self.items
            .values()
            .find(|item| item.kind == ResolvedItemKind::Function && item.qualified_name == "main")
            .or_else(|| {
                self.items
                    .values()
                    .filter(|item| matches!(item.origin, Origin::User(_)))
                    .min_by(|left, right| left.node_id.0.cmp(&right.node_id.0))
            })
            .map(|item| item.origin.user_span())
    }

    pub fn flow(&self, qualified_name: &str) -> Option<&ResolvedFlow> {
        self.flows.get(&FlowId(qualified_name.to_string()))
    }

    pub fn transition(&self, flow: &str, event: &str, source: &str) -> Option<&ResolvedTransition> {
        let flow = FlowId(flow.to_string());
        self.transitions.get(&TransitionId {
            source: StateId {
                flow: flow.clone(),
                name: source.to_string(),
            },
            flow,
            event: event.to_string(),
        })
    }

    pub fn validate_backend(&self, backend: BackendProfile) -> Result<(), Vec<Diagnostic>> {
        let unsupported = self
            .backend_requirements
            .iter()
            .filter(|requirement| !backend_supports(backend, requirement.capability))
            .map(|requirement| {
                Diagnostic::error(
                    format!(
                        "{}: backend {:?} does not support '{}' required by flow '{}'",
                        requirement.requirement_id,
                        backend,
                        requirement.capability,
                        (requirement.flow).0
                    ),
                    requirement.span,
                )
            })
            .collect::<Vec<_>>();
        if unsupported.is_empty() {
            Ok(())
        } else {
            Err(unsupported)
        }
    }
}

/// Resolve an inferred effect summary for a canonical callable name.  Impl
/// and actor method owners include trait/generic suffixes in Resolved IR,
/// whereas the checker effect pass keys methods by `Type::method` or
/// `Actor::method`; an exact hit wins and a unique `::method` suffix is the
/// only fallback accepted here.
fn effect_summary_for_name(
    summaries: &HashMap<String, crate::core::effects::EffectSummary>,
    name: &str,
) -> Option<crate::core::effects::EffectSummary> {
    if let Some(summary) = summaries.get(name) {
        return Some(summary.clone());
    }
    let short = name.rsplit("::").next()?;
    let mut candidates = summaries
        .iter()
        .filter(|(key, _)| key.rsplit("::").next() == Some(short))
        .map(|(_, summary)| summary.clone())
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        candidates.pop()
    } else {
        None
    }
}

fn validate_resolved_callable_bodies(program: &CheckedProgram) -> Result<(), Vec<Diagnostic>> {
    let mut errors = Vec::new();
    for (owner, body) in &program.resolved_bodies {
        let span = program
            .node_meta
            .get(owner)
            .map(|meta| meta.origin.user_span())
            .unwrap_or(Span::UNKNOWN);
        if &body.owner != owner {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: body map key '{}' disagrees with owner '{}'",
                    owner.0, body.owner.0
                ),
                span,
            ));
        }
        let Some(signature) = program.resolved_signatures.get(owner) else {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: callable body '{}' has no canonical signature",
                    owner.0
                ),
                span,
            ));
            continue;
        };
        if body.parameters.len() != signature.parameters.len() {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: callable '{}' body has {} parameters but its signature has {}",
                    owner.0,
                    body.parameters.len(),
                    signature.parameters.len()
                ),
                span,
            ));
            continue;
        }
        for (local_id, parameter) in body.parameters.iter().zip(&signature.parameters) {
            let Some(local) = body.locals.get(local_id) else {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: callable '{}' parameter local '{}' is missing",
                        owner.0, local_id.0 .0
                    ),
                    span,
                ));
                continue;
            };
            if local.ty != parameter.ty
                || local.mutable != parameter.mutable
                || local.display_name != parameter.name
            {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: callable '{}' parameter local '{}' disagrees with canonical parameter '{}'",
                        owner.0, local_id.0 .0, parameter.id.0 .0
                    ),
                    local.origin.user_span(),
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_resolved_variant_schemas(program: &CheckedProgram) -> Result<(), Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let mut referenced = std::collections::BTreeSet::new();
    for definition in program.type_defs.values() {
        for (name, binder) in &definition.generic_parameters {
            if !program.node_meta.contains_key(binder) {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: generic binder '{}::{}' has no NodeMeta",
                        definition.qualified_name, name
                    ),
                    definition.origin.user_span(),
                ));
            }
        }
        if definition.kind != ResolvedTypeKind::Enum {
            continue;
        }
        for (name, variant_id) in &definition.variant_ids {
            referenced.insert(variant_id.clone());
            let Some(schema) = program.resolved_variants.get(variant_id) else {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: enum variant '{}::{}' has no canonical schema",
                        definition.qualified_name, name
                    ),
                    definition.origin.user_span(),
                ));
                continue;
            };
            if schema.node_id != *variant_id
                || schema.owner != definition.node_id
                || schema.name != *name
            {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: enum variant schema '{}' disagrees with its declaration catalog",
                        variant_id.0
                    ),
                    definition.origin.user_span(),
                ));
            }
            if !program.node_meta.contains_key(variant_id) {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: enum variant '{}' has no NodeMeta",
                        variant_id.0
                    ),
                    definition.origin.user_span(),
                ));
            }
            let mut member_ids = std::collections::BTreeSet::new();
            let mut member_names = std::collections::BTreeSet::new();
            for member in &schema.members {
                if !member_ids.insert(member.node_id.clone())
                    || !member_names.insert(member.name.clone())
                {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: enum variant '{}' has duplicate canonical members",
                            variant_id.0
                        ),
                        definition.origin.user_span(),
                    ));
                }
                if !program.node_meta.contains_key(&member.node_id)
                    || program.resolved_field_types.get(&member.node_id) != Some(&member.ty)
                    || program.resolved_types.get(&member.ty).is_none()
                {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: enum member '{}' has incomplete canonical facts",
                            member.node_id.0
                        ),
                        definition.origin.user_span(),
                    ));
                }
            }
            if schema.shape == ResolvedVariantShape::Unit && !schema.members.is_empty() {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: unit variant '{}' has payload members",
                        variant_id.0
                    ),
                    definition.origin.user_span(),
                ));
            }
        }
    }
    for (variant_id, schema) in &program.resolved_variants {
        if !referenced.contains(variant_id) {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: variant schema '{}' is not owned by a resolved enum",
                    variant_id.0
                ),
                schema
                    .members
                    .first()
                    .and_then(|member| program.node_meta.get(&member.node_id))
                    .map(|meta| meta.origin.user_span())
                    .unwrap_or(Span::UNKNOWN),
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn backend_supports(backend: BackendProfile, capability: &str) -> bool {
    match backend {
        // Interpreter implements the current Flow surface, including experimental multi-target.
        BackendProfile::Interpreter => true,
        // v0.34.16 (ADR-002): Native gained the tagged-state-union ABI
        // (synthetic __MultiTarget enum + boxed payload) — multi-target
        // transitions are now lowered, so the gate is removed.
        BackendProfile::Native => true,
        // Verifier proves function contracts; it does not claim Proven for Flow turns.
        // Multi-target flows must not block unrelated contract verification.
        BackendProfile::Verifier => true,
        // Component IR consumers cannot yet lower Flow runtime features.
        BackendProfile::Component => !matches!(capability, "flow.multi_target"),
    }
}

/// Assign stable module-qualified names to colliding top-level functions.
/// The same map is consumed by item metadata, function bodies, and call-site
/// owners so a pair of same-named exports cannot collide in only one catalog.
pub(crate) fn top_level_function_qualified_names(
    items: &[Item],
    sources: &SourceRegistry,
) -> HashMap<(String, String), String> {
    let mut source_identities: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut function_sources = Vec::new();
    for item in items {
        let Item::Func(function) = item else { continue };
        let source_identity = sources
            .key(function.meta.span.source_id)
            .map(|key| key.as_str().to_string())
            .unwrap_or_else(|| format!("<source:{}>", function.meta.span.source_id.raw()));
        source_identities
            .entry(function.name.clone())
            .or_default()
            .insert(source_identity.clone());
        function_sources.push((source_identity, function.name.clone()));
    }

    function_sources
        .into_iter()
        .map(|(source_identity, name)| {
            let qualified = if source_identities
                .get(&name)
                .is_some_and(|identities| identities.len() > 1)
            {
                let module = sources
                    .records()
                    .iter()
                    .find(|record| record.key.as_str() == source_identity)
                    .and_then(|record| {
                        crate::module_system::module_path_from_source_key(&record.key)
                    })
                    .unwrap_or_else(|| source_identity.clone());
                format!("{module}::{name}")
            } else {
                name.clone()
            };
            ((source_identity, name), qualified)
        })
        .collect()
}

pub(crate) fn top_level_function_qualified_name(
    function: &crate::ast::FuncDef,
    sources: &SourceRegistry,
    names: &HashMap<(String, String), String>,
) -> String {
    let source_identity = sources
        .key(function.meta.span.source_id)
        .map(|key| key.as_str().to_string())
        .unwrap_or_else(|| format!("<source:{}>", function.meta.span.source_id.raw()));
    names
        .get(&(source_identity, function.name.clone()))
        .cloned()
        .unwrap_or_else(|| function.name.clone())
}

fn collect_items(
    items: &[Item],
    sources: &SourceRegistry,
    resolved_items: &mut HashMap<NodeId, ResolvedItem>,
    node_meta: &mut HashMap<NodeId, NodeMeta>,
    functions: &mut HashMap<NodeId, ResolvedFunction>,
    sessions: &mut HashMap<NodeId, ResolvedSession>,
    actors: &mut HashMap<NodeId, ResolvedActor>,
    capabilities: &mut HashMap<NodeId, ResolvedCapability>,
    constants: &mut HashMap<NodeId, ResolvedConstant>,
    traits: &mut HashMap<NodeId, ResolvedTrait>,
    impls: &mut HashMap<NodeId, ResolvedImpl>,
    type_defs: &mut HashMap<NodeId, ResolvedTypeDef>,
    extern_blocks: &mut HashMap<NodeId, ResolvedExternBlock>,
    flows: &mut HashMap<FlowId, ResolvedFlow>,
    transitions: &mut HashMap<TransitionId, ResolvedTransition>,
    backend_requirements: &mut Vec<CapabilityRequirement>,
    errors: &mut Vec<Diagnostic>,
) {
    let ids = NodeIdBuilder::new(sources);
    // File-merge compatibility keeps declarations with their surface names,
    // but a qualified import needs both same-named exports to survive the
    // canonical catalog. Pre-compute cross-source collisions and qualify only
    // that narrow slice; same-file duplicate definitions retain the existing
    // duplicate diagnostic.
    let function_qualified_names = top_level_function_qualified_names(items, sources);
    for item in items {
        collect_item_meta(item, &ids, &function_qualified_names, node_meta, errors);
        match item {
            Item::Flow(flow) => {
                let qualified = flow.name.clone();
                let span = declaration_span(flow.meta, flow.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Flow,
                    &qualified,
                    flow.meta,
                    span,
                    errors,
                );
                collect_flow(
                    flow,
                    &qualified,
                    &ids,
                    flows,
                    transitions,
                    backend_requirements,
                    errors,
                );
                // Full audit 2026-08-05 (#10): nested callables declared in
                // user-implemented transition bodies were recorded by the
                // meta/call-site walks but never collected into the function
                // catalog, leaving dangling NestedCallable statements (no
                // signature, no body). The User-origin slice mirrors
                // collect_transition_syntax (ir/lower.rs), which is the slice
                // body lowering processes; synthetic matrix transitions must
                // not contribute catalog records their bodies never lower.
                for transition in &flow.transitions {
                    if !matches!(transition.meta.origin, AstOrigin::User) {
                        continue;
                    }
                    let Some(body) = &transition.body else {
                        continue;
                    };
                    let transition_owner = NodeId(format!(
                        "transition:{qualified}::{}::{}",
                        transition.name, transition.from_state
                    ));
                    collect_nested_function_records(
                        body,
                        &transition_owner,
                        &format!("{qualified}::{}", transition.name),
                        (&ids, &[]),
                        node_meta,
                        functions,
                        errors,
                    );
                }
            }
            Item::Func(function) => {
                let qualified =
                    top_level_function_qualified_name(function, sources, &function_qualified_names);
                let span = declaration_span(function.meta, function.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Function,
                    &qualified,
                    function.meta,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("function:{}", qualified));
                let generic_binders =
                    callable_generic_binders(&function.generics, &node_id, &ids, errors);
                let origin = resolve_named_origin(
                    ResolvedItemKind::Function,
                    &qualified,
                    &node_id,
                    function.meta,
                    span,
                    errors,
                );
                let params = function
                    .params
                    .iter()
                    .map(|param| (param.name.clone(), param.ty.clone()))
                    .collect::<Vec<_>>();
                for (name, ty) in &params {
                    if contains_unresolved_type(ty) {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: unresolved or erased type '{}' in function '{}' parameter '{}'",
                                crate::core::fmt_type(ty),
                                qualified,
                                name
                            ),
                            span,
                        ));
                    }
                }
                let ret = function
                    .ret
                    .clone()
                    .unwrap_or_else(|| Type::Name("unit".into(), vec![]));
                if contains_unresolved_type(&ret) {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: unresolved or erased return type '{}' in function '{}'",
                            crate::core::fmt_type(&ret),
                            qualified
                        ),
                        span,
                    ));
                }
                functions.insert(
                    node_id.clone(),
                    ResolvedFunction {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        params,
                        param_decls: function.params.clone(),
                        ret,
                        effects: function.effects.clone(),
                        direct_effect_sources: BTreeMap::new(),
                        pub_: function.pub_,
                        is_comptime: function.is_comptime,
                        is_async: function.is_async,
                        extern_abi: function.extern_abi.clone(),
                        generics: function.generics.clone(),
                        generic_binders: generic_binders.clone(),
                        where_clause: function.where_clause.clone(),
                        origin,
                    },
                );
                collect_nested_function_records(
                    &function.body,
                    &node_id,
                    &qualified,
                    (&ids, &generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            Item::Type(type_def) => {
                let qualified = type_def.name.clone();
                let node_id = NodeId(format!("type:{}", qualified));
                let fallback = type_def.meta.span;
                let span = declaration_span(type_def.meta, fallback);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Type,
                    &qualified,
                    type_def.meta,
                    span,
                    errors,
                );
                let kind = match &type_def.kind {
                    crate::ast::TypeDefKind::Alias(_) => ResolvedTypeKind::Alias,
                    crate::ast::TypeDefKind::Newtype(_) => ResolvedTypeKind::Newtype,
                    crate::ast::TypeDefKind::Record(_) => ResolvedTypeKind::Record,
                    crate::ast::TypeDefKind::Enum(_) => ResolvedTypeKind::Enum,
                    crate::ast::TypeDefKind::Union(_) => ResolvedTypeKind::Union,
                };
                let mut alias_of = None;
                let mut fields = Vec::new();
                let mut field_ids = BTreeMap::new();
                let mut variants = Vec::new();
                let mut variant_ids = BTreeMap::new();
                let generic_parameters =
                    callable_generic_binders(&type_def.generics, &node_id, &ids, errors);
                match &type_def.kind {
                    crate::ast::TypeDefKind::Alias(ty) | crate::ast::TypeDefKind::Newtype(ty) => {
                        if contains_unresolved_type(ty) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved alias/newtype target in type '{}'",
                                    qualified
                                ),
                                span,
                            ));
                        }
                        alias_of = Some(crate::core::fmt_type(ty));
                    }
                    crate::ast::TypeDefKind::Record(record_fields)
                    | crate::ast::TypeDefKind::Union(record_fields) => {
                        for field in record_fields {
                            if contains_unresolved_type(&field.ty) {
                                errors.push(Diagnostic::error(
                                    format!(
                                        "TOOL-RESOLUTION-001: unresolved field type in type '{}' field '{}'",
                                        qualified,
                                        field.name
                                    ),
                                    span,
                                ));
                            }
                            fields.push((field.name.clone(), crate::core::fmt_type(&field.ty)));
                            field_ids.insert(
                                field.name.clone(),
                                ids.anonymous(
                                    &node_id,
                                    "decl.field",
                                    &format!("field.{}", stable_id_fragment(&field.name)),
                                    usable_span(field.meta.span),
                                    field.meta.origin,
                                    errors,
                                ),
                            );
                        }
                    }
                    crate::ast::TypeDefKind::Enum(enum_variants) => {
                        for variant in enum_variants {
                            variant_ids.insert(
                                variant.name.clone(),
                                ids.anonymous(
                                    &node_id,
                                    "decl.variant",
                                    &format!("variant.{}", stable_id_fragment(&variant.name)),
                                    usable_span(variant.meta.span),
                                    variant.meta.origin,
                                    errors,
                                ),
                            );
                            let payload = match &variant.payload {
                                Some(crate::ast::VariantPayload::Tuple(types)) => {
                                    for ty in types {
                                        if contains_unresolved_type(ty) {
                                            errors.push(Diagnostic::error(
                                                format!(
                                                    "TOOL-RESOLUTION-001: unresolved enum payload in type '{}' variant '{}'",
                                                    qualified,
                                                    variant.name
                                                ),
                                                span,
                                            ));
                                        }
                                    }
                                    Some(format!(
                                        "({})",
                                        types
                                            .iter()
                                            .map(crate::core::fmt_type)
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ))
                                }
                                Some(crate::ast::VariantPayload::Record(record_fields)) => {
                                    let mut parts = Vec::new();
                                    for field in record_fields {
                                        if contains_unresolved_type(&field.ty) {
                                            errors.push(Diagnostic::error(
                                                format!(
                                                    "TOOL-RESOLUTION-001: unresolved enum record field in type '{}' variant '{}'",
                                                    qualified,
                                                    variant.name
                                                ),
                                                span,
                                            ));
                                        }
                                        parts.push(format!(
                                            "{}: {}",
                                            field.name,
                                            crate::core::fmt_type(&field.ty)
                                        ));
                                    }
                                    Some(format!("{{{}}}", parts.join(", ")))
                                }
                                None => None,
                            };
                            variants.push((variant.name.clone(), payload));
                        }
                    }
                }
                type_defs.insert(
                    node_id.clone(),
                    ResolvedTypeDef {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        kind,
                        alias_of,
                        fields,
                        field_ids,
                        variants,
                        variant_ids,
                        generic_parameters,
                        declaration: type_def.clone(),
                        origin: resolve_named_origin(
                            ResolvedItemKind::Type,
                            &qualified,
                            &node_id,
                            type_def.meta,
                            span,
                            errors,
                        ),
                    },
                );
            }
            Item::Const {
                meta,
                name,
                ty,
                value,
                extern_abi,
                ..
            } => {
                let qualified = name.clone();
                let span = declaration_span(*meta, meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Constant,
                    &qualified,
                    *meta,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("constant:{}", qualified));
                let ty_str = ty.as_ref().map(crate::core::fmt_type);
                if ty.as_ref().is_some_and(contains_unresolved_type) {
                    errors.push(Diagnostic::error(
                        format!(
                            "const `{}` has unresolved type in CheckedProgram materialization",
                            qualified
                        ),
                        span,
                    ));
                }
                constants.insert(
                    node_id.clone(),
                    ResolvedConstant {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        ty: ty_str,
                        value: materialize_const_value(value),
                        extern_abi: extern_abi.clone(),
                        native_data_export: None,
                        origin: resolve_named_origin(
                            ResolvedItemKind::Constant,
                            &qualified,
                            &node_id,
                            *meta,
                            span,
                            errors,
                        ),
                    },
                );
            }
            Item::Cap(cap) => {
                let qualified = cap.name.clone();
                let span = declaration_span(cap.meta, cap.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Capability,
                    &qualified,
                    cap.meta,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("capability:{}", qualified));
                capabilities.insert(
                    node_id.clone(),
                    ResolvedCapability {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        combined_with: cap.combined_with.clone(),
                        components: capability_components(&cap.name, cap.combined_with.as_deref()),
                        origin: resolve_named_origin(
                            ResolvedItemKind::Capability,
                            &qualified,
                            &node_id,
                            cap.meta,
                            span,
                            errors,
                        ),
                    },
                );
                // 0.34.19 CHECKER-GAP: capability identifiers (e.g.
                // `let c = FullAccess;`) must resolve in typed bodies. Register
                // the capability as a catalog constant so the Ident arm of
                // typed-body lowering resolves it to Constant(capability node).
                // The value is Unit: interp constructs a real cap via bytecode
                // NewCap from the raw AST, and codegen lowers Cap to i64 where
                // Unit materializes as const_zero (dummy handle).
                constants.insert(
                    node_id.clone(),
                    ResolvedConstant {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        ty: Some(format!("cap:{}", qualified)),
                        value: ResolvedConstValue::Unit,
                        extern_abi: None,
                        native_data_export: None,
                        origin: resolve_named_origin(
                            ResolvedItemKind::Constant,
                            &qualified,
                            &node_id,
                            cap.meta,
                            span,
                            errors,
                        ),
                    },
                );
            }
            Item::Trait(trait_def) => {
                let qualified = trait_def.name.clone();
                let span = declaration_span(trait_def.meta, trait_def.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Trait,
                    &qualified,
                    trait_def.meta,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("trait:{}", qualified));
                let methods = trait_def
                    .methods
                    .iter()
                    .map(|method| method.name.clone())
                    .collect();
                let mut method_owners = Vec::with_capacity(trait_def.methods.len());
                let mut method_signatures = Vec::new();
                for method in &trait_def.methods {
                    let signature =
                        method_signature_key(&method.name, &method.params, method.ret.as_ref());
                    method_owners.push(NodeId(format!(
                        "{}/method:{}:{:016x}",
                        node_id.0,
                        stable_id_fragment(&method.name),
                        stable_text_hash(&signature)
                    )));
                    for param in &method.params {
                        if contains_unresolved_type(&param.ty) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved type in trait '{}' method '{}' parameter",
                                    qualified,
                                    method.name
                                ),
                                span,
                            ));
                        }
                    }
                    if let Some(ret) = &method.ret {
                        if contains_unresolved_type(ret) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved return type in trait '{}' method '{}'",
                                    qualified,
                                    method.name
                                ),
                                span,
                            ));
                        }
                    }
                    method_signatures.push(ResolvedMethodSig {
                        name: method.name.clone(),
                        params: method
                            .params
                            .iter()
                            .map(|param| (param.name.clone(), crate::core::fmt_type(&param.ty)))
                            .collect(),
                        ret: method
                            .ret
                            .as_ref()
                            .map(crate::core::fmt_type)
                            .unwrap_or_else(|| "unit".into()),
                        effects: Vec::new(),
                    });
                }
                traits.insert(
                    node_id.clone(),
                    ResolvedTrait {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        methods,
                        method_owners,
                        method_signatures,
                        origin: resolve_named_origin(
                            ResolvedItemKind::Trait,
                            &qualified,
                            &node_id,
                            trait_def.meta,
                            span,
                            errors,
                        ),
                    },
                );
            }
            Item::Impl(impl_def) => {
                let qualified = impl_qualified_name(
                    &impl_def.trait_name,
                    &impl_def.trait_args,
                    &impl_def.type_name,
                );
                let span = declaration_span(impl_def.meta, impl_def.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Impl,
                    &qualified,
                    impl_def.meta,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("impl:{}", qualified));
                let impl_generic_binders =
                    callable_generic_binders(&impl_def.generics, &node_id, &ids, errors);
                let methods = impl_def
                    .methods
                    .iter()
                    .map(|method| method.name.clone())
                    .collect();
                let mut method_signatures = Vec::new();
                let mut method_owners = Vec::with_capacity(impl_def.methods.len());
                for method in &impl_def.methods {
                    let method_id = impl_method_owner(&qualified, method);
                    method_owners.push(method_id.clone());
                    let mut generic_binders = impl_generic_binders.clone();
                    generic_binders.extend(callable_generic_binders(
                        &method.generics,
                        &method_id,
                        &ids,
                        errors,
                    ));
                    let self_param = (!method
                        .params
                        .first()
                        .is_some_and(|param| param.name == "self"))
                    .then(|| {
                        implicit_self_param(
                            method.meta.span,
                            Type::Name(impl_def.type_name.clone(), impl_def.type_args.clone()),
                        )
                    });
                    if let Some(self_param) = &self_param {
                        insert_child_meta(
                            self_param.meta,
                            &method_id,
                            "decl.parameter",
                            "parameter.self",
                            span,
                            &ids,
                            node_meta,
                            errors,
                        );
                    }
                    for param in &method.params {
                        if contains_unresolved_type(&param.ty) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved type in impl '{}' method '{}' parameter",
                                    qualified,
                                    method.name
                                ),
                                span,
                            ));
                        }
                    }
                    if let Some(ret) = &method.ret {
                        if contains_unresolved_type(ret) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved return type in impl '{}' method '{}'",
                                    qualified,
                                    method.name
                                ),
                                span,
                            ));
                        }
                    }
                    method_signatures.push(ResolvedMethodSig {
                        name: method.name.clone(),
                        params: method
                            .params
                            .iter()
                            .map(|param| (param.name.clone(), crate::core::fmt_type(&param.ty)))
                            .collect(),
                        ret: method
                            .ret
                            .as_ref()
                            .map(crate::core::fmt_type)
                            .unwrap_or_else(|| "unit".into()),
                        effects: method.effects.clone(),
                    });
                    let mut param_decls = self_param.into_iter().collect::<Vec<_>>();
                    param_decls.extend(method.params.clone());
                    let params = param_decls
                        .iter()
                        .map(|param| (param.name.clone(), param.ty.clone()))
                        .collect();
                    functions.insert(
                        method_id.clone(),
                        ResolvedFunction {
                            node_id: method_id.clone(),
                            qualified_name: impl_method_key(
                                &impl_def.type_name,
                                &method.name,
                                &impl_def.trait_name,
                                &impl_def.trait_args,
                                &impl_def
                                    .generics
                                    .iter()
                                    .map(|g| g.name.clone())
                                    .collect::<Vec<_>>(),
                            ),
                            params,
                            param_decls,
                            ret: method
                                .ret
                                .clone()
                                .unwrap_or_else(|| Type::Name("unit".into(), Vec::new())),
                            effects: method.effects.clone(),
                            direct_effect_sources: BTreeMap::new(),
                            pub_: method.pub_,
                            is_comptime: method.is_comptime,
                            is_async: method.is_async,
                            extern_abi: method.extern_abi.clone(),
                            generics: method.generics.clone(),
                            generic_binders: generic_binders.clone(),
                            where_clause: method.where_clause.clone(),
                            origin: node_meta
                                .get(&method_id)
                                .map(|meta| meta.origin.clone())
                                .unwrap_or_else(|| Origin::User(method.meta.span)),
                        },
                    );
                    collect_nested_function_records(
                        &method.body,
                        &method_id,
                        &format!("{}_{}", impl_def.type_name, method.name),
                        (&ids, &generic_binders),
                        node_meta,
                        functions,
                        errors,
                    );
                }
                impls.insert(
                    node_id.clone(),
                    ResolvedImpl {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        trait_name: impl_def.trait_name.clone(),
                        type_name: impl_def.type_name.clone(),
                        generics: impl_def.generics.clone(),
                        generic_parameters: impl_generic_binders.clone(),
                        methods,
                        method_owners,
                        method_signatures,
                        origin: resolve_named_origin(
                            ResolvedItemKind::Impl,
                            &qualified,
                            &node_id,
                            impl_def.meta,
                            span,
                            errors,
                        ),
                    },
                );
            }
            Item::ExternBlock(block) => {
                let qualified = extern_block_key(block).clone();
                let span = declaration_span(block.meta, block.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::ExternBlock,
                    &qualified,
                    block.meta,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("extern:{}", qualified));
                let funcs = block.funcs.iter().map(|func| func.name.clone()).collect();
                let mut signatures = Vec::new();
                for func in &block.funcs {
                    let function_id = extern_function_owner(&node_id, func);
                    for param in &func.params {
                        if contains_unresolved_type(&param.ty) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved or erased type '{}' in extern function '{}' parameter",
                                    crate::core::fmt_type(&param.ty),
                                    func.name
                                ),
                                span,
                            ));
                        }
                    }
                    if let Some(ret) = &func.ret {
                        if contains_unresolved_type(ret) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved or erased return type '{}' in extern function '{}'",
                                    crate::core::fmt_type(ret),
                                    func.name
                                ),
                                span,
                            ));
                        }
                    }
                    signatures.push(ResolvedExternFunc {
                        node_id: function_id.clone(),
                        name: func.name.clone(),
                        span: func.meta.span,
                        params: func
                            .params
                            .iter()
                            .map(|param| (param.name.clone(), crate::core::fmt_type(&param.ty)))
                            .collect(),
                        typed_params: func
                            .params
                            .iter()
                            .map(|param| (param.name.clone(), param.ty.clone(), param.cap_mode))
                            .collect(),
                        parameter_ids: func
                            .params
                            .iter()
                            .map(|param| {
                                ids.anonymous(
                                    &function_id,
                                    "decl.extern_parameter",
                                    &format!("parameter.{}", stable_id_fragment(&param.name)),
                                    usable_span(param.meta.span),
                                    param.meta.origin,
                                    errors,
                                )
                            })
                            .collect(),
                        ret: func
                            .ret
                            .as_ref()
                            .map(crate::core::fmt_type)
                            .unwrap_or_else(|| "unit".into()),
                        ret_type: func.ret.clone(),
                        requires: func.requires.clone(),
                        ensures: func.ensures.clone(),
                        variadic: func.variadic,
                        no_panic: func.no_panic,
                        returns_errno: func.returns_errno,
                        declared_returns_errno: block
                            .explicit_errno_funcs
                            .iter()
                            .any(|name| name == &func.name),
                    });
                }
                extern_blocks.insert(
                    node_id.clone(),
                    ResolvedExternBlock {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        abi: block.abi.clone(),
                        funcs,
                        signatures,
                        no_panic: block.no_panic,
                        unsafe_: block.unsafe_,
                        returns_errno: block.returns_errno,
                        origin: resolve_named_origin(
                            ResolvedItemKind::ExternBlock,
                            &qualified,
                            &node_id,
                            block.meta,
                            span,
                            errors,
                        ),
                    },
                );
            }

            Item::Actor(actor) => {
                let qualified = actor.name.clone();
                let span = declaration_span(actor.meta, actor.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Actor,
                    &qualified,
                    actor.meta,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("actor:{}", qualified));
                let fields = actor
                    .fields
                    .iter()
                    .map(|field| (field.name.clone(), field.ty.clone(), field.mut_))
                    .collect::<Vec<_>>();
                let field_ids = actor
                    .fields
                    .iter()
                    .map(|field| {
                        let field_id = ids.anonymous(
                            &node_id,
                            "decl.actor_field",
                            &format!("field.{}", stable_id_fragment(&field.name)),
                            usable_span(field.meta.span),
                            field.meta.origin,
                            errors,
                        );
                        (field.name.clone(), field_id)
                    })
                    .collect();
                for (name, ty, _) in &fields {
                    if contains_unresolved_type(ty) {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: unresolved or erased type '{}' in actor '{}' field '{}'",
                                crate::core::fmt_type(ty),
                                qualified,
                                name
                            ),
                            span,
                        ));
                    }
                }
                let mut methods = actor
                    .methods
                    .iter()
                    .map(|method| method.name.clone())
                    .collect::<Vec<_>>();
                let mut method_signatures = Vec::new();
                for method in &actor.methods {
                    let method_id = NodeId(format!("function:{qualified}::{}", method.name));
                    let generic_binders =
                        callable_generic_binders(&method.generics, &method_id, &ids, errors);
                    let self_param = (!method
                        .params
                        .first()
                        .is_some_and(|param| param.name == "self"))
                    .then(|| {
                        implicit_self_param(
                            method.meta.span,
                            Type::Name(actor.name.clone(), Vec::new()),
                        )
                    });
                    if let Some(self_param) = &self_param {
                        insert_child_meta(
                            self_param.meta,
                            &method_id,
                            "decl.parameter",
                            "parameter.self",
                            span,
                            &ids,
                            node_meta,
                            errors,
                        );
                    }
                    for param in &method.params {
                        if contains_unresolved_type(&param.ty) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved or erased type '{}' in actor '{}' method '{}' parameter",
                                    crate::core::fmt_type(&param.ty),
                                    qualified,
                                    method.name
                                ),
                                span,
                            ));
                        }
                    }
                    if let Some(ret) = &method.ret {
                        if contains_unresolved_type(ret) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved or erased return type '{}' in actor '{}' method '{}'",
                                    crate::core::fmt_type(ret),
                                    qualified,
                                    method.name
                                ),
                                span,
                            ));
                        }
                    }
                    method_signatures.push(ResolvedActorMethod {
                        name: method.name.clone(),
                        params: method
                            .params
                            .iter()
                            .map(|param| (param.name.clone(), crate::core::fmt_type(&param.ty)))
                            .collect(),
                        ret: method
                            .ret
                            .as_ref()
                            .map(crate::core::fmt_type)
                            .unwrap_or_else(|| "unit".into()),
                        effects: method.effects.clone(),
                    });
                    let mut param_decls = self_param.into_iter().collect::<Vec<_>>();
                    param_decls.extend(method.params.clone());
                    let params = param_decls
                        .iter()
                        .map(|param| (param.name.clone(), param.ty.clone()))
                        .collect();
                    functions.insert(
                        method_id.clone(),
                        ResolvedFunction {
                            node_id: method_id.clone(),
                            qualified_name: format!("{qualified}::{}", method.name),
                            params,
                            param_decls,
                            ret: method
                                .ret
                                .clone()
                                .unwrap_or_else(|| Type::Name("unit".into(), Vec::new())),
                            effects: method.effects.clone(),
                            direct_effect_sources: BTreeMap::new(),
                            pub_: method.pub_,
                            is_comptime: method.is_comptime,
                            is_async: method.is_async,
                            extern_abi: method.extern_abi.clone(),
                            generics: method.generics.clone(),
                            generic_binders: generic_binders.clone(),
                            where_clause: method.where_clause.clone(),
                            origin: node_meta
                                .get(&method_id)
                                .map(|meta| meta.origin.clone())
                                .unwrap_or_else(|| Origin::User(method.meta.span)),
                        },
                    );
                    collect_nested_function_records(
                        &method.body,
                        &method_id,
                        &format!("{qualified}::{}", method.name),
                        (&ids, &generic_binders),
                        node_meta,
                        functions,
                        errors,
                    );
                }
                // 0.35.14 (DX backlog #13, layer ③): an actor that `runs` a
                // Flow dispatches messages through the transition table —
                // register each transition as a synthetic callable so the
                // resolved directory carries `function:{Actor}::{transition}`
                // identity and `lower_method_call` stops failing with "no
                // callable identity". Mirrors the checker/items.rs synthetic
                // signature: (self, event params…) -> ToState; with `fails E`
                // the return is Result<ToState, (FromState, E)>. Explicit
                // actor methods keep precedence on name collision.
                if let Some(flow_name) = actor.runs_flow.as_deref() {
                    if let Some(Item::Flow(flow)) = items
                        .iter()
                        .find(|item| matches!(item, Item::Flow(f) if f.name == flow_name))
                    {
                        for transition in &flow.transitions {
                            if methods.contains(&transition.name) {
                                continue;
                            }
                            let method_id =
                                NodeId(format!("function:{qualified}::{}", transition.name));
                            let self_param = implicit_self_param(
                                actor.meta.span,
                                Type::Name(actor.name.clone(), Vec::new()),
                            );
                            insert_child_meta(
                                self_param.meta,
                                &method_id,
                                "decl.parameter",
                                "parameter.self",
                                span,
                                &ids,
                                node_meta,
                                errors,
                            );
                            for param in &transition.params {
                                insert_child_meta(
                                    param.meta,
                                    &method_id,
                                    "decl.parameter",
                                    &format!("parameter.{}", stable_id_fragment(&param.name)),
                                    span,
                                    &ids,
                                    node_meta,
                                    errors,
                                );
                            }
                            let target = transition
                                .to_states
                                .first()
                                .map(|state| Type::Name(state.clone(), Vec::new()))
                                .unwrap_or_else(|| Type::Name("unit".into(), Vec::new()));
                            let ret = if let Some(err_ty) = &transition.fails {
                                let err_tuple = Type::Tuple(vec![
                                    Type::Name(transition.from_state.clone(), Vec::new()),
                                    err_ty.clone(),
                                ]);
                                Type::Result(Box::new(target), Box::new(err_tuple))
                            } else {
                                target
                            };
                            let mut param_decls = vec![self_param];
                            param_decls.extend(transition.params.clone());
                            let params = param_decls
                                .iter()
                                .map(|param| (param.name.clone(), param.ty.clone()))
                                .collect();
                            functions.insert(
                                method_id.clone(),
                                ResolvedFunction {
                                    node_id: method_id.clone(),
                                    qualified_name: format!("{qualified}::{}", transition.name),
                                    params,
                                    param_decls,
                                    ret: ret.clone(),
                                    effects: Vec::new(),
                                    direct_effect_sources: BTreeMap::new(),
                                    pub_: actor.pub_,
                                    is_comptime: false,
                                    is_async: false,
                                    extern_abi: None,
                                    generics: Vec::new(),
                                    generic_binders: Vec::new(),
                                    where_clause: Vec::new(),
                                    origin: Origin::Desugared {
                                        parent: node_id.clone(),
                                        rule: "resolved.runs_flow_transition_method".to_string(),
                                        span,
                                    },
                                },
                            );
                            methods.push(transition.name.clone());
                            method_signatures.push(ResolvedActorMethod {
                                name: transition.name.clone(),
                                params: std::iter::once(("self".to_string(), actor.name.clone()))
                                    .chain(transition.params.iter().map(|param| {
                                        (param.name.clone(), crate::core::fmt_type(&param.ty))
                                    }))
                                    .collect(),
                                ret: crate::core::fmt_type(&ret),
                                effects: Vec::new(),
                            });
                        }
                    }
                }
                actors.insert(
                    node_id.clone(),
                    ResolvedActor {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        fields,
                        field_ids,
                        methods,
                        method_signatures,
                        origin: resolve_named_origin(
                            ResolvedItemKind::Actor,
                            &qualified,
                            &node_id,
                            actor.meta,
                            span,
                            errors,
                        ),
                    },
                );
            }
            Item::Session(session) => {
                let qualified = session.name.clone();
                let span = declaration_span(session.meta, session.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Session,
                    &qualified,
                    session.meta,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("session:{}", qualified));
                sessions.insert(
                    node_id.clone(),
                    ResolvedSession {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        body: session.body.clone(),
                        body_display: format_session_type(&session.body),
                        origin: resolve_named_origin(
                            ResolvedItemKind::Session,
                            &qualified,
                            &node_id,
                            session.meta,
                            span,
                            errors,
                        ),
                    },
                );
            }
        }
    }
}

fn collect_nested_function_records(
    block: &[Stmt],
    owner: &NodeId,
    parent_qualified: &str,
    generic_context: (&NodeIdBuilder<'_>, &[(String, NodeId)]),
    node_meta: &HashMap<NodeId, NodeMeta>,
    functions: &mut HashMap<NodeId, ResolvedFunction>,
    errors: &mut Vec<Diagnostic>,
) {
    let (ids, inherited_generic_binders) = generic_context;
    for statement in block {
        match statement.unlocated() {
            Stmt::Func(function) => {
                let node_id = nested_function_owner(owner, function);
                let qualified_name = format!("{parent_qualified}::{}", function.name);
                let mut generic_binders = inherited_generic_binders.to_vec();
                generic_binders.extend(callable_generic_binders(
                    &function.generics,
                    &node_id,
                    ids,
                    errors,
                ));
                let params = function
                    .params
                    .iter()
                    .map(|parameter| (parameter.name.clone(), parameter.ty.clone()))
                    .collect::<Vec<_>>();
                let ret = function
                    .ret
                    .clone()
                    .unwrap_or_else(|| Type::Name("unit".into(), Vec::new()));
                let record = ResolvedFunction {
                    node_id: node_id.clone(),
                    qualified_name: qualified_name.clone(),
                    params,
                    param_decls: function.params.clone(),
                    ret,
                    effects: function.effects.clone(),
                    direct_effect_sources: BTreeMap::new(),
                    pub_: function.pub_,
                    is_comptime: function.is_comptime,
                    is_async: function.is_async,
                    extern_abi: function.extern_abi.clone(),
                    generics: function.generics.clone(),
                    generic_binders: generic_binders.clone(),
                    where_clause: function.where_clause.clone(),
                    origin: node_meta
                        .get(&node_id)
                        .map(|meta| meta.origin.clone())
                        .unwrap_or_else(|| Origin::User(function.meta.span)),
                };
                if functions.insert(node_id.clone(), record).is_some() {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: duplicate nested callable identity '{}'",
                            node_id.0
                        ),
                        function.meta.span,
                    ));
                }
                collect_nested_function_records(
                    &function.body,
                    &node_id,
                    &qualified_name,
                    (ids, &generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            Stmt::If { cond, then_, else_ } => {
                collect_nested_function_records_in_expr(
                    cond,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                collect_nested_function_records(
                    then_,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                if let Some(else_) = else_ {
                    collect_nested_function_records(
                        else_,
                        owner,
                        parent_qualified,
                        (ids, inherited_generic_binders),
                        node_meta,
                        functions,
                        errors,
                    );
                }
            }
            Stmt::IfLet {
                init, then_, else_, ..
            } => {
                collect_nested_function_records_in_expr(
                    init,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                collect_nested_function_records(
                    then_,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                if let Some(else_) = else_ {
                    collect_nested_function_records(
                        else_,
                        owner,
                        parent_qualified,
                        (ids, inherited_generic_binders),
                        node_meta,
                        functions,
                        errors,
                    );
                }
            }
            Stmt::While { cond, body } => {
                collect_nested_function_records_in_expr(
                    cond,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                collect_nested_function_records(
                    body,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            Stmt::WhileLet { init, body, .. } => {
                collect_nested_function_records_in_expr(
                    init,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                collect_nested_function_records(
                    body,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            Stmt::For { iterable, body, .. } => {
                collect_nested_function_records_in_expr(
                    iterable,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                collect_nested_function_records(
                    body,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            Stmt::Loop(body)
            | Stmt::Block(body)
            | Stmt::Arena(body)
            | Stmt::Unsafe(body)
            | Stmt::IeeeFloat(body)
            | Stmt::OnFailure(body)
            | Stmt::Parasteps(body)
            | Stmt::Defer(body) => collect_nested_function_records(
                body,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            ),
            Stmt::Pinned { expr, body, .. } => {
                collect_nested_function_records_in_expr(
                    expr,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                collect_nested_function_records(
                    body,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            // Full audit 2026-08-05 (#10): expression positions can carry
            // blocks/lambdas containing nested function declarations. The
            // meta walk records those nodes; the catalog must too, or the
            // bodies dangle. Mirrors collect_nested_function_syntax
            // (ir/lower.rs), which keeps the syntax map in the same shape.
            Stmt::Let {
                init: Some(init), ..
            }
            | Stmt::SharedLet { init, .. } => {
                collect_nested_function_records_in_expr(
                    init,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            Stmt::Expr(expr)
            | Stmt::Return(Some(expr))
            | Stmt::Break(Some(expr))
            | Stmt::Drop(expr) => collect_nested_function_records_in_expr(
                expr,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            ),
            Stmt::Assign { target, value } => {
                collect_nested_function_records_in_expr(
                    target,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                collect_nested_function_records_in_expr(
                    value,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            Stmt::Requires(expr, _) | Stmt::Ensures(expr, _) | Stmt::Invariant(expr, _) => {
                collect_nested_function_records_in_expr(
                    expr,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            Stmt::Math(expressions) => {
                for expression in expressions {
                    collect_nested_function_records_in_expr(
                        expression,
                        owner,
                        parent_qualified,
                        (ids, inherited_generic_binders),
                        node_meta,
                        functions,
                        errors,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Full audit 2026-08-05 (#10): expression-tree walker pairing with
/// `collect_nested_function_records`. Descends every sub-expression and
/// collects statement-level function declarations from embedded blocks
/// (block expressions, lambda bodies, `if`/`arena` block expressions).
/// Quote and Comptime interiors are deliberately skipped: the checker never
/// finalizes nested function declarations there (`infer_expr` types a quote
/// as an opaque AST value; `infer_comptime` ignores `Stmt::Func`), so a
/// catalog record would have no checker-finalized signature (fail-closed
/// contract — records and finalized signatures must pair).
fn collect_nested_function_records_in_expr(
    expr: &Expr,
    owner: &NodeId,
    parent_qualified: &str,
    generic_context: (&NodeIdBuilder<'_>, &[(String, NodeId)]),
    node_meta: &HashMap<NodeId, NodeMeta>,
    functions: &mut HashMap<NodeId, ResolvedFunction>,
    errors: &mut Vec<Diagnostic>,
) {
    let (ids, inherited_generic_binders) = generic_context;
    match expr.unlocated() {
        Expr::Block(body) | Expr::Arena(body) => collect_nested_function_records(
            body,
            owner,
            parent_qualified,
            (ids, inherited_generic_binders),
            node_meta,
            functions,
            errors,
        ),
        Expr::If { cond, then_, else_ } => {
            collect_nested_function_records_in_expr(
                cond,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
            collect_nested_function_records(
                then_,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
            if let Some(else_) = else_ {
                collect_nested_function_records(
                    else_,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
        }
        Expr::Lambda { body, .. } => collect_nested_function_records(
            body,
            owner,
            parent_qualified,
            (ids, inherited_generic_binders),
            node_meta,
            functions,
            errors,
        ),
        Expr::Binary(_, left, right) => {
            collect_nested_function_records_in_expr(
                left,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
            collect_nested_function_records_in_expr(
                right,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
        }
        Expr::Unary(_, operand)
        | Expr::Try(operand)
        | Expr::OptionalChain(operand, _)
        | Expr::Spawn(operand)
        | Expr::Await(operand)
        | Expr::TypeOf(operand)
        | Expr::Old(operand)
        | Expr::QuoteInterpolate(operand)
        | Expr::NamedArg(_, operand)
        | Expr::Cast(operand, _) => collect_nested_function_records_in_expr(
            operand,
            owner,
            parent_qualified,
            (ids, inherited_generic_binders),
            node_meta,
            functions,
            errors,
        ),
        Expr::Call(callee, arguments) => {
            collect_nested_function_records_in_expr(
                callee,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
            for argument in arguments {
                collect_nested_function_records_in_expr(
                    argument,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
        }
        Expr::Field(base, _) | Expr::TupleIndex(base, _) => {
            collect_nested_function_records_in_expr(
                base,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
        }
        Expr::Index(base, index) => {
            collect_nested_function_records_in_expr(
                base,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
            collect_nested_function_records_in_expr(
                index,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
        }
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            for item in items {
                collect_nested_function_records_in_expr(
                    item,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            collect_nested_function_records_in_expr(
                expr,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
            collect_nested_function_records_in_expr(
                iter,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
            if let Some(guard) = guard {
                collect_nested_function_records_in_expr(
                    guard,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
        }
        Expr::Match(scrutinee, arms) => {
            collect_nested_function_records_in_expr(
                scrutinee,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_nested_function_records_in_expr(
                        guard,
                        owner,
                        parent_qualified,
                        (ids, inherited_generic_binders),
                        node_meta,
                        functions,
                        errors,
                    );
                }
                collect_nested_function_records_in_expr(
                    &arm.body,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
        }
        Expr::Record { fields, rest, .. } => {
            for field in fields {
                collect_nested_function_records_in_expr(
                    &field.value,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            if let Some(rest) = rest {
                collect_nested_function_records_in_expr(
                    rest,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
        }
        Expr::SliceExpr { target, start, end } => {
            collect_nested_function_records_in_expr(
                target,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            );
            if let Some(start) = start {
                collect_nested_function_records_in_expr(
                    start,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
            if let Some(end) = end {
                collect_nested_function_records_in_expr(
                    end,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
        }
        Expr::Turbofish(_, _, arguments) => {
            for argument in arguments {
                collect_nested_function_records_in_expr(
                    argument,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
        }
        Expr::MapLiteral { entries } => {
            for (key, value) in entries {
                collect_nested_function_records_in_expr(
                    key,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
                collect_nested_function_records_in_expr(
                    value,
                    owner,
                    parent_qualified,
                    (ids, inherited_generic_binders),
                    node_meta,
                    functions,
                    errors,
                );
            }
        }
        // See the doc comment: checker never finalizes nested declarations
        // inside these interiors.
        Expr::Quote(_) | Expr::Comptime(_) => {}
        Expr::Literal(_) | Expr::Ident(_) | Expr::TypeInfo(_) => {}
        Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
    }
}

fn insert_item(
    items: &mut HashMap<NodeId, ResolvedItem>,
    kind: ResolvedItemKind,
    qualified_name: &str,
    meta: AstNodeMeta,
    span: Span,
    errors: &mut Vec<Diagnostic>,
) {
    let kind_name = match kind {
        ResolvedItemKind::Function => "function",
        ResolvedItemKind::Type => "type",
        ResolvedItemKind::Constant => "const",
        ResolvedItemKind::Capability => "capability",
        ResolvedItemKind::Trait => "trait",
        ResolvedItemKind::Impl => "impl",
        ResolvedItemKind::ExternBlock => "extern",
        ResolvedItemKind::Actor => "actor",
        ResolvedItemKind::Flow => "flow",
        ResolvedItemKind::Session => "session",
    };
    let node_id = NodeId(format!("{}:{}", kind_name, qualified_name));
    let item_origin = resolve_named_origin(kind, qualified_name, &node_id, meta, span, errors);
    let item = ResolvedItem {
        node_id: node_id.clone(),
        qualified_name: qualified_name.to_string(),
        kind,
        origin: item_origin,
    };
    if items.insert(node_id, item).is_some() {
        errors.push(Diagnostic::error(
            format!(
                "TOOL-RESOLUTION-001: duplicate canonical {} '{}'",
                kind_name, qualified_name
            ),
            span,
        ));
    }
}

fn collect_block_meta(
    block: &[Stmt],
    owner: &NodeId,
    context: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    for (index, stmt) in block.iter().enumerate() {
        let role = stmt_sibling_role(context, block, index);
        collect_stmt_meta(stmt, owner, &role, fallback, ids, out, errors);
    }
}

pub(crate) fn stmt_sibling_roles(context: &str, block: &[Stmt]) -> Vec<String> {
    semantic_sibling_roles(&format!("{context}.statement"), block, stmt_semantic_key)
}

pub(crate) fn stmt_sibling_role(context: &str, block: &[Stmt], index: usize) -> String {
    semantic_sibling_role(
        &format!("{context}.statement"),
        block,
        index,
        stmt_semantic_key,
    )
}

pub(crate) fn expr_sibling_roles(context: &str, exprs: &[Expr]) -> Vec<String> {
    semantic_sibling_roles(context, exprs, expr_semantic_key)
}

pub(crate) fn expr_sibling_role(context: &str, exprs: &[Expr], index: usize) -> String {
    semantic_sibling_role(context, exprs, index, expr_semantic_key)
}

pub(crate) fn pattern_sibling_roles(context: &str, patterns: &[Pattern]) -> Vec<String> {
    semantic_sibling_roles(context, patterns, pattern_semantic_key)
}

pub(crate) fn type_sibling_roles(context: &str, types: &[Type]) -> Vec<String> {
    semantic_sibling_roles(context, types, type_semantic_key)
}

fn match_arm_semantic_key(arm: &crate::ast::MatchArm) -> String {
    format!(
        "{}|guard:{}|body:{}",
        pattern_semantic_key(&arm.pat),
        arm.guard
            .as_ref()
            .map(expr_semantic_key)
            .unwrap_or_default(),
        expr_semantic_key(&arm.body)
    )
}

pub(crate) fn match_arm_roles(context: &str, arms: &[crate::ast::MatchArm]) -> Vec<String> {
    semantic_sibling_roles(context, arms, match_arm_semantic_key)
}

fn map_entry_semantic_key(entry: &(Expr, Expr)) -> String {
    format!(
        "{}=>{}",
        expr_semantic_key(&entry.0),
        expr_semantic_key(&entry.1)
    )
}

pub(crate) fn map_entry_roles(context: &str, entries: &[(Expr, Expr)]) -> Vec<String> {
    semantic_sibling_roles(context, entries, map_entry_semantic_key)
}

pub(crate) fn interpolation_role(
    context: &str,
    parts: &[FStringPart],
    part_index: usize,
) -> String {
    let FStringPart::Interp(expr) = &parts[part_index] else {
        unreachable!("interpolation role requested for text")
    };
    let key = expr_semantic_key(expr);
    let occurrence = parts[..part_index]
        .iter()
        .filter_map(|part| match part {
            FStringPart::Interp(expr) => Some(expr_semantic_key(expr)),
            FStringPart::Text(_) => None,
        })
        .filter(|candidate| candidate == &key)
        .count();
    format!(
        "{}.{:016x}.same.{}",
        context,
        stable_text_hash(&key),
        occurrence
    )
}

/// 0.39.136 perf (check-phase O(n²) → O(n)): sibling roles used to recompute
/// every PREVIOUS sibling's semantic key for each element (n²/2 String
/// allocations — a 20k-element list literal spent ~6 s here alone). The batch
/// form computes each key exactly once and counts occurrences incrementally.
fn semantic_sibling_roles<T>(
    context: &str,
    values: &[T],
    key_of: impl Fn(&T) -> String,
) -> Vec<String> {
    let mut occurrence: HashMap<String, usize> = HashMap::with_capacity(values.len());
    values
        .iter()
        .map(|value| {
            let key = key_of(value);
            let occ = {
                let slot = occurrence.entry(key.clone()).or_insert(0);
                let current = *slot;
                *slot += 1;
                current
            };
            format!("{}.{:016x}.same.{}", context, stable_text_hash(&key), occ)
        })
        .collect()
}

fn semantic_sibling_role<T>(
    context: &str,
    values: &[T],
    index: usize,
    key_of: impl Fn(&T) -> String,
) -> String {
    semantic_sibling_roles(context, values, key_of).swap_remove(index)
}

pub(crate) fn stable_text_hash(value: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    value.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

const COMPILATION_ROOT_NODE_ID: &str = "compilation:root";

fn declaration_span(meta: crate::ast::AstNodeMeta, fallback: Span) -> Span {
    usable_span(meta.span).unwrap_or(fallback)
}

/// 追加 A: Detect cross-boundary operations in a statement block.
/// Returns true if the block contains any of:
/// - Channel send (session_send, channel_send)
/// - FFI call (extern function calls)
/// - Actor event emit (emit, send_event)
/// Used to determine if a transition can be "silent" (no Generation increment).
/// §4-#39 (closed 0.36.76): Defer / OnFailure / Parasteps bodies are included
/// in the statement scan below, so a cross-boundary op inside those wrappers
/// also prevents a silent transition.
fn has_cross_boundary_ops(stmts: &[crate::ast::Stmt]) -> bool {
    use crate::ast::{Expr, Stmt};

    fn expr_has_cross_boundary(expr: &Expr) -> bool {
        match expr.unlocated() {
            Expr::Located { expr, .. } => expr_has_cross_boundary(expr),
            Expr::Call(callee, args) => {
                // Check if callee is a cross-boundary function
                if let Expr::Ident(name) = callee.unlocated() {
                    let cross_boundary_fns = [
                        "session_send",
                        "session_recv",
                        "session_close",
                        "channel_send",
                        "channel_recv",
                        "flow_pack",
                        "emit",
                        "send_event",
                        "spawn_actor",
                    ];
                    if cross_boundary_fns.contains(&name.as_str()) {
                        return true;
                    }
                }
                // Recurse into callee and args
                expr_has_cross_boundary(callee) || args.iter().any(expr_has_cross_boundary)
            }
            Expr::Binary(_, l, r) => expr_has_cross_boundary(l) || expr_has_cross_boundary(r),
            Expr::Unary(_, e) | Expr::Try(e) | Expr::Await(e) | Expr::Spawn(e) => {
                expr_has_cross_boundary(e)
            }
            Expr::Block(stmts) => stmts.iter().any(stmt_has_cross_boundary),
            Expr::If { cond, then_, else_ } => {
                expr_has_cross_boundary(cond)
                    || then_.iter().any(stmt_has_cross_boundary)
                    || else_
                        .as_ref()
                        .map(|b| b.iter().any(stmt_has_cross_boundary))
                        .unwrap_or(false)
            }
            Expr::Match(scrutinee, arms) => {
                expr_has_cross_boundary(scrutinee)
                    || arms.iter().any(|arm| {
                        arm.guard
                            .as_ref()
                            .map(expr_has_cross_boundary)
                            .unwrap_or(false)
                            || expr_has_cross_boundary(&arm.body)
                    })
            }
            Expr::Lambda { body, .. } => body.iter().any(stmt_has_cross_boundary),
            Expr::Comprehension {
                expr, iter, guard, ..
            } => {
                expr_has_cross_boundary(expr)
                    || expr_has_cross_boundary(iter)
                    || guard
                        .as_ref()
                        .map(|g| expr_has_cross_boundary(g))
                        .unwrap_or(false)
            }
            _ => false,
        }
    }

    fn stmt_has_cross_boundary(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Located { stmt, .. } => stmt_has_cross_boundary(stmt),
            Stmt::Expr(e) => expr_has_cross_boundary(e),
            Stmt::Return(Some(e)) | Stmt::Break(Some(e)) => expr_has_cross_boundary(e),
            Stmt::Let { init: Some(e), .. } => expr_has_cross_boundary(e),
            Stmt::Assign { target, value } => {
                expr_has_cross_boundary(target) || expr_has_cross_boundary(value)
            }
            Stmt::If { cond, then_, else_ } => {
                expr_has_cross_boundary(cond)
                    || then_.iter().any(stmt_has_cross_boundary)
                    || else_
                        .as_ref()
                        .map(|b| b.iter().any(stmt_has_cross_boundary))
                        .unwrap_or(false)
            }
            Stmt::IfLet {
                init, then_, else_, ..
            } => {
                expr_has_cross_boundary(init)
                    || then_.iter().any(stmt_has_cross_boundary)
                    || else_
                        .as_ref()
                        .map(|b| b.iter().any(stmt_has_cross_boundary))
                        .unwrap_or(false)
            }
            Stmt::While { cond, body } => {
                expr_has_cross_boundary(cond) || body.iter().any(stmt_has_cross_boundary)
            }
            Stmt::WhileLet { init, body, .. } => {
                expr_has_cross_boundary(init) || body.iter().any(stmt_has_cross_boundary)
            }
            Stmt::Loop(body) => body.iter().any(stmt_has_cross_boundary),
            Stmt::For { iterable, body, .. } => {
                expr_has_cross_boundary(iterable) || body.iter().any(stmt_has_cross_boundary)
            }
            Stmt::Block(stmts)
            | Stmt::Arena(stmts)
            | Stmt::Unsafe(stmts)
            | Stmt::IeeeFloat(stmts)
            | Stmt::Defer(stmts)
            | Stmt::OnFailure(stmts)
            | Stmt::Parasteps(stmts) => stmts.iter().any(stmt_has_cross_boundary),
            Stmt::Pinned { expr, body, .. } => {
                expr_has_cross_boundary(expr) || body.iter().any(stmt_has_cross_boundary)
            }
            Stmt::Drop(e) => expr_has_cross_boundary(e),
            _ => false,
        }
    }

    stmts.iter().any(stmt_has_cross_boundary)
}

fn top_level_enclosing_parent(qualified_name: &str) -> NodeId {
    qualified_name
        .rsplit_once("::")
        .map(|(module, _)| NodeId(format!("module:{module}")))
        .unwrap_or_else(|| NodeId(COMPILATION_ROOT_NODE_ID.to_string()))
}

fn named_function_parent(name: &str, qualified_scope: Option<&str>) -> NodeId {
    let qualified = if name.contains("::") {
        name.to_string()
    } else if let Some((module, _)) = qualified_scope.and_then(|scope| scope.rsplit_once("::")) {
        format!("{module}::{name}")
    } else {
        name.to_string()
    };
    NodeId(format!("function:{qualified}"))
}

fn explicit_origin_parent(
    node_id: &NodeId,
    origin: AstOrigin,
    hint: AstParentHint,
    enclosing: &NodeId,
    qualified_scope: Option<&str>,
    span: Span,
    errors: &mut Vec<Diagnostic>,
) -> NodeId {
    if origin == AstOrigin::User {
        if hint != AstParentHint::None {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: user NodeId '{}' must not declare a generated AST parent hint",
                    node_id.0
                ),
                span,
            ));
        }
        return enclosing.clone();
    }

    match hint {
        AstParentHint::None => {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: generated NodeId '{}' is missing an explicit AST parent hint",
                    node_id.0
                ),
                span,
            ));
            enclosing.clone()
        }
        AstParentHint::Enclosing => enclosing.clone(),
        AstParentHint::NamedFunction(name) => named_function_parent(name, qualified_scope),
        AstParentHint::CompilationRoot => NodeId(COMPILATION_ROOT_NODE_ID.to_string()),
    }
}

fn resolve_named_origin(
    _kind: ResolvedItemKind,
    qualified_name: &str,
    node_id: &NodeId,
    meta: AstNodeMeta,
    span: Span,
    errors: &mut Vec<Diagnostic>,
) -> Origin {
    let enclosing = top_level_enclosing_parent(qualified_name);
    let parent = explicit_origin_parent(
        node_id,
        meta.origin,
        meta.parent,
        &enclosing,
        Some(qualified_name),
        span,
        errors,
    );
    resolve_origin(meta.origin, &parent, span)
}

fn resolve_enclosed_origin(
    node_id: &NodeId,
    meta: AstNodeMeta,
    enclosing: &NodeId,
    span: Span,
    errors: &mut Vec<Diagnostic>,
) -> Origin {
    let parent = explicit_origin_parent(
        node_id,
        meta.origin,
        meta.parent,
        enclosing,
        None,
        span,
        errors,
    );
    resolve_origin(meta.origin, &parent, span)
}

fn insert_canonical_meta(
    node_id: NodeId,
    _kind: ResolvedItemKind,
    qualified_name: &str,
    meta: crate::ast::AstNodeMeta,
    fallback: Span,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let parent = top_level_enclosing_parent(qualified_name);
    insert_node_meta(
        node_id,
        meta.origin,
        meta.parent,
        ast_meta_anchor(meta),
        fallback,
        &parent,
        Some(qualified_name),
        out,
        errors,
    );
}

fn insert_child_meta(
    meta: crate::ast::AstNodeMeta,
    owner: &NodeId,
    kind: &str,
    role: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) -> NodeId {
    let anchor = ast_meta_anchor(meta);
    let node_id = ids.anonymous(
        owner,
        kind,
        role,
        anchor.map(|(span, _)| span),
        meta.origin,
        errors,
    );
    insert_node_meta(
        node_id.clone(),
        meta.origin,
        meta.parent,
        anchor,
        fallback,
        owner,
        None,
        out,
        errors,
    );
    node_id
}

fn type_semantic_key(ty: &Type) -> String {
    crate::core::fmt_type(ty)
}

pub(crate) fn type_kind(ty: &Type) -> &'static str {
    match ty.unlocated() {
        Type::Name(_, _) => "type.name",
        Type::Ref(_, _) => "type.ref",
        Type::RefMut(_, _) => "type.ref_mut",
        Type::Option(_) => "type.option",
        Type::Result(_, _) => "type.result",
        Type::Tuple(_) => "type.tuple",
        Type::Func(_, _) => "type.function",
        Type::ExternFunc(_, _) => "type.extern_function",
        Type::CBuffer(_) => "type.c_buffer",
        Type::Cap(_) | Type::CapAtom(_) => "type.capability",
        Type::Shared(_) => "type.shared",
        Type::Weak(_) => "type.weak",
        Type::Newtype(_, _) => "type.newtype",
        Type::Nothing => "type.nothing",
        Type::TyErr => "type.error",
        Type::Array(_, _) => "type.array",
        Type::Slice(_) => "type.slice",
        Type::ImplTrait(_) => "type.impl_trait",
        Type::DynTrait(_) => "type.dyn_trait",
        Type::RawPtr(_) => "type.raw_ptr",
        Type::RawPtrMut(_) => "type.raw_ptr_mut",
        Type::Infer => "type.infer",
        Type::TypeVar(_) => "type.variable",
        Type::ForAll(_, _) => "type.for_all",
        Type::Located { .. } => unreachable!("Type::unlocated returned Located"),
    }
}

fn collect_type_meta(
    ty: &Type,
    owner: &NodeId,
    role: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let meta = ty.meta();
    let ast_origin = meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User);
    let anchor = meta.and_then(ast_meta_anchor);
    let node_id = ids.anonymous(
        owner,
        type_kind(ty),
        role,
        anchor.map(|(span, _)| span),
        ast_origin,
        errors,
    );
    insert_node_meta(
        node_id.clone(),
        ast_origin,
        meta.map(|meta| meta.parent).unwrap_or(AstParentHint::None),
        anchor,
        fallback,
        owner,
        None,
        out,
        errors,
    );
    if let Some(node_meta) = out.get_mut(&node_id) {
        if node_meta.type_operand.replace(ty.clone()).is_some() {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: type NodeId '{}' has more than one canonical operand",
                    node_id.0
                ),
                anchor.map(|(span, _)| span).unwrap_or(fallback),
            ));
        }
    }
    match ty.unlocated() {
        Type::Name(_, args) => {
            let child_roles = type_sibling_roles(&format!("{role}.argument"), args);
            for index in 0..args.len() {
                let child_role = &child_roles[index];
                collect_type_meta(
                    &args[index],
                    &node_id,
                    child_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Type::Ref(_, inner)
        | Type::RefMut(_, inner)
        | Type::Option(inner)
        | Type::CBuffer(inner)
        | Type::Shared(inner)
        | Type::Weak(inner)
        | Type::Newtype(_, inner)
        | Type::Slice(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::Array(inner, _)
        | Type::ForAll(_, inner) => collect_type_meta(
            inner,
            &node_id,
            &format!("{role}.inner"),
            fallback,
            ids,
            out,
            errors,
        ),
        Type::Result(ok, err) => {
            collect_type_meta(
                ok,
                &node_id,
                &format!("{role}.ok"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_type_meta(
                err,
                &node_id,
                &format!("{role}.error"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Type::Tuple(items) => {
            let child_roles = type_sibling_roles(&format!("{role}.element"), items);
            for index in 0..items.len() {
                let child_role = &child_roles[index];
                collect_type_meta(
                    &items[index],
                    &node_id,
                    child_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Type::Func(params, ret) | Type::ExternFunc(params, ret) => {
            let child_roles = type_sibling_roles(&format!("{role}.parameter"), params);
            for index in 0..params.len() {
                let child_role = &child_roles[index];
                collect_type_meta(
                    &params[index],
                    &node_id,
                    child_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
            collect_type_meta(
                ret,
                &node_id,
                &format!("{role}.return"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Type::Cap(_)
        | Type::CapAtom(_)
        | Type::Nothing
        | Type::ImplTrait(_)
        | Type::DynTrait(_)
        | Type::Infer
        | Type::TyErr
        | Type::TypeVar(_) => {}
        Type::Located { .. } => unreachable!("Type::unlocated returned Located"),
    }
}

fn collect_generic_param_meta(
    param: &crate::ast::GenericParam,
    owner: &NodeId,
    context: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    insert_child_meta(
        param.meta,
        owner,
        "decl.generic_parameter",
        &format!("{context}.{}", stable_id_fragment(&param.name)),
        fallback,
        ids,
        out,
        errors,
    );
}

fn callable_generic_binders(
    generics: &[crate::ast::GenericParam],
    owner: &NodeId,
    ids: &NodeIdBuilder<'_>,
    errors: &mut Vec<Diagnostic>,
) -> Vec<(String, NodeId)> {
    generics
        .iter()
        .map(|generic| {
            let id = ids.anonymous(
                owner,
                "decl.generic_parameter",
                &format!("generic.{}", stable_id_fragment(&generic.name)),
                ast_meta_anchor(generic.meta).map(|(span, _)| span),
                generic.meta.origin,
                errors,
            );
            (generic.name.clone(), id)
        })
        .collect()
}

fn collect_param_meta(
    param: &crate::ast::Param,
    owner: &NodeId,
    context: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let role = format!("{context}.{}", stable_id_fragment(&param.name));
    let param_id = insert_child_meta(
        param.meta,
        owner,
        "decl.parameter",
        &role,
        fallback,
        ids,
        out,
        errors,
    );
    collect_type_meta(&param.ty, &param_id, "type", fallback, ids, out, errors);
    if let Some(default) = &param.default_value {
        collect_expr_meta(
            default,
            owner,
            &format!("{role}.default"),
            fallback,
            ids,
            out,
            errors,
        );
    }
}

fn collect_where_clause_meta(
    clause: &crate::ast::WhereClause,
    owner: &NodeId,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    insert_child_meta(
        clause.meta,
        owner,
        "decl.where_clause",
        &format!("where.{}", stable_id_fragment(&clause.type_param)),
        fallback,
        ids,
        out,
        errors,
    );
}

fn collect_func_meta(
    function: &crate::ast::FuncDef,
    node_id: NodeId,
    parent: &NodeId,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let span = declaration_span(function.meta, fallback);
    insert_node_meta(
        node_id.clone(),
        function.meta.origin,
        function.meta.parent,
        ast_meta_anchor(function.meta),
        fallback,
        parent,
        None,
        out,
        errors,
    );
    for generic in &function.generics {
        collect_generic_param_meta(generic, &node_id, "generic", span, ids, out, errors);
    }
    for param in &function.params {
        collect_param_meta(param, &node_id, "parameter", span, ids, out, errors);
    }
    if let Some(ret) = &function.ret {
        collect_type_meta(ret, &node_id, "return_type", span, ids, out, errors);
    }
    for clause in &function.where_clause {
        collect_where_clause_meta(clause, &node_id, span, ids, out, errors);
    }
    collect_block_meta(&function.body, &node_id, "body", span, ids, out, errors);
}

fn collect_field_meta(
    field: &crate::ast::Field,
    owner: &NodeId,
    context: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let role = format!("{context}.{}", stable_id_fragment(&field.name));
    let field_id = insert_child_meta(
        field.meta,
        owner,
        "decl.field",
        &role,
        fallback,
        ids,
        out,
        errors,
    );
    collect_type_meta(&field.ty, &field_id, "type", fallback, ids, out, errors);
}

fn session_kind(session: &crate::ast::SessionType) -> &'static str {
    match session.unlocated() {
        crate::ast::SessionType::Send(_, _) => "session.send",
        crate::ast::SessionType::Recv(_, _) => "session.recv",
        crate::ast::SessionType::Dual(_) => "session.dual",
        crate::ast::SessionType::Name(_) => "session.name",
        crate::ast::SessionType::End => "session.end",
        crate::ast::SessionType::Located { .. } => {
            unreachable!("SessionType::unlocated returned Located")
        }
    }
}

fn collect_session_type_meta(
    session: &crate::ast::SessionType,
    owner: &NodeId,
    role: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let meta = session.meta();
    let ast_origin = meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User);
    let anchor = meta.and_then(ast_meta_anchor);
    let node_id = ids.anonymous(
        owner,
        session_kind(session),
        role,
        anchor.map(|(span, _)| span),
        ast_origin,
        errors,
    );
    insert_node_meta(
        node_id.clone(),
        ast_origin,
        meta.map(|meta| meta.parent).unwrap_or(AstParentHint::None),
        anchor,
        fallback,
        owner,
        None,
        out,
        errors,
    );
    match session.unlocated() {
        crate::ast::SessionType::Send(payload, continuation)
        | crate::ast::SessionType::Recv(payload, continuation) => {
            collect_type_meta(
                payload,
                &node_id,
                &format!("{role}.payload"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_session_type_meta(
                continuation,
                &node_id,
                &format!("{role}.continuation"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        crate::ast::SessionType::Dual(inner) => collect_session_type_meta(
            inner,
            &node_id,
            &format!("{role}.inner"),
            fallback,
            ids,
            out,
            errors,
        ),
        crate::ast::SessionType::Name(_) | crate::ast::SessionType::End => {}
        crate::ast::SessionType::Located { .. } => {
            unreachable!("SessionType::unlocated returned Located")
        }
    }
}

pub(crate) fn method_signature_key(
    name: &str,
    params: &[crate::ast::Param],
    ret: Option<&Type>,
) -> String {
    format!(
        "{}({})->{}",
        name,
        params
            .iter()
            .map(|param| crate::core::fmt_type(&param.ty))
            .collect::<Vec<_>>()
            .join(","),
        ret.map(crate::core::fmt_type)
            .unwrap_or_else(|| "unit".to_string())
    )
}

fn implicit_self_param(span: Span, ty: Type) -> crate::ast::Param {
    crate::ast::Param {
        meta: AstNodeMeta::inherited(
            span,
            AstOrigin::Desugared("normalization.implicit_self_parameter"),
        ),
        name: "self".into(),
        ty,
        mut_: true,
        default_value: None,
        borrow: Some(crate::ast::ParamBorrow::Mutate),
    }
}

fn extern_function_signature_key(function: &crate::ast::ExternFunc) -> String {
    format!(
        "{}({})->{}",
        function.name,
        function
            .params
            .iter()
            .map(|param| crate::core::fmt_type(&param.ty))
            .collect::<Vec<_>>()
            .join(","),
        function
            .ret
            .as_ref()
            .map(crate::core::fmt_type)
            .unwrap_or_else(|| "unit".to_string())
    )
}

pub(crate) fn extern_function_owner(
    block_owner: &NodeId,
    function: &crate::ast::ExternFunc,
) -> NodeId {
    NodeId(format!(
        "{}/function:{}:{:016x}",
        block_owner.0,
        stable_id_fragment(&function.name),
        stable_text_hash(&extern_function_signature_key(function))
    ))
}

/// Format a checker/resolved method-slot key.
///
/// Ordinary impl methods keep the historical `Type_method` key.  Impls with
/// concrete trait arguments (e.g. `From<FsError, AppError>` for AppError)
/// append a stable trait-key suffix so multiple `from` conversion impls
/// targeting the same type do not silently overwrite each other's
/// checker-finalized signatures.
pub(crate) fn impl_method_key(
    type_name: &str,
    method_name: &str,
    trait_name: &str,
    trait_args: &[crate::ast::Type],
    impl_generics: &[String],
) -> String {
    let base = format!("{}_{}", type_name, method_name);
    // Keep the historical `Type_method` key for generic impls such as
    // `impl<T> Head<T> for List<T>`; only concrete trait instantiations
    // (e.g. `From<FsError, AppError>` for AppError) need disambiguation.
    let uses_impl_generic = trait_args.iter().any(|arg| {
        matches!(arg.unlocated(), crate::ast::Type::Name(n, args)
            if args.is_empty() && impl_generics.iter().any(|generic| generic == n))
    });
    if trait_args.is_empty() || uses_impl_generic {
        base
    } else {
        format!(
            "{}_{}<{}>",
            base,
            trait_name,
            trait_args
                .iter()
                .map(crate::core::fmt_type)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

/// Format the core impl key without the module prefix.
///
/// For ordinary impls this stays `Trait:for:Type`.  For generic traits with
/// concrete arguments (e.g. `From<FsError, AppError>`) the args are folded in
/// so multiple conversions targeting the same type do not collide.
pub(crate) fn impl_qualified_key(
    trait_name: &str,
    trait_args: &[crate::ast::Type],
    type_name: &str,
) -> String {
    if trait_args.is_empty() {
        format!("{}:for:{}", trait_name, type_name)
    } else {
        format!(
            "{}<{}>:for:{}",
            trait_name,
            trait_args
                .iter()
                .map(crate::core::fmt_type)
                .collect::<Vec<_>>()
                .join(","),
            type_name
        )
    }
}

/// Format the fully qualified impl name.
pub(crate) fn impl_qualified_name(
    trait_name: &str,
    trait_args: &[crate::ast::Type],
    type_name: &str,
) -> String {
    impl_qualified_key(trait_name, trait_args, type_name)
}

pub(crate) fn impl_method_owner(impl_qualified_name: &str, method: &crate::ast::FuncDef) -> NodeId {
    NodeId(format!(
        "function:{}::{}:{:016x}",
        impl_qualified_name,
        method.name,
        stable_text_hash(&method_signature_key(
            &method.name,
            &method.params,
            method.ret.as_ref()
        ))
    ))
}

pub(crate) fn nested_function_owner(owner: &NodeId, function: &crate::ast::FuncDef) -> NodeId {
    // Full audit 2026-08-05 (#11): same-named, same-signature nested
    // callables in DISJOINT branches (if/else, match arms) are accepted by
    // the checker, but the structural key alone (name+params+ret) collided
    // into one NodeId and aborted compilation with a duplicate-identity
    // diagnostic. Fold the declaration's source anchor into the hashed
    // signature fragment. Every consumer (checker signature recording,
    // meta walk, call-site walk, IR body lowering, CFG lowering) calls this
    // helper on the same AST node, so the anchor stays consistent across
    // catalogs. The span is a per-compilation anchor — the same discipline
    // NodeIdBuilder::anonymous uses for user-node source ranges — and
    // synthetic declarations on Span::Unknown keep the legacy key value.
    let span = function.meta.span;
    let mut signature_key =
        method_signature_key(&function.name, &function.params, function.ret.as_ref());
    {
        use std::fmt::Write as _;
        let _ = write!(
            signature_key,
            "@{}:{}:{}:{}",
            span.start_line, span.start_col, span.end_line, span.end_col
        );
    }
    NodeId(format!(
        "{}/function:{}:{:016x}",
        owner.0,
        stable_id_fragment(&function.name),
        stable_text_hash(&signature_key)
    ))
}

fn collect_item_meta(
    item: &Item,
    ids: &NodeIdBuilder<'_>,
    function_qualified_names: &HashMap<(String, String), String>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    match item {
        Item::Func(function) => {
            let qualified =
                top_level_function_qualified_name(function, ids.sources, function_qualified_names);
            let node_id = NodeId(format!("function:{qualified}"));
            let fallback = function.meta.span;
            let parent = top_level_enclosing_parent(&qualified);
            collect_func_meta(function, node_id, &parent, fallback, ids, out, errors);
        }
        Item::Type(type_def) => {
            let qualified = type_def.name.clone();
            let node_id = NodeId(format!("type:{qualified}"));
            let fallback = type_def.meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::Type,
                &qualified,
                type_def.meta,
                fallback,
                out,
                errors,
            );
            let span = declaration_span(type_def.meta, fallback);
            for generic in &type_def.generics {
                collect_generic_param_meta(generic, &node_id, "generic", span, ids, out, errors);
            }
            match &type_def.kind {
                crate::ast::TypeDefKind::Alias(ty) | crate::ast::TypeDefKind::Newtype(ty) => {
                    collect_type_meta(ty, &node_id, "target", span, ids, out, errors);
                }
                crate::ast::TypeDefKind::Record(fields)
                | crate::ast::TypeDefKind::Union(fields) => {
                    for field in fields {
                        collect_field_meta(field, &node_id, "field", span, ids, out, errors);
                    }
                }
                crate::ast::TypeDefKind::Enum(variants) => {
                    for variant in variants {
                        let role = format!("variant.{}", stable_id_fragment(&variant.name));
                        let variant_id = insert_child_meta(
                            variant.meta,
                            &node_id,
                            "decl.variant",
                            &role,
                            span,
                            ids,
                            out,
                            errors,
                        );
                        match &variant.payload {
                            Some(crate::ast::VariantPayload::Tuple(types)) => {
                                let child_roles = type_sibling_roles("payload.element", types);
                                for index in 0..types.len() {
                                    let child_role = &child_roles[index];
                                    collect_type_meta(
                                        &types[index],
                                        &variant_id,
                                        child_role,
                                        span,
                                        ids,
                                        out,
                                        errors,
                                    );
                                }
                            }
                            Some(crate::ast::VariantPayload::Record(fields)) => {
                                for field in fields {
                                    collect_field_meta(
                                        field,
                                        &variant_id,
                                        "payload.field",
                                        span,
                                        ids,
                                        out,
                                        errors,
                                    );
                                }
                            }
                            None => {}
                        }
                    }
                }
            }
        }
        Item::Actor(actor) => {
            let qualified = actor.name.clone();
            let node_id = NodeId(format!("actor:{qualified}"));
            let fallback = actor.meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::Actor,
                &qualified,
                actor.meta,
                fallback,
                out,
                errors,
            );
            let span = declaration_span(actor.meta, fallback);
            for field in &actor.fields {
                let role = format!("field.{}", stable_id_fragment(&field.name));
                let field_id = insert_child_meta(
                    field.meta,
                    &node_id,
                    "decl.actor_field",
                    &role,
                    span,
                    ids,
                    out,
                    errors,
                );
                collect_type_meta(&field.ty, &field_id, "type", span, ids, out, errors);
                if let Some(init) = &field.init {
                    collect_expr_meta(
                        init,
                        &node_id,
                        &format!("{role}.initializer"),
                        span,
                        ids,
                        out,
                        errors,
                    );
                }
            }
            for method in &actor.methods {
                let method_id = NodeId(format!("function:{qualified}::{}", method.name));
                collect_func_meta(method, method_id, &node_id, span, ids, out, errors);
            }
        }
        Item::Cap(cap) => {
            let qualified = cap.name.clone();
            insert_canonical_meta(
                NodeId(format!("capability:{qualified}")),
                ResolvedItemKind::Capability,
                &qualified,
                cap.meta,
                cap.meta.span,
                out,
                errors,
            );
        }
        Item::Trait(trait_def) => {
            let qualified = trait_def.name.clone();
            let node_id = NodeId(format!("trait:{qualified}"));
            let fallback = trait_def.meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::Trait,
                &qualified,
                trait_def.meta,
                fallback,
                out,
                errors,
            );
            let span = declaration_span(trait_def.meta, fallback);
            for generic in &trait_def.generics {
                collect_generic_param_meta(generic, &node_id, "generic", span, ids, out, errors);
            }
            for method in &trait_def.methods {
                let signature =
                    method_signature_key(&method.name, &method.params, method.ret.as_ref());
                let method_id = NodeId(format!(
                    "{}/method:{}:{:016x}",
                    node_id.0,
                    stable_id_fragment(&method.name),
                    stable_text_hash(&signature)
                ));
                insert_node_meta(
                    method_id.clone(),
                    method.meta.origin,
                    method.meta.parent,
                    ast_meta_anchor(method.meta),
                    span,
                    &node_id,
                    None,
                    out,
                    errors,
                );
                for generic in &method.generics {
                    collect_generic_param_meta(
                        generic, &method_id, "generic", span, ids, out, errors,
                    );
                }
                for param in &method.params {
                    collect_param_meta(param, &method_id, "parameter", span, ids, out, errors);
                }
                if let Some(ret) = &method.ret {
                    collect_type_meta(ret, &method_id, "return_type", span, ids, out, errors);
                }
            }
        }
        Item::Impl(impl_def) => {
            let qualified = impl_qualified_name(
                &impl_def.trait_name,
                &impl_def.trait_args,
                &impl_def.type_name,
            );
            let node_id = NodeId(format!("impl:{qualified}"));
            let fallback = impl_def.meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::Impl,
                &qualified,
                impl_def.meta,
                fallback,
                out,
                errors,
            );
            let span = declaration_span(impl_def.meta, fallback);
            for generic in &impl_def.generics {
                collect_generic_param_meta(generic, &node_id, "generic", span, ids, out, errors);
            }
            let role_batch = type_sibling_roles("trait_argument", &impl_def.trait_args);
            for index in 0..impl_def.trait_args.len() {
                let role = &role_batch[index];
                collect_type_meta(
                    &impl_def.trait_args[index],
                    &node_id,
                    role,
                    span,
                    ids,
                    out,
                    errors,
                );
            }
            let role_batch = type_sibling_roles("type_argument", &impl_def.type_args);
            for index in 0..impl_def.type_args.len() {
                let role = &role_batch[index];
                collect_type_meta(
                    &impl_def.type_args[index],
                    &node_id,
                    role,
                    span,
                    ids,
                    out,
                    errors,
                );
            }
            for method in &impl_def.methods {
                collect_func_meta(
                    method,
                    impl_method_owner(&qualified, method),
                    &node_id,
                    span,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Item::ExternBlock(block) => {
            let qualified = extern_block_key(block).clone();
            let node_id = NodeId(format!("extern:{qualified}"));
            let fallback = block.meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::ExternBlock,
                &qualified,
                block.meta,
                fallback,
                out,
                errors,
            );
            let span = declaration_span(block.meta, fallback);
            for function in &block.funcs {
                let function_id = extern_function_owner(&node_id, function);
                insert_node_meta(
                    function_id.clone(),
                    function.meta.origin,
                    function.meta.parent,
                    ast_meta_anchor(function.meta),
                    span,
                    &node_id,
                    None,
                    out,
                    errors,
                );
                for param in &function.params {
                    let role = format!("parameter.{}", stable_id_fragment(&param.name));
                    let param_id = insert_child_meta(
                        param.meta,
                        &function_id,
                        "decl.extern_parameter",
                        &role,
                        span,
                        ids,
                        out,
                        errors,
                    );
                    collect_type_meta(&param.ty, &param_id, "type", span, ids, out, errors);
                }
                if let Some(ret) = &function.ret {
                    collect_type_meta(ret, &function_id, "return_type", span, ids, out, errors);
                }
                if let Some(requires) = &function.requires {
                    collect_expr_meta(requires, &function_id, "requires", span, ids, out, errors);
                }
                if let Some(ensures) = &function.ensures {
                    collect_expr_meta(ensures, &function_id, "ensures", span, ids, out, errors);
                }
            }
        }
        Item::Const {
            meta,
            name,
            ty,
            value,
            ..
        } => {
            let qualified = name.clone();
            let node_id = NodeId(format!("constant:{qualified}"));
            let fallback = meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::Constant,
                &qualified,
                *meta,
                fallback,
                out,
                errors,
            );
            let span = declaration_span(*meta, fallback);
            if let Some(ty) = ty {
                collect_type_meta(ty, &node_id, "type", span, ids, out, errors);
            }
            collect_expr_meta(value, &node_id, "value", span, ids, out, errors);
        }
        Item::Flow(flow) => {
            let qualified = flow.name.clone();
            let node_id = NodeId(format!("flow:{qualified}"));
            let fallback = flow.meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::Flow,
                &qualified,
                flow.meta,
                fallback,
                out,
                errors,
            );
            let span = declaration_span(flow.meta, fallback);
            for generic in &flow.generics {
                collect_generic_param_meta(generic, &node_id, "generic", span, ids, out, errors);
            }
            for annotation in &flow.annotations {
                let role = match annotation.kind {
                    crate::ast::FlowAnnotationKind::MailboxDepth(depth) => {
                        format!("annotation.mailbox_depth.{depth}")
                    }
                    crate::ast::FlowAnnotationKind::MaxChildren(max) => {
                        format!("annotation.max_children.{max}")
                    }
                    crate::ast::FlowAnnotationKind::Sparse => "annotation.sparse".to_string(),
                };
                insert_child_meta(
                    annotation.meta,
                    &node_id,
                    "decl.flow_annotation",
                    &role,
                    span,
                    ids,
                    out,
                    errors,
                );
            }
            for state in &flow.states {
                let state_id = NodeId(format!("state:{qualified}::{}", state.name));
                insert_node_meta(
                    state_id.clone(),
                    state.meta.origin,
                    state.meta.parent,
                    ast_meta_anchor(state.meta),
                    span,
                    &node_id,
                    None,
                    out,
                    errors,
                );
                for field in state.payload.as_deref().unwrap_or_default() {
                    collect_field_meta(field, &state_id, "payload.field", span, ids, out, errors);
                }
            }
            for transition in &flow.transitions {
                let transition_id = NodeId(format!(
                    "transition:{qualified}::{}::{}",
                    transition.name, transition.from_state
                ));
                insert_node_meta(
                    transition_id.clone(),
                    transition.meta.origin,
                    transition.meta.parent,
                    ast_meta_anchor(transition.meta),
                    span,
                    &node_id,
                    None,
                    out,
                    errors,
                );
                let transition_span = declaration_span(transition.meta, span);
                insert_child_meta(
                    AstNodeMeta::inherited(
                        transition_span,
                        AstOrigin::Desugared("normalization.transition_source_parameter"),
                    ),
                    &transition_id,
                    "decl.parameter",
                    "parameter.self",
                    transition_span,
                    ids,
                    out,
                    errors,
                );
                for param in &transition.params {
                    collect_param_meta(
                        param,
                        &transition_id,
                        "parameter",
                        transition_span,
                        ids,
                        out,
                        errors,
                    );
                }
                if let Some(body) = &transition.body {
                    collect_block_meta(
                        body,
                        &transition_id,
                        "body",
                        transition_span,
                        ids,
                        out,
                        errors,
                    );
                }
            }
        }
        Item::Session(session) => {
            let qualified = session.name.clone();
            let node_id = NodeId(format!("session:{qualified}"));
            let fallback = session.meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::Session,
                &qualified,
                session.meta,
                fallback,
                out,
                errors,
            );
            collect_session_type_meta(
                &session.body,
                &node_id,
                "body",
                declaration_span(session.meta, fallback),
                ids,
                out,
                errors,
            );
        }
    }
}

fn stmt_semantic_key(stmt: &Stmt) -> String {
    match stmt.unlocated() {
        Stmt::Let { pat, .. } => format!("let:{}", pattern_semantic_key(pat)),
        Stmt::Return(value) => format!(
            "return:{}",
            value.as_ref().map(expr_semantic_key).unwrap_or_default()
        ),
        Stmt::Break(value) => format!(
            "break:{}",
            value.as_ref().map(expr_semantic_key).unwrap_or_default()
        ),
        Stmt::Continue => "continue".into(),
        Stmt::Expr(expr) => format!("expr:{}", expr_semantic_key(expr)),
        Stmt::If { cond, .. } => format!("if:{}", expr_semantic_key(cond)),
        Stmt::IfLet { pat, init, .. } => format!(
            "if-let:{}:{}",
            pattern_semantic_key(pat),
            expr_semantic_key(init)
        ),
        Stmt::While { cond, .. } => format!("while:{}", expr_semantic_key(cond)),
        Stmt::WhileLet { pat, init, .. } => format!(
            "while-let:{}:{}",
            pattern_semantic_key(pat),
            expr_semantic_key(init)
        ),
        Stmt::Loop(_) => "loop".into(),
        Stmt::For { var, iterable, .. } => {
            format!("for:{:?}:{}", var.kind, expr_semantic_key(iterable))
        }
        Stmt::Block(_) => "block".into(),
        Stmt::Requires(expr, _) => format!("requires:{}", expr_semantic_key(expr)),
        Stmt::Ensures(expr, _) => format!("ensures:{}", expr_semantic_key(expr)),
        Stmt::Invariant(expr, _) => format!("invariant:{}", expr_semantic_key(expr)),
        Stmt::Math(exprs) => format!("math:{}", exprs.len()),
        Stmt::Assign { target, .. } => format!("assign:{}", expr_semantic_key(target)),
        Stmt::Arena(_) => "arena".into(),
        Stmt::Unsafe(_) => "unsafe".into(),
        Stmt::IeeeFloat(_) => "ieee-float".into(),
        Stmt::Drop(expr) => format!("drop:{}", expr_semantic_key(expr)),
        Stmt::Defer(_) => "defer".into(),
        Stmt::SharedLet { name, .. } => format!("shared-let:{name}"),
        Stmt::OnFailure(_) => "on-failure".into(),
        Stmt::Pinned { var, .. } => format!("pinned:{}", var.as_deref().unwrap_or("_")),
        Stmt::Parasteps(_) => "parasteps".into(),
        Stmt::Func(function) => format!("function:{}", function.name),
        Stmt::Ellipsis => "ellipsis".into(),
        Stmt::Located { .. } => unreachable!("Stmt::unlocated returned Located"),
    }
}

pub(crate) fn stmt_kind(stmt: &Stmt) -> &'static str {
    match stmt.unlocated() {
        Stmt::Let { .. } => "stmt.let",
        Stmt::Return(_) => "stmt.return",
        Stmt::Break(_) => "stmt.break",
        Stmt::Continue => "stmt.continue",
        Stmt::Expr(_) => "stmt.expr",
        Stmt::If { .. } => "stmt.if",
        Stmt::IfLet { .. } => "stmt.if_let",
        Stmt::While { .. } => "stmt.while",
        Stmt::WhileLet { .. } => "stmt.while_let",
        Stmt::Loop(_) => "stmt.loop",
        Stmt::For { .. } => "stmt.for",
        Stmt::Block(_) => "stmt.block",
        Stmt::Requires(_, _) => "stmt.requires",
        Stmt::Ensures(_, _) => "stmt.ensures",
        Stmt::Invariant(_, _) => "stmt.invariant",
        Stmt::Math(_) => "stmt.math",
        Stmt::Assign { .. } => "stmt.assign",
        Stmt::Arena(_) => "stmt.arena",
        Stmt::Unsafe(_) => "stmt.unsafe",
        Stmt::IeeeFloat(_) => "stmt.ieee_float",
        Stmt::Drop(_) => "stmt.drop",
        Stmt::Defer(_) => "stmt.defer",
        Stmt::SharedLet { .. } => "stmt.shared_let",
        Stmt::OnFailure(_) => "stmt.on_failure",
        Stmt::Pinned { .. } => "stmt.pinned",
        Stmt::Parasteps(_) => "stmt.parasteps",
        Stmt::Func(_) => "stmt.function",
        Stmt::Ellipsis => "stmt.ellipsis",
        Stmt::Located { .. } => unreachable!("Stmt::unlocated returned Located"),
    }
}

fn usable_span(span: Span) -> Option<Span> {
    (span.start_line > 0 && span.start_col > 0).then_some(span)
}

fn ast_meta_anchor(meta: crate::ast::AstNodeMeta) -> Option<(Span, SpanPrecision)> {
    usable_span(meta.span).map(|span| {
        let precision = if meta.origin == AstOrigin::User {
            SpanPrecision::Exact
        } else {
            // Lowering frequently inherits the triggering user construct's
            // range.  It remains an honest source anchor, but it is not the
            // generated child's own exact syntax range.
            SpanPrecision::SourceAnchor
        };
        (span, precision)
    })
}

fn expr_span(expr: &Expr) -> Option<Span> {
    expr.meta().and_then(|meta| usable_span(meta.span))
}

pub(crate) fn stmt_anchor(stmt: &Stmt, fallback: Span) -> Option<(Span, SpanPrecision)> {
    if let Some(anchor) = stmt.meta().and_then(ast_meta_anchor) {
        return Some(anchor);
    }
    let anchored = |span| usable_span(span).map(|span| (span, SpanPrecision::SourceAnchor));
    match stmt.unlocated() {
        Stmt::Let { pat, .. } => {
            // Unwrapped stmt without a `Located` shell: fall back to the
            // pattern's own span, which still carries a SourceId.
            anchored(pat.meta.span)
        }
        Stmt::Expr(expr) => expr_span(expr).map(|span| (span, SpanPrecision::Exact)),
        Stmt::Return(Some(expr))
        | Stmt::Break(Some(expr))
        | Stmt::Drop(expr)
        | Stmt::SharedLet { init: expr, .. } => {
            expr_span(expr).map(|span| (span, SpanPrecision::SourceAnchor))
        }
        Stmt::If { cond, .. } | Stmt::While { cond, .. } => {
            expr_span(cond).map(|span| (span, SpanPrecision::SourceAnchor))
        }
        Stmt::IfLet { init, .. } => expr_span(init).map(|span| (span, SpanPrecision::SourceAnchor)),
        Stmt::WhileLet { pat, .. } => anchored(pat.meta.span),
        Stmt::For { iterable, .. } => {
            expr_span(iterable).map(|span| (span, SpanPrecision::SourceAnchor))
        }
        Stmt::Requires(_, span) | Stmt::Ensures(_, span) | Stmt::Invariant(_, span) => {
            anchored(*span)
        }
        Stmt::Assign { target, .. } => {
            expr_span(target).map(|span| (span, SpanPrecision::SourceAnchor))
        }
        Stmt::Pinned { expr, .. } => {
            expr_span(expr).map(|span| (span, SpanPrecision::SourceAnchor))
        }
        Stmt::Math(exprs) => exprs
            .first()
            .and_then(expr_span)
            .map(|span| (span, SpanPrecision::SourceAnchor)),
        Stmt::Func(function) => anchored(function.meta.span.with_source(fallback.source_id)),
        _ => None,
    }
}

fn collect_stmt_meta(
    stmt: &Stmt,
    owner: &NodeId,
    role: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let meta = stmt.meta();
    let anchor = stmt_anchor(stmt, fallback);
    let ast_origin = meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User);
    let node_id = ids.anonymous(
        owner,
        stmt_kind(stmt),
        role,
        anchor.map(|(span, _)| span),
        ast_origin,
        errors,
    );
    insert_node_meta(
        node_id.clone(),
        ast_origin,
        meta.map(|meta| meta.parent).unwrap_or(AstParentHint::None),
        anchor,
        fallback,
        owner,
        None,
        out,
        errors,
    );
    if let Stmt::SharedLet { kind, init, .. } = stmt.unlocated() {
        if let Some(meta) = out.get_mut(&node_id) {
            meta.shared_binding = Some((*kind, expression_type_key(init)));
        }
    }
    if let Stmt::Let {
        ref_: true,
        init: Some(init),
        ..
    } = stmt.unlocated()
    {
        if let Some(meta) = out.get_mut(&node_id) {
            meta.ref_binding = Some(expression_type_key(init));
        }
    }
    match stmt.unlocated() {
        Stmt::Let { pat, ty, init, .. } => {
            collect_pattern_meta(
                pat,
                owner,
                &format!("{role}.pattern"),
                fallback,
                ids,
                out,
                errors,
            );
            if let Some(ty) = ty {
                collect_type_meta(
                    ty,
                    owner,
                    &format!("{role}.type"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
            if let Some(expr) = init {
                collect_expr_meta(
                    expr,
                    owner,
                    &format!("{role}.initializer"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Stmt::Return(expr) | Stmt::Break(expr) => {
            if let Some(expr) = expr {
                collect_expr_meta(
                    expr,
                    owner,
                    &format!("{role}.value"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Stmt::Continue | Stmt::Ellipsis => {}
        Stmt::Expr(expr)
        | Stmt::Drop(expr)
        | Stmt::Requires(expr, _)
        | Stmt::Ensures(expr, _)
        | Stmt::Invariant(expr, _) => {
            collect_expr_meta(
                expr,
                owner,
                &format!("{role}.expression"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Stmt::If { cond, then_, else_ } => {
            collect_expr_meta(
                cond,
                owner,
                &format!("{role}.condition"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_block_meta(
                then_,
                owner,
                &format!("{role}.then"),
                fallback,
                ids,
                out,
                errors,
            );
            if let Some(block) = else_ {
                collect_block_meta(
                    block,
                    owner,
                    &format!("{role}.else"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Stmt::IfLet {
            pat,
            init,
            then_,
            else_,
        } => {
            collect_pattern_meta(
                pat,
                owner,
                &format!("{role}.pattern"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_expr_meta(
                init,
                owner,
                &format!("{role}.init"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_block_meta(
                then_,
                owner,
                &format!("{role}.then"),
                fallback,
                ids,
                out,
                errors,
            );
            if let Some(block) = else_ {
                collect_block_meta(
                    block,
                    owner,
                    &format!("{role}.else"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Stmt::While { cond, body } => {
            collect_expr_meta(
                cond,
                owner,
                &format!("{role}.condition"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_block_meta(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Stmt::WhileLet { pat, init, body } => {
            collect_pattern_meta(
                pat,
                owner,
                &format!("{role}.pattern"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_expr_meta(
                init,
                owner,
                &format!("{role}.initializer"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_block_meta(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Stmt::Loop(body)
        | Stmt::Block(body)
        | Stmt::Arena(body)
        | Stmt::Unsafe(body)
        | Stmt::IeeeFloat(body)
        | Stmt::OnFailure(body)
        | Stmt::Parasteps(body)
        | Stmt::Defer(body) => collect_block_meta(
            body,
            owner,
            &format!("{role}.body"),
            fallback,
            ids,
            out,
            errors,
        ),
        Stmt::For {
            var,
            iterable,
            body,
            ..
        } => {
            // v0.34.3: for-loop pattern must be registered so lowering can
            // resolve its NodeMeta (single ident AND tuple destructuring).
            collect_pattern_meta(
                var,
                owner,
                &format!("{role}.pattern"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_expr_meta(
                iterable,
                owner,
                &format!("{role}.iterable"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_block_meta(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Stmt::Math(exprs) => {
            let child_roles = expr_sibling_roles(&format!("{role}.math"), exprs);
            for index in 0..exprs.len() {
                let child_role = &child_roles[index];
                collect_expr_meta(&exprs[index], owner, child_role, fallback, ids, out, errors);
            }
        }
        Stmt::Assign { target, value } => {
            collect_expr_meta(
                target,
                owner,
                &format!("{role}.target"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_expr_meta(
                value,
                owner,
                &format!("{role}.value"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Stmt::SharedLet { ty, init, .. } => {
            if let Some(ty) = ty {
                collect_type_meta(
                    ty,
                    owner,
                    &format!("{role}.type"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
            collect_expr_meta(
                init,
                owner,
                &format!("{role}.initializer"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Stmt::Pinned { expr, body, .. } => {
            collect_expr_meta(
                expr,
                owner,
                &format!("{role}.expression"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_block_meta(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Stmt::Func(function) => {
            let nested_owner = nested_function_owner(owner, function);
            let nested_fallback = function.meta.span.with_source(fallback.source_id);
            collect_func_meta(
                function,
                nested_owner,
                owner,
                nested_fallback,
                ids,
                out,
                errors,
            );
        }
        Stmt::Located { .. } => unreachable!("Stmt::unlocated returned Located"),
    }
}

fn expr_semantic_key(expr: &Expr) -> String {
    match expr.unlocated() {
        Expr::Literal(lit) => match lit {
            crate::ast::Lit::Int(value) => format!("literal.int:{value}"),
            crate::ast::Lit::Float(value) => format!("literal.float:{:016x}", value.to_bits()),
            crate::ast::Lit::Bool(value) => format!("literal.bool:{value}"),
            crate::ast::Lit::String(value) => {
                format!("literal.string:{:016x}", stable_text_hash(value))
            }
            crate::ast::Lit::FString(parts) => format!("literal.fstring:{}", parts.len()),
            crate::ast::Lit::Unit => "literal.unit".into(),
        },
        Expr::Ident(name) => format!("ident:{name}"),
        Expr::Binary(op, _, _) => format!("binary:{op:?}"),
        Expr::Unary(op, _) => format!("unary:{op:?}"),
        Expr::Call(callee, _) => format!("call:{}", expr_semantic_key(callee)),
        Expr::Field(_, name) => format!("field:{name}"),
        Expr::Index(_, _) => "index".into(),
        Expr::Tuple(items) => format!("tuple:{}", items.len()),
        Expr::List(items) => format!("list:{}", items.len()),
        Expr::Comprehension { var, .. } => format!("comprehension:{var}"),
        Expr::Match(_, arms) => format!("match:{}", arms.len()),
        Expr::Record { ty, .. } => format!("record:{}", ty.as_deref().unwrap_or("_")),
        Expr::Block(_) => "block".into(),
        Expr::Try(_) => "try".into(),
        Expr::OptionalChain(_, field) => format!("optional-chain:{field}"),
        Expr::Spawn(_) => "spawn".into(),
        Expr::Await(_) => "await".into(),
        Expr::Quote(_) => "quote".into(),
        Expr::QuoteInterpolate(_) => "quote-interpolate".into(),
        Expr::Comptime(_) => "comptime".into(),
        Expr::TypeOf(_) => "type-of".into(),
        Expr::TypeInfo(ty) => format!("type-info:{}", crate::core::fmt_type(ty)),
        Expr::If { .. } => "if".into(),
        Expr::Lambda { params, .. } => format!("lambda:{}", params.len()),
        Expr::Old(_) => "old".into(),
        Expr::SliceExpr { .. } => "slice".into(),
        Expr::Turbofish(name, types, _) => format!("turbofish:{name}:{}", types.len()),
        Expr::TupleIndex(_, index) => format!("tuple-index:{index}"),
        Expr::Arena(_) => "arena".into(),
        Expr::MapLiteral { entries } => format!("map:{}", entries.len()),
        Expr::SetLiteral(items) => format!("set:{}", items.len()),
        Expr::NamedArg(name, _) => format!("named-argument:{name}"),
        Expr::Cast(_, ty) => format!("cast:{}", crate::core::fmt_type(ty)),
        Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
    }
}

pub(crate) fn expr_kind(expr: &Expr) -> &'static str {
    match expr.unlocated() {
        Expr::Literal(_) => "expr.literal",
        Expr::Ident(_) => "expr.identifier",
        Expr::Binary(_, _, _) => "expr.binary",
        Expr::Unary(_, _) => "expr.unary",
        Expr::Call(_, _) => "expr.call",
        Expr::Field(_, _) => "expr.field",
        Expr::Index(_, _) => "expr.index",
        Expr::Tuple(_) => "expr.tuple",
        Expr::List(_) => "expr.list",
        Expr::Comprehension { .. } => "expr.comprehension",
        Expr::Match(_, _) => "expr.match",
        Expr::Record { .. } => "expr.record",
        Expr::Block(_) => "expr.block",
        Expr::Try(_) => "expr.try",
        Expr::OptionalChain(_, _) => "expr.optional_chain",
        Expr::Spawn(_) => "expr.spawn",
        Expr::Await(_) => "expr.await",
        Expr::Quote(_) => "expr.quote",
        Expr::QuoteInterpolate(_) => "expr.quote_interpolate",
        Expr::Comptime(_) => "expr.comptime",
        Expr::TypeOf(_) => "expr.type_of",
        Expr::TypeInfo(_) => "expr.type_info",
        Expr::If { .. } => "expr.if",
        Expr::Lambda { .. } => "expr.lambda",
        Expr::Old(_) => "expr.old",
        Expr::SliceExpr { .. } => "expr.slice",
        Expr::Turbofish(_, _, _) => "expr.turbofish",
        Expr::TupleIndex(_, _) => "expr.tuple_index",
        Expr::Arena(_) => "expr.arena",
        Expr::MapLiteral { .. } => "expr.map",
        Expr::SetLiteral(_) => "expr.set",
        Expr::NamedArg(_, _) => "expr.named_argument",
        Expr::Cast(_, _) => "expr.cast",
        Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
    }
}

pub(crate) fn expression_type_key(expr: &Expr) -> ExpressionTypeKey {
    let meta = expr
        .meta()
        .unwrap_or_else(|| AstNodeMeta::synthetic(AstOrigin::User));
    ExpressionTypeKey {
        source_id: meta.span.source_id.raw(),
        start_line: meta.span.start_line,
        start_col: meta.span.start_col,
        end_line: meta.span.end_line,
        end_col: meta.span.end_col,
        origin_kind: meta.origin.kind(),
        origin_rule: meta.origin.rule(),
        expression_kind: expr_kind(expr),
    }
}

fn collect_expr_meta(
    expr: &Expr,
    owner: &NodeId,
    role: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let meta = expr.meta();
    let anchor = meta.and_then(ast_meta_anchor);
    let exact = anchor.map(|(span, _)| span);
    let ast_origin = meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User);
    let node_id = ids.anonymous(owner, expr_kind(expr), role, exact, ast_origin, errors);
    insert_node_meta(
        node_id.clone(),
        ast_origin,
        meta.map(|meta| meta.parent).unwrap_or(AstParentHint::None),
        anchor,
        fallback,
        owner,
        None,
        out,
        errors,
    );
    if let Some(node_meta) = out.get_mut(&node_id) {
        let key = expression_type_key(expr);
        if node_meta.expression_key.replace(key).is_some() {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: expression NodeId '{}' has more than one type key",
                    node_id.0
                ),
                exact.unwrap_or(fallback),
            ));
        }
        if let Expr::TypeInfo(ty) = expr.unlocated() {
            if node_meta.type_operand.replace(ty.clone()).is_some() {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: expression NodeId '{}' has more than one explicit type operand",
                        node_id.0
                    ),
                    exact.unwrap_or(fallback),
                ));
            }
        }
        if let Expr::Turbofish(_, arguments, _) = expr.unlocated() {
            if !node_meta.type_arguments.is_empty() {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: expression NodeId '{}' has more than one generic argument list",
                        node_id.0
                    ),
                    exact.unwrap_or(fallback),
                ));
            } else {
                node_meta.type_arguments.clone_from(arguments);
            }
        }
    }
    match expr.unlocated() {
        Expr::Literal(lit) => {
            if let crate::ast::Lit::FString(parts) = lit {
                for (part_index, part) in parts.iter().enumerate() {
                    if let FStringPart::Interp(expr) = part {
                        let child_role =
                            interpolation_role(&format!("{role}.interpolation"), parts, part_index);
                        collect_expr_meta(expr, owner, &child_role, fallback, ids, out, errors);
                    }
                }
            }
        }
        Expr::Ident(_) => {}
        Expr::TypeInfo(ty) => {
            collect_type_meta(
                ty,
                owner,
                &format!("{role}.type"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Expr::Binary(_, left, right) | Expr::Index(left, right) => {
            collect_expr_meta(
                left,
                owner,
                &format!("{role}.left"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_expr_meta(
                right,
                owner,
                &format!("{role}.right"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Expr::Unary(_, inner)
        | Expr::Field(inner, _)
        | Expr::Try(inner)
        | Expr::OptionalChain(inner, _)
        | Expr::Spawn(inner)
        | Expr::Await(inner)
        | Expr::QuoteInterpolate(inner)
        | Expr::TypeOf(inner)
        | Expr::Old(inner)
        | Expr::TupleIndex(inner, _)
        | Expr::NamedArg(_, inner) => {
            collect_expr_meta(
                inner,
                owner,
                &format!("{role}.inner"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Expr::Cast(inner, ty) => {
            collect_expr_meta(
                inner,
                owner,
                &format!("{role}.inner"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_type_meta(
                ty,
                owner,
                &format!("{role}.type"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Expr::Call(callee, args) => {
            collect_expr_meta(
                callee,
                owner,
                &format!("{role}.callee"),
                fallback,
                ids,
                out,
                errors,
            );
            let child_roles = expr_sibling_roles(&format!("{role}.argument"), args);
            for index in 0..args.len() {
                let child_role = &child_roles[index];
                collect_expr_meta(&args[index], owner, child_role, fallback, ids, out, errors);
            }
        }
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            let child_roles = expr_sibling_roles(&format!("{role}.element"), items);
            for index in 0..items.len() {
                let child_role = &child_roles[index];
                collect_expr_meta(&items[index], owner, child_role, fallback, ids, out, errors);
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            collect_expr_meta(
                expr,
                owner,
                &format!("{role}.value"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_expr_meta(
                iter,
                owner,
                &format!("{role}.iterable"),
                fallback,
                ids,
                out,
                errors,
            );
            if let Some(guard) = guard {
                collect_expr_meta(
                    guard,
                    owner,
                    &format!("{role}.guard"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Expr::Match(scrutinee, arms) => {
            collect_expr_meta(
                scrutinee,
                owner,
                &format!("{role}.scrutinee"),
                fallback,
                ids,
                out,
                errors,
            );
            let arm_role_batch = match_arm_roles(&format!("{role}.arm"), arms);
            for (index, arm) in arms.iter().enumerate() {
                let arm_role = &arm_role_batch[index];
                insert_child_meta(
                    arm.meta,
                    owner,
                    "match.arm",
                    arm_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
                collect_pattern_meta(
                    &arm.pat,
                    owner,
                    &format!("{arm_role}.pattern"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
                if let Some(guard) = &arm.guard {
                    collect_expr_meta(
                        guard,
                        owner,
                        &format!("{arm_role}.guard"),
                        fallback,
                        ids,
                        out,
                        errors,
                    );
                }
                collect_expr_meta(
                    &arm.body,
                    owner,
                    &format!("{arm_role}.body"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Expr::Record { fields, rest, .. } => {
            for field in fields {
                let field_role = format!("{role}.field.{}", stable_id_fragment(&field.name));
                insert_child_meta(
                    field.meta,
                    owner,
                    "record.field",
                    &field_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
                collect_expr_meta(
                    &field.value,
                    owner,
                    &format!("{field_role}.value"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
            if let Some(rest) = rest {
                collect_expr_meta(
                    rest,
                    owner,
                    &format!("{role}.rest"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Expr::Block(block) | Expr::Quote(block) | Expr::Comptime(block) | Expr::Arena(block) => {
            collect_block_meta(
                block,
                owner,
                &format!("{role}.block"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Expr::If { cond, then_, else_ } => {
            collect_expr_meta(
                cond,
                owner,
                &format!("{role}.condition"),
                fallback,
                ids,
                out,
                errors,
            );
            collect_block_meta(
                then_,
                owner,
                &format!("{role}.then"),
                fallback,
                ids,
                out,
                errors,
            );
            if let Some(block) = else_ {
                collect_block_meta(
                    block,
                    owner,
                    &format!("{role}.else"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Expr::Lambda { params, ret, body } => {
            for param in params {
                collect_param_meta(
                    param,
                    owner,
                    &format!("{role}.parameter"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
            if let Some(ret) = ret {
                collect_type_meta(
                    ret,
                    owner,
                    &format!("{role}.return_type"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
            collect_block_meta(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                out,
                errors,
            );
        }
        Expr::SliceExpr { target, start, end } => {
            collect_expr_meta(
                target,
                owner,
                &format!("{role}.target"),
                fallback,
                ids,
                out,
                errors,
            );
            if let Some(start) = start {
                collect_expr_meta(
                    start,
                    owner,
                    &format!("{role}.start"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
            if let Some(end) = end {
                collect_expr_meta(
                    end,
                    owner,
                    &format!("{role}.end"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Expr::Turbofish(_, types, args) => {
            let child_roles = type_sibling_roles(&format!("{role}.type_argument"), types);
            for index in 0..types.len() {
                let child_role = &child_roles[index];
                collect_type_meta(&types[index], owner, child_role, fallback, ids, out, errors);
            }
            let arg_roles_batch = expr_sibling_roles(&format!("{role}.argument"), args);
            for index in 0..args.len() {
                let child_role = &arg_roles_batch[index];
                collect_expr_meta(&args[index], owner, child_role, fallback, ids, out, errors);
            }
        }
        Expr::MapLiteral { entries } => {
            let entry_roles_batch = map_entry_roles(&format!("{role}.entry"), entries);
            for (index, (key, value)) in entries.iter().enumerate() {
                let entry_role = &entry_roles_batch[index];
                collect_expr_meta(
                    key,
                    owner,
                    &format!("{entry_role}.key"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
                collect_expr_meta(
                    value,
                    owner,
                    &format!("{entry_role}.value"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
    }
}

fn pattern_semantic_key(pattern: &Pattern) -> String {
    match &pattern.kind {
        PatternKind::Wildcard => "wildcard".into(),
        PatternKind::Variable(name) => format!("variable:{name}"),
        PatternKind::Literal(lit) => match lit {
            crate::ast::Lit::Int(value) => format!("literal.int:{value}"),
            crate::ast::Lit::Float(value) => format!("literal.float:{:016x}", value.to_bits()),
            crate::ast::Lit::Bool(value) => format!("literal.bool:{value}"),
            crate::ast::Lit::String(value) => {
                format!("literal.string:{:016x}", stable_text_hash(value))
            }
            crate::ast::Lit::FString(parts) => format!("literal.fstring:{}", parts.len()),
            crate::ast::Lit::Unit => "literal.unit".into(),
        },
        PatternKind::Constructor(name, _) => format!("constructor:{name}"),
        PatternKind::Tuple(items) => format!("tuple:{}", items.len()),
        PatternKind::Array(items) => format!("array:{}", items.len()),
        PatternKind::Slice(items, rest) => format!("slice:{}:{}", items.len(), rest.is_some()),
    }
}

pub(crate) fn pattern_kind(pattern: &Pattern) -> &'static str {
    match &pattern.kind {
        PatternKind::Wildcard => "pattern.wildcard",
        PatternKind::Variable(_) => "pattern.variable",
        PatternKind::Literal(_) => "pattern.literal",
        PatternKind::Constructor(_, _) => "pattern.constructor",
        PatternKind::Tuple(_) => "pattern.tuple",
        PatternKind::Array(_) => "pattern.array",
        PatternKind::Slice(_, _) => "pattern.slice",
    }
}

fn collect_pattern_meta(
    pattern: &Pattern,
    owner: &NodeId,
    role: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let anchor = ast_meta_anchor(pattern.meta);
    let exact = anchor.map(|(span, _)| span);
    let node_id = ids.anonymous(
        owner,
        pattern_kind(pattern),
        role,
        exact,
        pattern.meta.origin,
        errors,
    );
    insert_node_meta(
        node_id,
        pattern.meta.origin,
        pattern.meta.parent,
        anchor,
        fallback,
        owner,
        None,
        out,
        errors,
    );
    match &pattern.kind {
        PatternKind::Wildcard | PatternKind::Variable(_) | PatternKind::Literal(_) => {}
        PatternKind::Constructor(_, fields) => {
            for (name, pattern) in fields {
                collect_pattern_meta(
                    pattern,
                    owner,
                    &format!("{role}.field.{}", stable_id_fragment(name)),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        PatternKind::Tuple(items) | PatternKind::Array(items) => {
            let child_roles = pattern_sibling_roles(&format!("{role}.element"), items);
            for index in 0..items.len() {
                let child_role = &child_roles[index];
                collect_pattern_meta(&items[index], owner, child_role, fallback, ids, out, errors);
            }
        }
        PatternKind::Slice(items, rest) => {
            let child_roles = pattern_sibling_roles(&format!("{role}.element"), items);
            for index in 0..items.len() {
                let child_role = &child_roles[index];
                collect_pattern_meta(&items[index], owner, child_role, fallback, ids, out, errors);
            }
            if let Some(rest) = rest {
                collect_pattern_meta(
                    rest,
                    owner,
                    &format!("{role}.rest"),
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
    }
}

fn insert_node_meta(
    node_id: NodeId,
    ast_origin: AstOrigin,
    parent_hint: AstParentHint,
    anchor: Option<(Span, SpanPrecision)>,
    fallback: Span,
    enclosing: &NodeId,
    qualified_scope: Option<&str>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    let (span, precision) = anchor.unwrap_or((fallback, SpanPrecision::DeclarationFallback));
    if out.contains_key(&node_id) {
        errors.push(Diagnostic::error(
            format!(
                "TOOL-RESOLUTION-001: duplicate canonical NodeId '{}'",
                node_id.0
            ),
            span,
        ));
        return;
    }
    let parent = explicit_origin_parent(
        &node_id,
        ast_origin,
        parent_hint,
        enclosing,
        qualified_scope,
        span,
        errors,
    );
    let origin = resolve_origin(ast_origin, &parent, span);
    out.insert(
        node_id.clone(),
        NodeMeta {
            node_id,
            origin,
            precision,
            expression_key: None,
            shared_binding: None,
            ref_binding: None,
            type_operand: None,
            type_arguments: Vec::new(),
        },
    );
}

fn collect_flow(
    flow: &FlowDef,
    qualified_name: &str,
    ids: &NodeIdBuilder<'_>,
    flows: &mut HashMap<FlowId, ResolvedFlow>,
    transitions: &mut HashMap<TransitionId, ResolvedTransition>,
    backend_requirements: &mut Vec<CapabilityRequirement>,
    errors: &mut Vec<Diagnostic>,
) {
    let flow_id = FlowId(qualified_name.to_string());
    let flow_node_id = NodeId(format!("flow:{}", qualified_name));
    let generic_parameters = callable_generic_binders(&flow.generics, &flow_node_id, ids, errors);
    let flow_span = declaration_span(flow.meta, flow.meta.span);
    // Full audit 2026-08-05 (#12): duplicate state names were silently
    // last-wins through `HashMap::collect`. The checker guards this at the
    // surface (E0402), so the diagnostic below is defense-in-depth mirroring
    // the duplicate-transition check further down — a shadowed canonical
    // state would corrupt the shared transition tables.
    let mut states: HashMap<String, ResolvedState> = HashMap::new();
    let mut duplicate_states = 0usize;
    for state in &flow.states {
        {
            let id = StateId {
                flow: flow_id.clone(),
                name: state.name.clone(),
            };
            let payload = state
                .payload
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|field| (field.name.clone(), field.ty.clone()))
                .collect::<Vec<_>>();
            for (field, ty) in &payload {
                if contains_unresolved_type(ty) {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: unresolved or erased type '{}' in state '{}::{}' field '{}'",
                            crate::core::fmt_type(ty), qualified_name, state.name, field
                        ),
                        declaration_span(state.meta, state.meta.span),
                    ));
                }
            }
            let node_id = NodeId(format!("state:{}::{}", qualified_name, state.name));
            let mut field_ids = BTreeMap::new();
            for field in state.payload.as_deref().unwrap_or_default() {
                let field_id = ids.anonymous(
                    &node_id,
                    "decl.field",
                    &format!("payload.field.{}", stable_id_fragment(&field.name)),
                    usable_span(field.meta.span),
                    field.meta.origin,
                    errors,
                );
                field_ids.insert(field.name.clone(), field_id);
            }
            let origin = resolve_enclosed_origin(
                &node_id,
                state.meta,
                &flow_node_id,
                declaration_span(state.meta, state.meta.span),
                errors,
            );
            let resolved_state = ResolvedState {
                node_id,
                id,
                payload,
                field_ids,
                origin,
            };
            if states.insert(state.name.clone(), resolved_state).is_some() {
                duplicate_states += 1;
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: duplicate canonical state '{}::{}'",
                        qualified_name, state.name
                    ),
                    declaration_span(state.meta, state.meta.span),
                ));
            }
        }
    }
    mimi_debug_assert!(
        states.len() + duplicate_states == flow.states.len(),
        "flow '{}' state collection must account for every declaration",
        qualified_name
    );
    let mut flow_transition_ids = Vec::with_capacity(flow.transitions.len());
    for transition in &flow.transitions {
        let source = StateId {
            flow: flow_id.clone(),
            name: transition.from_state.clone(),
        };
        let id = TransitionId {
            flow: flow_id.clone(),
            event: transition.name.clone(),
            source,
        };
        let span = declaration_span(transition.meta, transition.meta.span);
        let node_id = NodeId(format!(
            "transition:{}::{}::{}",
            qualified_name, transition.name, transition.from_state
        ));
        let transition_origin =
            resolve_enclosed_origin(&node_id, transition.meta, &flow_node_id, span, errors);
        let source_parameter_id = ids.anonymous(
            &node_id,
            "decl.parameter",
            "parameter.self",
            usable_span(span),
            AstOrigin::Desugared("normalization.transition_source_parameter"),
            errors,
        );
        let resolved = ResolvedTransition {
            node_id,
            id: id.clone(),
            targets: transition
                .to_states
                .iter()
                .map(|name| StateId {
                    flow: flow_id.clone(),
                    name: name.clone(),
                })
                .collect(),
            source_parameter_id,
            params: {
                let params = transition
                    .params
                    .iter()
                    .map(|param| (param.name.clone(), param.ty.clone()))
                    .collect::<Vec<_>>();
                for (name, ty) in &params {
                    if contains_unresolved_type(ty) {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: unresolved or erased type '{}' in transition '{}::{}({})' parameter '{}'",
                                crate::core::fmt_type(ty),
                                qualified_name,
                                transition.name,
                                transition.from_state,
                                name
                            ),
                            span,
                        ));
                    }
                }
                params
            },
            parameter_ids: transition
                .params
                .iter()
                .map(|parameter| {
                    ids.anonymous(
                        &NodeId(format!(
                            "transition:{}::{}::{}",
                            qualified_name, transition.name, transition.from_state
                        )),
                        "decl.parameter",
                        &format!("parameter.{}", stable_id_fragment(&parameter.name)),
                        usable_span(parameter.meta.span),
                        parameter.meta.origin,
                        errors,
                    )
                })
                .collect(),
            is_fallback: transition.is_fallback,
            is_ffi_pinned: transition.is_ffi_pinned,
            // 追加 A / Phase C: Silent Transition — local self-loop strips
            // TransitionEpoch (clause 5.1). Cross-boundary ops force packing.
            silent_transition: {
                // A stay transition targets only the source state
                let is_stay = transition.to_states.len() == 1
                    && transition.to_states[0] == transition.from_state;
                if is_stay {
                    // Check if body has cross-boundary operations
                    let has_cross_boundary = transition
                        .body
                        .as_ref()
                        .map(|body| has_cross_boundary_ops(body))
                        .unwrap_or(false);
                    !has_cross_boundary
                } else {
                    false
                }
            },
            origin: transition_origin,
            span,
            fails: transition.fails.clone(),
        };
        flow_transition_ids.push(id.clone());
        if transitions.insert(id.clone(), resolved).is_some() {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: duplicate canonical transition '{}::{}({})'",
                    (id.flow).0,
                    id.event,
                    id.source.name
                ),
                span,
            ));
        }
        if transition.to_states.len() > 1 {
            backend_requirements.push(CapabilityRequirement {
                requirement_id: "FLOW-MULTI-001",
                capability: "flow.multi_target",
                flow: flow_id.clone(),
                span,
            });
        }
    }
    let mut max_children = None;
    let mut mailbox_depth = None;
    for annotation in &flow.annotations {
        match &annotation.kind {
            crate::ast::FlowAnnotationKind::MaxChildren(n) => max_children = Some(*n),
            crate::ast::FlowAnnotationKind::MailboxDepth(n) => mailbox_depth = Some(*n),
            crate::ast::FlowAnnotationKind::Sparse => {}
        }
    }
    let resolved_flow = ResolvedFlow {
        node_id: flow_node_id.clone(),
        id: flow_id.clone(),
        generic_parameters,
        states,
        transitions: flow_transition_ids,
        max_children,
        mailbox_depth,
        persistent_fields: flow.persistent_fields.clone(),
        origin: resolve_named_origin(
            ResolvedItemKind::Flow,
            qualified_name,
            &flow_node_id,
            flow.meta,
            flow_span,
            errors,
        ),
    };
    if flows.insert(flow_id.clone(), resolved_flow).is_some() {
        errors.push(Diagnostic::error(
            format!(
                "TOOL-RESOLUTION-001: duplicate canonical flow '{}'",
                flow_id.0
            ),
            flow_span,
        ));
    }
}

pub(crate) fn extern_block_key(block: &crate::ast::ExternBlock) -> String {
    let mut symbols = block
        .funcs
        .iter()
        .map(|func| func.name.as_str())
        .collect::<Vec<_>>();
    symbols.sort_unstable();
    format!(
        "{}:{}",
        block.abi,
        if symbols.is_empty() {
            "empty".to_string()
        } else {
            symbols.join("+")
        }
    )
}

fn resolve_origin(origin: AstOrigin, parent: &NodeId, span: Span) -> Origin {
    match origin {
        AstOrigin::User => Origin::User(span),
        AstOrigin::Desugared(rule) => Origin::Desugared {
            parent: parent.clone(),
            rule: rule.to_string(),
            span,
        },
        AstOrigin::PrototypeFallback(rule) => Origin::PrototypeFallback {
            parent: parent.clone(),
            rule: rule.to_string(),
            span,
        },
        AstOrigin::RuntimeSystem(rule) => Origin::RuntimeSystem {
            parent: parent.clone(),
            rule: rule.to_string(),
            span,
        },
    }
}

fn collect_program_call_sites(
    file: &File,
    functions: &HashMap<NodeId, ResolvedFunction>,
    extern_blocks: &HashMap<NodeId, ResolvedExternBlock>,
    actors: &HashMap<NodeId, ResolvedActor>,
    impls: &HashMap<NodeId, ResolvedImpl>,
    _transitions: &HashMap<TransitionId, ResolvedTransition>,
    out: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    // Full audit 2026-08-05 (#13): these fact tables were built over raw
    // HashMap iteration — bare-name collisions resolved by nondeterministic
    // last-wins. Every catalog is now iterated in sorted node-id order.
    // Qualified `Actor.method` / `Type.method` keys are recorded alongside
    // bare names; resolve_call_callee prefers the qualified key, and bare
    // entries take a deterministic first-in-sorted-order signature when
    // several definitions share the name (see the admission note below).
    let mut function_info: HashMap<String, (usize, Vec<String>, String)> = HashMap::new();
    let mut function_entries = functions.values().collect::<Vec<_>>();
    function_entries.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for function in function_entries {
        function_info.insert(
            function.qualified_name.clone(),
            (
                function.params.len(),
                function.effects.clone(),
                crate::core::fmt_type(&function.ret),
            ),
        );
    }
    let mut extern_info: HashMap<String, (usize, String)> = HashMap::new();
    let mut extern_entries = extern_blocks.values().collect::<Vec<_>>();
    extern_entries.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for block in extern_entries {
        for sig in CheckedProgram::extern_signatures_sorted(block) {
            extern_info
                .entry(sig.name.clone())
                .or_insert_with(|| (sig.params.len(), sig.ret.clone()));
        }
        // Keep names even if signature missing (defensive).
        for func in &block.funcs {
            extern_info
                .entry(func.clone())
                .or_insert((0, "unit".into()));
        }
    }
    let mut method_info: HashMap<String, (usize, Vec<String>, String)> = HashMap::new();
    let mut bare_method_signatures: HashMap<String, Vec<(usize, Vec<String>, String)>> =
        HashMap::new();
    let mut actor_entries = actors.values().collect::<Vec<_>>();
    actor_entries.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for actor in actor_entries {
        for method in &actor.method_signatures {
            let signature = (
                method.params.len(),
                method.effects.clone(),
                method.ret.clone(),
            );
            bare_method_signatures
                .entry(method.name.clone())
                .or_default()
                .push(signature.clone());
            method_info.insert(
                format!("{}.{}", actor.qualified_name, method.name),
                signature,
            );
        }
    }
    let mut impl_entries = impls.values().collect::<Vec<_>>();
    impl_entries.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for impl_def in impl_entries {
        for method in &impl_def.method_signatures {
            let signature = (
                method.params.len(),
                method.effects.clone(),
                method.ret.clone(),
            );
            bare_method_signatures
                .entry(method.name.clone())
                .or_default()
                .push(signature.clone());
            method_info.insert(format!("{}.{}", impl_def.type_name, method.name), signature);
        }
    }
    // Bare-name admission: the bare entry takes the FIRST signature in sorted
    // catalog order (deterministic), replacing raw-HashMap last-wins. When
    // several actors/impls define the same bare method name with conflicting
    // signatures the bare entry is therefore marked ambiguous by comment, not
    // by shape — but it must stay PRESENT: the entry feeds the call-site
    // KIND facts (ResolvedCallKind::Method), and dropping it would push
    // receiver-typed method calls into the Unknown arm. `lower_method_call`
    // re-resolves the actual callable from the receiver type, so the choice
    // among conflicting entries does not change call semantics — it only
    // removes the nondeterminism.
    let mut bare_names = bare_method_signatures.into_iter().collect::<Vec<_>>();
    bare_names.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, signatures) in bare_names {
        if let Some(first) = signatures.into_iter().next() {
            method_info.insert(name, first);
        }
    }
    let ids = NodeIdBuilder::new(&file.sources);
    let function_qualified_names = top_level_function_qualified_names(&file.items, &file.sources);
    for item in &file.items {
        collect_item_call_sites(
            item,
            &ids,
            &file.sources,
            &function_qualified_names,
            &function_info,
            &extern_info,
            &method_info,
            out,
            errors,
        );
    }
}

/// Resolve the one conservative qualified-call spelling supported during the
/// file-merge migration: `use path as alias; alias::name(...)`.  The parser
/// represents `alias::name` as a field projection, so the surface collector
/// initially records `alias.name`.  Rebind that call site to the canonical
/// function node selected by source provenance; no bare-name or import-order
/// guess is allowed when more than one source exports the same name.
fn install_qualified_import_targets(
    file: &File,
    functions: &HashMap<NodeId, ResolvedFunction>,
    call_sites: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    let mut imports = HashMap::new();
    for import in &file.imports {
        let alias = import
            .alias
            .clone()
            .or_else(|| import.path.first().cloned());
        let Some(alias) = alias else { continue };
        imports.insert(alias, import.path.join("::"));
    }
    for site in call_sites.values_mut() {
        let Some((alias, name)) = site.callee.split_once('.') else {
            continue;
        };
        let Some(module_path) = imports.get(alias) else {
            continue;
        };
        let mut candidates = functions
            .values()
            .filter(|function| {
                function.qualified_name.rsplit("::").next() == Some(name)
                    && file
                        .sources
                        .key(function.origin.user_span().source_id)
                        .and_then(crate::module_system::module_path_from_source_key)
                        .is_some_and(|source_path| {
                            crate::module_system::module_path_matches_import_path(
                                &source_path,
                                module_path,
                            )
                        })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let [function] = candidates.as_slice() else {
            // Missing and ambiguous exports have already been diagnosed by
            // the checker or the legacy merge gate. Leave the call-site fact
            // unresolved so lowerers fail closed instead of choosing a name.
            continue;
        };
        site.callee = format!("{alias}::{name}");
        site.kind = ResolvedCallKind::Function;
        site.expected_argc = Some(function.params.len());
        site.effects = function.effects.clone();
        site.ret = Some(crate::core::fmt_type(&function.ret));
        site.target_function = Some(function.node_id.clone());
    }
    let _ = errors;
}

fn collect_param_default_call_sites(
    params: &[crate::ast::Param],
    owner: &NodeId,
    context: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
    out: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    for param in params {
        let Some(default) = &param.default_value else {
            continue;
        };
        let role = format!("{context}.{}", stable_id_fragment(&param.name));
        collect_expr_call_sites(
            default,
            owner,
            &format!("{role}.default"),
            fallback,
            ids,
            functions,
            externs,
            methods,
            out,
            errors,
        );
    }
}

fn collect_func_call_sites(
    function: &crate::ast::FuncDef,
    owner: &NodeId,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
    out: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    let span = declaration_span(function.meta, fallback);
    collect_param_default_call_sites(
        &function.params,
        owner,
        "parameter",
        span,
        ids,
        functions,
        externs,
        methods,
        out,
        errors,
    );
    collect_block_call_sites(
        &function.body,
        owner,
        "body",
        span,
        ids,
        functions,
        externs,
        methods,
        out,
        errors,
    );
}

fn collect_item_call_sites(
    item: &Item,
    ids: &NodeIdBuilder<'_>,
    sources: &SourceRegistry,
    function_qualified_names: &HashMap<(String, String), String>,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
    out: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    match item {
        Item::Func(function) => {
            let qualified =
                top_level_function_qualified_name(function, sources, function_qualified_names);
            let owner = NodeId(format!("function:{qualified}"));
            collect_func_call_sites(
                function,
                &owner,
                function.meta.span,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Item::Actor(actor) => {
            let qualified = actor.name.clone();
            let actor_owner = NodeId(format!("actor:{qualified}"));
            let actor_span = declaration_span(actor.meta, actor.meta.span);
            for field in &actor.fields {
                let Some(initializer) = &field.init else {
                    continue;
                };
                let role = format!("field.{}", stable_id_fragment(&field.name));
                collect_expr_call_sites(
                    initializer,
                    &actor_owner,
                    &format!("{role}.initializer"),
                    actor_span,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
            for method in &actor.methods {
                let owner = NodeId(format!("function:{qualified}::{}", method.name));
                collect_func_call_sites(
                    method, &owner, actor_span, ids, functions, externs, methods, out, errors,
                );
            }
        }
        Item::Trait(trait_def) => {
            let qualified = trait_def.name.clone();
            let trait_owner = NodeId(format!("trait:{qualified}"));
            let trait_span = declaration_span(trait_def.meta, trait_def.meta.span);
            for method in &trait_def.methods {
                let signature =
                    method_signature_key(&method.name, &method.params, method.ret.as_ref());
                let method_owner = NodeId(format!(
                    "{}/method:{}:{:016x}",
                    trait_owner.0,
                    stable_id_fragment(&method.name),
                    stable_text_hash(&signature)
                ));
                collect_param_default_call_sites(
                    &method.params,
                    &method_owner,
                    "parameter",
                    trait_span,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Item::Impl(impl_def) => {
            let qualified = impl_qualified_name(
                &impl_def.trait_name,
                &impl_def.trait_args,
                &impl_def.type_name,
            );
            let impl_span = declaration_span(impl_def.meta, impl_def.meta.span);
            for method in &impl_def.methods {
                let owner = impl_method_owner(&qualified, method);
                collect_func_call_sites(
                    method, &owner, impl_span, ids, functions, externs, methods, out, errors,
                );
            }
        }
        Item::ExternBlock(block) => {
            let qualified = extern_block_key(block).clone();
            let block_owner = NodeId(format!("extern:{qualified}"));
            let block_span = declaration_span(block.meta, block.meta.span);
            for function in &block.funcs {
                let function_owner = extern_function_owner(&block_owner, function);
                if let Some(requires) = &function.requires {
                    collect_expr_call_sites(
                        requires,
                        &function_owner,
                        "requires",
                        block_span,
                        ids,
                        functions,
                        externs,
                        methods,
                        out,
                        errors,
                    );
                }
                if let Some(ensures) = &function.ensures {
                    collect_expr_call_sites(
                        ensures,
                        &function_owner,
                        "ensures",
                        block_span,
                        ids,
                        functions,
                        externs,
                        methods,
                        out,
                        errors,
                    );
                }
            }
        }
        Item::Const {
            meta, name, value, ..
        } => {
            let owner = NodeId(format!("constant:{}", name.clone()));
            collect_expr_call_sites(
                value,
                &owner,
                "value",
                declaration_span(*meta, meta.span),
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Item::Flow(flow) => {
            let qualified = flow.name.clone();
            let flow_span = declaration_span(flow.meta, flow.meta.span);
            for transition in &flow.transitions {
                let owner = NodeId(format!(
                    "transition:{}::{}::{}",
                    qualified, transition.name, transition.from_state
                ));
                let transition_span = declaration_span(transition.meta, flow_span);
                collect_param_default_call_sites(
                    &transition.params,
                    &owner,
                    "parameter",
                    transition_span,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
                if let Some(body) = &transition.body {
                    collect_block_call_sites(
                        body,
                        &owner,
                        "body",
                        transition_span,
                        ids,
                        functions,
                        externs,
                        methods,
                        out,
                        errors,
                    );
                }
            }
        }
        _ => {}
    }
}

fn collect_block_call_sites(
    block: &[Stmt],
    owner: &NodeId,
    context: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
    out: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    let roles = stmt_sibling_roles(context, block);
    for (index, stmt) in block.iter().enumerate() {
        collect_stmt_call_sites(
            stmt,
            owner,
            &roles[index],
            fallback,
            ids,
            functions,
            externs,
            methods,
            out,
            errors,
        );
    }
}

type ResolvedCalleeFacts = (
    String,
    ResolvedCallKind,
    Option<usize>,
    Vec<String>,
    Option<String>,
);

fn record_expr_call_site(
    expr: &Expr,
    method_receiver: Option<&Expr>,
    owner: &NodeId,
    role: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    argc: usize,
    callee: ResolvedCalleeFacts,
    out: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    let (callee_name, kind, expected_argc, effects, ret) = callee;
    let span = expr_span(expr).unwrap_or(fallback);
    let ast_meta = expr.meta();
    let ast_origin = ast_meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User);
    // This is deliberately the exact same builder input as collect_expr_meta:
    // a call-site ID is the ID of its semantic Expr node, not a parallel ID.
    let node_id = ids.anonymous(
        owner,
        expr_kind(expr),
        role,
        expr_span(expr),
        ast_origin,
        errors,
    );
    let origin_parent = explicit_origin_parent(
        &node_id,
        ast_origin,
        ast_meta.map(|meta| meta.parent).unwrap_or_default(),
        owner,
        None,
        span,
        errors,
    );
    let call_site = ResolvedCallSite {
        node_id: node_id.clone(),
        owner: owner.0.clone(),
        callee: callee_name,
        method_receiver: method_receiver.map(|receiver| {
            let receiver_meta = receiver.meta();
            ids.anonymous(
                owner,
                expr_kind(receiver),
                &format!("{role}.callee.inner"),
                receiver_meta
                    .and_then(ast_meta_anchor)
                    .map(|(span, _)| span),
                receiver_meta
                    .map(|meta| meta.origin)
                    .unwrap_or(AstOrigin::User),
                errors,
            )
        }),
        argc,
        expected_argc,
        effects,
        ret,
        kind,
        target_function: None,
        resolved_callee: None,
        origin: resolve_origin(ast_origin, &origin_parent, span),
    };
    if out.insert(node_id.clone(), call_site).is_some() {
        errors.push(Diagnostic::error(
            format!(
                "TOOL-RESOLUTION-001: duplicate canonical call-site NodeId '{}'",
                node_id.0
            ),
            span,
        ));
    }
}

/// Close syntax-collected call-site records against Checker-issued FFI
/// declaration-root calls. Those calls do not live in a callable body's
/// `resolved_calls` map, so their exact Extern target must be installed from
/// the validated source-root handle here rather than left as `None` for a
/// later consumer to guess from the symbol spelling.
fn bind_checked_ffi_source_call_sites(program: &mut CheckedProgram) -> Vec<Diagnostic> {
    let roots = program.ffi_source_roots.clone();
    let mut errors = Vec::new();
    let mut bindings = Vec::new();

    for root in &roots {
        for call in root.calls() {
            let context = match program.checked_ffi_source_call_context(root, call) {
                Ok(context) => context,
                Err(source_errors) => {
                    errors.extend(source_errors.into_iter().map(|error| {
                        Diagnostic::error(
                            error.message,
                            root.origin()
                                .map_or(Span::UNKNOWN, |origin| origin.user_span()),
                        )
                    }));
                    continue;
                }
            };
            let crate::core::ir::ResolvedExprKind::Call(resolved_call) = &context.call.kind else {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: Checker FFI root call '{}' is no longer a typed Call",
                        call.call_node().0
                    ),
                    context.call.origin.user_span(),
                ));
                continue;
            };
            let expected_callee =
                crate::core::ir::ResolvedCallee::Extern(call.external_declaration().clone());
            if resolved_call.callee != expected_callee {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: Checker FFI root call '{}' resolved to a different extern declaration",
                        call.call_node().0
                    ),
                    context.call.origin.user_span(),
                ));
                continue;
            }

            let sites = program
                .call_sites
                .values()
                .filter(|site| site.node_id == *call.call_node())
                .collect::<Vec<_>>();
            let [site] = sites.as_slice() else {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: Checker FFI root call '{}' has {} call-site records for its exact NodeId",
                        call.call_node().0,
                        sites.len()
                    ),
                    context.call.origin.user_span(),
                ));
                continue;
            };

            let mut mismatches = Vec::new();
            if site.owner != root.owner().0 {
                mismatches.push("owner");
            }
            if site.node_id != *call.call_node()
                || !program.call_sites.contains_key(call.call_node())
            {
                mismatches.push("NodeId/key");
            }
            if site.kind != ResolvedCallKind::Extern {
                mismatches.push("call kind");
            }
            if site.callee != context.declaration.symbol {
                mismatches.push("extern symbol");
            }
            if site.target_function.is_some() {
                mismatches.push("function target");
            }
            if site.origin != context.call.origin {
                mismatches.push("source origin/span");
            }
            if !site.checked_ffi_source_arity_matches(
                resolved_call.arguments.len(),
                context.declaration,
            ) {
                mismatches.push("argument count/arity");
            }
            if site
                .resolved_callee
                .as_ref()
                .is_some_and(|existing| existing != &expected_callee)
            {
                mismatches.push("preexisting typed callee");
            }
            if !mismatches.is_empty() {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: Checker FFI root call '{}' disagrees with its exact call-site identity in: {}",
                        call.call_node().0,
                        mismatches.join(", ")
                    ),
                    context.call.origin.user_span(),
                ));
                continue;
            }

            bindings.push((call.call_node().clone(), expected_callee));
        }
    }

    if !errors.is_empty() {
        return errors;
    }
    for (call_node, expected_callee) in bindings {
        if let Some(site) = program.call_sites.get_mut(&call_node) {
            site.resolved_callee = Some(expected_callee);
        }
    }
    errors
}

fn collect_stmt_call_sites(
    stmt: &Stmt,
    owner: &NodeId,
    role: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
    out: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    match stmt.unlocated() {
        Stmt::Let {
            init: Some(expr), ..
        }
        | Stmt::Return(Some(expr))
        | Stmt::Break(Some(expr))
        | Stmt::Expr(expr)
        | Stmt::Drop(expr)
        | Stmt::Requires(expr, _)
        | Stmt::Ensures(expr, _)
        | Stmt::Invariant(expr, _)
        | Stmt::SharedLet { init: expr, .. } => {
            let syntax_role = match stmt.unlocated() {
                Stmt::Let { .. } | Stmt::SharedLet { .. } => "initializer",
                Stmt::Return(_) | Stmt::Break(_) => "value",
                _ => "expression",
            };
            collect_expr_call_sites(
                expr,
                owner,
                &format!("{role}.{syntax_role}"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Stmt::If { cond, then_, else_ } => {
            collect_expr_call_sites(
                cond,
                owner,
                &format!("{role}.condition"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_block_call_sites(
                then_,
                owner,
                &format!("{role}.then"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            if let Some(block) = else_ {
                collect_block_call_sites(
                    block,
                    owner,
                    &format!("{role}.else"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Stmt::IfLet {
            init, then_, else_, ..
        } => {
            collect_expr_call_sites(
                init,
                owner,
                &format!("{role}.init"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_block_call_sites(
                then_,
                owner,
                &format!("{role}.then"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            if let Some(block) = else_ {
                collect_block_call_sites(
                    block,
                    owner,
                    &format!("{role}.else"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Stmt::While { cond, body } => {
            collect_expr_call_sites(
                cond,
                owner,
                &format!("{role}.condition"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_block_call_sites(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Stmt::WhileLet { init, body, .. } => {
            collect_expr_call_sites(
                init,
                owner,
                &format!("{role}.initializer"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_block_call_sites(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Stmt::Loop(body)
        | Stmt::Block(body)
        | Stmt::Arena(body)
        | Stmt::Unsafe(body)
        | Stmt::IeeeFloat(body)
        | Stmt::OnFailure(body)
        | Stmt::Parasteps(body)
        | Stmt::Defer(body) => {
            collect_block_call_sites(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Stmt::For { iterable, body, .. } => {
            collect_expr_call_sites(
                iterable,
                owner,
                &format!("{role}.iterable"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_block_call_sites(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Stmt::Assign { target, value } => {
            collect_expr_call_sites(
                target,
                owner,
                &format!("{role}.target"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_expr_call_sites(
                value,
                owner,
                &format!("{role}.value"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Stmt::Pinned { expr, body, .. } => {
            collect_expr_call_sites(
                expr,
                owner,
                &format!("{role}.expression"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_block_call_sites(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Stmt::Func(function) => {
            let nested_owner = nested_function_owner(owner, function);
            let nested_fallback = function.meta.span.with_source(fallback.source_id);
            collect_func_call_sites(
                function,
                &nested_owner,
                nested_fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Stmt::Math(exprs) => {
            let child_roles = expr_sibling_roles(&format!("{role}.math"), exprs);
            for index in 0..exprs.len() {
                let child_role = &child_roles[index];
                collect_expr_call_sites(
                    &exprs[index],
                    owner,
                    child_role,
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        _ => {}
    }
}

fn collect_expr_call_sites(
    expr: &Expr,
    owner: &NodeId,
    role: &str,
    fallback: Span,
    ids: &NodeIdBuilder<'_>,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
    out: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    match expr.unlocated() {
        Expr::Call(callee, args) => {
            let method_receiver = match callee.unlocated() {
                Expr::Field(receiver, _) => Some(receiver.as_ref()),
                _ => None,
            };
            record_expr_call_site(
                expr,
                method_receiver,
                owner,
                role,
                fallback,
                ids,
                args.len(),
                resolve_call_callee(callee, functions, externs, methods),
                out,
                errors,
            );
            collect_expr_call_sites(
                callee,
                owner,
                &format!("{role}.callee"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            let child_roles = expr_sibling_roles(&format!("{role}.argument"), args);
            for index in 0..args.len() {
                let child_role = &child_roles[index];
                collect_expr_call_sites(
                    &args[index],
                    owner,
                    child_role,
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Expr::Binary(_, left, right) | Expr::Index(left, right) => {
            collect_expr_call_sites(
                left,
                owner,
                &format!("{role}.left"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_expr_call_sites(
                right,
                owner,
                &format!("{role}.right"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Expr::Unary(_, inner)
        | Expr::Field(inner, _)
        | Expr::Try(inner)
        | Expr::OptionalChain(inner, _)
        | Expr::Spawn(inner)
        | Expr::Await(inner)
        | Expr::QuoteInterpolate(inner)
        | Expr::TypeOf(inner)
        | Expr::Old(inner)
        | Expr::TupleIndex(inner, _)
        | Expr::NamedArg(_, inner)
        | Expr::Cast(inner, _) => {
            collect_expr_call_sites(
                inner,
                owner,
                &format!("{role}.inner"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            let child_roles = expr_sibling_roles(&format!("{role}.element"), items);
            for index in 0..items.len() {
                let child_role = &child_roles[index];
                collect_expr_call_sites(
                    &items[index],
                    owner,
                    child_role,
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Expr::Match(scrutinee, arms) => {
            collect_expr_call_sites(
                scrutinee,
                owner,
                &format!("{role}.scrutinee"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            let arm_role_batch = match_arm_roles(&format!("{role}.arm"), arms);
            for (index, arm) in arms.iter().enumerate() {
                let arm_role = &arm_role_batch[index];
                if let Some(guard) = &arm.guard {
                    collect_expr_call_sites(
                        guard,
                        owner,
                        &format!("{arm_role}.guard"),
                        fallback,
                        ids,
                        functions,
                        externs,
                        methods,
                        out,
                        errors,
                    );
                }
                collect_expr_call_sites(
                    &arm.body,
                    owner,
                    &format!("{arm_role}.body"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Expr::Record { fields, rest, .. } => {
            for field in fields {
                let field_role = format!("{role}.field.{}", stable_id_fragment(&field.name));
                collect_expr_call_sites(
                    &field.value,
                    owner,
                    &format!("{field_role}.value"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
            if let Some(rest) = rest {
                collect_expr_call_sites(
                    rest,
                    owner,
                    &format!("{role}.rest"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Expr::Block(block) | Expr::Comptime(block) | Expr::Quote(block) | Expr::Arena(block) => {
            collect_block_call_sites(
                block,
                owner,
                &format!("{role}.block"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Expr::Lambda { params, body, .. } => {
            for param in params {
                if let Some(default) = &param.default_value {
                    collect_expr_call_sites(
                        default,
                        owner,
                        &format!(
                            "{role}.parameter.{}.default",
                            stable_id_fragment(&param.name)
                        ),
                        fallback,
                        ids,
                        functions,
                        externs,
                        methods,
                        out,
                        errors,
                    );
                }
            }
            collect_block_call_sites(
                body,
                owner,
                &format!("{role}.body"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
        }
        Expr::If { cond, then_, else_ } => {
            collect_expr_call_sites(
                cond,
                owner,
                &format!("{role}.condition"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_block_call_sites(
                then_,
                owner,
                &format!("{role}.then"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            if let Some(block) = else_ {
                collect_block_call_sites(
                    block,
                    owner,
                    &format!("{role}.else"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            collect_expr_call_sites(
                expr,
                owner,
                &format!("{role}.value"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            collect_expr_call_sites(
                iter,
                owner,
                &format!("{role}.iterable"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            if let Some(guard) = guard {
                collect_expr_call_sites(
                    guard,
                    owner,
                    &format!("{role}.guard"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Expr::SliceExpr { target, start, end } => {
            collect_expr_call_sites(
                target,
                owner,
                &format!("{role}.target"),
                fallback,
                ids,
                functions,
                externs,
                methods,
                out,
                errors,
            );
            if let Some(start) = start {
                collect_expr_call_sites(
                    start,
                    owner,
                    &format!("{role}.start"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
            if let Some(end) = end {
                collect_expr_call_sites(
                    end,
                    owner,
                    &format!("{role}.end"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Expr::Turbofish(name, _, args) => {
            record_expr_call_site(
                expr,
                None,
                owner,
                role,
                fallback,
                ids,
                args.len(),
                resolve_named_call_callee(name, functions, externs, methods),
                out,
                errors,
            );
            let child_roles = expr_sibling_roles(&format!("{role}.argument"), args);
            for index in 0..args.len() {
                let child_role = &child_roles[index];
                collect_expr_call_sites(
                    &args[index],
                    owner,
                    child_role,
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Expr::MapLiteral { entries } => {
            let entry_roles = map_entry_roles(&format!("{role}.entry"), entries);
            for (index, (key, value)) in entries.iter().enumerate() {
                let entry_role = &entry_roles[index];
                collect_expr_call_sites(
                    key,
                    owner,
                    &format!("{entry_role}.key"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
                collect_expr_call_sites(
                    value,
                    owner,
                    &format!("{entry_role}.value"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
            }
        }
        Expr::Literal(lit) => {
            if let crate::ast::Lit::FString(parts) = lit {
                for (part_index, part) in parts.iter().enumerate() {
                    if let FStringPart::Interp(expr) = part {
                        let child_role =
                            interpolation_role(&format!("{role}.interpolation"), parts, part_index);
                        collect_expr_call_sites(
                            expr,
                            owner,
                            &child_role,
                            fallback,
                            ids,
                            functions,
                            externs,
                            methods,
                            out,
                            errors,
                        );
                    }
                }
            }
        }
        Expr::Ident(_) | Expr::TypeInfo(_) => {}
        Expr::Located { .. } => unreachable!("Expr::unlocated returned Located"),
    }
}

fn resolve_call_callee(
    callee: &Expr,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
) -> ResolvedCalleeFacts {
    match callee.unlocated() {
        Expr::Ident(name) => resolve_named_call_callee(name, functions, externs, methods),
        Expr::Field(obj, field) => {
            let base = match obj.unlocated() {
                Expr::Ident(name) => name.clone(),
                _ => "_".into(),
            };
            let qualified = format!("{base}.{field}");
            if let Some((arity, effects, ret)) =
                methods.get(&qualified).or_else(|| methods.get(field))
            {
                (
                    qualified,
                    ResolvedCallKind::Method,
                    Some(*arity),
                    effects.clone(),
                    Some(ret.clone()),
                )
            } else {
                (qualified, ResolvedCallKind::Unknown, None, Vec::new(), None)
            }
        }
        _ => (
            "<expr>".into(),
            ResolvedCallKind::Unknown,
            None,
            Vec::new(),
            None,
        ),
    }
}

/// RECORD-LIN-001 (0.1.9, Phase B): whether a resolved type is linear, used
/// to compute `linear_record_names`. Recurses through containers and nested
/// user records/unions (cycle-guarded for self-referential record types).
fn resolved_ty_is_linear(
    program: &CheckedProgram,
    types: &crate::core::ResolvedTypeTable,
    ty: &crate::core::ResolvedTypeId,
    substitutions: &BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    visiting: &mut BTreeSet<String>,
) -> bool {
    match types.get(ty) {
        Some(crate::core::ResolvedType::Capability(_))
        | Some(crate::core::ResolvedType::FlowStateSet { .. }) => true,
        Some(crate::core::ResolvedType::GenericParameter(parameter)) => {
            let key = format!("generic:{}", parameter.0);
            let Some(replacement) = substitutions.get(parameter) else {
                return false;
            };
            if !visiting.insert(key.clone()) {
                return false;
            }
            let linear =
                resolved_ty_is_linear(program, types, replacement, substitutions, visiting);
            visiting.remove(&key);
            linear
        }
        Some(crate::core::ResolvedType::Nominal { is_linear, .. }) if *is_linear => true,
        Some(crate::core::ResolvedType::Nominal {
            item, arguments, ..
        }) => {
            // Nominal identities are `type:<qualified-name>` NodeIds; a
            // display-name lookup cannot find them and used to skip every
            // named record/union field walk. Key by the canonical identity.
            let node_id = NodeId(item.as_str().to_owned());
            if let Some(td) = program.type_defs().get(&node_id) {
                if matches!(td.kind, ResolvedTypeKind::Alias) {
                    if !visiting.insert(item.as_str().to_string()) {
                        return false;
                    }
                    let linear = program
                        .resolved_type_target(&td.node_id)
                        .is_some_and(|target| {
                            let mut alias_substitutions = substitutions.clone();
                            for ((_, parameter), argument) in
                                td.generic_parameters.iter().zip(arguments.iter())
                            {
                                alias_substitutions.insert(parameter.clone(), argument.clone());
                            }
                            resolved_ty_is_linear(
                                program,
                                types,
                                target,
                                &alias_substitutions,
                                visiting,
                            )
                        });
                    visiting.remove(item.as_str());
                    return linear;
                }
                if matches!(td.kind, ResolvedTypeKind::Record | ResolvedTypeKind::Union) {
                    if !visiting.insert(item.as_str().to_string()) {
                        // cycle: assume non-linear (self-reference is not
                        // itself the carrier of the linear resource).
                        return false;
                    }
                    let mut field_substitutions = substitutions.clone();
                    for ((_, parameter), argument) in
                        td.generic_parameters.iter().zip(arguments.iter())
                    {
                        field_substitutions.insert(parameter.clone(), argument.clone());
                    }
                    let linear = td.fields.iter().any(|(field_name, _)| {
                        td.field_ids
                            .get(field_name)
                            .and_then(|fid| program.resolved_field_types().get(fid))
                            .is_some_and(|field_ty| {
                                resolved_ty_is_linear(
                                    program,
                                    types,
                                    field_ty,
                                    &field_substitutions,
                                    visiting,
                                )
                            })
                    });
                    visiting.remove(item.as_str());
                    return linear;
                }
            }
            // Builtin container (List/Map/Set), enum, or generic record:
            // linear iff any type argument is linear.
            arguments
                .iter()
                .any(|arg| resolved_ty_is_linear(program, types, arg, substitutions, visiting))
        }
        Some(crate::core::ResolvedType::Newtype { inner, .. }) => {
            resolved_ty_is_linear(program, types, inner, substitutions, visiting)
        }
        Some(crate::core::ResolvedType::Tuple(elements)) => elements
            .iter()
            .any(|element| resolved_ty_is_linear(program, types, element, substitutions, visiting)),
        Some(crate::core::ResolvedType::Option(inner)) => {
            resolved_ty_is_linear(program, types, inner, substitutions, visiting)
        }
        Some(crate::core::ResolvedType::Result { ok, error }) => {
            resolved_ty_is_linear(program, types, ok, substitutions, visiting)
                || resolved_ty_is_linear(program, types, error, substitutions, visiting)
        }
        Some(crate::core::ResolvedType::Array { element, .. }) => {
            resolved_ty_is_linear(program, types, element, substitutions, visiting)
        }
        Some(crate::core::ResolvedType::Slice(inner)) => {
            resolved_ty_is_linear(program, types, inner, substitutions, visiting)
        }
        Some(crate::core::ResolvedType::CBuffer(inner)) => {
            resolved_ty_is_linear(program, types, inner, substitutions, visiting)
        }
        _ => false,
    }
}

/// RECORD-LIN-001: collect qualified names of user records, unions, and
/// transparent aliases whose payloads transitively contain a linear resource.
fn compute_linear_record_names(program: &CheckedProgram) -> BTreeSet<String> {
    let types = program.resolved_types();
    let mut names = BTreeSet::new();
    for type_def in program.type_defs().values() {
        if !matches!(
            type_def.kind,
            ResolvedTypeKind::Record | ResolvedTypeKind::Union | ResolvedTypeKind::Alias
        ) {
            continue;
        }
        let mut visiting = BTreeSet::new();
        let substitutions = BTreeMap::new();
        let linear =
            match type_def.kind {
                ResolvedTypeKind::Alias => program
                    .resolved_type_target(&type_def.node_id)
                    .is_some_and(|target| {
                        resolved_ty_is_linear(program, types, target, &substitutions, &mut visiting)
                    }),
                ResolvedTypeKind::Record | ResolvedTypeKind::Union => {
                    type_def.fields.iter().any(|(field_name, _)| {
                        type_def
                            .field_ids
                            .get(field_name)
                            .and_then(|fid| program.resolved_field_types().get(fid))
                            .is_some_and(|field_ty| {
                                resolved_ty_is_linear(
                                    program,
                                    types,
                                    field_ty,
                                    &substitutions,
                                    &mut visiting,
                                )
                            })
                    })
                }
                ResolvedTypeKind::Newtype | ResolvedTypeKind::Enum => false,
            };
        if linear {
            names.insert(type_def.qualified_name.clone());
        }
    }
    names
}

fn resolve_named_call_callee(
    name: &str,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
) -> ResolvedCalleeFacts {
    // The checked-unpack source forms are builtin only when the Checker did
    // not resolve a user function with the same name (check_call gives user
    // globals precedence over builtins). Keep the syntax-only call-site
    // catalog aligned for these newly registered names.
    if matches!(name, "unpack_i32" | "unpack_string") {
        if let Some((arity, effects, ret)) = functions.get(name) {
            return (
                name.to_string(),
                ResolvedCallKind::Function,
                Some(*arity),
                effects.clone(),
                Some(ret.clone()),
            );
        }
    }
    // Keep the same precedence as Checker::check_call: language builtins are
    // resolved before flattened stdlib functions with the same surface name.
    if crate::core::builtins::is_builtin_callable(name)
        || crate::core::builtins::is_language_intrinsic_callable(name)
        || crate::core::builtins::is_language_constructor(name)
    {
        (
            name.to_string(),
            ResolvedCallKind::Builtin,
            None,
            Vec::new(),
            None,
        )
    } else if let Some((arity, effects, ret)) = functions.get(name) {
        (
            name.to_string(),
            ResolvedCallKind::Function,
            Some(*arity),
            effects.clone(),
            Some(ret.clone()),
        )
    } else if let Some((arity, ret)) = externs.get(name) {
        (
            name.to_string(),
            ResolvedCallKind::Extern,
            Some(*arity),
            Vec::new(),
            Some(ret.clone()),
        )
    } else if let Some((arity, effects, ret)) = methods.get(name) {
        (
            name.to_string(),
            ResolvedCallKind::Method,
            Some(*arity),
            effects.clone(),
            Some(ret.clone()),
        )
    } else {
        (
            name.to_string(),
            ResolvedCallKind::Unknown,
            None,
            Vec::new(),
            None,
        )
    }
}

fn format_session_type(ty: &crate::ast::SessionType) -> String {
    match ty.unlocated() {
        crate::ast::SessionType::Send(payload, cont) => {
            format!(
                "!{} . {}",
                crate::core::fmt_type(payload),
                format_session_type(cont)
            )
        }
        crate::ast::SessionType::Recv(payload, cont) => {
            format!(
                "?{} . {}",
                crate::core::fmt_type(payload),
                format_session_type(cont)
            )
        }
        crate::ast::SessionType::Dual(inner) => format!("dual({})", format_session_type(inner)),
        crate::ast::SessionType::Name(name) => name.clone(),
        crate::ast::SessionType::End => "end".into(),
        crate::ast::SessionType::Located { .. } => unreachable!("unlocated session type"),
    }
}

fn materialize_const_value(expr: &crate::ast::Expr) -> ResolvedConstValue {
    match expr.unlocated() {
        crate::ast::Expr::Literal(lit) => match lit {
            crate::ast::Lit::Int(v) => ResolvedConstValue::Int(*v),
            crate::ast::Lit::Float(v) => ResolvedConstValue::Float(*v),
            crate::ast::Lit::Bool(v) => ResolvedConstValue::Bool(*v),
            crate::ast::Lit::String(v) => ResolvedConstValue::String(v.clone()),
            crate::ast::Lit::Unit => ResolvedConstValue::Unit,
            crate::ast::Lit::FString(_) => ResolvedConstValue::Complex,
        },
        crate::ast::Expr::Unary(crate::ast::UnOp::Neg, inner) => {
            match materialize_const_value(inner) {
                ResolvedConstValue::Int(v) => ResolvedConstValue::Int(-v),
                ResolvedConstValue::Float(v) => ResolvedConstValue::Float(-v),
                other => other,
            }
        }
        _ => ResolvedConstValue::Complex,
    }
}

type EphemeralExpressionTypes = BTreeMap<NodeId, BTreeMap<ExpressionTypeKey, ZonkedTy>>;
type StableExpressionTypes = BTreeMap<NodeId, BTreeMap<NodeId, ZonkedTy>>;
type CanonicalFunctionArtifacts = (
    crate::core::ResolvedTypeTable,
    BTreeMap<NodeId, crate::core::ResolvedSignature>,
    BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    BTreeMap<NodeId, ResolvedVariantSchema>,
    BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    BTreeMap<NodeId, Vec<crate::core::ResolvedTypeId>>,
    BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    BTreeMap<NodeId, crate::core::ResolvedSignature>,
);

fn canonical_reference_binding_type(initializer: &ZonkedTy) -> Result<ZonkedTy, String> {
    let ty = Type::Ref(None, Box::new(initializer.as_type().clone()));
    ZonkedTy::from_resolved(ty).map_err(|error| error.to_string())
}

fn canonical_shared_binding_type(
    kind: crate::ast::SharedKind,
    initializer: &ZonkedTy,
) -> Result<ZonkedTy, String> {
    let ty = match kind {
        crate::ast::SharedKind::Shared => Type::Shared(Box::new(initializer.as_type().clone())),
        crate::ast::SharedKind::Weak => match initializer.as_type().unlocated() {
            Type::Shared(target) => Type::Weak(target.clone()),
            other => {
                return Err(format!(
                    "weak binding initializer is not shared: {}",
                    crate::core::fmt_type(other)
                ));
            }
        },
    };
    ZonkedTy::from_resolved(ty).map_err(|error| error.to_string())
}

/// 0.39.136 perf: single-pass index over `node_meta` replacing two O(n²)
/// patterns in the stabilizers below (per-owner full scans + per-missing-key
/// full rescans made large expressions quadratic in the check phase —
/// 20k-element literals took 20 s; now linear).
///
/// Semantics preserved from the previous implementation:
///  * owner membership = node-id string starting with "{owner}/"
///    (ids without `/` — the declaration node itself — are excluded);
///  * same-owner duplicate keys keep LAST-insert-wins mapping and emit
///    pairwise TOOL-RESOLUTION-001 diagnostics in encounter order;
///  * a missing key consults every meta carrying that key regardless of
///    owner ("foreign"): exactly one foreign hit ⇒ silently skipped
///    (callee-owned default re-checked at a call site); otherwise the
///    caller emits its no-stable-NodeId diagnostic.
struct ExpressionKeyIndex<'a> {
    /// owner id → (expression_key → same-owner nodes, in node_meta order).
    /// Ownership is PREFIX-hierarchical ("fn:a/fn:b/expr…" belongs to both
    /// "fn:a" and "fn:b" scopes), so membership keeps the exact
    /// `starts_with("{owner}/")` relation of the previous implementation.
    by_owner: HashMap<&'a str, HashMap<&'a ExpressionTypeKey, Vec<&'a NodeId>>>,
    /// key → (prefix-owner when matched, node) for every meta carrying a key.
    all_by_key: HashMap<&'a ExpressionTypeKey, Vec<(Option<&'a str>, &'a NodeId)>>,
}

impl<'a> ExpressionKeyIndex<'a> {
    fn build<'e>(program: &'a CheckedProgram, owners: impl Iterator<Item = &'e NodeId>) -> Self
    where
        'e: 'a,
    {
        // One pass over node_meta. 0.39.136 perf: ownership was resolved by
        // testing EVERY owner prefix per meta — O(metas × owners), which
        // exploded on actor-dense files (12k owners × ~200k metas ≈ billions
        // of `starts_with`). An owner's prefix `{o}/` matches a node id
        // exactly when it equals one of the node id's `/`-delimited prefixes,
        // so per meta only its OWN depth-many boundaries need a hash lookup:
        // O(metas × nesting depth + owners).
        //
        // Semantics preserved verbatim: every matching owner is recorded in
        // `by_owner`; `matched` keeps the LAST match in the owners'
        // (BTreeMap-sorted) iteration order, i.e. the lexicographic maximum.
        let mut owner_by_prefix: HashMap<String, &'a str> = HashMap::new();
        for owner in owners {
            owner_by_prefix.insert(format!("{}/", owner.0), owner.0.as_str());
        }
        let mut by_owner: HashMap<&'a str, HashMap<&'a ExpressionTypeKey, Vec<&'a NodeId>>> =
            HashMap::new();
        let mut all_by_key: HashMap<&'a ExpressionTypeKey, Vec<(Option<&'a str>, &'a NodeId)>> =
            HashMap::new();
        for meta in program.node_meta.values() {
            let Some(key) = &meta.expression_key else {
                continue;
            };
            let nid = meta.node_id.0.as_str();
            let mut matched: Option<&'a str> = None;
            for (boundary, ch) in nid.char_indices() {
                if ch != '/' {
                    continue;
                }
                let prefix = &nid[..boundary + 1];
                if let Some(&owner_id) = owner_by_prefix.get(prefix) {
                    by_owner
                        .entry(owner_id)
                        .or_default()
                        .entry(key)
                        .or_default()
                        .push(&meta.node_id);
                    if matched.map_or(true, |current| owner_id > current) {
                        matched = Some(owner_id);
                    }
                }
            }
            all_by_key
                .entry(key)
                .or_default()
                .push((matched, &meta.node_id));
        }
        Self {
            by_owner,
            all_by_key,
        }
    }

    /// Same-owner nodes for `key` in encounter order; the LAST entry is the
    /// authoritative mapping (insert-overwrite semantics).
    fn owned(&self, owner: &str, key: &ExpressionTypeKey) -> &[&'a NodeId] {
        self.by_owner
            .get(owner)
            .and_then(|m| m.get(key))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Nodes carrying `key` whose owner differs from `owner`.
    fn foreign<'slf>(
        &'slf self,
        owner: &'slf str,
        key: &'slf ExpressionTypeKey,
    ) -> impl Iterator<Item = &'a NodeId> + 'slf {
        self.all_by_key
            .get(key)
            .into_iter()
            .flatten()
            .filter(move |(mo, _)| mo.map_or(true, |matched| matched != owner))
            .map(|(_, n)| *n)
    }

    /// Emit pairwise duplicate diagnostics for every same-owner collision,
    /// mirroring the previous BTreeMap insert chain (`prev` = most recent
    /// existing entry for each successive insert).
    fn report_duplicates(
        &self,
        program: &CheckedProgram,
        owner: &str,
        errors: &mut Vec<Diagnostic>,
    ) {
        // 0.39.136 perf: direct bucket hit — the old full-map scan visited
        // every owner per call (O(owners²) across stabilize).
        let Some(bucket) = self.by_owner.get(owner) else {
            return;
        };
        {
            for (_key, nodes) in bucket.iter() {
                if nodes.len() < 2 {
                    continue;
                }
                for pair in nodes.windows(2) {
                    let (prev, new) = (pair[0], pair[1]);
                    let span = program
                        .node_meta
                        .get(new)
                        .map(|meta| meta.origin.user_span())
                        .unwrap_or(Span::UNKNOWN);
                    errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: callable '{}' expression key maps to both '{}' and '{}'",
                        owner, prev.0, new.0
                    ),
                    span,
                ));
                }
            }
        }
    }
}

/// Resolve provisional direct `.1 as bool` casts to canonical source nodes.
/// Inference records only the narrow syntax/type candidate; ownership does
/// not authorize it until the exact node can be joined to a Checker receipt.
fn stabilize_candidate_expression_nodes(
    program: &CheckedProgram,
    candidates: &BTreeMap<NodeId, BTreeSet<ExpressionTypeKey>>,
) -> Result<BTreeMap<NodeId, BTreeSet<NodeId>>, Vec<Diagnostic>> {
    if candidates.is_empty() {
        return Ok(BTreeMap::new());
    }
    let index = ExpressionKeyIndex::build(program, candidates.keys());
    let mut stable = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();
    let mut errors = Vec::new();
    for (owner, keys) in candidates {
        for key in keys {
            let matches = index.owned(&owner.0, key);
            if matches.len() != 1 {
                let span = program
                    .node_meta
                    .get(owner)
                    .map(|meta| meta.origin.user_span())
                    .unwrap_or(Span::UNKNOWN);
                errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0200,
                    "explicit Any-to-bool cast has no unique checked source identity",
                    span,
                ));
                continue;
            }
            stable
                .entry(owner.clone())
                .or_default()
                .insert((*matches[0]).clone());
        }
    }
    if errors.is_empty() {
        Ok(stable)
    } else {
        Err(errors)
    }
}

/// The inference exception above is provisional. Every candidate must join a
/// same-owner Checker V2 unpack receipt whose DynamicAny source and Bool
/// target were collected only after the GetAny receiver's persistent Set
/// lineage selected that exact payload type. In particular, `.1` syntax on an
/// ordinary tuple does not authorize an Any downcast.
fn validate_ordinary_map_owned_any_v2_bool_cast_receipts(
    program: &CheckedProgram,
    candidates: &BTreeMap<NodeId, BTreeSet<NodeId>>,
) -> Result<(), Vec<Diagnostic>> {
    if candidates.is_empty() {
        return Ok(());
    }
    let mut errors = Vec::new();
    for (owner, casts) in candidates {
        let receipt = program
            .resource_analyses
            .get(owner)
            .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref())
            .filter(|receipt| {
                receipt.profile_version == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && &receipt.owner == owner
            });
        for cast in casts {
            let authorized = receipt.is_some_and(|receipt| {
                receipt.unpack_receipts.iter().any(|unpack| {
                    &unpack.cast == cast
                        && unpack.profile_version
                            == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                        && unpack.conversion_kind
                            == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                        && matches!(
                            program.resolved_types.get(&unpack.conversion_from),
                            Some(crate::core::ResolvedType::DynamicAny { .. })
                        )
                        && matches!(
                            program.resolved_types.get(&unpack.conversion_to),
                            Some(crate::core::ResolvedType::Primitive(
                                crate::core::PrimitiveType::Bool
                            ))
                        )
                })
            });
            if !authorized {
                let span = program
                    .node_meta
                    .get(cast)
                    .map(|meta| meta.origin.user_span())
                    .unwrap_or_else(|| {
                        program
                            .node_meta
                            .get(owner)
                            .map(|meta| meta.origin.user_span())
                            .unwrap_or(Span::UNKNOWN)
                    });
                errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0200,
                    "explicit Any-to-bool cast requires an exact Checker receipt for a Map/Any GetAny whose persistent Set lineage selected bool",
                    span,
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// The only non-primitive DynamicAny unpack target in the closed Map/Any
/// profile is a concrete, non-generic record with one owned `text: string`
/// field. MIR rechecks the ABI and glue details; the Checked boundary must
/// already keep tuples, Option/Result, builtin Record, and arbitrary records
/// out of the typed program.
fn is_canonical_map_any_label_target(
    program: &CheckedProgram,
    target: &crate::core::ResolvedTypeId,
) -> bool {
    let Some(crate::core::ResolvedType::Nominal {
        item, arguments, ..
    }) = program.resolved_types.get(target)
    else {
        return false;
    };
    if !arguments.is_empty() {
        return false;
    }
    let Some(definition) = program.type_defs.get(&NodeId(item.as_str().to_owned())) else {
        return false;
    };
    if definition.kind != ResolvedTypeKind::Record
        || !definition.generic_parameters.is_empty()
        || definition.fields.len() != 1
        || definition.fields[0].0 != "text"
    {
        return false;
    }
    let Some(field) = definition.field_ids.get("text") else {
        return false;
    };
    program.resolved_field_types.get(field)
        == Some(&crate::core::ir::primitive_type_id(
            crate::core::PrimitiveType::String,
        ))
}

/// DynamicAny-to-nonprimitive casts are not a general cast form. Inference
/// records provisional candidates; this finalizer admits one only when it
/// joins bijectively to the same callable's exact V2 GetAny unpack receipt,
/// its typed Map call/result owner, and the pair's explicit whole-value Drop.
/// The CFG collector independently requires the target payload to be selected
/// on that GetAny root's persistent Set lineage.
fn validate_ordinary_map_owned_any_v2_nonprimitive_cast_receipts(
    program: &CheckedProgram,
    candidates: &BTreeMap<NodeId, BTreeSet<NodeId>>,
) -> Result<(), Vec<Diagnostic>> {
    use crate::core::ir::{
        CheckedConversionKind, ResolvedCallee, ResolvedExprKind, ResolvedPatternKind,
        ResolvedProjection, ResolvedStmtKind,
    };
    use crate::core::ownership::{
        OrdinaryMapOwnedAnyV2ActionKind, OrdinaryMapOwnedAnyV2ProfileVersion,
        OrdinaryMapOwnedAnyV2ResultSettlementKind,
    };

    if candidates.is_empty() {
        return Ok(());
    }

    fn e0200(program: &CheckedProgram, owner: &NodeId, cast: &NodeId, message: &str) -> Diagnostic {
        Diagnostic::error_code(
            crate::diagnostic::codes::E0200,
            message,
            program
                .node_meta
                .get(cast)
                .map(|meta| meta.origin.user_span())
                .or_else(|| {
                    program
                        .node_meta
                        .get(owner)
                        .map(|meta| meta.origin.user_span())
                })
                .unwrap_or(Span::UNKNOWN),
        )
    }

    fn direct_binding<'a>(
        body: &'a crate::core::ResolvedBody,
        local: &crate::core::ResolvedLocalId,
    ) -> Vec<(&'a crate::core::ResolvedStmt, &'a crate::core::ResolvedExpr)> {
        body.root
            .statements
            .iter()
            .filter_map(|statement| {
                let ResolvedStmtKind::Bind {
                    pattern:
                        crate::core::ResolvedPattern {
                            kind:
                                ResolvedPatternKind::Binding {
                                    local: bound,
                                    by_reference: None,
                                },
                            ..
                        },
                    initializer: Some(initializer),
                } = &statement.kind
                else {
                    return None;
                };
                (bound == local).then_some((statement, initializer))
            })
            .collect()
    }

    fn direct_binding_in_block<'a>(
        block: &'a crate::core::ResolvedBlock,
        local: &crate::core::ResolvedLocalId,
    ) -> Vec<(&'a crate::core::ResolvedStmt, &'a crate::core::ResolvedExpr)> {
        block
            .statements
            .iter()
            .filter_map(|statement| {
                let ResolvedStmtKind::Bind {
                    pattern:
                        crate::core::ResolvedPattern {
                            kind:
                                ResolvedPatternKind::Binding {
                                    local: bound,
                                    by_reference: None,
                                },
                            ..
                        },
                    initializer: Some(initializer),
                } = &statement.kind
                else {
                    return None;
                };
                (bound == local).then_some((statement, initializer))
            })
            .collect()
    }

    fn exact_result_arm<'a>(
        arms: &'a [crate::core::ir::MatchArm],
        expected_id: &crate::core::NodeId,
        expected_variant: &str,
    ) -> Option<&'a crate::core::ir::MatchArm> {
        let [first, second] = arms else {
            return None;
        };
        let matching = [first, second]
            .into_iter()
            .filter(|arm| arm.node_id == *expected_id)
            .collect::<Vec<_>>();
        let [arm] = matching.as_slice() else {
            return None;
        };
        (arm.guard.is_none()
            && matches!(&arm.pattern.kind, ResolvedPatternKind::Constructor { variant, .. }
                if variant.0 == format!("builtin:variant:Result::{expected_variant}")))
        .then_some(*arm)
    }

    fn computed_chain_result_paths<'a>(
        body: &'a crate::core::ResolvedBody,
        branch: &crate::core::ownership::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchReceipt,
    ) -> Option<(
        &'a crate::core::ResolvedBlock,
        crate::core::NodeId,
        crate::core::NodeId,
    )> {
        let Some(root_result) = body.root.result.as_deref() else {
            return None;
        };
        if root_result.node_id != branch.map_new_match {
            return None;
        }
        let ResolvedExprKind::Match {
            scrutinee: new_scrutinee,
            arms: new_arms,
        } = &root_result.kind
        else {
            return None;
        };
        if new_scrutinee.node_id != branch.map_new_operation
            || new_arms
                .iter()
                .map(|arm| arm.node_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
                != std::collections::BTreeSet::from([
                    branch.map_new_ok_arm.clone(),
                    branch.map_new_err_arm.clone(),
                ])
        {
            return None;
        }
        let Some(new_ok) = exact_result_arm(new_arms, &branch.map_new_ok_arm, "Ok") else {
            return None;
        };
        let Some(new_err) = exact_result_arm(new_arms, &branch.map_new_err_arm, "Err") else {
            return None;
        };
        let ResolvedExprKind::Block(new_ok_block) = &new_ok.body.kind else {
            return None;
        };
        let ResolvedExprKind::Block(new_err_block) = &new_err.body.kind else {
            return None;
        };
        let Some(set_match) = new_ok_block.result.as_deref() else {
            return None;
        };
        if set_match.node_id != branch.map_set_match {
            return None;
        }
        let ResolvedExprKind::Match {
            scrutinee: set_scrutinee,
            arms: set_arms,
        } = &set_match.kind
        else {
            return None;
        };
        if set_scrutinee.node_id != branch.map_set_operation
            || set_arms
                .iter()
                .map(|arm| arm.node_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
                != std::collections::BTreeSet::from([
                    branch.map_set_ok_arm.clone(),
                    branch.map_set_err_arm.clone(),
                ])
        {
            return None;
        }
        let Some(set_ok) = exact_result_arm(set_arms, &branch.map_set_ok_arm, "Ok") else {
            return None;
        };
        let Some(set_err) = exact_result_arm(set_arms, &branch.map_set_err_arm, "Err") else {
            return None;
        };
        let ResolvedExprKind::Block(success_block) = &set_ok.body.kind else {
            return None;
        };
        let ResolvedExprKind::Block(set_err_block) = &set_err.body.kind else {
            return None;
        };
        let Some(new_err_result) = new_err_block.result.as_deref() else {
            return None;
        };
        let Some(set_err_result) = set_err_block.result.as_deref() else {
            return None;
        };
        Some((
            success_block,
            new_err_result.node_id.clone(),
            set_err_result.node_id.clone(),
        ))
    }

    let mut errors = Vec::new();
    let route_admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    let computed_owner_chain_owner = NodeId("function:main".into());
    let candidate_count = candidates.values().map(BTreeSet::len).sum::<usize>();
    let exact_computed_owner_chain_candidate_set = route_admission
        .ordinary_map_owned_any_v2
        .is_computed_string_key_owner_chain()
        && candidates.len() == 1
        && candidates
            .get(&computed_owner_chain_owner)
            .is_some_and(|casts| casts.len() == 1 && candidate_count == 1);
    for (owner, casts) in candidates {
        let Some(body) = program.resolved_bodies.get(owner) else {
            errors.extend(casts.iter().map(|cast| {
                e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive cast has no exact same-owner GetAny receipt",
                )
            }));
            continue;
        };
        let cast_expressions =
            match body.cast_expressions(&program.resolved_types, &program.resolved_type_targets) {
                Ok(expressions) => expressions,
                Err(body_errors) => {
                    errors.extend(body_errors.into_iter().map(|error| {
                        Diagnostic::error(
                            format!("TOOL-RESOLUTION-001: {error}"),
                            body.root.origin.user_span(),
                        )
                    }));
                    continue;
                }
            };
        let cfg = program
            .resource_analyses
            .get(owner)
            .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref())
            .filter(|cfg| {
                cfg.owner == *owner
                    && cfg.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
            });
        let Some(cfg) = cfg else {
            errors.extend(casts.iter().map(|cast| {
                e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive cast requires an exact same-owner V2 GetAny receipt",
                )
            }));
            continue;
        };

        let mut claimed_receipts = BTreeSet::new();
        for cast in casts {
            let Some(expression) = cast_expressions.get(cast) else {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive cast has no unique resolved cast expression",
                ));
                continue;
            };
            let ResolvedExprKind::Cast { value, conversion } = &expression.kind else {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive candidate is not a resolved cast",
                ));
                continue;
            };
            if !is_canonical_map_any_label_target(program, &expression.ty) {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive cast target is outside the canonical Label profile",
                ));
                continue;
            }
            let ResolvedExprKind::Load(source_place) = &value.kind else {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive cast source must be direct tuple field 1",
                ));
                continue;
            };
            let [ResolvedProjection::Tuple {
                index: 1,
                ty: field_type,
            }] = source_place.projections.as_slice()
            else {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive cast source must be direct tuple field 1",
                ));
                continue;
            };
            let pair = &source_place.base;
            let pair_type = body.locals.get(pair).map(|local| &local.ty);
            let exact_pair_type = pair_type.is_some_and(|pair_type| {
                matches!(
                    program.resolved_types.get(pair_type),
                    Some(crate::core::ResolvedType::Tuple(fields))
                        if fields.len() == 2
                            && matches!(program.resolved_types.get(&fields[0]), Some(crate::core::ResolvedType::Primitive(crate::core::PrimitiveType::Bool)))
                            && fields[1] == *field_type
                            && matches!(program.resolved_types.get(&fields[1]), Some(crate::core::ResolvedType::DynamicAny { capability }) if capability == "type.dynamic_value")
                )
            });
            if !exact_pair_type
                || value.ty != *field_type
                || conversion.kind != CheckedConversionKind::DynamicAnyUnpack
                || conversion.from != *field_type
                || conversion.to != expression.ty
            {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive cast does not match its direct GetAny field and target types",
                ));
                continue;
            }

            let matching = cfg
                .unpack_receipts
                .iter()
                .filter(|receipt| receipt.cast == *cast)
                .collect::<Vec<_>>();
            let [receipt] = matching.as_slice() else {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive cast requires exactly one matching GetAny unpack receipt",
                ));
                continue;
            };
            if !claimed_receipts.insert(receipt.cast.clone()) {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "GetAny unpack receipt is claimed by more than one nonprimitive cast candidate",
                ));
                continue;
            }

            let matching_actions = cfg
                .actions
                .iter()
                .filter(|action| {
                    action.kind == OrdinaryMapOwnedAnyV2ActionKind::GetAny
                        && action.point == receipt.operation
                })
                .collect::<Vec<_>>();
            let matching_results = cfg
                .result_receipts
                .iter()
                .filter(|result| result.operation == receipt.operation)
                .collect::<Vec<_>>();
            let (Some(action), Some(result)) = (matching_actions.first(), matching_results.first())
            else {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "GetAny unpack receipt is missing its exact operation or result-owner receipt",
                ));
                continue;
            };
            let static_key_authorities = cfg
                .static_key_get_receipts
                .iter()
                .filter(|authority| authority.unpack.cast == *cast)
                .collect::<Vec<_>>();
            let exact_static_selected_payload = matches!(
                static_key_authorities.as_slice(),
                [authority]
                    if authority.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetProfileVersion::V1
                        && authority.owner == *owner
                        && authority.get_action == **action
                        && authority.unpack == **receipt
                        && authority.get_action.key_type.as_ref()
                            == Some(&crate::core::ir::primitive_type_id(
                                crate::core::PrimitiveType::String,
                            ))
                        && authority.get_action.key_static_digest.as_ref().is_some_and(|digest| {
                            digest == &authority.query_digest
                                && digest.len() == 64
                                && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                        })
                        && matches!(
                            &authority.payload_source,
                            crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource::SelectedSet {
                                value_type,
                            } if value_type == &expression.ty
                        )
                        && authority.selected_winning_set_action.as_ref().is_some_and(|set| {
                            set.kind == OrdinaryMapOwnedAnyV2ActionKind::Set
                                && set.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
                                && set.key_type.as_ref()
                                    == Some(&crate::core::ir::primitive_type_id(
                                        crate::core::PrimitiveType::String,
                                    ))
                                && set.key_static_digest.as_deref()
                                    == Some(authority.query_digest.as_str())
                                && set.payload_clone
                                && set.value_type.as_ref() == Some(&expression.ty)
                                && cfg.actions.iter().filter(|action| *action == set).count() == 1
                        })
            );
            let computed_owner_chain_authorities = cfg
                .result_receipts
                .iter()
                .filter_map(|result| {
                    result
                        .computed_string_key_owner_chain
                        .as_deref()
                        .map(|chain| (result, chain))
                })
                .collect::<Vec<_>>();
            let exact_computed_owner_chain_selected_payload =
                if exact_computed_owner_chain_candidate_set
                    && owner == &computed_owner_chain_owner
                    && casts.len() == 1
                {
                    match computed_owner_chain_authorities.as_slice() {
                        [(chain_result, chain)] => {
                            let mut pair_without_chain = (*chain_result).clone();
                            pair_without_chain.computed_string_key_owner_chain = None;
                            let set_action_is_unique = cfg
                                .actions
                                .iter()
                                .filter(|candidate| *candidate == &chain.map_set)
                                .count()
                                == 1;
                            let get_action_is_unique = cfg
                                .actions
                                .iter()
                                .filter(|candidate| {
                                    candidate.kind == OrdinaryMapOwnedAnyV2ActionKind::GetAny
                                        && candidate.point == receipt.operation
                                })
                                .count()
                                == 1;
                            let string_type = crate::core::ir::primitive_type_id(
                                crate::core::PrimitiveType::String,
                            );
                            let get_key_uses = chain
                                .key_uses
                                .iter()
                                .filter(|usage| usage.action == chain.get_any)
                                .collect::<Vec<_>>();
                            let set_key_uses = chain
                                .key_uses
                                .iter()
                                .filter(|usage| usage.action == chain.map_set)
                                .collect::<Vec<_>>();
                            let proof_matches_source = |usage: &crate::core::ownership::OrdinaryMapOwnedAnyV2ComputedStringKeyUseReceipt| {
                                let proof = &usage.value_proof;
                                let root_nodes = proof
                                    .nodes
                                    .iter()
                                    .filter(|node| node.node == proof.expression)
                                    .collect::<Vec<_>>();
                                usage.action_kind == usage.action.kind
                                    && usage.key_node == proof.expression
                                    && usage.key_type == string_type
                                    && proof.expression_type == string_type
                                    && usage.action.key.as_ref() == Some(&usage.key_node)
                                    && usage.action.key_type.as_ref() == Some(&string_type)
                                    && proof.digest
                                        == blake3::hash(&proof.bytes).to_hex().to_string()
                                    && proof.nodes.iter().all(|node| {
                                        node.ty == string_type
                                            && node.digest
                                                == blake3::hash(&node.bytes).to_hex().to_string()
                                    })
                                    && matches!(root_nodes.as_slice(), [root]
                                        if root.ty == string_type
                                            && root.bytes == proof.bytes
                                            && root.digest == proof.digest)
                            };
                            let exact_key_byte_dag = matches!(get_key_uses.as_slice(), [usage]
                                if proof_matches_source(usage)
                                    && usage.action_kind
                                        == OrdinaryMapOwnedAnyV2ActionKind::GetAny)
                                && matches!(set_key_uses.as_slice(), [usage]
                                    if proof_matches_source(usage)
                                        && usage.action_kind
                                            == OrdinaryMapOwnedAnyV2ActionKind::Set)
                                && get_key_uses[0].value_proof.bytes
                                    == set_key_uses[0].value_proof.bytes
                                && get_key_uses[0].value_proof.digest
                                    == set_key_uses[0].value_proof.digest;
                            let (Some(set_value), Some(set_value_type)) =
                                (&chain.map_set.value, &chain.map_set.value_type)
                            else {
                                errors.push(e0200(
                                    program,
                                    owner,
                                    cast,
                                    "computed String-key owner-chain receipt has no exact Set payload",
                                ));
                                continue;
                            };
                            chain.profile_version
                                == crate::core::ownership::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainProfileVersion::V1
                                && chain.owner == *owner
                                && chain.get_any == **action
                                && chain.get_any.kind
                                    == OrdinaryMapOwnedAnyV2ActionKind::GetAny
                                && chain.get_any.point == receipt.operation
                                && chain_result.operation == receipt.operation
                                && chain_result.kind == OrdinaryMapOwnedAnyV2ActionKind::GetAny
                                && pair_without_chain == chain.pair
                                && chain.pair.operation == receipt.operation
                                && chain.pair.result == result.result
                                && chain.pair.result_type == result.result_type
                                && chain.pair.binding == *pair
                                && chain.pair.binding_point == receipt.pair_binding_point
                                && chain.pair.source_local == action.local
                                && chain.pair.source == action.root
                                && action.source_node.as_ref() == Some(&chain.pair.source_node)
                                && action.source_type.as_ref() == Some(&chain.pair.source_type)
                                && chain.pair.key == action.key
                                && chain.pair.key_type == action.key_type
                                && chain.pair.key_local == action.key_local
                                && chain.pair.settlement
                                    == OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                                && chain.pair.settlement_point.as_ref()
                                    == Some(&chain.pair_drop.settlement_point)
                                && chain.pair_drop.local == chain.pair.binding
                                && chain.pair_drop.binding_point == chain.pair.binding_point
                                && chain.pair_drop.binding_type == chain.pair.result_type
                                && chain.pair_snapshot_observation.snapshot_projection
                                    == receipt.projection
                                && chain.pair_snapshot_observation.snapshot_unpack == *cast
                                && chain.pair_snapshot_observation.snapshot_binding_point
                                    == receipt.unpack_binding_point
                                && chain.pair_snapshot_observation.snapshot_local
                                    == receipt.unpack_binding
                                && chain.pair_snapshot_observation.snapshot_source_type
                                    == receipt.conversion_from
                                && chain.pair_snapshot_observation.snapshot_type
                                    == expression.ty
                                && chain.pair_snapshot_observation.snapshot_drop.local
                                    == receipt.unpack_binding
                                && chain.pair_snapshot_observation.snapshot_drop.binding_type
                                    == expression.ty
                                && chain.payload.set_action == chain.map_set
                                && chain.payload.set_value == *set_value
                                && chain.payload.set_value_type == *set_value_type
                                && chain.payload.set_value_type == expression.ty
                                && chain.payload.source.binding_type == expression.ty
                                && exact_key_byte_dag
                                && chain.map_set.kind == OrdinaryMapOwnedAnyV2ActionKind::Set
                                && chain.map_set.value_type.as_ref() == Some(&expression.ty)
                                && chain.map_set.value.as_ref() == Some(set_value)
                                && chain.map_set.payload_clone
                                && cfg.profile_version
                                    == OrdinaryMapOwnedAnyV2ProfileVersion::V2
                                && cfg.unpack_receipts.len() == 1
                                && cfg.result_receipts.len() == 1
                                && chain_result == result
                                && set_action_is_unique
                                && get_action_is_unique
                        }
                        _ => false,
                    }
                } else {
                    false
                };
            let computed_owner_chain = match computed_owner_chain_authorities.as_slice() {
                [(_, chain)] => Some(*chain),
                _ => None,
            };
            let computed_owner_chain_branch =
                computed_owner_chain.and_then(|chain| chain.result_branch.as_deref());
            let computed_owner_chain_source_paths = computed_owner_chain_branch
                .and_then(|branch| computed_chain_result_paths(body, branch));
            let exact_computed_owner_chain_result_branch =
                exact_computed_owner_chain_selected_payload
                    && computed_owner_chain.is_some_and(|chain| {
                        let Some(branch) = chain.result_branch.as_deref() else {
                            return false;
                        };
                        let Some(raw_cfg) = program.callable_cfgs.get(owner) else {
                            return false;
                        };
                        let Some((success_block, new_err_result, set_err_result)) =
                            computed_owner_chain_source_paths.as_ref()
                        else {
                            return false;
                        };
                        let new_error_paths = branch
                            .paths
                            .iter()
                            .filter(|path| {
                                path.kind
                                    == crate::core::ownership::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultPathKind::MapNewErr
                            })
                            .collect::<Vec<_>>();
                        let set_error_paths = branch
                            .paths
                            .iter()
                            .filter(|path| {
                                path.kind
                                    == crate::core::ownership::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultPathKind::MapSetErr
                            })
                            .collect::<Vec<_>>();
                        let success_paths = branch
                            .paths
                            .iter()
                            .filter(|path| {
                                path.kind
                                    == crate::core::ownership::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultPathKind::Success
                            })
                            .collect::<Vec<_>>();
                        let expected_success_drops = BTreeSet::from([
                            branch.initial_root.clone(),
                            branch.populated_root.clone(),
                            chain.map_remove.local.clone(),
                        ]);
                        branch.profile_version
                            == crate::core::ownership::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainResultBranchProfileVersion::V1
                            && branch.owner == *owner
                            && branch.cfg
                                == crate::core::ownership::ManagedAnyCfgReceipt::from_cfg(
                                    raw_cfg, body,
                                )
                            && branch.map_new_operation == chain.map_new.point
                            && body
                                .root
                                .result
                                .as_deref()
                                .is_some_and(|result| result.node_id == branch.map_new_match)
                            && branch.map_set_operation == chain.map_set.point
                            && branch.initial_root == chain.map_new.local
                            && branch.populated_root == chain.map_set.local
                            && branch.paths.len() == 3
                            && new_error_paths.len() == 1
                            && set_error_paths.len() == 1
                            && success_paths.len() == 1
                            && new_error_paths[0].arm_path
                                == [branch.map_new_err_arm.clone()]
                            && new_error_paths[0].result == *new_err_result
                            && new_error_paths[0].drops.is_empty()
                            && set_error_paths[0].arm_path
                                == [
                                    branch.map_new_ok_arm.clone(),
                                    branch.map_set_err_arm.clone(),
                                ]
                            && set_error_paths[0].result == *set_err_result
                            && set_error_paths[0].drops.len() == 1
                            && set_error_paths[0].drops[0].local == branch.initial_root
                            && success_paths[0].arm_path
                                == [
                                    branch.map_new_ok_arm.clone(),
                                    branch.map_set_ok_arm.clone(),
                                ]
                            && success_block
                                .result
                                .as_deref()
                                .is_some_and(|result| success_paths[0].result == result.node_id)
                            && success_paths[0].drops.len() == 3
                            && success_paths[0]
                                .drops
                                .iter()
                                .map(|drop| drop.local.clone())
                                .collect::<BTreeSet<_>>()
                                == expected_success_drops
                            && success_paths[0]
                                .drops
                                .iter()
                                .all(|drop| drop.settlement_place_index == 0)
                    });
            let computed_owner_chain_source_paths = exact_computed_owner_chain_result_branch
                .then_some(computed_owner_chain_source_paths)
                .flatten();
            let exact_selected_payload_authority =
                exact_static_selected_payload || exact_computed_owner_chain_result_branch;
            let call_bindings = direct_binding(body, pair);
            let exact_pair_binding_initializer = |binding: &(
                &crate::core::ResolvedStmt,
                &crate::core::ResolvedExpr,
            )| {
                let (statement, initializer) = binding;
                statement.node_id == receipt.pair_binding_point
                    && initializer.node_id == receipt.operation
                    && initializer.ty == result.result_type
                    && matches!(
                        &initializer.kind,
                        ResolvedExprKind::Call(call)
                            if matches!(&call.callee, ResolvedCallee::Builtin(name) if name.as_str() == "map_get")
                                && call.arguments.len() == 2
                                && matches!(&call.arguments[0].value.kind, ResolvedExprKind::Load(place) if place.base == action.local && place.projections.is_empty())
                                && action.key.as_ref().is_some_and(|key| call.arguments[1].value.node_id == *key)
                    )
            };
            let nested_call_bindings = computed_owner_chain_source_paths
                .as_ref()
                .map(|(block, _, _)| direct_binding_in_block(block, pair))
                .unwrap_or_default();
            let exact_pair_binding = if exact_computed_owner_chain_result_branch {
                call_bindings.is_empty()
                    && nested_call_bindings.len() == 1
                    && nested_call_bindings
                        .iter()
                        .any(exact_pair_binding_initializer)
            } else {
                call_bindings.len() == 1 && call_bindings.iter().any(exact_pair_binding_initializer)
            };
            let unpack_bindings = direct_binding(body, &receipt.unpack_binding);
            let exact_unpack_binding_initializer =
                |binding: &(&crate::core::ResolvedStmt, &crate::core::ResolvedExpr)| {
                    let (statement, initializer) = binding;
                    statement.node_id == receipt.unpack_binding_point
                        && initializer.node_id == *cast
                        && initializer.ty == expression.ty
                        && body
                            .locals
                            .get(&receipt.unpack_binding)
                            .is_some_and(|local| local.ty == expression.ty)
                };
            let nested_unpack_bindings = computed_owner_chain_source_paths
                .as_ref()
                .map(|(block, _, _)| direct_binding_in_block(block, &receipt.unpack_binding))
                .unwrap_or_default();
            let exact_unpack_binding = if exact_computed_owner_chain_result_branch {
                unpack_bindings.is_empty()
                    && nested_unpack_bindings.len() == 1
                    && nested_unpack_bindings
                        .iter()
                        .any(exact_unpack_binding_initializer)
            } else {
                unpack_bindings.len() == 1
                    && unpack_bindings.iter().any(exact_unpack_binding_initializer)
            };
            let pair_drops = body
                .root
                .statements
                .iter()
                .filter(|statement| {
                    statement.node_id == receipt.settlement_point
                        && matches!(
                            &statement.kind,
                            ResolvedStmtKind::Drop(places)
                                if places.len() == 1
                                    && places[0].base == *pair
                                    && places[0].projections.is_empty()
                        )
                })
                .collect::<Vec<_>>();
            let pair_order = body
                .root
                .statements
                .iter()
                .position(|statement| statement.node_id == receipt.pair_binding_point);
            let unpack_order = body
                .root
                .statements
                .iter()
                .position(|statement| statement.node_id == receipt.unpack_binding_point);
            let drop_order = body
                .root
                .statements
                .iter()
                .position(|statement| statement.node_id == receipt.settlement_point);
            let exact_root_pair_drop = pair_drops.len() == 1
                && matches!(
                    (pair_order, unpack_order, drop_order),
                    (Some(pair), Some(unpack), Some(drop)) if pair < unpack && unpack < drop
                );
            let exact_nested_pair_drop = exact_computed_owner_chain_result_branch
                && pair_drops.is_empty()
                && computed_owner_chain
                    .zip(computed_owner_chain_source_paths.as_ref())
                    .is_some_and(|(chain, (success_block, _, _))| {
                        let branch_pair_drops = success_block
                            .statements
                            .iter()
                            .filter(|statement| {
                                statement.node_id == receipt.settlement_point
                                    && matches!(
                                        &statement.kind,
                                        ResolvedStmtKind::Drop(places)
                                            if matches!(places.as_slice(), [place]
                                                if place.base == *pair && place.projections.is_empty())
                                    )
                            })
                            .collect::<Vec<_>>();
                        let pair_order = success_block
                            .statements
                            .iter()
                            .position(|statement| statement.node_id == receipt.pair_binding_point);
                        let found_binding_order = success_block.statements.iter().position(
                            |statement| {
                                statement.node_id
                                    == chain.pair_snapshot_observation.found_binding_point
                            },
                        );
                        let unpack_order = success_block
                            .statements
                            .iter()
                            .position(|statement| statement.node_id == receipt.unpack_binding_point);
                        let drop_order = success_block
                            .statements
                            .iter()
                            .position(|statement| statement.node_id == receipt.settlement_point);
                        let expected_source_uses = BTreeSet::from([
                            chain.pair_snapshot_observation.found_projection.clone(),
                            chain.pair_snapshot_observation.found_base_load.clone(),
                            receipt.projection.clone(),
                            chain.pair_snapshot_observation.snapshot_base_load.clone(),
                            receipt.settlement_point.clone(),
                        ]);
                        chain.pair_drop.local == *pair
                            && chain.pair_drop.binding_point == receipt.pair_binding_point
                            && chain.pair_drop.binding_type == result.result_type
                            && chain.pair_drop.settlement_point == receipt.settlement_point
                            && chain.pair_drop.settlement_place_index == 0
                            && chain.pair_drop.source_uses == expected_source_uses
                            && chain.pair_snapshot_observation.snapshot_projection
                                == receipt.projection
                            && chain.pair_snapshot_observation.snapshot_unpack == *cast
                            && chain.pair_snapshot_observation.snapshot_binding_point
                                == receipt.unpack_binding_point
                            && branch_pair_drops.len() == 1
                            && matches!(
                                (
                                    pair_order,
                                    found_binding_order,
                                    unpack_order,
                                    drop_order,
                                ),
                                (Some(pair), Some(found), Some(unpack), Some(drop))
                                    if pair < found && found < unpack && unpack < drop
                            )
                    },
                );
            let exact_pair_drop = exact_root_pair_drop || exact_nested_pair_drop;
            let exact_result_owner = matching_actions.len() == 1
                && matching_results.len() == 1
                && action.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
                && action.point == receipt.operation
                && action.result_local.as_ref() == Some(pair)
                && action.result_type.as_ref() == pair_type
                && result.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
                && result.kind == OrdinaryMapOwnedAnyV2ActionKind::GetAny
                && result.result == receipt.operation
                && Some(&result.result_type) == pair_type
                && result.binding == *pair
                && result.binding_point == receipt.pair_binding_point
                && result.source_local == action.local
                && result.source == action.root
                && action.source_node.as_ref() == Some(&result.source_node)
                && action.source_type.as_ref() == Some(&result.source_type)
                && action.key.as_ref() == result.key.as_ref()
                && result.settlement == OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                && result.settlement_point.as_ref() == Some(&receipt.settlement_point)
                && exact_selected_payload_authority
                && receipt.profile_version == OrdinaryMapOwnedAnyV2ProfileVersion::V2
                && receipt.cast == *cast
                && receipt.projection == value.node_id
                && receipt.pair_binding == *pair
                && receipt.conversion_kind == conversion.kind
                && receipt.conversion_from == conversion.from
                && receipt.conversion_to == expression.ty
                && exact_pair_binding
                && exact_unpack_binding
                && exact_pair_drop;
            let cfg_point_count = |node: &NodeId| {
                program
                    .callable_cfgs
                    .get(owner)
                    .into_iter()
                    .flat_map(|cfg| cfg.blocks.values())
                    .flat_map(|block| &block.points)
                    .filter(|point| point.source.node == *node)
                    .count()
            };
            let exact_cfg_points = [
                &receipt.operation,
                &receipt.pair_binding_point,
                &receipt.unpack_binding_point,
                &receipt.projection,
                &receipt.cast,
                &receipt.settlement_point,
            ]
            .into_iter()
            .all(|point| cfg_point_count(point) == 1);
            if !exact_result_owner || !exact_cfg_points {
                errors.push(e0200(
                    program,
                    owner,
                    cast,
                    "DynamicAny-to-nonprimitive cast does not match the unique GetAny call, pair binding, cast binding, and explicit pair Drop receipt",
                ));
            }
        }

        for receipt in &cfg.unpack_receipts {
            let nonprimitive_target = !matches!(
                program.resolved_types.get(&receipt.conversion_to),
                Some(crate::core::ResolvedType::Primitive(_))
            );
            if nonprimitive_target && !casts.contains(&receipt.cast) {
                errors.push(e0200(
                    program,
                    owner,
                    &receipt.cast,
                    "nonprimitive GetAny unpack receipt has no unique provisional cast candidate",
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Join the sole direct Checker attempt to the canonical stdlib helper and
/// replay both sides of the cross-call Map/Any ownership bridge.  The receipt
/// is intentionally attached to the caller's ordinary-map-v2 CFG receipt;
/// an attempt alone never authorizes a MIR route.
fn attach_ordinary_map_owned_any_v2_get_or_default_string_receipt(program: &mut CheckedProgram) {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind;

    let candidates = program
        .resource_analyses
        .iter()
        .flat_map(|(owner, analysis)| {
            analysis
                .ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .into_iter()
                .flat_map(move |cfg| {
                    cfg.attempts
                        .iter()
                        .filter(|attempt| attempt.kind == AttemptKind::GetOrDefaultString)
                        .map(move |attempt| (owner.clone(), attempt.clone()))
                })
        })
        .collect::<Vec<_>>();
    let [(caller_owner, attempt)] = candidates.as_slice() else {
        return;
    };
    let Some(caller_body) = program.resolved_bodies.get(caller_owner) else {
        return;
    };
    let Ok(caller_calls) =
        caller_body.call_expressions(&program.resolved_types, &program.resolved_type_targets)
    else {
        return;
    };
    let Some(call_expression) = caller_calls.get(&attempt.point) else {
        return;
    };
    let crate::core::ir::ResolvedExprKind::Call(call) = &call_expression.kind else {
        return;
    };
    let crate::core::ResolvedCallee::Function(helper_owner) = &call.callee else {
        return;
    };

    let canonical_helpers = program
        .functions
        .values()
        .filter(|function| canonical_std_maps_get_or_default(program, &function.node_id))
        .map(|function| function.node_id.clone())
        .collect::<Vec<_>>();
    let [canonical_helper] = canonical_helpers.as_slice() else {
        return;
    };
    if canonical_helper != helper_owner {
        return;
    }

    let mut canonical_call_sites = program
        .call_sites
        .values()
        .filter(|site| {
            site.resolved_callee
                == Some(crate::core::ResolvedCallee::Function(helper_owner.clone()))
        })
        .collect::<Vec<_>>();
    canonical_call_sites.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    if canonical_call_sites.len() != 1
        || canonical_call_sites[0].node_id != attempt.point
        || canonical_call_sites[0].owner != caller_owner.0
        || !ordinary_map_get_or_default_i32_caller_reachable_from_main(program, caller_owner)
    {
        return;
    }

    let Some(receipt) = collect_ordinary_map_owned_any_v2_get_or_default_string_receipt(
        program,
        caller_owner,
        attempt,
        call_expression,
        helper_owner,
    ) else {
        return;
    };
    if let Some(cfg_receipt) = program
        .resource_analyses
        .get_mut(caller_owner)
        .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_mut())
    {
        cfg_receipt.get_or_default_string_receipt = Some(receipt);
    }
}

fn attach_ordinary_map_owned_any_v2_get_or_default_i32_receipt(program: &mut CheckedProgram) {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind;

    let candidates = program
        .resource_analyses
        .iter()
        .flat_map(|(owner, analysis)| {
            analysis
                .ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .into_iter()
                .flat_map(move |cfg| {
                    cfg.attempts
                        .iter()
                        .filter(|attempt| attempt.kind == AttemptKind::GetOrDefaultI32)
                        .map(move |attempt| (owner.clone(), attempt.clone()))
                })
        })
        .collect::<Vec<_>>();
    let [(caller_owner, attempt)] = candidates.as_slice() else {
        return;
    };
    let Some(caller_body) = program.resolved_bodies.get(caller_owner) else {
        return;
    };
    let Ok(caller_calls) =
        caller_body.call_expressions(&program.resolved_types, &program.resolved_type_targets)
    else {
        return;
    };
    let Some(call_expression) = caller_calls.get(&attempt.point) else {
        return;
    };
    let crate::core::ir::ResolvedExprKind::Call(call) = &call_expression.kind else {
        return;
    };
    let crate::core::ResolvedCallee::Function(helper_owner) = &call.callee else {
        return;
    };

    let canonical_helpers = program
        .functions
        .values()
        .filter(|function| canonical_std_maps_get_or_default(program, &function.node_id))
        .map(|function| function.node_id.clone())
        .collect::<Vec<_>>();
    let [canonical_helper] = canonical_helpers.as_slice() else {
        return;
    };
    if canonical_helper != helper_owner {
        return;
    }

    let mut canonical_call_sites = program
        .call_sites
        .values()
        .filter(|site| {
            site.resolved_callee
                == Some(crate::core::ResolvedCallee::Function(helper_owner.clone()))
        })
        .collect::<Vec<_>>();
    canonical_call_sites.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    if canonical_call_sites.len() != 1
        || canonical_call_sites[0].node_id != attempt.point
        || canonical_call_sites[0].owner != caller_owner.0
        || ordinary_map_get_or_default_i32_caller_is_actor_method(program, caller_owner)
        || !ordinary_map_get_or_default_i32_caller_reachable_from_main(program, caller_owner)
    {
        return;
    }

    let Some(receipt) = collect_ordinary_map_owned_any_v2_get_or_default_i32_receipt(
        program,
        caller_owner,
        attempt,
        call_expression,
        helper_owner,
    ) else {
        return;
    };
    if let Some(cfg_receipt) = program
        .resource_analyses
        .get_mut(caller_owner)
        .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_mut())
    {
        cfg_receipt.get_or_default_i32_receipt = Some(receipt);
    }
}

fn canonical_std_maps_get_or_default(program: &CheckedProgram, owner: &NodeId) -> bool {
    let Some(function) = program.functions.get(owner) else {
        return false;
    };
    let span = program
        .node_meta
        .get(owner)
        .map(|meta| meta.origin.user_span())
        .unwrap_or_else(|| function.origin.user_span());
    program
        .source_registry
        .key(span.source_id)
        .is_some_and(|key| key.as_str() == "stdlib:maps.mimi")
        && matches!(
            function.qualified_name.as_str(),
            "get_or_default" | "std::maps::get_or_default"
        )
}

/// Prove that this call owner is in the Checker-resolved executable scope.
/// The scope includes the unique entrypoint and exact Actor method roots;
/// the latter are admitted here only so the route's separate Actor-owner gate
/// can reject them without turning a valid program into an unrelated Checker
/// error. Unreferenced helpers remain outside the scope.
fn ordinary_map_get_or_default_i32_caller_is_actor_method(
    program: &CheckedProgram,
    target_owner: &NodeId,
) -> bool {
    // Receipt attachment runs before ResolvedActorDescriptor materialization.
    // The Checker-owned ResolvedActor declaration already supplies the exact
    // callable identities here, so use that directory rather than guessing
    // from a name prefix or waiting for a later MIR actor plan.
    program.actors.values().any(|actor| {
        actor
            .methods
            .iter()
            .any(|method| target_owner.0 == format!("function:{}::{method}", actor.qualified_name))
    })
}

/// Prove a direct static call path from `main` to the candidate owner. Unknown
/// calls do not create edges, but they also cannot invalidate a separately
/// proven direct path to this owner.
fn ordinary_map_get_or_default_i32_caller_reachable_from_main(
    program: &CheckedProgram,
    target_owner: &NodeId,
) -> bool {
    use crate::core::ir::{ResolvedCallee as C, ResolvedExprKind as E};

    let main = NodeId("function:main".into());
    if program
        .functions
        .values()
        .filter(|function| function.qualified_name == "main")
        .count()
        != 1
        || !program.resolved_bodies.contains_key(&main)
    {
        return false;
    }
    let mut pending = vec![main];
    let mut visited = BTreeSet::new();
    while let Some(owner) = pending.pop() {
        if !visited.insert(owner.clone()) {
            continue;
        }
        if &owner == target_owner {
            return true;
        }
        let Some(body) = program.resolved_bodies.get(&owner) else {
            continue;
        };
        let Ok(calls) =
            body.direct_call_expressions(&program.resolved_types, &program.resolved_type_targets)
        else {
            continue;
        };
        for (call_node, expression) in calls {
            let E::Call(call) = &expression.kind else {
                continue;
            };
            let Some(site) = program.call_sites().get(&call_node) else {
                continue;
            };
            if site.owner != owner.0 || site.resolved_callee.as_ref() != Some(&call.callee) {
                continue;
            }
            if let C::Function(target) = &call.callee {
                if program.functions.contains_key(target)
                    && program.resolved_bodies.contains_key(target)
                {
                    pending.push(target.clone());
                }
            }
        }
    }
    false
}

/// Return the complete callable scope that can participate in a checked
/// GetOrDefault<i32> route. Imported-but-unreached stdlib functions are not
/// executable owners for the route census; Actor method bodies are explicit
/// roots because mailbox dispatch can reach them without an ordinary call
/// edge from `main`. Any callable edge whose target is not statically known
/// keeps this narrow scope unprovable.
pub(crate) fn ordinary_map_get_or_default_i32_executable_scope(
    program: &CheckedProgram,
) -> Option<BTreeSet<NodeId>> {
    use crate::core::ir::{ResolvedCallee as C, ResolvedExprKind as E};

    fn contains_callable_type(
        program: &CheckedProgram,
        ty: &crate::core::ResolvedTypeId,
        visiting: &mut BTreeSet<crate::core::ResolvedTypeId>,
        substitutions: &BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    ) -> bool {
        use crate::core::ResolvedType as T;

        if !visiting.insert(ty.clone()) {
            return false;
        }
        let contains = match program.resolved_types.get(ty) {
            Some(T::Function { .. }) => true,
            Some(T::GenericParameter(parameter)) => {
                substitutions.get(parameter).is_none_or(|argument| {
                    contains_callable_type(program, argument, visiting, substitutions)
                })
            }
            Some(T::Nominal {
                item, arguments, ..
            }) => {
                arguments.iter().any(|argument| {
                    contains_callable_type(program, argument, visiting, substitutions)
                }) || {
                    let identity = NodeId(item.as_str().to_owned());
                    let definition = program.type_defs.get(&identity);
                    let actor = program.actor_descriptors.get(&identity);
                    match (definition, actor) {
                        (Some(definition), _) => {
                            let mut field_substitutions = substitutions.clone();
                            for ((_, parameter), argument) in
                                definition.generic_parameters.iter().zip(arguments.iter())
                            {
                                field_substitutions.insert(parameter.clone(), argument.clone());
                            }
                            definition
                                .field_ids
                                .values()
                                .filter_map(|field| program.resolved_field_types.get(field))
                                .any(|field_type| {
                                    contains_callable_type(
                                        program,
                                        field_type,
                                        visiting,
                                        &field_substitutions,
                                    )
                                })
                                || (definition.kind == ResolvedTypeKind::Enum
                                    && program
                                        .resolved_variants()
                                        .values()
                                        .filter(|variant| variant.owner == identity)
                                        .flat_map(|variant| &variant.members)
                                        .any(|member| {
                                            contains_callable_type(
                                                program,
                                                &member.ty,
                                                visiting,
                                                &field_substitutions,
                                            )
                                        }))
                        }
                        (None, Some(actor)) => actor.fields.iter().any(|field| {
                            contains_callable_type(program, &field.ty, visiting, substitutions)
                        }),
                        (None, None) => {
                            !item.as_str().starts_with("builtin:type:")
                                && !item.as_str().starts_with("state:")
                                && !item.as_str().starts_with("cap:")
                        }
                    }
                }
            }
            Some(
                T::Reference { target, .. }
                | T::Option(target)
                | T::CBuffer(target)
                | T::Ownership { target, .. }
                | T::Newtype { inner: target, .. }
                | T::Array {
                    element: target, ..
                }
                | T::Slice(target)
                | T::RawPointer { target, .. },
            ) => contains_callable_type(program, target, visiting, substitutions),
            Some(T::Result { ok, error }) => {
                contains_callable_type(program, ok, visiting, substitutions)
                    || contains_callable_type(program, error, visiting, substitutions)
            }
            Some(T::Tuple(elements)) => elements
                .iter()
                .any(|element| contains_callable_type(program, element, visiting, substitutions)),
            Some(
                T::Primitive(_)
                | T::FlowStateSet { .. }
                | T::Capability(_)
                | T::Trait { .. }
                | T::DynamicAny { .. },
            ) => false,
            None => true,
        };
        visiting.remove(ty);
        contains
    }

    let main = NodeId("function:main".into());
    let main_functions = program
        .functions
        .values()
        .filter(|function| function.qualified_name == "main")
        .collect::<Vec<_>>();
    if main_functions.len() != 1
        || main_functions[0].node_id != main
        || !program.resolved_bodies.contains_key(&main)
    {
        return None;
    }

    let mut pending = vec![main];
    for descriptor in program.actor_descriptors.values() {
        for method in &descriptor.methods {
            if !program.functions.contains_key(&method.owner)
                || !program.resolved_bodies.contains_key(&method.owner)
            {
                return None;
            }
            pending.push(method.owner.clone());
        }
    }

    let mut reachable = BTreeSet::new();
    while let Some(owner) = pending.pop() {
        if !reachable.insert(owner.clone()) {
            continue;
        }
        let body = program.resolved_bodies.get(&owner)?;
        let calls = body
            .direct_call_expressions(&program.resolved_types, &program.resolved_type_targets)
            .ok()?;
        for (call_node, expression) in &calls {
            let E::Call(call) = &expression.kind else {
                continue;
            };
            let site = program.call_sites().get(call_node)?;
            if site.owner != owner.0 {
                return None;
            }
            let site_callee = site.resolved_callee.as_ref()?;
            if site_callee != &call.callee {
                return None;
            }
            if matches!(site_callee, C::Extern(_) | C::Builtin(_))
                && call.arguments.iter().any(|argument| {
                    contains_callable_type(
                        program,
                        &argument.value.ty,
                        &mut BTreeSet::new(),
                        &BTreeMap::new(),
                    )
                })
            {
                // Builtins and foreign declarations can execute a callback
                // without a canonical body edge. Keep the narrow census
                // closed whenever a callable can cross that boundary.
                return None;
            }
            match site_callee {
                C::Function(target) => {
                    if !program.functions.contains_key(target)
                        || !program.resolved_bodies.contains_key(target)
                    {
                        return None;
                    }
                    pending.push(target.clone());
                }
                C::ActorMethod { actor, method } => {
                    let owners = program
                        .actor_descriptors
                        .get(actor)?
                        .methods
                        .iter()
                        .filter(|candidate| candidate.owner.0 == method.as_str())
                        .map(|candidate| candidate.owner.clone())
                        .collect::<Vec<_>>();
                    let [method_owner] = owners.as_slice() else {
                        return None;
                    };
                    if !program.functions.contains_key(method_owner)
                        || !program.resolved_bodies.contains_key(method_owner)
                    {
                        return None;
                    }
                    pending.push(method_owner.clone());
                }
                C::LocalClosure(_) | C::ProtocolMethod { .. } | C::Transition(_) => return None,
                C::Constructor(_) | C::Extern(_) | C::Builtin(_) => {}
            }
        }
    }
    Some(reachable)
}

/// Return the closed callable scope used by the narrow String specialization.
/// Its call graph and Actor exclusion rules deliberately match the i32
/// profile; the String route performs its own source, receipt, and type proof
/// before it can use this generic static scope calculation.
pub(crate) fn ordinary_map_get_or_default_string_executable_scope(
    program: &CheckedProgram,
) -> Option<BTreeSet<NodeId>> {
    ordinary_map_get_or_default_i32_executable_scope(program)
}

fn validate_ordinary_map_owned_any_v2_get_or_default_i32_receipt(
    program: &CheckedProgram,
) -> Result<(), Vec<Diagnostic>> {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind;
    let canonical_helpers = program
        .functions
        .values()
        .filter(|function| canonical_std_maps_get_or_default(program, &function.node_id))
        .map(|function| function.node_id.clone())
        .collect::<BTreeSet<_>>();
    let provisional_candidates = program
        .resource_analyses
        .iter()
        .flat_map(|(owner, analysis)| {
            analysis
                .ordinary_map_owned_any_v2_operation_attempts
                .iter()
                .filter(|attempt| attempt.kind == AttemptKind::GetOrDefaultI32)
                .map(move |attempt| (owner.clone(), attempt.clone()))
        })
        .collect::<Vec<_>>();
    // Resource lowering records a provisional attempt from source shape
    // alone. It is not yet route authority: a same-spelled local helper must
    // remain an ordinary call unless its resolved target is the canonical
    // stdlib function.
    let candidates = provisional_candidates
        .into_iter()
        .filter(|(owner, attempt)| {
            let Some(body) = program.resolved_bodies.get(owner) else {
                return false;
            };
            let Ok(calls) =
                body.call_expressions(&program.resolved_types, &program.resolved_type_targets)
            else {
                return false;
            };
            matches!(
                calls.get(&attempt.point).map(|expression| &expression.kind),
                Some(crate::core::ir::ResolvedExprKind::Call(call))
                    if matches!(
                        &call.callee,
                        crate::core::ResolvedCallee::Function(target)
                            if canonical_helpers.contains(target)
                    )
            )
        })
        .collect::<Vec<_>>();
    let attached = program
        .resource_analyses
        .iter()
        .flat_map(|(owner, analysis)| {
            analysis
                .ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .and_then(|cfg| cfg.get_or_default_i32_receipt.as_ref())
                .map(|receipt| (owner.clone(), receipt))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() && attached.is_empty() {
        return Ok(());
    }

    let mut errors = Vec::new();
    let error_span = |node: &NodeId| {
        program
            .node_meta
            .get(node)
            .map(|meta| meta.origin.user_span())
            .unwrap_or(Span::UNKNOWN)
    };
    if attached.is_empty() {
        // Unsupported reachable shapes (wrong payload/default, dynamic key,
        // multiple callsites, and so on) stay outside this profile and are
        // rejected by normal source/type checks or route coverage. The
        // dedicated Checker error is reserved for an otherwise exact
        // canonical call that is unreachable from `main`, because such a
        // call must never mint a production route receipt.
        if let [(caller_owner, attempt)] = candidates.as_slice() {
            let expected = (|| {
                let caller_body = program.resolved_bodies.get(caller_owner)?;
                let calls = caller_body
                    .call_expressions(&program.resolved_types, &program.resolved_type_targets)
                    .ok()?;
                let call_expression = calls.get(&attempt.point)?;
                let crate::core::ir::ResolvedExprKind::Call(call) = &call_expression.kind else {
                    return None;
                };
                let crate::core::ResolvedCallee::Function(helper_owner) = &call.callee else {
                    return None;
                };
                let matching_helpers = canonical_helpers
                    .iter()
                    .filter(|helper| *helper == helper_owner)
                    .collect::<Vec<_>>();
                let [canonical_helper] = matching_helpers.as_slice() else {
                    return None;
                };
                let source_calls = program
                    .call_sites
                    .values()
                    .filter(|site| {
                        site.resolved_callee
                            == Some(crate::core::ResolvedCallee::Function(
                                (*canonical_helper).clone(),
                            ))
                    })
                    .collect::<Vec<_>>();
                if source_calls.len() != 1
                    || source_calls[0].node_id != attempt.point
                    || source_calls[0].owner != caller_owner.0
                {
                    return None;
                }
                collect_ordinary_map_owned_any_v2_get_or_default_i32_receipt(
                    program,
                    caller_owner,
                    attempt,
                    call_expression,
                    canonical_helper,
                )
                .map(|receipt| {
                    (
                        receipt,
                        ordinary_map_get_or_default_i32_caller_reachable_from_main(
                            program,
                            caller_owner,
                        ),
                    )
                })
            })();
            if let Some((_, false)) = expected {
                if !ordinary_map_get_or_default_i32_caller_is_actor_method(program, caller_owner) {
                    errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0218,
                        "GetOrDefault<i32> caller is unreachable from main and cannot receive its Checker route receipt",
                        error_span(&attempt.point),
                    ));
                }
            }
        }
        return if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        };
    }

    if let ([(caller_owner, attempt)], [(receipt_owner, receipt)]) =
        (candidates.as_slice(), attached.as_slice())
    {
        let expected = (|| {
            let caller_body = program.resolved_bodies.get(caller_owner)?;
            let calls = caller_body
                .call_expressions(&program.resolved_types, &program.resolved_type_targets)
                .ok()?;
            let call_expression = calls.get(&attempt.point)?;
            let crate::core::ir::ResolvedExprKind::Call(call) = &call_expression.kind else {
                return None;
            };
            let crate::core::ResolvedCallee::Function(helper_owner) = &call.callee else {
                return None;
            };
            let canonical_helpers = program
                .functions
                .values()
                .filter(|function| canonical_std_maps_get_or_default(program, &function.node_id))
                .map(|function| function.node_id.clone())
                .collect::<Vec<_>>();
            let [canonical_helper] = canonical_helpers.as_slice() else {
                return None;
            };
            if helper_owner != canonical_helper {
                return None;
            }
            let source_calls = program
                .call_sites
                .values()
                .filter(|site| {
                    site.resolved_callee
                        == Some(crate::core::ResolvedCallee::Function(helper_owner.clone()))
                })
                .collect::<Vec<_>>();
            if source_calls.len() != 1
                || source_calls[0].node_id != attempt.point
                || source_calls[0].owner != caller_owner.0
                || !ordinary_map_get_or_default_i32_caller_reachable_from_main(
                    program,
                    caller_owner,
                )
            {
                return None;
            }
            collect_ordinary_map_owned_any_v2_get_or_default_i32_receipt(
                program,
                caller_owner,
                attempt,
                call_expression,
                helper_owner,
            )
        })();
        if receipt_owner != caller_owner
            || receipt.caller_owner != *caller_owner
            || receipt.call_node != attempt.point
            || expected.as_ref() != Some(*receipt)
        {
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0218,
                "get_or_default<i32> Checker receipt does not replay the exact caller, helper, and ownership bridge",
                error_span(&attempt.point),
            ));
        }
        if program
            .resource_analyses
            .get(caller_owner)
            .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref())
            .is_none_or(|cfg| !cfg.attempts.contains(attempt))
        {
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0218,
                "get_or_default<i32> attempt is not present in its Checker CFG receipt",
                error_span(&attempt.point),
            ));
        }
    } else {
        let node = candidates
            .first()
            .map(|(_, attempt)| &attempt.point)
            .or_else(|| attached.first().map(|(_, receipt)| &receipt.call_node))
            .cloned()
            .unwrap_or_else(|| NodeId("ordinary-map-get-or-default-i32".into()));
        errors.push(Diagnostic::error_code(
            crate::diagnostic::codes::E0218,
            format!(
                "GetOrDefault<i32> requires exactly one canonical Checker attempt and one cross-call receipt; found {} attempts and {} receipts",
                candidates.len(),
                attached.len()
            ),
            error_span(&node),
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_ordinary_map_owned_any_v2_get_or_default_string_receipt(
    program: &CheckedProgram,
) -> Result<(), Vec<Diagnostic>> {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind;
    let canonical_helpers = program
        .functions
        .values()
        .filter(|function| canonical_std_maps_get_or_default(program, &function.node_id))
        .map(|function| function.node_id.clone())
        .collect::<BTreeSet<_>>();
    let provisional_candidates = program
        .resource_analyses
        .iter()
        .flat_map(|(owner, analysis)| {
            analysis
                .ordinary_map_owned_any_v2_operation_attempts
                .iter()
                .filter(|attempt| attempt.kind == AttemptKind::GetOrDefaultString)
                .map(move |attempt| (owner.clone(), attempt.clone()))
        })
        .collect::<Vec<_>>();
    // Resource lowering records a provisional attempt from source shape
    // alone. It is not yet route authority: a same-spelled local helper must
    // remain an ordinary call unless its resolved target is the canonical
    // stdlib function.
    let candidates = provisional_candidates
        .into_iter()
        .filter(|(owner, attempt)| {
            let Some(body) = program.resolved_bodies.get(owner) else {
                return false;
            };
            let Ok(calls) =
                body.call_expressions(&program.resolved_types, &program.resolved_type_targets)
            else {
                return false;
            };
            matches!(
                calls.get(&attempt.point).map(|expression| &expression.kind),
                Some(crate::core::ir::ResolvedExprKind::Call(call))
                    if matches!(
                        &call.callee,
                        crate::core::ResolvedCallee::Function(target)
                            if canonical_helpers.contains(target)
                    )
            )
        })
        .collect::<Vec<_>>();
    let attached = program
        .resource_analyses
        .iter()
        .flat_map(|(owner, analysis)| {
            analysis
                .ordinary_map_owned_any_v2_cfg_receipt
                .as_ref()
                .and_then(|cfg| cfg.get_or_default_string_receipt.as_ref())
                .map(|receipt| (owner.clone(), receipt))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() && attached.is_empty() {
        return Ok(());
    }

    let mut errors = Vec::new();
    let error_span = |node: &NodeId| {
        program
            .node_meta
            .get(node)
            .map(|meta| meta.origin.user_span())
            .unwrap_or(Span::UNKNOWN)
    };
    if attached.is_empty() {
        // Unsupported reachable shapes (wrong payload/default, dynamic key,
        // multiple callsites, and so on) stay outside this profile and are
        // rejected by normal source/type checks or route coverage. The
        // dedicated Checker error is reserved for an otherwise exact
        // canonical call that is unreachable from `main`, because such a
        // call must never mint a production route receipt.
        if let [(caller_owner, attempt)] = candidates.as_slice() {
            let expected = (|| {
                let caller_body = program.resolved_bodies.get(caller_owner)?;
                let calls = caller_body
                    .call_expressions(&program.resolved_types, &program.resolved_type_targets)
                    .ok()?;
                let call_expression = calls.get(&attempt.point)?;
                let crate::core::ir::ResolvedExprKind::Call(call) = &call_expression.kind else {
                    return None;
                };
                let crate::core::ResolvedCallee::Function(helper_owner) = &call.callee else {
                    return None;
                };
                let matching_helpers = canonical_helpers
                    .iter()
                    .filter(|helper| *helper == helper_owner)
                    .collect::<Vec<_>>();
                let [canonical_helper] = matching_helpers.as_slice() else {
                    return None;
                };
                let source_calls = program
                    .call_sites
                    .values()
                    .filter(|site| {
                        site.resolved_callee
                            == Some(crate::core::ResolvedCallee::Function(
                                (*canonical_helper).clone(),
                            ))
                    })
                    .collect::<Vec<_>>();
                if source_calls.len() != 1
                    || source_calls[0].node_id != attempt.point
                    || source_calls[0].owner != caller_owner.0
                {
                    return None;
                }
                collect_ordinary_map_owned_any_v2_get_or_default_string_receipt(
                    program,
                    caller_owner,
                    attempt,
                    call_expression,
                    canonical_helper,
                )
                .map(|receipt| {
                    (
                        receipt,
                        ordinary_map_get_or_default_i32_caller_reachable_from_main(
                            program,
                            caller_owner,
                        ),
                    )
                })
            })();
            if let Some((_, false)) = expected {
                if !ordinary_map_get_or_default_i32_caller_is_actor_method(program, caller_owner) {
                    errors.push(Diagnostic::error_code(
                        crate::diagnostic::codes::E0218,
                        "get_or_default<string> caller is unreachable from main and cannot receive its Checker route receipt",
                        error_span(&attempt.point),
                    ));
                }
            }
        }
        return if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        };
    }

    if let ([(caller_owner, attempt)], [(receipt_owner, receipt)]) =
        (candidates.as_slice(), attached.as_slice())
    {
        let expected = (|| {
            let caller_body = program.resolved_bodies.get(caller_owner)?;
            let calls = caller_body
                .call_expressions(&program.resolved_types, &program.resolved_type_targets)
                .ok()?;
            let call_expression = calls.get(&attempt.point)?;
            let crate::core::ir::ResolvedExprKind::Call(call) = &call_expression.kind else {
                return None;
            };
            let crate::core::ResolvedCallee::Function(helper_owner) = &call.callee else {
                return None;
            };
            let canonical_helpers = program
                .functions
                .values()
                .filter(|function| canonical_std_maps_get_or_default(program, &function.node_id))
                .map(|function| function.node_id.clone())
                .collect::<Vec<_>>();
            let [canonical_helper] = canonical_helpers.as_slice() else {
                return None;
            };
            if helper_owner != canonical_helper {
                return None;
            }
            let source_calls = program
                .call_sites
                .values()
                .filter(|site| {
                    site.resolved_callee
                        == Some(crate::core::ResolvedCallee::Function(helper_owner.clone()))
                })
                .collect::<Vec<_>>();
            if source_calls.len() != 1
                || source_calls[0].node_id != attempt.point
                || source_calls[0].owner != caller_owner.0
                || !ordinary_map_get_or_default_i32_caller_reachable_from_main(
                    program,
                    caller_owner,
                )
            {
                return None;
            }
            collect_ordinary_map_owned_any_v2_get_or_default_string_receipt(
                program,
                caller_owner,
                attempt,
                call_expression,
                helper_owner,
            )
        })();
        if receipt_owner != caller_owner
            || receipt.caller_owner != *caller_owner
            || receipt.call_node != attempt.point
            || expected.as_ref() != Some(*receipt)
        {
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0218,
                "get_or_default<String> Checker receipt does not replay the exact caller, helper, and ownership bridge",
                error_span(&attempt.point),
            ));
        }
        if program
            .resource_analyses
            .get(caller_owner)
            .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref())
            .is_none_or(|cfg| !cfg.attempts.contains(attempt))
        {
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0218,
                "get_or_default<String> attempt is not present in its Checker CFG receipt",
                error_span(&attempt.point),
            ));
        }
    } else {
        let node = candidates
            .first()
            .map(|(_, attempt)| &attempt.point)
            .or_else(|| attached.first().map(|(_, receipt)| &receipt.call_node))
            .cloned()
            .unwrap_or_else(|| NodeId("ordinary-map-get-or-default-i32".into()));
        errors.push(Diagnostic::error_code(
            crate::diagnostic::codes::E0218,
            format!(
                "GetOrDefault<String> requires exactly one canonical Checker attempt and one cross-call receipt; found {} attempts and {} receipts",
                candidates.len(),
                attached.len()
            ),
            error_span(&node),
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn collect_ordinary_map_owned_any_v2_get_or_default_i32_receipt(
    program: &CheckedProgram,
    caller_owner: &NodeId,
    get_attempt: &crate::core::ownership::OrdinaryMapOwnedAnyV2Attempt,
    call_expression: &crate::core::ResolvedExpr,
    helper_owner: &NodeId,
) -> Option<crate::core::ownership::OrdinaryMapOwnedAnyV2GetOrDefaultI32Receipt> {
    use crate::core::ir::{
        CheckedConversionKind as ConversionKind, ResolvedExprKind as E, ResolvedLiteral as L,
        ResolvedPatternKind as P, ResolvedStmtKind as S, ResolvedType as T,
    };
    use crate::core::ownership::{
        OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
        OrdinaryMapOwnedAnyV2GetOrDefaultI32ProfileVersion as ProfileVersion,
        OrdinaryMapOwnedAnyV2GetOrDefaultI32Receipt as Receipt,
    };

    fn direct_local(
        expression: &crate::core::ResolvedExpr,
    ) -> Option<&crate::core::ResolvedLocalId> {
        match &expression.kind {
            E::Load(place) if place.projections.is_empty() => Some(&place.base),
            _ => None,
        }
    }

    fn direct_tuple_projection(
        expression: &crate::core::ResolvedExpr,
        local: &crate::core::ResolvedLocalId,
        index: usize,
    ) -> bool {
        match &expression.kind {
            E::Load(place) => {
                place.base == *local
                    && matches!(
                        place.projections.as_slice(),
                        [crate::core::ir::ResolvedProjection::Tuple { index: found, .. }]
                            if *found == index
                    )
            }
            E::Project {
                value,
                projection: crate::core::ir::ResolvedValueProjection::Tuple(found),
            } => *found == index && direct_local(value).is_some_and(|base| base == local),
            _ => false,
        }
    }

    fn binding(
        statement: &crate::core::ResolvedStmt,
    ) -> Option<(&crate::core::ResolvedLocalId, &crate::core::ResolvedExpr)> {
        let S::Bind {
            pattern:
                crate::core::ir::ResolvedPattern {
                    kind:
                        P::Binding {
                            local,
                            by_reference: None,
                        },
                    ..
                },
            initializer: Some(initializer),
        } = &statement.kind
        else {
            return None;
        };
        Some((local, initializer))
    }

    fn binding_at(
        body: &crate::core::ResolvedBody,
        expression: &NodeId,
    ) -> Option<(usize, crate::core::ResolvedLocalId)> {
        let mut found = body
            .root
            .statements
            .iter()
            .enumerate()
            .filter_map(|(index, statement)| {
                let (local, initializer) = binding(statement)?;
                (initializer.node_id == *expression).then_some((index, local.clone()))
            });
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    fn explicit_drop_at(
        body: &crate::core::ResolvedBody,
        local: &crate::core::ResolvedLocalId,
        point: &NodeId,
    ) -> Option<(usize, usize)> {
        let mut found = body
            .root
            .statements
            .iter()
            .enumerate()
            .filter_map(|(index, statement)| {
                if statement.node_id != *point {
                    return None;
                }
                let S::Drop(places) = &statement.kind else {
                    return None;
                };
                let matching = places
                    .iter()
                    .enumerate()
                    .filter(|(_, place)| place.base == *local && place.projections.is_empty())
                    .map(|(place_index, _)| place_index)
                    .collect::<Vec<_>>();
                (places.len() == 1 && matching.as_slice() == [0]).then_some((index, 0))
            });
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    fn builtin_call<'a>(
        expression: &'a crate::core::ResolvedExpr,
        name: &str,
        arity: usize,
    ) -> Option<&'a crate::core::ir::ResolvedCall> {
        let E::Call(call) = &expression.kind else {
            return None;
        };
        (call.arguments.len() == arity
            && matches!(&call.callee, crate::core::ResolvedCallee::Builtin(builtin) if builtin.as_str() == name))
        .then_some(call)
    }

    fn is_record(types: &crate::core::ResolvedTypeTable, ty: &crate::core::ResolvedTypeId) -> bool {
        matches!(
            types.get(ty),
            Some(T::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
        )
    }

    fn is_dynamic_any(
        types: &crate::core::ResolvedTypeTable,
        ty: &crate::core::ResolvedTypeId,
    ) -> bool {
        matches!(
            types.get(ty),
            Some(T::DynamicAny { capability }) if capability == "type.dynamic_value"
        )
    }

    let caller_body = program.resolved_bodies.get(caller_owner)?;
    let caller_signature = program.resolved_signatures.get(caller_owner)?;
    let caller_analysis = program.resource_analyses.get(caller_owner)?;
    let caller_cfg_receipt = caller_analysis
        .ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()?;
    if caller_cfg_receipt.owner != *caller_owner
        || caller_cfg_receipt.profile_version
            != crate::core::ownership::OrdinaryMapOwnedAnyV2ProfileVersion::V2
    {
        return None;
    }
    let E::Call(call) = &call_expression.kind else {
        return None;
    };
    let helper_signature = program.resolved_signatures.get(helper_owner)?;
    let helper_body = program.resolved_bodies.get(helper_owner)?;
    let helper_function = program.functions.get(helper_owner)?;
    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);

    // Map Set is persistent, so both its source and result remain separate
    // live roots across this call.
    if caller_signature.owner != *caller_owner
        || call.callee != crate::core::ResolvedCallee::Function(helper_owner.clone())
        || call.type_arguments.as_slice() != [i32_type.clone()]
        || call.result != i32_type
        || call.arguments.len() != 3
        || call.permission.is_some()
        || !call.session.is_empty()
        || get_attempt.point != call_expression.node_id
        || get_attempt.arguments
            != call
                .arguments
                .iter()
                .map(|argument| argument.value.node_id.clone())
                .collect::<Vec<_>>()
    {
        return None;
    }

    let [root_argument, key_argument, default_argument] = call.arguments.as_slice() else {
        return None;
    };
    let root_local = direct_local(&root_argument.value)?.clone();
    let root_type = root_argument.value.ty.clone();
    let key_literal = match &key_argument.value.kind {
        E::Literal(L::String(value)) if key_argument.value.ty == string_type => value.clone(),
        _ => return None,
    };
    let default_i32 = match &default_argument.value.kind {
        E::Literal(L::Int(value)) if default_argument.value.ty == i32_type => *value,
        _ => return None,
    };
    if !is_record(&program.resolved_types, &root_type)
        || root_argument.conversion.kind != ConversionKind::Identity
        || root_argument.conversion.from != root_type
        || root_argument.conversion.to != root_argument.value.ty
        || key_argument.value.ty != string_type
        || key_argument.conversion.kind != ConversionKind::Identity
        || key_argument.conversion.from != string_type
        || key_argument.conversion.to != string_type
        || default_argument.conversion.kind != ConversionKind::Identity
        || default_argument.conversion.from != i32_type
        || default_argument.conversion.to != i32_type
    {
        return None;
    }

    let new_actions = caller_cfg_receipt
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::New)
        .collect::<Vec<_>>();
    let [new_action] = new_actions.as_slice() else {
        return None;
    };
    let set_actions = caller_cfg_receipt
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::Set)
        .collect::<Vec<_>>();
    let [set_action] = set_actions.as_slice() else {
        return None;
    };
    let drop_actions = caller_cfg_receipt
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::Drop)
        .collect::<Vec<_>>();
    if caller_cfg_receipt.actions.len() != 4
        || caller_cfg_receipt.attempts.len() != 5
        || caller_cfg_receipt
            .attempts
            .iter()
            .filter(|candidate| {
                candidate.kind == crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::New
            })
            .count()
            != 1
        || caller_cfg_receipt
            .attempts
            .iter()
            .filter(|candidate| {
                candidate.kind == crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::Set
            })
            .count()
            != 1
        || caller_cfg_receipt
            .attempts
            .iter()
            .filter(|candidate| {
                candidate.kind == crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::Drop
            })
            .count()
            != 2
        || caller_cfg_receipt
            .attempts
            .iter()
            .filter(|candidate| {
                candidate.kind
                    == crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::GetOrDefaultI32
            })
            .count()
            != 1
        || get_attempt.receiver_local.as_ref() != Some(&root_local)
        || get_attempt.result_local.is_none()
        || new_action.local == root_local
        || set_action.local != root_local
        || set_action.source_local.as_ref() != Some(&new_action.local)
        || set_action.value_type.as_ref() != Some(&i32_type)
        || !set_action.payload_clone
        || new_action.root
            != crate::core::ResourceId(crate::core::NodeId(format!(
                "ordinary-map-owned-any-v2:{}",
                new_action.local.0 .0
            )))
        || set_action.root
            != crate::core::ResourceId(crate::core::NodeId(format!(
                "ordinary-map-owned-any-v2:{}",
                root_local.0 .0
            )))
        || !drop_actions
            .iter()
            .any(|drop| drop.local == new_action.local)
        || !drop_actions.iter().any(|drop| drop.local == root_local)
    {
        return None;
    }

    let set_expression = caller_body
        .call_expressions(&program.resolved_types, &program.resolved_type_targets)
        .ok()?
        .remove(&set_action.point)?;
    let set_call = builtin_call(&set_expression, "map_set", 3)?;
    let [set_root, set_key, set_value] = set_call.arguments.as_slice() else {
        return None;
    };
    let set_initial_root = direct_local(&set_root.value)?.clone();
    let set_key_value = match &set_key.value.kind {
        E::Literal(L::String(value)) if set_key.value.ty == string_type => value.clone(),
        _ => return None,
    };
    let set_value_i32 = match &set_value.value.kind {
        E::Literal(L::Int(value)) if set_value.value.ty == i32_type => *value,
        _ => return None,
    };
    // The built-in `map_set` call retains the concrete payload type in its
    // resolved argument conversion; erasure happens at the Map ABI boundary.
    // Bind the profile to an exact String identity conversion here rather than
    // requiring a generic-call `DynamicAnyPack` that this builtin does not
    // emit.
    if set_initial_root != new_action.local
        || set_action.key.as_ref() != Some(&set_key.value.node_id)
        || set_expression.ty != root_type
        || set_call.result != root_type
        || set_root.conversion.kind != ConversionKind::Identity
        || set_root.conversion.from != root_type
        || set_root.conversion.to != root_type
        || set_key.value.ty != string_type
        || set_key.conversion.kind != ConversionKind::Identity
        || set_key.conversion.from != string_type
        || set_key.conversion.to != string_type
        || set_value.value.ty != i32_type
        || set_value.conversion.kind != ConversionKind::Identity
        || set_value.conversion.from != i32_type
        || set_value.conversion.to != i32_type
        || set_action.value.as_ref() != Some(&set_value.value.node_id)
        || set_action.key_type.as_ref() != Some(&string_type)
        || set_action.value_type.as_ref() != Some(&i32_type)
    {
        return None;
    }
    let (set_binding_index, set_result_local) = binding_at(caller_body, &set_action.point)?;
    if set_result_local != root_local {
        return None;
    }
    let (call_binding_index, caller_result_local) =
        binding_at(caller_body, &call_expression.node_id)?;
    if get_attempt.result_local.as_ref() != Some(&caller_result_local)
        || caller_body.locals.get(&caller_result_local)?.ty != i32_type
    {
        return None;
    }
    let new_expression = caller_body
        .call_expressions(&program.resolved_types, &program.resolved_type_targets)
        .ok()?
        .remove(&new_action.point)?;
    let new_call = builtin_call(&new_expression, "map_new", 0)?;
    let (new_binding_index, initial_root_local) = binding_at(caller_body, &new_action.point)?;
    if initial_root_local != new_action.local
        || new_expression.ty != root_type
        || new_call.result != root_type
        || caller_body.locals.get(&initial_root_local)?.ty != root_type
        || caller_body.locals.get(&initial_root_local)?.mutable
        || caller_body.locals.get(&root_local)?.mutable
        || helper_signature.parameters.len() != 3
        || call.arguments[0].parameter != helper_signature.parameters[0].id
        || call.arguments[1].parameter != helper_signature.parameters[1].id
        || call.arguments[2].parameter != helper_signature.parameters[2].id
    {
        return None;
    }
    let (root_drop_index, root_drop_place_index) = explicit_drop_at(
        caller_body,
        &root_local,
        &drop_actions
            .iter()
            .find(|drop| drop.local == root_local)?
            .point,
    )?;
    let (initial_drop_index, initial_drop_place_index) = explicit_drop_at(
        caller_body,
        &initial_root_local,
        &drop_actions
            .iter()
            .find(|drop| drop.local == initial_root_local)?
            .point,
    )?;
    if !(new_binding_index < set_binding_index
        && set_binding_index < call_binding_index
        && call_binding_index < initial_drop_index
        && call_binding_index < root_drop_index)
    {
        return None;
    }
    let helper_source_key = program
        .node_meta
        .get(helper_owner)
        .map(|meta| meta.origin.user_span())
        .unwrap_or_else(|| helper_function.origin.user_span());
    if !canonical_std_maps_get_or_default(program, helper_owner)
        || helper_function.generics.len() != 1
        || !helper_function.pub_
        || helper_signature.owner != *helper_owner
        || helper_signature.generic_parameters.len() != 1
        || helper_signature.parameters.len() != 3
        || helper_body.owner != *helper_owner
        || helper_body.parameters.len() != 3
        || program
            .source_registry
            .key(helper_source_key.source_id)
            .is_none()
    {
        return None;
    }
    let generic = helper_signature.generic_parameters[0].clone();
    let [map_parameter, key_parameter, default_parameter] = helper_signature.parameters.as_slice()
    else {
        return None;
    };
    let [map_local, key_local, default_local] = helper_body.parameters.as_slice() else {
        return None;
    };
    let generic_type = &helper_signature.result;
    if helper_signature.result != helper_signature.parameters[2].ty
        || helper_signature.parameters[0].id != map_parameter.id
        || helper_signature.parameters[1].id != key_parameter.id
        || helper_signature.parameters[2].id != default_parameter.id
        || helper_body.locals.get(map_local)?.ty != map_parameter.ty
        || helper_body.locals.get(key_local)?.ty != key_parameter.ty
        || helper_body.locals.get(default_local)?.ty != default_parameter.ty
        || map_parameter.permission != Some(crate::core::Permission::View)
        || !is_record(&program.resolved_types, &map_parameter.ty)
        || key_parameter.ty != string_type
        || !matches!(program.resolved_types.get(generic_type), Some(T::GenericParameter(id)) if id == &generic)
        || !matches!(program.resolved_types.get(&default_parameter.ty), Some(T::GenericParameter(id)) if id == &generic)
    {
        return None;
    }
    let [pair_statement, selected_statement, pair_drop_statement] =
        helper_body.root.statements.as_slice()
    else {
        return None;
    };
    let (pair_local, pair_initializer) = binding(pair_statement)?;
    let (selected_local, selected_initializer) = binding(selected_statement)?;
    let pair_call = builtin_call(pair_initializer, "map_get", 2)?;
    let [map_argument, helper_key_argument] = pair_call.arguments.as_slice() else {
        return None;
    };
    let T::Tuple(pair_fields) = program.resolved_types.get(&pair_initializer.ty)? else {
        return None;
    };
    if pair_fields.len() != 2
        || pair_fields[0] != bool_type
        || !is_dynamic_any(&program.resolved_types, &pair_fields[1])
        || pair_initializer.ty != pair_call.result
        || helper_body.locals.get(pair_local)?.ty != pair_initializer.ty
        || pair_call.type_arguments.len() != 0
        || pair_call.permission.is_some()
        || !pair_call.session.is_empty()
        || direct_local(&map_argument.value) != Some(map_local)
        || direct_local(&helper_key_argument.value) != Some(key_local)
        || map_argument.conversion.kind != ConversionKind::Identity
        || map_argument.value.ty != map_parameter.ty
        || helper_key_argument.conversion.kind != ConversionKind::Identity
        || helper_key_argument.value.ty != string_type
        || !matches!(pair_statement.kind, S::Bind { .. })
        || selected_initializer.ty != pair_fields[1]
        || helper_body.locals.get(selected_local)?.ty != pair_fields[1]
    {
        return None;
    }
    let E::If {
        condition,
        then_block,
        else_block,
    } = &selected_initializer.kind
    else {
        return None;
    };
    if condition.ty != bool_type
        || !direct_tuple_projection(condition, pair_local, 0)
        || then_block.statements.len() != 0
        || else_block.statements.len() != 0
    {
        return None;
    }
    let found_value = then_block.result.as_deref()?;
    let default_branch_value = else_block.result.as_deref()?;
    let E::Cast {
        value: default_load,
        conversion: default_pack,
    } = &default_branch_value.kind
    else {
        return None;
    };
    let E::Load(default_place) = &default_load.kind else {
        return None;
    };
    if default_place.base != *default_local
        || !default_place.projections.is_empty()
        || default_load.ty != *generic_type
        || default_pack.kind != ConversionKind::DynamicAnyPack
        || default_pack.from != *generic_type
        || default_pack.to != pair_fields[1]
        || default_branch_value.ty != pair_fields[1]
        || !direct_tuple_projection(found_value, pair_local, 1)
        || found_value.ty != pair_fields[1]
    {
        return None;
    }
    let helper_return = helper_body.root.result.as_deref()?;
    let E::Cast {
        value: selected_value,
        conversion: unpack,
    } = &helper_return.kind
    else {
        return None;
    };
    if direct_local(selected_value) != Some(selected_local)
        || selected_value.ty != pair_fields[1]
        || unpack.kind != ConversionKind::DynamicAnyUnpack
        || unpack.from != pair_fields[1]
        || unpack.to != *generic_type
        || helper_return.ty != *generic_type
    {
        return None;
    }
    let S::Drop(pair_drops) = &pair_drop_statement.kind else {
        return None;
    };
    if pair_drops.len() != 1
        || pair_drops[0].base != *pair_local
        || !pair_drops[0].projections.is_empty()
    {
        return None;
    }
    let pair_drop_point = pair_drop_statement.node_id.clone();

    Some(Receipt {
        profile_version: ProfileVersion::V1,
        caller_owner: caller_owner.clone(),
        call_node: call_expression.node_id.clone(),
        call_target: helper_owner.clone(),
        call_type_argument: i32_type.clone(),
        call_arguments: call
            .arguments
            .iter()
            .map(|argument| argument.value.node_id.clone())
            .collect(),
        call_parameters: call
            .arguments
            .iter()
            .map(|argument| argument.parameter.clone())
            .collect(),
        call_argument_types: call
            .arguments
            .iter()
            .map(|argument| argument.value.ty.clone())
            .collect(),
        call_conversions: call
            .arguments
            .iter()
            .map(|argument| argument.conversion.clone())
            .collect(),
        call_result_type: call.result.clone(),
        caller_result_local,
        caller_call_binding_point: caller_body.root.statements[call_binding_index]
            .node_id
            .clone(),
        caller_initial_root_local: initial_root_local.clone(),
        caller_root_local: root_local.clone(),
        caller_root: set_action.root.clone(),
        caller_root_type: root_type,
        caller_new_point: new_action.point.clone(),
        caller_set_point: set_action.point.clone(),
        caller_set_key: set_key.value.node_id.clone(),
        caller_set_key_bytes: set_key_value,
        caller_set_value: set_value.value.node_id.clone(),
        caller_set_value_i32: set_value_i32,
        caller_set_value_type: set_value.value.ty.clone(),
        caller_query_key: key_argument.value.node_id.clone(),
        caller_query_key_bytes: key_literal,
        caller_default: default_argument.value.node_id.clone(),
        caller_default_i32: default_i32,
        caller_default_type: default_argument.value.ty.clone(),
        caller_root_drop_point: drop_actions
            .iter()
            .find(|drop| drop.local == root_local)?
            .point
            .clone(),
        caller_root_drop_place_index: root_drop_place_index,
        caller_initial_root_drop_point: drop_actions
            .iter()
            .find(|drop| drop.local == initial_root_local)?
            .point
            .clone(),
        caller_initial_root_drop_place_index: initial_drop_place_index,
        helper_owner: helper_owner.clone(),
        helper_map_parameter: map_parameter.id.clone(),
        helper_map_permission: crate::core::Permission::View,
        helper_key_parameter: key_parameter.id.clone(),
        helper_default_parameter: default_parameter.id.clone(),
        helper_map_local: map_local.clone(),
        helper_key_local: key_local.clone(),
        helper_default_local: default_local.clone(),
        helper_get_any_call: pair_initializer.node_id.clone(),
        helper_pair_local: pair_local.clone(),
        helper_pair_binding_point: pair_statement.node_id.clone(),
        helper_pair_type: pair_initializer.ty.clone(),
        helper_found_condition: condition.node_id.clone(),
        helper_found_projection: condition.node_id.clone(),
        helper_payload_projection: found_value.node_id.clone(),
        helper_unpack_cast: helper_return.node_id.clone(),
        helper_unpack_target: unpack.to.clone(),
        helper_if_node: selected_initializer.node_id.clone(),
        helper_found_arm_block: then_block.node_id.clone(),
        helper_default_arm_block: else_block.node_id.clone(),
        helper_selected_local: selected_local.clone(),
        helper_selected_binding_point: selected_statement.node_id.clone(),
        helper_default_load: default_load.node_id.clone(),
        helper_pair_drop_point: pair_drop_point,
        helper_return_node: helper_return.node_id.clone(),
        helper_result_type: helper_signature.result.clone(),
    })
}

fn collect_ordinary_map_owned_any_v2_get_or_default_string_receipt(
    program: &CheckedProgram,
    caller_owner: &NodeId,
    get_attempt: &crate::core::ownership::OrdinaryMapOwnedAnyV2Attempt,
    call_expression: &crate::core::ResolvedExpr,
    helper_owner: &NodeId,
) -> Option<crate::core::ownership::OrdinaryMapOwnedAnyV2GetOrDefaultStringReceipt> {
    use crate::core::ir::{
        CheckedConversionKind as ConversionKind, ResolvedExprKind as E, ResolvedLiteral as L,
        ResolvedPatternKind as P, ResolvedStmtKind as S, ResolvedType as T,
    };
    use crate::core::ownership::{
        OrdinaryMapOwnedAnyV2ActionKind as ActionKind,
        OrdinaryMapOwnedAnyV2AttemptKind as AttemptKind,
        OrdinaryMapOwnedAnyV2GetOrDefaultStringProfileVersion as ProfileVersion,
        OrdinaryMapOwnedAnyV2GetOrDefaultStringReceipt as Receipt,
    };

    fn direct_local(
        expression: &crate::core::ResolvedExpr,
    ) -> Option<&crate::core::ResolvedLocalId> {
        match &expression.kind {
            E::Load(place) if place.projections.is_empty() => Some(&place.base),
            _ => None,
        }
    }

    fn direct_tuple_projection(
        expression: &crate::core::ResolvedExpr,
        local: &crate::core::ResolvedLocalId,
        index: usize,
    ) -> bool {
        match &expression.kind {
            E::Load(place) => {
                place.base == *local
                    && matches!(
                        place.projections.as_slice(),
                        [crate::core::ir::ResolvedProjection::Tuple { index: found, .. }]
                            if *found == index
                    )
            }
            E::Project {
                value,
                projection: crate::core::ir::ResolvedValueProjection::Tuple(found),
            } => *found == index && direct_local(value).is_some_and(|base| base == local),
            _ => false,
        }
    }

    fn binding(
        statement: &crate::core::ResolvedStmt,
    ) -> Option<(&crate::core::ResolvedLocalId, &crate::core::ResolvedExpr)> {
        let S::Bind {
            pattern:
                crate::core::ir::ResolvedPattern {
                    kind:
                        P::Binding {
                            local,
                            by_reference: None,
                        },
                    ..
                },
            initializer: Some(initializer),
        } = &statement.kind
        else {
            return None;
        };
        Some((local, initializer))
    }

    fn binding_at(
        body: &crate::core::ResolvedBody,
        expression: &NodeId,
    ) -> Option<(usize, crate::core::ResolvedLocalId)> {
        let mut found = body
            .root
            .statements
            .iter()
            .enumerate()
            .filter_map(|(index, statement)| {
                let (local, initializer) = binding(statement)?;
                (initializer.node_id == *expression).then_some((index, local.clone()))
            });
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    fn string_literal_source(
        body: &crate::core::ResolvedBody,
        expression: &crate::core::ResolvedExpr,
        string_type: &crate::core::ResolvedTypeId,
    ) -> Option<(
        String,
        Option<(crate::core::ResolvedLocalId, NodeId, usize)>,
    )> {
        match &expression.kind {
            E::Literal(L::String(value)) if &expression.ty == string_type => {
                Some((value.clone(), None))
            }
            E::Load(place) if place.projections.is_empty() && &expression.ty == string_type => {
                let local = place.base.clone();
                if body
                    .locals
                    .get(&local)
                    .is_none_or(|definition| definition.mutable)
                {
                    return None;
                }
                let mut matches =
                    body.root
                        .statements
                        .iter()
                        .enumerate()
                        .filter_map(|(index, statement)| {
                            let (bound_local, initializer) = binding(statement)?;
                            if bound_local != &local || &initializer.ty != string_type {
                                return None;
                            }
                            let E::Literal(L::String(value)) = &initializer.kind else {
                                return None;
                            };
                            Some((value.clone(), statement.node_id.clone(), index))
                        });
                let (value, point, index) = matches.next()?;
                matches
                    .next()
                    .is_none()
                    .then_some((value, Some((local, point, index))))
            }
            _ => None,
        }
    }

    fn unique_explicit_drop(
        body: &crate::core::ResolvedBody,
        local: &crate::core::ResolvedLocalId,
    ) -> Option<(NodeId, usize, usize)> {
        let mut matches =
            body.root
                .statements
                .iter()
                .enumerate()
                .filter_map(|(index, statement)| {
                    let S::Drop(places) = &statement.kind else {
                        return None;
                    };
                    let matching = places
                        .iter()
                        .enumerate()
                        .filter(|(_, place)| place.base == *local && place.projections.is_empty())
                        .map(|(place_index, _)| place_index)
                        .collect::<Vec<_>>();
                    (places.len() == 1 && matching.as_slice() == [0]).then_some((
                        statement.node_id.clone(),
                        index,
                        0,
                    ))
                });
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    fn has_any_direct_drop(
        body: &crate::core::ResolvedBody,
        local: &crate::core::ResolvedLocalId,
    ) -> bool {
        body.root.statements.iter().any(|statement| {
            matches!(
                &statement.kind,
                S::Drop(places)
                    if places.iter().any(|place| {
                        place.base == *local && place.projections.is_empty()
                    })
            )
        })
    }

    fn explicit_drop_at(
        body: &crate::core::ResolvedBody,
        local: &crate::core::ResolvedLocalId,
        point: &NodeId,
    ) -> Option<(usize, usize)> {
        let mut found = body
            .root
            .statements
            .iter()
            .enumerate()
            .filter_map(|(index, statement)| {
                if statement.node_id != *point {
                    return None;
                }
                let S::Drop(places) = &statement.kind else {
                    return None;
                };
                let matching = places
                    .iter()
                    .enumerate()
                    .filter(|(_, place)| place.base == *local && place.projections.is_empty())
                    .map(|(place_index, _)| place_index)
                    .collect::<Vec<_>>();
                (places.len() == 1 && matching.as_slice() == [0]).then_some((index, 0))
            });
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    fn builtin_call<'a>(
        expression: &'a crate::core::ResolvedExpr,
        name: &str,
        arity: usize,
    ) -> Option<&'a crate::core::ir::ResolvedCall> {
        let E::Call(call) = &expression.kind else {
            return None;
        };
        (call.arguments.len() == arity
            && matches!(&call.callee, crate::core::ResolvedCallee::Builtin(builtin) if builtin.as_str() == name))
        .then_some(call)
    }

    fn is_record(types: &crate::core::ResolvedTypeTable, ty: &crate::core::ResolvedTypeId) -> bool {
        matches!(
            types.get(ty),
            Some(T::Nominal { item, .. }) if item.as_str() == "builtin:type:Record"
        )
    }

    fn is_dynamic_any(
        types: &crate::core::ResolvedTypeTable,
        ty: &crate::core::ResolvedTypeId,
    ) -> bool {
        matches!(
            types.get(ty),
            Some(T::DynamicAny { capability }) if capability == "type.dynamic_value"
        )
    }

    let caller_body = program.resolved_bodies.get(caller_owner)?;
    let caller_signature = program.resolved_signatures.get(caller_owner)?;
    let caller_analysis = program.resource_analyses.get(caller_owner)?;
    let caller_cfg_receipt = caller_analysis
        .ordinary_map_owned_any_v2_cfg_receipt
        .as_ref()?;
    if caller_cfg_receipt.owner != *caller_owner
        || caller_cfg_receipt.profile_version
            != crate::core::ownership::OrdinaryMapOwnedAnyV2ProfileVersion::V2
    {
        return None;
    }
    let E::Call(call) = &call_expression.kind else {
        return None;
    };
    let helper_signature = program.resolved_signatures.get(helper_owner)?;
    let helper_body = program.resolved_bodies.get(helper_owner)?;
    let helper_function = program.functions.get(helper_owner)?;
    let payload_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let miss_payload_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);

    // Map Set is persistent, so both its source and result remain separate
    // live roots across this call.
    if caller_signature.owner != *caller_owner
        || call.callee != crate::core::ResolvedCallee::Function(helper_owner.clone())
        || call.type_arguments.as_slice() != [payload_type.clone()]
        || call.result != payload_type
        || call.arguments.len() != 3
        || call.permission.is_some()
        || !call.session.is_empty()
        || get_attempt.point != call_expression.node_id
        || get_attempt.arguments
            != call
                .arguments
                .iter()
                .map(|argument| argument.value.node_id.clone())
                .collect::<Vec<_>>()
    {
        return None;
    }

    let [root_argument, key_argument, default_argument] = call.arguments.as_slice() else {
        return None;
    };
    let root_local = direct_local(&root_argument.value)?.clone();
    let root_type = root_argument.value.ty.clone();
    let key_literal = match &key_argument.value.kind {
        E::Literal(L::String(value)) if key_argument.value.ty == string_type => value.clone(),
        _ => return None,
    };
    let (default_string, default_source) =
        string_literal_source(caller_body, &default_argument.value, &payload_type)?;
    if !is_record(&program.resolved_types, &root_type)
        || root_argument.conversion.kind != ConversionKind::Identity
        || root_argument.conversion.from != root_type
        || root_argument.conversion.to != root_argument.value.ty
        || key_argument.value.ty != string_type
        || key_argument.conversion.kind != ConversionKind::Identity
        || key_argument.conversion.from != string_type
        || key_argument.conversion.to != string_type
        || default_argument.conversion.kind != ConversionKind::Identity
        || default_argument.conversion.from != payload_type
        || default_argument.conversion.to != payload_type
    {
        return None;
    }

    let new_actions = caller_cfg_receipt
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::New)
        .collect::<Vec<_>>();
    let [new_action] = new_actions.as_slice() else {
        return None;
    };
    let set_actions = caller_cfg_receipt
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::Set)
        .collect::<Vec<_>>();
    let [set_action] = set_actions.as_slice() else {
        return None;
    };
    let drop_actions = caller_cfg_receipt
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::Drop)
        .collect::<Vec<_>>();
    let clone_actions = caller_cfg_receipt
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::Clone)
        .collect::<Vec<_>>();
    let root_clone = match clone_actions.as_slice() {
        [] => None,
        [clone] if clone.local == root_local => Some(*clone),
        _ => return None,
    };
    let has_alias_clone = root_clone.is_some();
    let expected_drop_count = if has_alias_clone { 3 } else { 2 };
    let expected_action_count = if has_alias_clone { 6 } else { 4 };
    let expected_attempt_count = if has_alias_clone { 7 } else { 5 };
    let v2_resource = |local: &crate::core::ResolvedLocalId| {
        crate::core::ResourceId(crate::core::NodeId(format!(
            "ordinary-map-owned-any-v2:{}",
            local.0 .0
        )))
    };
    if caller_cfg_receipt.actions.len() != expected_action_count
        || caller_cfg_receipt.attempts.len() != expected_attempt_count
        || caller_cfg_receipt
            .attempts
            .iter()
            .filter(|candidate| {
                candidate.kind == crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::New
            })
            .count()
            != 1
        || caller_cfg_receipt
            .attempts
            .iter()
            .filter(|candidate| {
                candidate.kind == crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::Set
            })
            .count()
            != 1
        || caller_cfg_receipt
            .attempts
            .iter()
            .filter(|candidate| {
                candidate.kind == crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::Drop
            })
            .count()
            != expected_drop_count
        || caller_cfg_receipt
            .attempts
            .iter()
            .filter(|candidate| {
                candidate.kind
                    == crate::core::ownership::OrdinaryMapOwnedAnyV2AttemptKind::GetOrDefaultString
            })
            .count()
            != 1
        || get_attempt.receiver_local.as_ref() != Some(&root_local)
        || get_attempt.result_local.is_none()
        || new_action.local == root_local
        || new_action.local == set_action.local
        || (root_clone.is_none() && set_action.local != root_local)
        || (root_clone.is_some()
            && root_clone.is_none_or(|clone| {
                clone.local != root_local
                    || clone.source_local.as_ref() != Some(&set_action.local)
                    || set_action.local == new_action.local
                    || set_action.local == root_local
            }))
        || set_action.source_local.as_ref() != Some(&new_action.local)
        || set_action.value_type.as_ref() != Some(&payload_type)
        || !set_action.payload_clone
        || new_action.root != v2_resource(&new_action.local)
        || set_action.root != v2_resource(&set_action.local)
        || set_action.source.as_ref() != Some(&v2_resource(&new_action.local))
        || clone_actions.len() != if has_alias_clone { 1 } else { 0 }
        || !drop_actions
            .iter()
            .any(|drop| drop.local == new_action.local && drop.root == new_action.root)
        || !drop_actions
            .iter()
            .any(|drop| drop.local == root_local && drop.root == v2_resource(&root_local))
        || (has_alias_clone
            && !drop_actions
                .iter()
                .any(|drop| drop.local == set_action.local && drop.root == set_action.root))
        || {
            let expected = if has_alias_clone {
                [
                    new_action.local.clone(),
                    root_local.clone(),
                    set_action.local.clone(),
                ]
                .into_iter()
                .collect::<BTreeSet<_>>()
            } else {
                [new_action.local.clone(), root_local.clone()]
                    .into_iter()
                    .collect::<BTreeSet<_>>()
            };
            drop_actions
                .iter()
                .map(|drop| drop.local.clone())
                .collect::<BTreeSet<_>>()
                != expected
        }
        || drop_actions.iter().any(|drop| {
            drop.profile_version != crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                || drop.root != v2_resource(&drop.local)
                || drop.key.is_some()
                || drop.key_type.is_some()
                || drop.key_local.is_some()
                || drop.key_local_mutable.is_some()
                || drop.key_provenance.is_some()
                || drop.key_static_digest.is_some()
                || drop.value.is_some()
                || drop.value_type.is_some()
                || drop.payload_clone
                || drop.source_local.is_some()
                || drop.source.is_some()
                || drop.source_node.is_some()
                || drop.source_type.is_some()
                || drop.result_type.is_some()
                || drop.result_local.is_some()
        })
    {
        return None;
    }

    let root_drop_actions = drop_actions
        .iter()
        .filter(|drop| drop.local == root_local)
        .collect::<Vec<_>>();
    let initial_drop_actions = drop_actions
        .iter()
        .filter(|drop| drop.local == new_action.local)
        .collect::<Vec<_>>();
    let [root_drop_action] = root_drop_actions.as_slice() else {
        return None;
    };
    let [initial_drop_action] = initial_drop_actions.as_slice() else {
        return None;
    };
    if explicit_drop_at(caller_body, &root_local, &root_drop_action.point).is_none()
        || explicit_drop_at(caller_body, &new_action.local, &initial_drop_action.point).is_none()
    {
        return None;
    }
    let clone_binding_index = if let Some(clone) = root_clone {
        let clone_bindings = caller_body
            .root
            .statements
            .iter()
            .filter_map(|statement| {
                let (local, initializer) = binding(statement)?;
                (initializer.node_id == clone.point).then_some((local, initializer))
            })
            .collect::<Vec<_>>();
        let [(bound_local, initializer)] = clone_bindings.as_slice() else {
            return None;
        };
        let (binding_index, bound_local_id) = binding_at(caller_body, &clone.point)?;
        let source_local = set_action.local.clone();
        if *bound_local != &root_local
            || bound_local_id != root_local
            || initializer.ty != root_type
            || !matches!(
                &initializer.kind,
                E::Load(place)
                    if place.base == source_local && place.projections.is_empty()
            )
            || clone.point != initializer.node_id
            || clone.root != v2_resource(&root_local)
            || clone.source_local.as_ref() != Some(&source_local)
            || clone.source.as_ref() != Some(&set_action.root)
            || clone.source_node.as_ref() != Some(&initializer.node_id)
            || clone.source_type.as_ref() != Some(&root_type)
            || clone.key.is_some()
            || clone.key_type.is_some()
            || clone.key_local.is_some()
            || clone.key_local_mutable.is_some()
            || clone.key_provenance.is_some()
            || clone.key_static_digest.is_some()
            || clone.value.is_some()
            || clone.value_type.is_some()
            || clone.payload_clone
            || clone.result_type.is_some()
            || clone.result_local.is_some()
            || caller_body.locals.get(&root_local)?.mutable
            || caller_body.locals.get(&source_local)?.ty != root_type
        {
            return None;
        }
        Some(binding_index)
    } else {
        None
    };
    let source_drop_action = if let Some(clone) = root_clone {
        let source_drops = drop_actions
            .iter()
            .filter(|drop| drop.local == set_action.local && drop.root == set_action.root)
            .collect::<Vec<_>>();
        let [source_drop] = source_drops.as_slice() else {
            return None;
        };
        if source_drop.point == root_drop_action.point
            || source_drop.point == initial_drop_action.point
            || source_drop.local == clone.local
            || explicit_drop_at(caller_body, &set_action.local, &source_drop.point).is_none()
        {
            return None;
        }
        Some(*source_drop)
    } else {
        None
    };

    let new_attempts = caller_cfg_receipt
        .attempts
        .iter()
        .filter(|attempt| attempt.kind == AttemptKind::New)
        .collect::<Vec<_>>();
    let set_attempts = caller_cfg_receipt
        .attempts
        .iter()
        .filter(|attempt| attempt.kind == AttemptKind::Set)
        .collect::<Vec<_>>();
    let get_attempts = caller_cfg_receipt
        .attempts
        .iter()
        .filter(|attempt| attempt.kind == AttemptKind::GetOrDefaultString)
        .collect::<Vec<_>>();
    let drop_attempts = caller_cfg_receipt
        .attempts
        .iter()
        .filter(|attempt| attempt.kind == AttemptKind::Drop)
        .collect::<Vec<_>>();
    let clone_attempts = caller_cfg_receipt
        .attempts
        .iter()
        .filter(|attempt| attempt.kind == AttemptKind::Clone)
        .collect::<Vec<_>>();
    let [new_attempt] = new_attempts.as_slice() else {
        return None;
    };
    let [set_attempt] = set_attempts.as_slice() else {
        return None;
    };
    let [get_attempt_receipt] = get_attempts.as_slice() else {
        return None;
    };
    if new_attempt.point != new_action.point
        || new_attempt.receiver_local.is_some()
        || new_attempt.result_local.as_ref() != Some(&new_action.local)
        || !new_attempt.arguments.is_empty()
        || set_attempt.point != set_action.point
        || set_attempt.receiver_local.as_ref() != Some(&new_action.local)
        || set_attempt.result_local.as_ref() != Some(&set_action.local)
        || set_attempt.arguments.len() != 3
        || set_attempt.arguments.first() != set_action.source_node.as_ref()
        || set_attempt.arguments.get(1) != set_action.key.as_ref()
        || set_attempt.arguments.get(2) != set_action.value.as_ref()
        || *get_attempt_receipt != get_attempt
        || get_attempt.point != call_expression.node_id
        || get_attempt.receiver_local.as_ref() != Some(&root_local)
        || get_attempt.result_local.is_none()
        || get_attempt.arguments.len() != 3
    {
        return None;
    }
    match (root_clone, clone_attempts.as_slice()) {
        (Some(clone), [attempt])
            if attempt.point == clone.point
                && attempt.arguments.as_slice() == [clone.point.clone()]
                && attempt.receiver_local.as_ref() == Some(&set_action.local)
                && attempt.result_local.as_ref() == Some(&root_local) => {}
        (None, []) => {}
        _ => return None,
    }
    if drop_attempts.len() != expected_drop_count
        || drop_actions.iter().any(|drop| {
            drop_attempts
                .iter()
                .filter(|attempt| {
                    attempt.point == drop.point
                        && attempt.receiver_local.as_ref() == Some(&drop.local)
                        && attempt.result_local.is_none()
                        && attempt.arguments.as_slice() == [drop.local.0.clone()]
                })
                .count()
                != 1
        })
    {
        return None;
    }

    let set_expression = caller_body
        .call_expressions(&program.resolved_types, &program.resolved_type_targets)
        .ok()?
        .remove(&set_action.point)?;
    let set_call = builtin_call(&set_expression, "map_set", 3)?;
    let [set_root, set_key, set_value] = set_call.arguments.as_slice() else {
        return None;
    };
    let set_initial_root = direct_local(&set_root.value)?.clone();
    let set_key_value = match &set_key.value.kind {
        E::Literal(L::String(value)) if set_key.value.ty == string_type => value.clone(),
        _ => return None,
    };
    let (set_value_string, set_value_source) =
        string_literal_source(caller_body, &set_value.value, &payload_type)?;
    // The built-in `map_set` call retains the concrete payload type in its
    // resolved argument conversion; erasure happens at the Map ABI boundary.
    // Bind the profile to an exact String identity conversion here rather than
    // requiring a generic-call `DynamicAnyPack` that this builtin does not
    // emit.
    if set_initial_root != new_action.local
        || set_action.key.as_ref() != Some(&set_key.value.node_id)
        || set_expression.ty != root_type
        || set_call.result != root_type
        || set_root.conversion.kind != ConversionKind::Identity
        || set_root.conversion.from != root_type
        || set_root.conversion.to != root_type
        || set_key.value.ty != string_type
        || set_key.conversion.kind != ConversionKind::Identity
        || set_key.conversion.from != string_type
        || set_key.conversion.to != string_type
        || set_value.value.ty != payload_type
        || set_value.conversion.kind != ConversionKind::Identity
        || set_value.conversion.from != payload_type
        || set_value.conversion.to != payload_type
        || set_action.value.as_ref() != Some(&set_value.value.node_id)
        || set_action.key_type.as_ref() != Some(&string_type)
        || set_action.value_type.as_ref() != Some(&payload_type)
    {
        return None;
    }
    let (set_binding_index, set_result_local) = binding_at(caller_body, &set_action.point)?;
    if set_result_local != set_action.local
        || (root_clone.is_none() && set_result_local != root_local)
        || root_clone.is_some_and(|clone| {
            clone.local != root_local || clone.source_local.as_ref() != Some(&set_result_local)
        })
    {
        return None;
    }
    let (call_binding_index, caller_result_local) =
        binding_at(caller_body, &call_expression.node_id)?;
    if get_attempt.result_local.as_ref() != Some(&caller_result_local)
        || caller_body.locals.get(&caller_result_local)?.ty != payload_type
    {
        return None;
    }
    let (set_value_local, set_value_binding_point, set_value_binding_index, set_value_drop) =
        if let Some((local, binding_point, binding_index)) = set_value_source {
            let (drop_point, drop_index, drop_place_index) =
                unique_explicit_drop(caller_body, &local)?;
            (
                Some(local),
                Some(binding_point),
                Some(binding_index),
                Some((drop_point, drop_index, drop_place_index)),
            )
        } else {
            (None, None, None, None)
        };
    let (caller_default_local, caller_default_binding_point, default_binding_index): (
        Option<crate::core::ir::ResolvedLocalId>,
        Option<NodeId>,
        Option<usize>,
    ) = if let Some((local, binding_point, binding_index)) = default_source {
        (Some(local), Some(binding_point), Some(binding_index))
    } else {
        (None, None, None)
    };
    let new_expression = caller_body
        .call_expressions(&program.resolved_types, &program.resolved_type_targets)
        .ok()?
        .remove(&new_action.point)?;
    let new_call = builtin_call(&new_expression, "map_new", 0)?;
    let (new_binding_index, initial_root_local) = binding_at(caller_body, &new_action.point)?;
    if initial_root_local != new_action.local
        || new_expression.ty != root_type
        || new_call.result != root_type
        || caller_body.locals.get(&initial_root_local)?.ty != root_type
        || caller_body.locals.get(&initial_root_local)?.mutable
        || caller_body.locals.get(&root_local)?.mutable
        || helper_signature.parameters.len() != 3
        || call.arguments[0].parameter != helper_signature.parameters[0].id
        || call.arguments[1].parameter != helper_signature.parameters[1].id
        || call.arguments[2].parameter != helper_signature.parameters[2].id
    {
        return None;
    }
    let (root_drop_index, root_drop_place_index) = explicit_drop_at(
        caller_body,
        &root_local,
        &drop_actions
            .iter()
            .find(|drop| drop.local == root_local)?
            .point,
    )?;
    let (initial_drop_index, initial_drop_place_index) = explicit_drop_at(
        caller_body,
        &initial_root_local,
        &drop_actions
            .iter()
            .find(|drop| drop.local == initial_root_local)?
            .point,
    )?;
    let set_root_drop_action = source_drop_action.unwrap_or(*root_drop_action);
    let (set_root_drop_index, set_root_drop_place_index) =
        explicit_drop_at(caller_body, &set_action.local, &set_root_drop_action.point)?;
    let (result_settlement, result_settlement_index) = if let Some((point, index, place_index)) =
        unique_explicit_drop(caller_body, &caller_result_local)
    {
        (
                crate::core::ownership::OrdinaryMapOwnedAnyV2GetOrDefaultStringResultSettlement::ExplicitDrop {
                    point,
                    place_index,
                },
                index,
            )
    } else {
        if has_any_direct_drop(caller_body, &caller_result_local) {
            return None;
        }
        let returned = caller_body.root.result.as_deref()?;
        if direct_local(returned) != Some(&caller_result_local) || returned.ty != payload_type {
            return None;
        }
        (
                crate::core::ownership::OrdinaryMapOwnedAnyV2GetOrDefaultStringResultSettlement::TailReturn {
                    point: returned.node_id.clone(),
                },
                caller_body.root.statements.len(),
            )
    };
    if default_binding_index.is_some_and(|index| index >= call_binding_index)
        || set_value_binding_index.is_some_and(|index| index >= set_binding_index)
        || set_value_drop.as_ref().is_some_and(|(_, index, _)| {
            *index <= call_binding_index
                || *index <= initial_drop_index
                || *index <= root_drop_index
        })
        || result_settlement_index <= call_binding_index
        || set_root_drop_index <= call_binding_index
        || source_drop_action.is_some_and(|drop| {
            explicit_drop_at(caller_body, &drop.local, &drop.point)
                .is_none_or(|(index, _)| index <= call_binding_index)
        })
        || (set_action.local == root_local) != (set_root_drop_index == root_drop_index)
    {
        return None;
    }
    if !(new_binding_index < set_binding_index
        && set_binding_index < call_binding_index
        && clone_binding_index
            .is_none_or(|index| set_binding_index < index && index < call_binding_index)
        && call_binding_index < initial_drop_index
        && call_binding_index < root_drop_index)
    {
        return None;
    }
    let helper_source_key = program
        .node_meta
        .get(helper_owner)
        .map(|meta| meta.origin.user_span())
        .unwrap_or_else(|| helper_function.origin.user_span());
    if !canonical_std_maps_get_or_default(program, helper_owner)
        || helper_function.generics.len() != 1
        || !helper_function.pub_
        || helper_signature.owner != *helper_owner
        || helper_signature.generic_parameters.len() != 1
        || helper_signature.parameters.len() != 3
        || helper_body.owner != *helper_owner
        || helper_body.parameters.len() != 3
        || program
            .source_registry
            .key(helper_source_key.source_id)
            .is_none()
    {
        return None;
    }
    let generic = helper_signature.generic_parameters[0].clone();
    let [map_parameter, key_parameter, default_parameter] = helper_signature.parameters.as_slice()
    else {
        return None;
    };
    let [map_local, key_local, default_local] = helper_body.parameters.as_slice() else {
        return None;
    };
    let generic_type = &helper_signature.result;
    if helper_signature.result != helper_signature.parameters[2].ty
        || helper_signature.parameters[0].id != map_parameter.id
        || helper_signature.parameters[1].id != key_parameter.id
        || helper_signature.parameters[2].id != default_parameter.id
        || helper_body.locals.get(map_local)?.ty != map_parameter.ty
        || helper_body.locals.get(key_local)?.ty != key_parameter.ty
        || helper_body.locals.get(default_local)?.ty != default_parameter.ty
        || map_parameter.permission != Some(crate::core::Permission::View)
        || !is_record(&program.resolved_types, &map_parameter.ty)
        || key_parameter.ty != string_type
        || !matches!(program.resolved_types.get(generic_type), Some(T::GenericParameter(id)) if id == &generic)
        || !matches!(program.resolved_types.get(&default_parameter.ty), Some(T::GenericParameter(id)) if id == &generic)
    {
        return None;
    }
    let [pair_statement, selected_statement, pair_drop_statement] =
        helper_body.root.statements.as_slice()
    else {
        return None;
    };
    let (pair_local, pair_initializer) = binding(pair_statement)?;
    let (selected_local, selected_initializer) = binding(selected_statement)?;
    let pair_call = builtin_call(pair_initializer, "map_get", 2)?;
    let [map_argument, helper_key_argument] = pair_call.arguments.as_slice() else {
        return None;
    };
    let T::Tuple(pair_fields) = program.resolved_types.get(&pair_initializer.ty)? else {
        return None;
    };
    if pair_fields.len() != 2
        || pair_fields[0] != bool_type
        || !is_dynamic_any(&program.resolved_types, &pair_fields[1])
        || pair_initializer.ty != pair_call.result
        || helper_body.locals.get(pair_local)?.ty != pair_initializer.ty
        || pair_call.type_arguments.len() != 0
        || pair_call.permission.is_some()
        || !pair_call.session.is_empty()
        || direct_local(&map_argument.value) != Some(map_local)
        || direct_local(&helper_key_argument.value) != Some(key_local)
        || map_argument.conversion.kind != ConversionKind::Identity
        || map_argument.value.ty != map_parameter.ty
        || helper_key_argument.conversion.kind != ConversionKind::Identity
        || helper_key_argument.value.ty != string_type
        || !matches!(pair_statement.kind, S::Bind { .. })
        || selected_initializer.ty != pair_fields[1]
        || helper_body.locals.get(selected_local)?.ty != pair_fields[1]
    {
        return None;
    }
    let E::If {
        condition,
        then_block,
        else_block,
    } = &selected_initializer.kind
    else {
        return None;
    };
    if condition.ty != bool_type
        || !direct_tuple_projection(condition, pair_local, 0)
        || then_block.statements.len() != 0
        || else_block.statements.len() != 0
    {
        return None;
    }
    let found_value = then_block.result.as_deref()?;
    let default_branch_value = else_block.result.as_deref()?;
    let E::Cast {
        value: default_load,
        conversion: default_pack,
    } = &default_branch_value.kind
    else {
        return None;
    };
    let E::Load(default_place) = &default_load.kind else {
        return None;
    };
    if default_place.base != *default_local
        || !default_place.projections.is_empty()
        || default_load.ty != *generic_type
        || default_pack.kind != ConversionKind::DynamicAnyPack
        || default_pack.from != *generic_type
        || default_pack.to != pair_fields[1]
        || default_branch_value.ty != pair_fields[1]
        || !direct_tuple_projection(found_value, pair_local, 1)
        || found_value.ty != pair_fields[1]
    {
        return None;
    }
    let helper_return = helper_body.root.result.as_deref()?;
    let E::Cast {
        value: selected_value,
        conversion: unpack,
    } = &helper_return.kind
    else {
        return None;
    };
    if direct_local(selected_value) != Some(selected_local)
        || selected_value.ty != pair_fields[1]
        || unpack.kind != ConversionKind::DynamicAnyUnpack
        || unpack.from != pair_fields[1]
        || unpack.to != *generic_type
        || helper_return.ty != *generic_type
    {
        return None;
    }
    let S::Drop(pair_drops) = &pair_drop_statement.kind else {
        return None;
    };
    if pair_drops.len() != 1
        || pair_drops[0].base != *pair_local
        || !pair_drops[0].projections.is_empty()
    {
        return None;
    }
    let pair_drop_point = pair_drop_statement.node_id.clone();

    Some(Receipt {
        profile_version: ProfileVersion::V1,
        caller_owner: caller_owner.clone(),
        call_node: call_expression.node_id.clone(),
        call_target: helper_owner.clone(),
        call_type_argument: payload_type.clone(),
        miss_payload_type,
        call_arguments: call
            .arguments
            .iter()
            .map(|argument| argument.value.node_id.clone())
            .collect(),
        call_parameters: call
            .arguments
            .iter()
            .map(|argument| argument.parameter.clone())
            .collect(),
        call_argument_types: call
            .arguments
            .iter()
            .map(|argument| argument.value.ty.clone())
            .collect(),
        call_conversions: call
            .arguments
            .iter()
            .map(|argument| argument.conversion.clone())
            .collect(),
        call_result_type: call.result.clone(),
        caller_result_local,
        caller_call_binding_point: caller_body.root.statements[call_binding_index]
            .node_id
            .clone(),
        caller_initial_root_local: initial_root_local.clone(),
        caller_root_local: root_local.clone(),
        caller_set_root_local: set_action.local.clone(),
        caller_root: set_action.root.clone(),
        caller_root_type: root_type,
        caller_new_point: new_action.point.clone(),
        caller_set_point: set_action.point.clone(),
        caller_set_key: set_key.value.node_id.clone(),
        caller_set_key_bytes: set_key_value,
        caller_set_value: set_value.value.node_id.clone(),
        caller_set_value_local: set_value_local,
        caller_set_value_binding_point: set_value_binding_point,
        caller_set_value_drop_point: set_value_drop.as_ref().map(|(point, _, _)| point.clone()),
        caller_set_value_drop_place_index: set_value_drop
            .as_ref()
            .map(|(_, _, place_index)| *place_index),
        caller_set_value_string: set_value_string,
        caller_set_value_type: set_value.value.ty.clone(),
        caller_query_key: key_argument.value.node_id.clone(),
        caller_query_key_bytes: key_literal,
        caller_default: default_argument.value.node_id.clone(),
        caller_default_local,
        caller_default_binding_point,
        caller_default_string: default_string,
        caller_default_type: default_argument.value.ty.clone(),
        caller_result_settlement: result_settlement,
        caller_root_drop_point: drop_actions
            .iter()
            .find(|drop| drop.local == root_local)?
            .point
            .clone(),
        caller_root_drop_place_index: root_drop_place_index,
        caller_set_root_drop_point: set_root_drop_action.point.clone(),
        caller_set_root_drop_place_index: set_root_drop_place_index,
        caller_initial_root_drop_point: drop_actions
            .iter()
            .find(|drop| drop.local == initial_root_local)?
            .point
            .clone(),
        caller_initial_root_drop_place_index: initial_drop_place_index,
        helper_owner: helper_owner.clone(),
        helper_map_parameter: map_parameter.id.clone(),
        helper_map_permission: crate::core::Permission::View,
        helper_key_parameter: key_parameter.id.clone(),
        helper_default_parameter: default_parameter.id.clone(),
        helper_map_local: map_local.clone(),
        helper_key_local: key_local.clone(),
        helper_default_local: default_local.clone(),
        helper_get_any_call: pair_initializer.node_id.clone(),
        helper_pair_local: pair_local.clone(),
        helper_pair_binding_point: pair_statement.node_id.clone(),
        helper_pair_type: pair_initializer.ty.clone(),
        helper_found_condition: condition.node_id.clone(),
        helper_found_projection: condition.node_id.clone(),
        helper_payload_projection: found_value.node_id.clone(),
        helper_default_pack: default_branch_value.node_id.clone(),
        // The resolved std::maps helper body is generic, so its raw pack
        // conversion starts from T. The callsite receipt records the checked
        // specialization of that source type (String) while preserving the
        // helper's DynamicAny destination below.
        helper_default_pack_from: payload_type.clone(),
        helper_default_pack_to: default_pack.to.clone(),
        helper_unpack_cast: helper_return.node_id.clone(),
        helper_unpack_target: unpack.to.clone(),
        helper_if_node: selected_initializer.node_id.clone(),
        helper_found_arm_block: then_block.node_id.clone(),
        helper_default_arm_block: else_block.node_id.clone(),
        helper_selected_local: selected_local.clone(),
        helper_selected_binding_point: selected_statement.node_id.clone(),
        helper_default_load: default_load.node_id.clone(),
        helper_pair_drop_point: pair_drop_point,
        helper_return_node: helper_return.node_id.clone(),
        helper_result_type: helper_signature.result.clone(),
    })
}

/// Audit every non-primitive DynamicAnyUnpack belonging to an owner-v2
/// GetAny/Values attempt, even when source inference could not produce a
/// provisional cast candidate. Older owner profiles keep their own conversion
/// contract; they do not inherit owner-v2's canonical-Label restriction.
fn validate_ordinary_map_owned_any_v2_nonprimitive_unpack_boundary(
    program: &CheckedProgram,
    candidates: &BTreeMap<NodeId, BTreeSet<NodeId>>,
) -> Result<(), Vec<Diagnostic>> {
    use crate::core::ir::{CheckedConversionKind, ResolvedExprKind};
    use crate::core::ownership::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;

    fn direct_map_get_payload(
        body: &crate::core::ResolvedBody,
        value: &crate::core::ResolvedExpr,
    ) -> bool {
        let payload_local = match &value.kind {
            ResolvedExprKind::Load(place)
                if matches!(
                    place.projections.as_slice(),
                    [crate::core::ir::ResolvedProjection::Tuple { index: 1, .. }]
                ) =>
            {
                Some(place.base.clone())
            }
            ResolvedExprKind::Project {
                value,
                projection: crate::core::ir::ResolvedValueProjection::Tuple(1),
            } => match &value.kind {
                ResolvedExprKind::Load(place) if place.projections.is_empty() => {
                    Some(place.base.clone())
                }
                _ => None,
            },
            _ => None,
        };
        let Some(payload_local) = payload_local else {
            return false;
        };
        let mut initializers = body.root.statements.iter().filter_map(|statement| {
            let crate::core::ResolvedStmtKind::Bind {
                pattern:
                    crate::core::ResolvedPattern {
                        kind:
                            crate::core::ResolvedPatternKind::Binding {
                                local,
                                by_reference: None,
                            },
                        ..
                    },
                initializer: Some(initializer),
            } = &statement.kind
            else {
                return None;
            };
            (local == &payload_local).then_some(initializer)
        });
        let Some(initializer) = initializers.next() else {
            return false;
        };
        initializers.next().is_none()
            && matches!(
                &initializer.kind,
                ResolvedExprKind::Call(call)
                    if call.arguments.len() == 2
                        && matches!(
                            &call.callee,
                            crate::core::ResolvedCallee::Builtin(name)
                                if name.as_str() == "map_get"
                        )
            )
    }

    fn canonical_std_maps_get_or_default(program: &CheckedProgram, owner: &NodeId) -> bool {
        let Some(function) = program.functions.get(owner) else {
            return false;
        };
        let span = program
            .node_meta
            .get(owner)
            .map(|meta| meta.origin.user_span())
            .unwrap_or_else(|| function.origin.user_span());
        program
            .source_registry
            .key(span.source_id)
            .is_some_and(|key| key.as_str() == "stdlib:maps.mimi")
            && matches!(
                function.qualified_name.as_str(),
                "get_or_default" | "std::maps::get_or_default"
            )
    }

    let mut errors = Vec::new();
    let route_admission = crate::core::mir::classify_canonical_mir_route_admission(program);
    let computed_owner_chain_owner = NodeId("function:main".into());
    let computed_owner_chain_candidates = candidates.values().map(BTreeSet::len).sum::<usize>()
        == 1
        && candidates.len() == 1
        && candidates
            .get(&computed_owner_chain_owner)
            .is_some_and(|casts| casts.len() == 1);
    for (owner, body) in &program.resolved_bodies {
        let casts =
            match body.cast_expressions(&program.resolved_types, &program.resolved_type_targets) {
                Ok(casts) => casts,
                Err(body_errors) => {
                    errors.extend(body_errors.into_iter().map(|error| {
                        Diagnostic::error(
                            format!("TOOL-RESOLUTION-001: {error}"),
                            body.root.origin.user_span(),
                        )
                    }));
                    continue;
                }
            };
        let cfg = program
            .resource_analyses
            .get(owner)
            .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref())
            .filter(|cfg| cfg.owner == *owner && cfg.profile_version == ProfileVersion::V2);
        for (cast, expression) in casts {
            let ResolvedExprKind::Cast { value, conversion } = &expression.kind else {
                continue;
            };
            if conversion.kind != CheckedConversionKind::DynamicAnyUnpack
                || !matches!(
                    program.resolved_types.get(&conversion.from),
                    Some(crate::core::ResolvedType::DynamicAny { capability })
                        if capability == "type.dynamic_value"
                )
                || matches!(
                    program.resolved_types.get(&conversion.to),
                    Some(crate::core::ResolvedType::Primitive(_))
                )
            {
                continue;
            }

            // Generic source bodies are checked before their type arguments
            // are known. Preserve only the std::maps `get_or_default<T>`
            // template conversion from the direct `.1` payload of a builtin
            // Map.Get result to its declared T. Concrete executable routes
            // still run the DynamicAny backend gate (E0722) until a call-site
            // type receipt exists; arbitrary `Any as T` remains rejected.
            let owner_generic_target = canonical_std_maps_get_or_default(program, owner)
                && conversion.to == expression.ty
                && direct_map_get_payload(body, value)
                && program
                    .resolved_signatures
                    .get(owner)
                    .is_some_and(|signature| {
                        signature.owner == *owner
                            && matches!(
                                program.resolved_types.get(&conversion.to),
                                Some(crate::core::ResolvedType::GenericParameter(binder))
                                    if signature.generic_parameters.contains(binder)
                            )
                    });
            if owner_generic_target {
                continue;
            }

            let is_getany_candidate = cfg.is_some()
                && candidates
                    .get(owner)
                    .is_some_and(|owner_candidates| owner_candidates.contains(&cast));
            let is_cfg_unpack_attempt = cfg.is_some_and(|cfg| {
                cfg.unpack_receipts
                    .iter()
                    .any(|receipt| receipt.cast == cast)
            });
            // The computed-string owner-chain cast is nominal (`as Label`),
            // so it is intentionally absent from the builtin unpack call
            // census. Admit it here only when the independently checked
            // chain receipt names this exact cast, GetAny pair, selected Set
            // payload, and one explicit pair Drop. The detailed source DAG
            // and byte equality are replayed by the nonprimitive receipt
            // validator earlier in CheckedProgram finalization.
            let is_exact_computed_string_owner_chain =
                route_admission.ordinary_map_owned_any_v2.is_computed_string_key_owner_chain()
                    && computed_owner_chain_candidates
                    && owner == &computed_owner_chain_owner
                    && candidates
                        .get(owner)
                        .is_some_and(|casts| casts.contains(&cast))
                    && cfg.is_some_and(|cfg| {
                        let matching_unpacks = cfg
                            .unpack_receipts
                            .iter()
                            .filter(|receipt| receipt.cast == cast)
                            .collect::<Vec<_>>();
                        let [unpack] = matching_unpacks.as_slice() else {
                            return false;
                        };
                        let matching_chains = cfg
                            .result_receipts
                            .iter()
                            .filter_map(|result| {
                                result
                                    .computed_string_key_owner_chain
                                    .as_deref()
                                    .map(|chain| (result, chain))
                            })
                            .collect::<Vec<_>>();
                        let [(result, chain)] = matching_chains.as_slice() else {
                            return false;
                        };
                        let mut pair_without_chain = (*result).clone();
                        pair_without_chain.computed_string_key_owner_chain = None;
                        let matching_gets = cfg
                            .actions
                            .iter()
                            .filter(|action| {
                                action.kind
                                    == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                                    && action.point == unpack.operation
                            })
                            .collect::<Vec<_>>();
                        let [get] = matching_gets.as_slice() else {
                            return false;
                        };
                        let exact_key_uses = {
                            let get_uses = chain
                                .key_uses
                                .iter()
                                .filter(|usage| usage.action == chain.get_any)
                                .collect::<Vec<_>>();
                            let set_uses = chain
                                .key_uses
                                .iter()
                                .filter(|usage| usage.action == chain.map_set)
                                .collect::<Vec<_>>();
                            matches!((get_uses.as_slice(), set_uses.as_slice()), ([get_use], [set_use])
                                if get_use.action_kind
                                    == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                                    && set_use.action_kind
                                        == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                                    && get_use.key_type == set_use.key_type
                                    && get_use.value_proof.expression_type
                                        == set_use.value_proof.expression_type
                                    && get_use.value_proof.bytes == set_use.value_proof.bytes
                                    && get_use.value_proof.digest == set_use.value_proof.digest)
                        };
                        cfg.owner == *owner
                            && cfg.profile_version
                                == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                            && chain.profile_version
                                == crate::core::ownership::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChainProfileVersion::V1
                            && chain.owner == *owner
                            && chain.get_any == **get
                            && chain.pair == pair_without_chain
                            && result.operation == unpack.operation
                            && result.kind
                                == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                            && result.binding == unpack.pair_binding
                            && result.binding_point == unpack.pair_binding_point
                            && result.settlement
                                == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                            && result.settlement_point.as_ref()
                                == Some(&unpack.settlement_point)
                            && unpack.profile_version
                                == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                            && unpack.conversion_kind == conversion.kind
                            && unpack.conversion_from == conversion.from
                            && unpack.conversion_to == conversion.to
                            && unpack.settlement
                                == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                            && chain.pair.binding == unpack.pair_binding
                            && chain.pair_drop.local == unpack.pair_binding
                            && chain.pair_drop.binding_point == unpack.pair_binding_point
                            && chain.pair_drop.binding_type == result.result_type
                            && chain.pair_drop.settlement_point == unpack.settlement_point
                            && chain.pair.settlement_point.as_ref()
                                == Some(&chain.pair_drop.settlement_point)
                            && chain.payload.set_action == chain.map_set
                            && chain.payload.set_value_type == expression.ty
                            && chain.map_set.value_type.as_ref() == Some(&expression.ty)
                            && chain.map_set.payload_clone
                            && chain.pair_snapshot_observation.snapshot_unpack == cast
                            && chain.pair_snapshot_observation.snapshot_projection
                                == unpack.projection
                            && chain.pair_snapshot_observation.snapshot_type == expression.ty
                            && exact_key_uses
                    });
            let is_values_receipt = cfg.is_some_and(|cfg| {
                cfg.values_unpack_receipts.iter().any(|receipt| {
                    receipt.profile_version == ProfileVersion::V2
                        && receipt.cast == cast
                        && receipt.conversion_kind == conversion.kind
                        && receipt.conversion_from == conversion.from
                        && receipt.conversion_to == conversion.to
                })
            });
            if !is_getany_candidate
                && !is_cfg_unpack_attempt
                && !is_values_receipt
                && !is_exact_computed_string_owner_chain
            {
                continue;
            }

            let span = program
                .node_meta
                .get(&cast)
                .map(|meta| meta.origin.user_span())
                .unwrap_or_else(|| {
                    program
                        .node_meta
                        .get(owner)
                        .map(|meta| meta.origin.user_span())
                        .unwrap_or(Span::UNKNOWN)
                });
            if conversion.to != expression.ty
                || !is_canonical_map_any_label_target(program, &conversion.to)
            {
                errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0200,
                    "DynamicAnyUnpack target must be an exact receipt-backed canonical Label",
                    span,
                ));
                continue;
            }

            if !is_getany_candidate && !is_values_receipt && !is_exact_computed_string_owner_chain {
                errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0200,
                    "DynamicAnyUnpack has no exact GetAny or Values source receipt",
                    span,
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// The inference exception for local `List<Any>[index] as T` is provisional.
/// Admit it across CheckedProgram finalization only when every direct local
/// DynamicAny index cast joins exactly one same-owner Values unpack receipt,
/// and every receipt points back to one such cast with the same target type.
/// The receipt carries payload selection and the explicit snapshot Drop proof;
/// unsupported roots, indexes, payloads, or settlement order therefore remain
/// closed here even though the cast had to be typed to build the CFG.
fn validate_ordinary_map_owned_any_v2_values_local_cast_receipts(
    program: &CheckedProgram,
) -> Result<(), Vec<Diagnostic>> {
    use crate::core::ir::{CheckedConversion, CheckedConversionKind};
    use crate::core::ownership::OrdinaryMapOwnedAnyV2ProfileVersion as ProfileVersion;

    fn is_dynamic_any(
        types: &crate::core::ResolvedTypeTable,
        ty: &crate::core::ResolvedTypeId,
    ) -> bool {
        matches!(
            types.get(ty),
            Some(crate::core::ResolvedType::DynamicAny { capability })
                if capability == "type.dynamic_value"
        )
    }

    fn is_dynamic_any_list(
        types: &crate::core::ResolvedTypeTable,
        ty: &crate::core::ResolvedTypeId,
    ) -> bool {
        matches!(
            types.get(ty),
            Some(crate::core::ResolvedType::Nominal { item, arguments, .. })
                if item.as_str() == "builtin:type:List"
                    && arguments.len() == 1
                    && is_dynamic_any(types, &arguments[0])
        )
    }

    fn direct_index_base(
        source: &crate::core::ResolvedExpr,
    ) -> Option<(
        crate::core::ResolvedLocalId,
        NodeId,
        crate::core::ResolvedTypeId,
        Option<crate::core::ResolvedLocalId>,
    )> {
        let crate::core::ResolvedExprKind::Project {
            value,
            projection: crate::core::ir::ResolvedValueProjection::Index(index),
        } = &source.kind
        else {
            return None;
        };
        let crate::core::ResolvedExprKind::Load(place) = &value.kind else {
            return None;
        };
        place.projections.is_empty().then(|| {
            let index_local = match &index.kind {
                crate::core::ResolvedExprKind::Load(index_place)
                    if index_place.projections.is_empty() =>
                {
                    Some(index_place.base.clone())
                }
                _ => None,
            };
            (
                place.base.clone(),
                index.node_id.clone(),
                index.ty.clone(),
                index_local,
            )
        })
    }

    #[derive(Clone)]
    struct Candidate {
        snapshot: crate::core::ResolvedLocalId,
        snapshot_type: crate::core::ResolvedTypeId,
        index_projection: NodeId,
        index_expression: NodeId,
        index_type: crate::core::ResolvedTypeId,
        index_local: Option<crate::core::ResolvedLocalId>,
        conversion: CheckedConversion,
        target: crate::core::ResolvedTypeId,
    }

    let mut candidates = BTreeMap::<(NodeId, NodeId), Candidate>::new();
    let mut errors = Vec::new();
    for (owner, body) in &program.resolved_bodies {
        let casts = body
            .cast_expressions(&program.resolved_types, &program.resolved_type_targets)
            .map_err(|body_errors| {
                body_errors
                    .into_iter()
                    .map(|error| {
                        Diagnostic::error(
                            format!("TOOL-RESOLUTION-001: {error}"),
                            body.root.origin.user_span(),
                        )
                    })
                    .collect::<Vec<_>>()
            })?;
        for (cast_id, expression) in casts {
            let crate::core::ResolvedExprKind::Cast { value, conversion } = &expression.kind else {
                continue;
            };
            if conversion.kind != CheckedConversionKind::DynamicAnyUnpack
                || !is_dynamic_any(&program.resolved_types, &conversion.from)
            {
                continue;
            }
            let Some((snapshot, index_expression, index_type, index_local)) =
                direct_index_base(value)
            else {
                continue;
            };
            let Some(local) = body.locals.get(&snapshot) else {
                continue;
            };
            if !is_dynamic_any_list(&program.resolved_types, &local.ty) {
                continue;
            }
            let key = (owner.clone(), cast_id.clone());
            let candidate = Candidate {
                snapshot,
                snapshot_type: local.ty.clone(),
                index_projection: value.node_id.clone(),
                index_expression,
                index_type,
                index_local,
                conversion: conversion.clone(),
                target: expression.ty.clone(),
            };
            if candidates.insert(key, candidate).is_some() {
                errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0218,
                    "local Values unpack cast has a duplicated Checker source identity",
                    expression.origin.user_span(),
                ));
            }
        }
    }

    let mut receipts = BTreeMap::<
        (NodeId, NodeId),
        Vec<(
            NodeId,
            ProfileVersion,
            crate::core::OrdinaryMapOwnedAnyV2ValuesUnpackReceipt,
        )>,
    >::new();
    for (owner, analysis) in &program.resource_analyses {
        let Some(cfg) = analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref() else {
            continue;
        };
        for receipt in &cfg.values_unpack_receipts {
            receipts
                .entry((owner.clone(), receipt.cast.clone()))
                .or_default()
                .push((cfg.owner.clone(), cfg.profile_version, receipt.clone()));
        }
    }

    for (key @ (owner, cast), candidate) in &candidates {
        let matches = receipts.get(key).map(Vec::as_slice).unwrap_or_default();
        let [(receipt_owner, cfg_profile, receipt)] = matches else {
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0200,
                "direct List<Any> indexed cast requires one exact Values unpack receipt with payload disposition and explicit snapshot Drop",
                program
                    .node_meta
                    .get(cast)
                    .map(|meta| meta.origin.user_span())
                    .or_else(|| program.node_meta.get(owner).map(|meta| meta.origin.user_span()))
                    .unwrap_or(Span::UNKNOWN),
            ));
            continue;
        };
        let exact = receipt_owner == owner
            && *cfg_profile == ProfileVersion::V2
            && receipt.profile_version == ProfileVersion::V2
            && receipt.cast == *cast
            && receipt.snapshot_binding == candidate.snapshot
            && receipt.snapshot_type == candidate.snapshot_type
            && receipt.index_projection == candidate.index_projection
            && receipt.index_expression == candidate.index_expression
            && receipt.index_type == candidate.index_type
            && receipt.index_local == candidate.index_local
            && receipt.conversion_kind == candidate.conversion.kind
            && receipt.conversion_from == candidate.conversion.from
            && receipt.conversion_to == candidate.target
            && candidate.target == candidate.conversion.to
            && receipt.index_disposition.is_canonical()
            && match receipt.index_disposition {
                crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::SelectedPayload => {
                    !receipt.payload_selection.is_empty()
                }
                crate::core::OrdinaryMapOwnedAnyV2ValuesIndexDisposition::AlwaysOutOfBounds {
                    ..
                } => receipt.payload_selection.is_empty(),
            }
            && receipt.settlement
                == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
            && !receipt.settlement_point.0.trim().is_empty();
        if !exact {
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0200,
                "direct List<Any> indexed cast disagrees with its exact Values unpack receipt",
                program
                    .node_meta
                    .get(cast)
                    .map(|meta| meta.origin.user_span())
                    .unwrap_or(Span::UNKNOWN),
            ));
        }
    }

    for (key @ (owner, cast), matching_receipts) in &receipts {
        if matching_receipts.len() != 1 || !candidates.contains_key(key) {
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0218,
                format!(
                    "Checker Values unpack receipt for '{}' has no unique provisional local indexed cast",
                    owner.0
                ),
                program
                    .node_meta
                    .get(cast)
                    .map(|meta| meta.origin.user_span())
                    .or_else(|| program.node_meta.get(owner).map(|meta| meta.origin.user_span()))
                    .unwrap_or(Span::UNKNOWN),
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// A bare List has no source-visible element type. The one provisional
/// `bare_list[index] as string` form is allowed to cross the CheckedProgram
/// boundary only when the whole-program Checker pass attached its independent
/// Values producer/caller receipt to the exact cast. This keeps unrelated
/// bare List indexing fail-closed even though inference needed a temporary
/// DynamicAny type to build the source-shaped resolved body.
fn validate_ordinary_map_owned_any_v2_values_crosscall_string_receipts(
    program: &CheckedProgram,
    candidates: &BTreeMap<NodeId, BTreeSet<NodeId>>,
) -> Result<(), Vec<Diagnostic>> {
    use crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerProfileVersion as ProfileVersion;
    use crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesResultProducerProfileVersion as ProducerProfileVersion;

    let mut expected = BTreeSet::new();
    let mut errors = Vec::new();
    for (caller_owner, casts) in candidates {
        for cast in casts {
            if !expected.insert((caller_owner.clone(), cast.clone())) {
                errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0218,
                    "bare List String extraction has a duplicated Checker candidate identity",
                    program
                        .node_meta
                        .get(cast)
                        .map(|meta| meta.origin.user_span())
                        .unwrap_or(Span::UNKNOWN),
                ));
            }
        }
    }

    let mut attached = BTreeSet::new();
    let mut duplicate_attached = BTreeSet::new();
    for (producer_owner, analysis) in program.resource_analyses() {
        let Some(cfg) = analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref() else {
            continue;
        };
        let Some(receipt) = cfg.values_crosscall_string_caller_receipt.as_ref() else {
            continue;
        };
        let identity = (receipt.caller_owner.clone(), receipt.unpack_cast.clone());
        if !attached.insert(identity.clone()) {
            duplicate_attached.insert(identity);
            continue;
        }
        let (expected_element_count, expected_index) = match receipt.profile_version {
            ProfileVersion::V1 | ProfileVersion::V3 => (1, 0),
            ProfileVersion::V2 | ProfileVersion::V4 => (2, 1),
        };
        let result_branch_profile = matches!(
            receipt.profile_version,
            ProfileVersion::V3 | ProfileVersion::V4
        );
        let expected_result_producer_profile = match receipt.profile_version {
            ProfileVersion::V3 => Some(ProducerProfileVersion::V1),
            ProfileVersion::V4 => Some(ProducerProfileVersion::V2),
            ProfileVersion::V1 | ProfileVersion::V2 => None,
        };
        let profile_fields_match = receipt.values_element_count == expected_element_count
            && receipt.index_value == expected_index
            && receipt.producer_sorted_keys.len() == expected_element_count
            && receipt
                .producer_sorted_keys
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            && (!matches!(
                receipt.profile_version,
                ProfileVersion::V2 | ProfileVersion::V4
            ) || receipt.producer_sorted_keys == ["a", "b"]);
        let straightline_producer_matches = cfg
            .values_producer_return_receipt
            .as_ref()
            .is_some_and(|producer| {
                producer.owner == receipt.producer_owner
                    && producer.values_operation == receipt.producer_values_operation
                    && producer.return_value == receipt.producer_return
                    && producer.root_settlements.len() == expected_element_count + 1
                    && producer.payload_settlements.len() == expected_element_count
            });
        let result_producer_matches = cfg
            .values_result_producer_receipt
            .as_ref()
            .is_some_and(|producer| {
                let expected_sets = expected_element_count;
                let expected_drops = (expected_sets + 1) * (expected_sets + 2) / 2;
                let expected_actions = expected_sets * 4 + 2;
                let non_drop_actions = cfg
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind != crate::core::OrdinaryMapOwnedAnyV2ActionKind::Drop
                    })
                    .collect::<Vec<_>>();
                Some(producer) == receipt.producer_result_receipt.as_ref()
                    && Some(producer.profile_version) == expected_result_producer_profile
                    && producer.owner == receipt.producer_owner
                    && producer.values_operation == receipt.producer_values_operation
                    && producer.success_result == receipt.producer_return
                    && producer.set_branches.len() == expected_sets
                    && producer.paths.len() == expected_sets + 2
                    && producer
                        .paths
                        .iter()
                        .filter(|path| {
                            path.kind
                                == crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesResultPathKind::ValuesSnapshot
                        })
                        .count()
                        == 1
                    && producer
                        .paths
                        .iter()
                        .filter(|path| {
                            path.kind
                                == crate::core::ownership::OrdinaryMapOwnedAnyV2ValuesResultPathKind::EmptyList
                        })
                        .count()
                        == expected_sets + 1
                    && producer.root_settlements.len() == expected_drops
                    && producer.payload_settlements.len() == expected_sets * (expected_sets + 2)
                    && cfg.actions.len() == expected_actions
                    && cfg.attempts.len() == expected_actions
                    && non_drop_actions
                        .iter()
                        .filter(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::New
                        })
                        .count()
                        == 1
                    && non_drop_actions
                        .iter()
                        .filter(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                        })
                        .count()
                        == expected_sets
                    && non_drop_actions
                        .iter()
                        .filter(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Values
                        })
                        .count()
                        == 1
                    && cfg
                        .actions
                        .iter()
                        .filter(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Drop
                        })
                        .count()
                        == expected_drops
            });
        let producer_matches = cfg.owner == *producer_owner
            && receipt.producer_owner == *producer_owner
            && if result_branch_profile {
                cfg.values_producer_return_receipt.is_none() && result_producer_matches
            } else {
                receipt.producer_result_receipt.is_none()
                    && cfg.values_result_producer_receipt.is_none()
                    && cfg.actions.len() == expected_element_count * 2 + 3
                    && cfg.attempts.len() == expected_element_count * 2 + 3
                    && cfg
                        .actions
                        .iter()
                        .filter(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::New
                        })
                        .count()
                        == 1
                    && cfg
                        .actions
                        .iter()
                        .filter(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                        })
                        .count()
                        == expected_element_count
                    && cfg
                        .actions
                        .iter()
                        .filter(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Values
                        })
                        .count()
                        == 1
                    && cfg
                        .actions
                        .iter()
                        .filter(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Drop
                        })
                        .count()
                        == expected_element_count + 1
                    && straightline_producer_matches
            };
        let caller_matches = receipt.caller_owner != receipt.producer_owner
            && receipt.call_target == receipt.producer_owner
            && profile_fields_match
            && receipt.string_type
                == crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String)
            && if result_branch_profile {
                cfg.values_result_producer_receipt
                    .as_ref()
                    .is_some_and(|producer| {
                        receipt.producer_result_receipt.as_ref() == Some(producer)
                            && receipt.call_result_type == producer.surface_return_type
                            && receipt.snapshot_type == producer.snapshot_type
                    })
            } else {
                cfg.values_producer_return_receipt
                    .as_ref()
                    .is_some_and(|producer| {
                        receipt.call_result_type == producer.surface_return_type
                            && receipt.snapshot_type == producer.snapshot_type
                    })
            }
            && receipt.snapshot_source_uses
                == BTreeSet::from([
                    receipt.snapshot_load.clone(),
                    receipt.snapshot_drop_point.clone(),
                ])
            && receipt.string_source_uses
                == BTreeSet::from([
                    receipt.comparison_load.clone(),
                    receipt.string_drop_point.clone(),
                ]);
        if !profile_fields_match || !producer_matches || !caller_matches {
            errors.push(Diagnostic::error_code(
                crate::diagnostic::codes::E0218,
                "bare List String extraction has an inconsistent cross-call Values receipt",
                program
                    .node_meta
                    .get(&receipt.unpack_cast)
                    .map(|meta| meta.origin.user_span())
                    .unwrap_or(Span::UNKNOWN),
            ));
        }
    }

    for identity in duplicate_attached {
        errors.push(Diagnostic::error_code(
            crate::diagnostic::codes::E0218,
            "bare List String extraction has multiple producer receipts for one cast",
            program
                .node_meta
                .get(&identity.1)
                .map(|meta| meta.origin.user_span())
                .unwrap_or(Span::UNKNOWN),
        ));
    }
    for (caller_owner, cast) in expected.difference(&attached) {
        errors.push(Diagnostic::error_code(
            crate::diagnostic::codes::E0218,
            format!(
                "bare List String extraction in '{}' requires the exact cross-call Values producer/caller receipt",
                caller_owner.0
            ),
            program
                .node_meta
                .get(cast)
                .map(|meta| meta.origin.user_span())
                .unwrap_or(Span::UNKNOWN),
        ));
    }
    for (caller_owner, cast) in attached.difference(&expected) {
        errors.push(Diagnostic::error_code(
            crate::diagnostic::codes::E0218,
            format!(
                "cross-call Values receipt in '{}' has no provisional bare List String cast candidate",
                caller_owner.0
            ),
            program
                .node_meta
                .get(cast)
                .map(|meta| meta.origin.user_span())
                .unwrap_or(Span::UNKNOWN),
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// A dedicated checked-unpack builtin is only a source spelling for one
/// existing `DynamicAnyUnpack` conversion. It is admitted after the exact
/// call NodeId joins the same owner's GetAny CFG receipt, and only when the
/// complete source classifier selects a narrow Map/Any profile. The
/// MapRootGetAnyCaller profile is the explicit cross-call exception: it joins
/// the caller's local GetAny/unpack CFG receipt to the producer's whole-program
/// root-transfer receipt. CheckedGet admits a matching static key's selected
/// Set type or the canonical i32 miss; ComputedKeyGet requires the same
/// immutable dynamic key and selected Set type; RemovedKeyCheckedMiss requires
/// its exact String-Set/Remove/GetAny receipt and canonical i32 miss.
fn validate_ordinary_map_owned_any_v2_checked_unpack_receipts(
    program: &CheckedProgram,
    candidates: &BTreeMap<NodeId, BTreeSet<NodeId>>,
) -> Result<(), Vec<Diagnostic>> {
    if candidates.is_empty() {
        return Ok(());
    }

    fn valid_static_digest(value: Option<&String>) -> bool {
        value.is_some_and(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }

    let candidate_count = candidates.values().map(BTreeSet::len).sum::<usize>();
    let route_admission =
        crate::core::mir::classify_canonical_mir_route_admission(program).ordinary_map_owned_any_v2;
    let checked_get = route_admission.is_checked_get();
    // Checked-unpack validation runs during CheckedProgram construction,
    // before the final route admission has necessarily classified this
    // source as CheckedGet. Use the route's complete source-only predicate
    // here to break that classification/validation cycle; candidate count
    // alone never activates the multi-query profile.
    let checked_get_multi = candidate_count == 2
        && candidates.len() == 1
        && candidates
            .get(&crate::core::NodeId("function:main".into()))
            .is_some_and(|calls| calls.len() == 2)
        && crate::core::mir::ordinary_map_owned_any_v2_checked_get_multi_lookup_source_candidate(
            program,
        );
    let computed_key_get = route_admission.is_computed_key_get();
    let dynamic_keys_get = route_admission.ordinary_map_owned_any_v2_dynamic_keys_get();
    let removed_key_checked_miss = route_admission.is_removed_key_checked_miss();
    let map_root_get_any_caller = route_admission.is_map_root_get_any_caller();
    let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
    let bool_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool);
    let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
    let set_get_any_i32_owner = crate::core::NodeId("function:main".into());
    let set_get_any_i32_source_receipt = program
        .resource_analysis(&set_get_any_i32_owner)
        .and_then(|analysis| {
            analysis
                .ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt
                .as_ref()
                .map(|receipt| (analysis, receipt))
        });
    let set_get_any_i32 = set_get_any_i32_source_receipt.is_some_and(|(analysis, receipt)| {
        let Some(cfg) = program.callable_cfgs.get(&set_get_any_i32_owner) else {
            return false;
        };
        let Some(body) = program.resolved_bodies.get(&set_get_any_i32_owner) else {
            return false;
        };
        let Some(signature) = program.resolved_signatures.get(&set_get_any_i32_owner) else {
            return false;
        };
        receipt.owner == set_get_any_i32_owner
            && crate::core::cfg::validate_ordinary_map_owned_any_v2_set_get_any_i32_lineage_receipt(
                receipt,
                cfg,
                body,
                signature,
                &program.resolved_types,
                &analysis.ordinary_map_owned_any_v2_operation_attempts,
            )
            .is_ok()
    });
    let set_get_any_string_owner = crate::core::NodeId("function:main".into());
    let set_get_any_string_source_receipt = program
        .resource_analysis(&set_get_any_string_owner)
        .and_then(|analysis| {
            analysis
                .ordinary_map_owned_any_v2_set_get_any_string_lineage_receipt
                .as_ref()
                .map(|receipt| (analysis, receipt))
        });
    let set_get_any_string = set_get_any_string_source_receipt
        .is_some_and(|(analysis, receipt)| {
            let Some(cfg) = program.callable_cfgs.get(&set_get_any_string_owner) else {
                return false;
            };
            let Some(body) = program.resolved_bodies.get(&set_get_any_string_owner) else {
                return false;
            };
            let Some(signature) = program.resolved_signatures.get(&set_get_any_string_owner) else {
                return false;
            };
            receipt.owner == set_get_any_string_owner
                && receipt.profile_version
                    == crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyStringLineageProfileVersion::V1
                && crate::core::cfg::validate_ordinary_map_owned_any_v2_set_get_any_string_lineage_receipt(
                    receipt,
                    cfg,
                    body,
                    signature,
                    &program.resolved_types,
                    &analysis.ordinary_map_owned_any_v2_operation_attempts,
                )
                .is_ok()
        });
    let direct_tuple_get_owner = crate::core::NodeId("function:main".into());
    let direct_tuple_get_source_receipt = program
        .resource_analysis(&direct_tuple_get_owner)
        .and_then(|analysis| {
            analysis
                .ordinary_map_owned_any_v2_direct_tuple_get_unpack_receipt
                .as_ref()
                .map(|receipt| (analysis, receipt))
        });
    let direct_tuple_get_unpack = direct_tuple_get_source_receipt
        .is_some_and(|(analysis, receipt)| {
            let Some(cfg) = program.callable_cfgs.get(&direct_tuple_get_owner) else {
                return false;
            };
            let Some(body) = program.resolved_bodies.get(&direct_tuple_get_owner) else {
                return false;
            };
            let Some(signature) = program.resolved_signatures.get(&direct_tuple_get_owner) else {
                return false;
            };
            receipt.owner == direct_tuple_get_owner
                && receipt.profile_version
                    == crate::core::ownership::OrdinaryMapOwnedAnyV2DirectTupleGetUnpackProfileVersion::V1
                && (receipt.unpack_target_type == i32_type
                    || receipt.unpack_target_type == string_type)
                && receipt.unpack_conversion_kind
                    == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                && matches!(
                    program.resolved_types.get(&receipt.unpack_source_type),
                    Some(crate::core::ResolvedType::DynamicAny { .. })
                )
                && crate::core::cfg::validate_ordinary_map_owned_any_v2_direct_tuple_get_unpack_receipt(
                    receipt,
                    cfg,
                    body,
                    signature,
                    &program.resolved_types,
                    &analysis.ordinary_map_owned_any_v2_operation_attempts,
                )
                .is_ok()
        });
    let set_get_any_result_i32_owner = crate::core::NodeId("function:main".into());
    let set_get_any_result_i32_source_receipt = program
        .resource_analysis(&set_get_any_result_i32_owner)
        .and_then(|analysis| {
            analysis
                .ordinary_map_owned_any_v2_set_get_any_result_i32_lineage_receipt
                .as_ref()
                .map(|receipt| (analysis, receipt))
        });
    // Keep this receipt family separate from the tuple-valued `map_get`
    // source receipt above. The public map_get_any route owns a Result
    // envelope and independently proves the selected i32 payload, both
    // projections, explicit pair/root cleanup, and the complete local CFG.
    // The route candidate also replays the whole-program callable/callsite
    // census, so another Map operation cannot borrow this credential.
    let set_get_any_result_i32 = set_get_any_result_i32_source_receipt
        .is_some_and(|(_, receipt)| {
            receipt.owner == set_get_any_result_i32_owner
                && receipt.profile_version
                    == crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyResultI32LineageProfileVersion::V1
                && receipt.get_api
                    == crate::core::ownership::OrdinaryMapOwnedAnyV2GetApi::MapGetAnyResult
                && receipt.get_unpack_conversion_kind
                    == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                && receipt.get_unpack_target_type == i32_type
                && matches!(
                    program.resolved_types.get(&receipt.get_unpack_source_type),
                    Some(crate::core::ResolvedType::DynamicAny { .. })
                )
        });
    let zero_miss_i32 = route_admission.is_zero_miss_i32();
    let clone_isolation = route_admission.is_ordinary_map_owned_any_v2_clone_isolation();
    let multi_unpack_count = if checked_get_multi {
        Some(2)
    } else if zero_miss_i32 {
        Some(2)
    } else if clone_isolation {
        Some(3)
    } else {
        None
    };
    let mut errors = Vec::new();

    // Multi-unpack profiles have their own whole-source classifiers. Keep
    // their Checker candidate census closed to the exact main callable and
    // require a one-to-one correspondence with every CFG unpack receipt.
    // CheckedGet multi-lookup binds every candidate to its own static-key,
    // result, found, unpack, and pair-settlement receipt; it admits exactly
    // one selected String Set hit and one canonical i32 miss. Clone Isolation
    // additionally binds each candidate to a selected static-key Set; its
    // all-String source shape cannot use the canonical i32 miss.
    let exact_multi_profile_candidate_set = multi_unpack_count.is_some_and(|expected| {
        let main_owner = crate::core::NodeId("function:main".into());
        let Some(calls) = candidates.get(&main_owner) else {
            return false;
        };
        if candidates.len() != 1 || calls.len() != expected || candidate_count != expected {
            return false;
        }
        let Some(cfg) = program
            .resource_analysis(&main_owner)
            .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref())
        else {
            return false;
        };
        let receipt_calls = cfg
            .unpack_receipts
            .iter()
            .map(|receipt| receipt.cast.clone())
            .collect::<BTreeSet<_>>();
        let operations = cfg
            .unpack_receipts
            .iter()
            .map(|receipt| receipt.operation.clone())
            .collect::<BTreeSet<_>>();
        let clone_static_key_casts = cfg
            .static_key_get_receipts
            .iter()
            .map(|receipt| receipt.unpack.cast.clone())
            .collect::<BTreeSet<_>>();
        let clone_static_key_cover = !clone_isolation
            || (cfg.static_key_get_receipts.len() == expected
                && clone_static_key_casts.len() == expected
                && &clone_static_key_casts == calls);
        let checked_get_multi_static_key_cover = !checked_get_multi || {
            let found_receipts = &cfg.found_observation_receipts;
            let found_bindings = found_receipts
                .iter()
                .map(|receipt| receipt.found_binding.clone())
                .collect::<BTreeSet<_>>();
            let found_pairs = found_receipts
                .iter()
                .map(|receipt| receipt.pair_binding.clone())
                .collect::<BTreeSet<_>>();
            let found_operations = found_receipts
                .iter()
                .map(|receipt| receipt.operation.clone())
                .collect::<BTreeSet<_>>();
            let found_outcomes = found_receipts
                .iter()
                .map(|receipt| receipt.outcome_node.clone())
                .collect::<BTreeSet<_>>();
            let found_source_uses = found_receipts
                .iter()
                .flat_map(|receipt| receipt.found_source_uses.iter().cloned())
                .collect::<BTreeSet<_>>();
            let pair_settlements = cfg
                .static_key_get_receipts
                .iter()
                .map(|receipt| receipt.unpack.settlement_point.clone())
                .collect::<BTreeSet<_>>();
            let query_digests = cfg
                .static_key_get_receipts
                .iter()
                .map(|receipt| receipt.query_digest.clone())
                .collect::<BTreeSet<_>>();
            let selected_string_count = cfg
                .static_key_get_receipts
                .iter()
                .filter(|receipt| {
                    matches!(
                        &receipt.payload_source,
                        crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource::SelectedSet { value_type }
                            if value_type == &string_type
                    )
                })
                .count();
            let canonical_i32_miss_count = cfg
                .static_key_get_receipts
                .iter()
                .filter(|receipt| {
                    matches!(
                        &receipt.payload_source,
                        crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource::I32MissAtNew
                            | crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource::I32MissAtRemove
                    )
                })
                .count();
            let pairings_are_exact = cfg.static_key_get_receipts.iter().all(|authority| {
                let action = &authority.get_action;
                let unpack = &authority.unpack;
                let matching_actions = cfg
                    .actions
                    .iter()
                    .filter(|candidate| {
                        candidate.point == action.point
                            && candidate.kind
                                == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                            && **candidate == *action
                    })
                    .count();
                let matching_unpacks = cfg
                    .unpack_receipts
                    .iter()
                    .filter(|candidate| **candidate == *unpack)
                    .count();
                let matching_results = cfg
                    .result_receipts
                    .iter()
                    .filter(|result| {
                        result.operation == action.point
                            && result.kind
                                == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                            && result.binding == unpack.pair_binding
                            && result.binding_point == unpack.pair_binding_point
                            && action.result_local.as_ref() == Some(&result.binding)
                    })
                    .collect::<Vec<_>>();
                let [result] = matching_results.as_slice() else {
                    return false;
                };
                let matching_found = found_receipts
                    .iter()
                    .filter(|found| {
                        found.operation == action.point
                            && found.pair_binding == result.binding
                            && found.pair_binding_point == result.binding_point
                    })
                    .collect::<Vec<_>>();
                let [found] = matching_found.as_slice() else {
                    return false;
                };
                let target_type = program
                    .call_sites
                    .get(&unpack.cast)
                    .and_then(|site| match site.callee.as_str() {
                        "unpack_i32" => Some(&i32_type),
                        "unpack_string" => Some(&string_type),
                        _ => None,
                    });
                let payload_matches = match &authority.payload_source {
                    crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource::SelectedSet {
                        value_type,
                    } => {
                        target_type == Some(value_type)
                            && authority.selected_winning_set_action.as_ref().is_some_and(
                                |set| {
                                    set.kind
                                        == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                                        && set.key_static_digest.as_deref()
                                            == Some(authority.query_digest.as_str())
                                        && set.value_type.as_ref() == Some(value_type)
                                        && set.payload_clone
                                },
                            )
                    }
                    crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource::I32MissAtNew
                    | crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource::I32MissAtRemove => {
                        target_type == Some(&i32_type)
                            && authority.selected_winning_set_action.is_none()
                    }
                };
                authority.profile_version
                    == crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetProfileVersion::V1
                    && authority.owner == main_owner
                    && authority.query_digest == action.key_static_digest.clone().unwrap_or_default()
                    && valid_static_digest(Some(&authority.query_digest))
                    && action.key_type.as_ref() == Some(&string_type)
                    && unpack.operation == action.point
                    && unpack.pair_binding == result.binding
                    && unpack.pair_binding_point == result.binding_point
                    && unpack.settlement
                        == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                    && result.settlement == unpack.settlement
                    && result.settlement_point.as_ref() == Some(&unpack.settlement_point)
                    && matching_actions == 1
                    && matching_unpacks == 1
                    && matching_results.len() == 1
                    && found.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && found.operation == action.point
                    && found.pair_binding == result.binding
                    && found.pair_binding_point == result.binding_point
                    && !found.found_source_uses.is_empty()
                    && payload_matches
            });
            cfg.static_key_get_receipts.len() == expected
                && found_receipts.len() == expected
                && clone_static_key_casts.len() == expected
                && &clone_static_key_casts == calls
                && found_bindings.len() == expected
                && found_pairs.len() == expected
                && found_operations.len() == expected
                && found_outcomes.len() == 1
                && found_source_uses.len()
                    == found_receipts
                        .iter()
                        .map(|receipt| receipt.found_source_uses.len())
                        .sum::<usize>()
                && pair_settlements.len() == expected
                && query_digests.len() == expected
                && selected_string_count == 1
                && canonical_i32_miss_count == 1
                && pairings_are_exact
        };
        cfg.owner == main_owner
            && cfg.profile_version == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
            && cfg.unpack_receipts.len() == expected
            && cfg.result_receipts.len() == expected
            && cfg
                .result_receipts
                .iter()
                .all(|result| result.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny)
            && receipt_calls.len() == expected
            && operations.len() == expected
            && &receipt_calls == calls
            && clone_static_key_cover
            && checked_get_multi_static_key_cover
    });

    // DynamicKeysGet is one closed two-GetAny profile. Its Checker receipt
    // already pairs each miss/hit action, result, unpack, and payload local;
    // admit only those two cast nodes as one candidate set.
    let dynamic_keys_get_owner = crate::core::NodeId("function:main".into());
    let exact_dynamic_keys_get_candidate_set = dynamic_keys_get
        && candidate_count == 2
        && candidates.len() == 1
        && candidates
            .get(&dynamic_keys_get_owner)
            .is_some_and(|calls| {
                if calls.len() != 2 {
                    return false;
                }
                let Some(cfg) = program
                    .resource_analysis(&dynamic_keys_get_owner)
                    .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref())
                else {
                    return false;
                };
                let [receipt] = cfg.dynamic_keys_get_receipts.as_slice() else {
                    return false;
                };
                let receipt_calls = BTreeSet::from([
                    receipt.miss_unpack.cast.clone(),
                    receipt.hit_unpack.cast.clone(),
                ]);
                let cfg_unpack_calls = cfg
                    .unpack_receipts
                    .iter()
                    .map(|unpack| unpack.cast.clone())
                    .collect::<BTreeSet<_>>();
                cfg.owner == dynamic_keys_get_owner
                    && receipt.owner == dynamic_keys_get_owner
                    && receipt_calls.len() == 2
                    && calls == &receipt_calls
                    && cfg.unpack_receipts.len() == 2
                    && cfg_unpack_calls.len() == 2
                    && cfg_unpack_calls == receipt_calls
            });

    // The Result-aware public GetAny route has a separate composite Checker
    // receipt. It authorizes exactly its one `unpack_i32` callsite; a generic
    // V2 GetAny receipt alone must not open this cast boundary.
    let exact_set_get_any_i32_candidate_set = set_get_any_i32
        && candidate_count == 1
        && candidates.len() == 1
        && candidates
            .get(&set_get_any_i32_owner)
            .is_some_and(|calls| {
                let Some((_, receipt)) = set_get_any_i32_source_receipt else {
                    return false;
                };
                receipt.owner == set_get_any_i32_owner
                    && receipt.profile_version
                        == crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyI32LineageProfileVersion::V1
                    && calls == &BTreeSet::from([receipt.unpack_operation.clone()])
            });
    let exact_set_get_any_result_i32_candidate_set = set_get_any_result_i32
        && candidate_count == 1
        && candidates.len() == 1
        && candidates
            .get(&set_get_any_result_i32_owner)
            .is_some_and(|calls| {
                let Some((_, receipt)) = set_get_any_result_i32_source_receipt else {
                    return false;
                };
                calls == &BTreeSet::from([receipt.get_unpack_operation.clone()])
            });
    let exact_set_get_any_string_candidate_set = set_get_any_string
        && candidate_count == 1
        && candidates.len() == 1
        && candidates
            .get(&set_get_any_string_owner)
            .is_some_and(|calls| {
                let Some((_, receipt)) = set_get_any_string_source_receipt else {
                    return false;
                };
                receipt.owner == set_get_any_string_owner
                    && receipt.profile_version
                        == crate::core::ownership::OrdinaryMapOwnedAnyV2SetGetAnyStringLineageProfileVersion::V1
                    && calls == &BTreeSet::from([receipt.unpack_operation.clone()])
            });
    let exact_direct_tuple_get_candidate_set = direct_tuple_get_unpack
        && candidate_count == 1
        && candidates.len() == 1
        && candidates
            .get(&direct_tuple_get_owner)
            .is_some_and(|calls| {
                let Some((_, receipt)) = direct_tuple_get_source_receipt else {
                    return false;
                };
                receipt.owner == direct_tuple_get_owner
                    && calls == &BTreeSet::from([receipt.unpack_operation.clone()])
            });

    // ZeroMissI32 is a two-lookup profile, but the lookup count alone must
    // never make two unrelated GetAny casts interchangeable. Join both
    // observed pair identities to their exact GetAny action, unpack, selected
    // or canonical-miss payload lineage, and one explicit whole-pair Drop.
    let exact_zero_miss_i32_candidate_set = zero_miss_i32
        && candidate_count == 2
        && candidates.len() == 1
        && candidates
            .get(&crate::core::NodeId("function:main".into()))
            .is_some_and(|calls| {
                let owner = crate::core::NodeId("function:main".into());
                if calls.len() != 2 {
                    return false;
                }
                let Some(cfg) = program
                    .resource_analysis(&owner)
                    .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref())
                else {
                    return false;
                };
                let Some(source) = cfg.zero_miss_i32_observation_receipt.as_ref() else {
                    return false;
                };
                let observations = [&source.hit_observation, &source.miss_observation];
                let observation_calls = observations
                    .iter()
                    .map(|observation| {
                        cfg.unpack_receipts
                            .iter()
                            .find(|unpack| {
                                unpack.operation == observation.operation
                                    && unpack.pair_binding == observation.pair_binding
                            })
                            .map(|unpack| unpack.cast.clone())
                    })
                    .collect::<Option<BTreeSet<_>>>();
                let Some(observation_calls) = observation_calls else {
                    return false;
                };
                if observation_calls.len() != 2 || &observation_calls != calls {
                    return false;
                }
                let new_actions = cfg
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::New
                    })
                    .collect::<Vec<_>>();
                let set_actions = cfg
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                    })
                    .collect::<Vec<_>>();
                let get_actions = cfg
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                    })
                    .collect::<Vec<_>>();
                let root_drops = cfg
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Drop
                    })
                    .collect::<Vec<_>>();
                let ([new], [set]) = (new_actions.as_slice(), set_actions.as_slice()) else {
                    return false;
                };
                let hit_digest = blake3::hash(b"hit").to_hex().to_string();
                let miss_digest = blake3::hash(b"absent").to_hex().to_string();
                let exact_observation_pair = observations.iter().all(|observation| {
                    let actions = cfg
                        .actions
                        .iter()
                        .filter(|action| {
                            action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                                && action.point == observation.operation
                        })
                        .collect::<Vec<_>>();
                    let [action] = actions.as_slice() else {
                        return false;
                    };
                    let results = cfg
                        .result_receipts
                        .iter()
                        .filter(|result| {
                            result.operation == observation.operation
                                && result.kind
                                    == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                                && result.binding == observation.pair_binding
                                && result.binding_point == observation.pair_binding_point
                        })
                        .collect::<Vec<_>>();
                    let [result] = results.as_slice() else {
                        return false;
                    };
                    let unpacks = cfg
                        .unpack_receipts
                        .iter()
                        .filter(|unpack| {
                            unpack.operation == observation.operation
                                && unpack.pair_binding == observation.pair_binding
                                && unpack.pair_binding_point == observation.pair_binding_point
                        })
                        .collect::<Vec<_>>();
                    let [unpack] = unpacks.as_slice() else {
                        return false;
                    };
                    let found_receipts = cfg
                        .found_observation_receipts
                        .iter()
                        .filter(|found| **found == **observation)
                        .count();
                    let is_hit = observation.operation == source.hit_observation.operation;
                    let expected_digest = if is_hit { &hit_digest } else { &miss_digest };
                    let exact_lookup_key = action.key_type.as_ref() == Some(&string_type)
                        && action.key_static_digest.as_ref() == Some(expected_digest)
                        && action.key_local.is_none()
                        && action.key_provenance
                            == Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::StringLiteral)
                        && action.local == set.local
                        && action.root == set.root
                        && action.source_local.as_ref() == Some(&set.local)
                        && action.source.as_ref() == Some(&set.root);
                    let exact_payload_lineage = if is_hit {
                        set.key_type.as_ref() == Some(&string_type)
                            && set.key_static_digest.as_deref() == Some(hit_digest.as_str())
                            && set.value_type.as_ref() == Some(&i32_type)
                            && set.payload_clone
                    } else {
                        set.key_type.as_ref() == Some(&string_type)
                            && set.key_static_digest.as_deref() == Some(hit_digest.as_str())
                            && set.value_type.as_ref() == Some(&i32_type)
                            && set.payload_clone
                            && action.key_static_digest.as_deref() == Some(miss_digest.as_str())
                    };
                    action.result_local.as_ref() == Some(&result.binding)
                        && action.result_type.as_ref() == Some(&result.result_type)
                        && action.source_local.as_ref() == Some(&result.source_local)
                        && action.source.as_ref() == Some(&result.source)
                        && exact_lookup_key
                        && exact_payload_lineage
                        && result.profile_version
                            == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                        && result.settlement
                            == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                        && result.settlement_point.as_ref() == Some(&unpack.settlement_point)
                        && unpack.profile_version
                            == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                        && calls.contains(&unpack.cast)
                        && unpack.conversion_kind
                            == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                        && unpack.conversion_to == i32_type
                        && unpack.conversion_from
                            == match program.resolved_types.get(&result.result_type) {
                                Some(crate::core::ResolvedType::Tuple(fields))
                                    if fields.len() == 2
                                        && fields[0] == bool_type
                                        && matches!(
                                            program.resolved_types.get(&fields[1]),
                                            Some(crate::core::ResolvedType::DynamicAny { .. })
                                        ) =>
                                {
                                    fields[1].clone()
                                }
                                _ => return false,
                            }
                        && found_receipts == 1
                        && observation.profile_version
                            == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                });
                cfg.owner == owner
                    && cfg.profile_version == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && source.owner == owner
                    && source.hit_observation.operation != source.miss_observation.operation
                    && cfg.actions.len() == 6
                    && cfg.actions.iter().all(|action| {
                        matches!(
                            action.kind,
                            crate::core::OrdinaryMapOwnedAnyV2ActionKind::New
                                | crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                                | crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                                | crate::core::OrdinaryMapOwnedAnyV2ActionKind::Drop
                        )
                    })
                    && set.source_local.as_ref() == Some(&new.local)
                    && set.source.as_ref() == Some(&new.root)
                    && get_actions.len() == 2
                    && root_drops.len() == 2
                    && root_drops
                        .iter()
                        .filter(|drop| drop.local == new.local && drop.root == new.root)
                        .count()
                        == 1
                    && root_drops
                        .iter()
                        .filter(|drop| drop.local == set.local && drop.root == set.root)
                        .count()
                        == 1
                    && cfg.result_receipts.len() == 2
                    && cfg.unpack_receipts.len() == 2
                    && cfg.found_observation_receipts.len() == 2
                    && exact_observation_pair
            });

    // Complete-coverage diagnostic materialization may contain several
    // independently checked GetAny unpacks (including more than one owner),
    // but every builtin candidate must have exactly one distinct static-key
    // Checker credential and no extra credential may be smuggled in. Nominal
    // `pair.1 as Label` conversions are intentionally outside this builtin
    // census and keep their separate conversion/TypeDesc validation.
    let candidate_identities = candidates
        .iter()
        .flat_map(|(owner, calls)| calls.iter().map(|call| (owner.clone(), call.clone())))
        .collect::<BTreeSet<_>>();
    let static_key_get_receipts = program
        .resource_analyses()
        .iter()
        .flat_map(|(owner, analysis)| {
            analysis
                .ordinary_map_owned_any_v2_cfg_receipt
                .iter()
                .flat_map(move |cfg| {
                    cfg.static_key_get_receipts
                        .iter()
                        .map(move |receipt| (owner.clone(), receipt))
                })
        })
        .collect::<Vec<_>>();
    let builtin_static_key_get_receipts = static_key_get_receipts
        .iter()
        .filter(|(owner, receipt)| {
            program
                .call_sites
                .get(&receipt.unpack.cast)
                .is_some_and(|site| {
                    site.node_id == receipt.unpack.cast
                        && site.owner == owner.0
                        && site.kind == ResolvedCallKind::Builtin
                        && site.argc == 1
                        && site.arity_matches()
                        && matches!(site.callee.as_str(), "unpack_i32" | "unpack_string")
                })
        })
        .collect::<Vec<_>>();
    let static_key_candidate_identities = builtin_static_key_get_receipts
        .iter()
        .map(|(owner, receipt)| (owner.clone(), receipt.unpack.cast.clone()))
        .collect::<BTreeSet<_>>();
    let exact_complete_coverage_candidate_set = route_admission.is_complete()
        && multi_unpack_count.is_none()
        && candidate_count > 0
        && candidate_identities.len() == candidate_count
        && builtin_static_key_get_receipts.len() == candidate_count
        && static_key_candidate_identities.len() == candidate_count
        && candidate_identities == static_key_candidate_identities;
    let candidate_census_is_exact = if dynamic_keys_get {
        exact_dynamic_keys_get_candidate_set
    } else if direct_tuple_get_unpack {
        exact_direct_tuple_get_candidate_set
    } else if set_get_any_result_i32 {
        exact_set_get_any_result_i32_candidate_set
    } else if set_get_any_string {
        exact_set_get_any_string_candidate_set
    } else if set_get_any_i32 {
        exact_set_get_any_i32_candidate_set
    } else if zero_miss_i32 {
        exact_zero_miss_i32_candidate_set
    } else if let Some(expected) = multi_unpack_count {
        candidate_count == expected && exact_multi_profile_candidate_set
    } else if route_admission.is_complete() {
        exact_complete_coverage_candidate_set
    } else {
        candidate_count == 1
    };

    for (owner, calls) in candidates {
        for call in calls {
            let site = program.call_sites.get(call);
            let target_type = site.and_then(|site| match site.callee.as_str() {
                "unpack_i32" => Some(&i32_type),
                "unpack_string" => Some(&string_type),
                _ => None,
            });
            let exact_builtin_call = site.is_some_and(|site| {
                site.node_id == *call
                    && site.owner == owner.0
                    && site.kind == ResolvedCallKind::Builtin
                    && site.argc == 1
                    && site.arity_matches()
                    && site.resolved_callee.as_ref().is_some_and(|callee| {
                        matches!(callee, crate::core::ir::ResolvedCallee::Builtin(builtin)
                            if builtin.as_str() == site.callee.as_str())
                    })
            });
            let cfg = program
                .resource_analysis(owner)
                .and_then(|analysis| analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref());
            let exact_cfg = cfg.is_some_and(|cfg| {
                let matching_unpacks = cfg
                    .unpack_receipts
                    .iter()
                    .filter(|receipt| receipt.cast == *call)
                    .collect::<Vec<_>>();
                let [unpack] = matching_unpacks.as_slice() else {
                    return false;
                };
                let matching_results = cfg
                    .result_receipts
                    .iter()
                    .filter(|result| {
                        result.operation == unpack.operation
                            && result.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                            && result.binding == unpack.pair_binding
                            && result.binding_point == unpack.pair_binding_point
                    })
                    .collect::<Vec<_>>();
                let [result] = matching_results.as_slice() else {
                    return false;
                };
                let exact_tuple_getany_result = matches!(
                    program.resolved_types.get(&result.result_type),
                    Some(crate::core::ResolvedType::Tuple(fields))
                        if fields.len() == 2
                            && fields[0] == bool_type
                            && matches!(
                                program.resolved_types.get(&fields[1]),
                                Some(crate::core::ResolvedType::DynamicAny { .. })
                            )
                            && fields[1] == unpack.conversion_from
                );
                cfg.owner == *owner
                    && cfg.profile_version == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && cfg.values_unpack_receipts.is_empty()
                    && unpack.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && unpack.conversion_kind
                        == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                    && Some(&unpack.conversion_to) == target_type
                    && unpack.settlement
                        == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                    && result.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && exact_tuple_getany_result
                    && result.settlement == unpack.settlement
                    && result.settlement_point.as_ref() == Some(&unpack.settlement_point)
                    && matches!(
                        program.resolved_types.get(&unpack.conversion_from),
                        Some(crate::core::ResolvedType::DynamicAny { .. })
                    )
            });
            let exact_map_root_get_any_caller = if map_root_get_any_caller
                && target_type == Some(&string_type)
            {
                let matching_receipts = program
                    .resource_analyses()
                    .iter()
                    .filter_map(|(holder, analysis)| {
                        let cfg_receipt =
                            analysis.ordinary_map_owned_any_v2_cfg_receipt.as_ref()?;
                        let receipt = cfg_receipt.map_root_get_any_caller_receipt.as_ref()?;
                        (receipt.caller_owner == *owner && receipt.unpack_cast == *call)
                            .then_some((holder, cfg_receipt, receipt))
                    })
                    .collect::<Vec<_>>();
                matches!(matching_receipts.as_slice(), [(holder, producer_cfg, receipt)]
                    if receipt.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2MapRootGetAnyCallerProfileVersion::V1
                        && *holder == &receipt.producer_owner
                        && producer_cfg.owner == receipt.producer_owner
                        && producer_cfg.profile_version
                            == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                        && producer_cfg.map_root_producer_return_receipt.as_ref().is_some_and(
                            |producer| {
                                producer.owner == receipt.producer_owner
                                    && producer.map_set_operation
                                        == receipt.producer_map_set_operation
                                    && producer.return_value == receipt.producer_return_value
                                    && producer.final_root_local == receipt.producer_final_local
                                    && producer.final_root_binding_point
                                        == receipt.producer_final_binding_point
                                    && producer.final_root == receipt.logical_root
                                    && producer.final_root_type == receipt.caller_root_type
                            }
                        )
                        && receipt.caller_owner == *owner
                        && receipt.unpack_cast == *call
                        && receipt.producer_owner != receipt.caller_owner
                        && receipt.call_target == receipt.producer_owner
                        && receipt.string_type == string_type
                )
            } else {
                false
            };
            let exact_dynamic_keys_get_selected_payload_authority = cfg.is_some_and(|cfg| {
                if !dynamic_keys_get
                    || owner != &dynamic_keys_get_owner
                    || target_type != Some(&i32_type)
                {
                    return false;
                }
                let [source] = cfg.dynamic_keys_get_receipts.as_slice() else {
                    return false;
                };
                let (action, result, found, unpack, pair_drop, found_term, payload_term) =
                    if source.miss_unpack.cast == *call {
                        (
                            &source.miss_get_action,
                            &source.miss_result,
                            &source.miss_found,
                            &source.miss_unpack,
                            &source.miss_pair_drop_point,
                            &source.outcome_terms[2],
                            &source.outcome_terms[3],
                        )
                    } else if source.hit_unpack.cast == *call {
                        (
                            &source.hit_get_action,
                            &source.hit_result,
                            &source.hit_found,
                            &source.hit_unpack,
                            &source.hit_pair_drop_point,
                            &source.outcome_terms[0],
                            &source.outcome_terms[1],
                        )
                    } else {
                        return false;
                    };
                let pair_payload_type = match program.resolved_types.get(&result.result_type) {
                    Some(crate::core::ResolvedType::Tuple(fields))
                        if fields.len() == 2 && fields[0] == bool_type =>
                    {
                        fields.get(1)
                    }
                    _ => None,
                };
                let payload_is_dynamic_any = pair_payload_type.is_some_and(|payload| {
                    matches!(
                        program.resolved_types.get(payload),
                        Some(crate::core::ResolvedType::DynamicAny { .. })
                    )
                });
                let required_source_nodes = [
                    &action.point,
                    &result.result,
                    &result.binding_point,
                    &result.source_node,
                    &found.projection,
                    &unpack.unpack_binding_point,
                    &unpack.projection,
                    &unpack.cast,
                    &unpack.settlement_point,
                    &source.outcome_observation_call,
                    &source.outcome_observation_load,
                    &found_term.expression,
                    &found_term.operand,
                    &payload_term.expression,
                    &payload_term.operand,
                ];
                let action_is_unique = cfg
                    .actions
                    .iter()
                    .filter(|candidate| candidate.point == action.point)
                    .count()
                    == 1
                    && cfg
                        .actions
                        .iter()
                        .filter(|candidate| *candidate == action)
                        .count()
                        == 1;
                let result_is_unique = cfg
                    .result_receipts
                    .iter()
                    .filter(|candidate| *candidate == result)
                    .count()
                    == 1;
                let unpack_is_unique = cfg
                    .unpack_receipts
                    .iter()
                    .filter(|candidate| *candidate == unpack)
                    .count()
                    == 1;
                let found_is_unique = cfg
                    .found_observation_receipts
                    .iter()
                    .filter(|candidate| *candidate == found)
                    .count()
                    == 1;
                cfg.owner == *owner
                    && cfg.profile_version == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && cfg.dynamic_keys_get_receipts.len() == 1
                    && cfg.actions.len() == 7
                    && cfg.result_receipts.len() == 2
                    && cfg.unpack_receipts.len() == 2
                    && cfg.found_observation_receipts.len() == 2
                    && cfg.static_key_get_receipts.is_empty()
                    && cfg.computed_key_string_get_receipts.is_empty()
                    && source.owner == *owner
                    && source.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2DynamicKeysGetProfileVersion::V1
                    && source.miss_unpack.cast != source.hit_unpack.cast
                    && source.miss_result.binding != source.hit_result.binding
                    && source.miss_unpack.pair_binding != source.hit_unpack.pair_binding
                    && source.miss_unpack.unpack_binding != source.hit_unpack.unpack_binding
                    && action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                    && action.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && action.result_local.as_ref() == Some(&result.binding)
                    && action.result_type.as_ref() == Some(&result.result_type)
                    && action.source_local.as_ref() == Some(&result.source_local)
                    && action.source.as_ref() == Some(&result.source)
                    && action.source_node.as_ref() == Some(&result.source_node)
                    && action.source_type.as_ref() == Some(&result.source_type)
                    && action.key == result.key
                    && action.key_type == result.key_type
                    && action.key_local == result.key_local
                    && action.key_type.as_ref() == Some(&string_type)
                    && action.key_local.as_ref() == Some(&source.key_local)
                    && action.key_local_mutable == Some(false)
                    && action.key_provenance
                        == Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::DirectLocal)
                    && action.key_static_digest.is_none()
                    && result.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && result.operation == action.point
                    && result.result == action.point
                    && result.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                    && result.settlement
                        == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                    && result.settlement_point.as_ref() == Some(pair_drop)
                    && payload_is_dynamic_any
                    && pair_payload_type == Some(&unpack.conversion_from)
                    && found.profile_version == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && found.operation == action.point
                    && found.pair_binding == result.binding
                    && found.pair_binding_point == result.binding_point
                    && unpack.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                    && unpack.operation == action.point
                    && unpack.pair_binding == result.binding
                    && unpack.pair_binding_point == result.binding_point
                    && unpack.cast == *call
                    && unpack.conversion_kind
                        == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                    && unpack.conversion_to == i32_type
                    && target_type == Some(&unpack.conversion_to)
                    && unpack.settlement == result.settlement
                    && &unpack.settlement_point == pair_drop
                    && unpack.unpack_binding_point != *pair_drop
                    && found_term.local == found.found_binding
                    && payload_term.local == unpack.unpack_binding
                    && required_source_nodes
                        .iter()
                        .all(|node| source.source_identity_nodes.contains(*node))
                    && [
                        &result.binding_point,
                        &unpack.unpack_binding_point,
                        pair_drop,
                    ]
                    .iter()
                    .all(|node| source.source_statement_nodes.contains(*node))
                    && action_is_unique
                    && result_is_unique
                    && unpack_is_unique
                    && found_is_unique
            });
            let exact_static_key_selected_payload_authority = cfg.is_some_and(|cfg| {
                use crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetPayloadSource as PayloadSource;
                use crate::core::OrdinaryMapOwnedAnyV2StaticKeyGetProfileVersion as ProfileVersion;

                if !route_admission.is_complete() && !clone_isolation && !checked_get_multi {
                    return false;
                }
                let matching_authorities = cfg
                    .static_key_get_receipts
                    .iter()
                    .filter(|receipt| receipt.unpack.cast == *call)
                    .collect::<Vec<_>>();
                let [authority] = matching_authorities.as_slice() else {
                    return false;
                };
                let matching_unpacks = cfg
                    .unpack_receipts
                    .iter()
                    .filter(|unpack| unpack.cast == *call)
                    .collect::<Vec<_>>();
                let [unpack] = matching_unpacks.as_slice() else {
                    return false;
                };
                let matching_gets = cfg
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                            && action.point == unpack.operation
                    })
                    .collect::<Vec<_>>();
                let [get] = matching_gets.as_slice() else {
                    return false;
                };
                let matching_results = cfg
                    .result_receipts
                    .iter()
                    .filter(|result| {
                        result.operation == get.point
                            && result.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                            && result.binding == unpack.pair_binding
                            && result.binding_point == unpack.pair_binding_point
                    })
                    .collect::<Vec<_>>();
                let [result] = matching_results.as_slice() else {
                    return false;
                };
                let exact_join = cfg.owner == *owner
                    && authority.profile_version == ProfileVersion::V1
                    && authority.owner == *owner
                    && authority.get_action == **get
                    && authority.unpack == **unpack
                    && get.key_static_digest.as_ref().is_some_and(|digest| {
                        digest == &authority.query_digest && valid_static_digest(Some(digest))
                    })
                    && get.key_type.as_ref() == Some(&string_type)
                    && get.result_local.as_ref() == Some(&result.binding)
                    && result.source_local == get.local
                    && result.source == get.root
                    && result.key == get.key
                    && result.key_type == get.key_type
                    && result.key_local == get.key_local
                    && result.settlement
                        == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                    && unpack.settlement == result.settlement
                    && result.settlement_point.as_ref() == Some(&unpack.settlement_point);
                let exact_payload_source = match &authority.payload_source {
                    PayloadSource::SelectedSet { value_type } => authority
                        .selected_winning_set_action
                        .as_ref()
                        .is_some_and(|set| {
                            set.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                                && set.profile_version
                                    == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                                && set.key_type.as_ref() == Some(&string_type)
                                && set.key_static_digest.as_deref()
                                    == Some(authority.query_digest.as_str())
                                && set.payload_clone
                                && set.value_type.as_ref() == Some(value_type)
                                && Some(value_type) == target_type
                                && cfg.actions.iter().filter(|action| *action == set).count() == 1
                        }),
                    PayloadSource::I32MissAtNew | PayloadSource::I32MissAtRemove
                        if !clone_isolation =>
                    {
                        target_type == Some(&i32_type)
                            && authority.selected_winning_set_action.is_none()
                    }
                    PayloadSource::I32MissAtNew | PayloadSource::I32MissAtRemove => false,
                };
                exact_join && exact_payload_source
            });
            let exact_source_lineage = cfg.is_some_and(|cfg| {
                let sets = cfg
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                    })
                    .collect::<Vec<_>>();
                let gets = cfg
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                    })
                    .collect::<Vec<_>>();
                let [set] = sets.as_slice() else {
                    return false;
                };
                let [get] = gets.as_slice() else {
                    return false;
                };
                let [unpack] = cfg.unpack_receipts.as_slice() else {
                    return false;
                };
                let Some(set_type) = set.value_type.as_ref() else {
                    return false;
                };
                let Some(result) = cfg.result_receipts.iter().find(|result| {
                    result.operation == get.point
                        && result.kind == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                }) else {
                    return false;
                };
                let key_matches = set.key_static_digest == get.key_static_digest;
                let exact_computed_key_string_credential =
                    if computed_key_get && target_type == Some(&string_type) {
                        let [credential] = cfg.computed_key_string_get_receipts.as_slice() else {
                            return false;
                        };
                        credential.owner == *owner
                            && credential.map_set == **set
                            && credential.get_any == **get
                            && credential.result == *result
                            && credential.unpack == *unpack
                            && credential.unpack.cast == *call
                    } else {
                        !computed_key_get || cfg.computed_key_string_get_receipts.is_empty()
                    };
                let exact_unpack_result_join = unpack.operation == get.point
                    && unpack.pair_binding == result.binding
                    && unpack.pair_binding_point == result.binding_point
                    && unpack.settlement == result.settlement
                    && result.settlement
                        == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                    && result.settlement_point.as_ref() == Some(&unpack.settlement_point)
                    && result.key == get.key
                    && result.key_type == get.key_type
                    && result.key_local == get.key_local;
                let exact_direct_get_set_lineage =
                    get.source_local.as_ref() == Some(&set.local)
                        && get.source.as_ref() == Some(&set.root);
                let exact_checked_get_key_payload = checked_get
                    && valid_static_digest(set.key_static_digest.as_ref())
                    && valid_static_digest(get.key_static_digest.as_ref())
                    && if key_matches {
                        target_type == Some(set_type)
                    } else {
                        target_type == Some(&i32_type)
                    };
                let exact_computed_key_payload = computed_key_get
                    && set.key_static_digest.is_none()
                    && get.key_static_digest.is_none()
                    && set.key_local.is_some()
                    && set.key_local == get.key_local
                    && set.key_local_mutable == Some(false)
                    && get.key_local_mutable == Some(false)
                    && set.key_type == Some(string_type.clone())
                    && get.key_type == Some(string_type.clone())
                    && set.key_provenance
                        == Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::DirectLocal)
                    && get.key_provenance
                        == Some(crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::DirectLocal)
                    && target_type == Some(set_type)
                    && exact_computed_key_string_credential;
                let exact_removed_key_checked_miss = if !removed_key_checked_miss
                    || target_type != Some(&i32_type)
                {
                    false
                } else {
                    let Some(removed_key) = cfg.removed_key_checked_miss_receipt.as_ref() else {
                        return false;
                    };
                    let [found_observation] = cfg.found_observation_receipts.as_slice() else {
                        return false;
                    };
                    let new_actions = cfg
                        .actions
                        .iter()
                        .filter(|action| {
                            action.kind
                                == crate::core::OrdinaryMapOwnedAnyV2ActionKind::New
                        })
                        .collect::<Vec<_>>();
                    let remove_actions = cfg
                        .actions
                        .iter()
                        .filter(|action| {
                            action.kind
                                == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Remove
                        })
                        .collect::<Vec<_>>();
                    let [new] = new_actions.as_slice() else {
                        return false;
                    };
                    let [remove] = remove_actions.as_slice() else {
                        return false;
                    };
                    let map_type = get.source_type.as_ref();
                    let root_actions = [
                        &removed_key.new_action,
                        &removed_key.set_action,
                        &removed_key.remove_action,
                    ];
                    let root_settlements_match = removed_key.root_settlements.len() == 3
                        && root_actions.iter().all(|root| {
                            removed_key
                                .root_settlements
                                .iter()
                                .filter(|drop| {
                                    drop.kind
                                        == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Drop
                                        && drop.local == root.local
                                        && drop.root == root.root
                                })
                                .count()
                                == 1
                        })
                        && removed_key.root_settlements.iter().all(|drop| {
                            cfg.actions.iter().any(|action| action == drop)
                        });
                    let expected_key_digest =
                        blake3::hash(&removed_key.key_bytes).to_hex().to_string();
                    let same_literal_key = !removed_key.key_bytes.is_empty()
                        && [*set, *remove, *get].iter().all(|action| {
                            action.key_type == Some(string_type.clone())
                                && action.key_local.is_none()
                                && action.key_provenance
                                    == Some(
                                        crate::core::OrdinaryMapOwnedAnyV2KeyProvenance::StringLiteral,
                                    )
                                && action.key_static_digest.as_deref()
                                    == Some(expected_key_digest.as_str())
                        });
                    removed_key.profile_version
                        == crate::core::OrdinaryMapOwnedAnyV2ProfileVersion::V2
                        && removed_key.new_action == **new
                        && removed_key.set_action == **set
                        && removed_key.remove_action == **remove
                        && removed_key.get_action == **get
                        && removed_key.get_result == *result
                        && removed_key.found_observation == *found_observation
                        && removed_key.unpack == *unpack
                        && removed_key.new_action.kind
                            == crate::core::OrdinaryMapOwnedAnyV2ActionKind::New
                        && removed_key.set_action.kind
                            == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Set
                        && removed_key.remove_action.kind
                            == crate::core::OrdinaryMapOwnedAnyV2ActionKind::Remove
                        && removed_key.get_action.kind
                            == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                        && removed_key.set_action.source_local
                            == Some(removed_key.new_action.local.clone())
                        && removed_key.set_action.source.as_ref()
                            == Some(&removed_key.new_action.root)
                        && removed_key.remove_action.source_local
                            == Some(removed_key.set_action.local.clone())
                        && removed_key.remove_action.source.as_ref()
                            == Some(&removed_key.set_action.root)
                        && removed_key.get_action.source_local
                            == Some(removed_key.remove_action.local.clone())
                        && removed_key.get_action.source.as_ref()
                            == Some(&removed_key.remove_action.root)
                        && removed_key.set_action.value_type == Some(string_type.clone())
                        && removed_key.get_result.kind
                            == crate::core::OrdinaryMapOwnedAnyV2ActionKind::GetAny
                        && removed_key.get_result.operation == removed_key.get_action.point
                        && removed_key.get_result.source_local == removed_key.remove_action.local
                        && removed_key.get_result.source == removed_key.remove_action.root
                        && map_type.is_some_and(|map_type| {
                            removed_key.get_action.source_type.as_ref() == Some(map_type)
                                && removed_key.remove_action.source_type.as_ref()
                                    == Some(map_type)
                                && removed_key.set_action.source_type.as_ref() == Some(map_type)
                                && removed_key.get_result.source_type == *map_type
                        })
                        && removed_key.unpack.operation == removed_key.get_action.point
                        && removed_key.unpack.pair_binding == removed_key.get_result.binding
                        && removed_key.unpack.pair_binding_point
                            == removed_key.get_result.binding_point
                        && removed_key.unpack.cast == *call
                        && removed_key.unpack.conversion_to == i32_type
                        && removed_key.unpack.settlement
                            == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                        && removed_key.get_result.settlement
                            == crate::core::OrdinaryMapOwnedAnyV2ResultSettlementKind::ExplicitDrop
                        && removed_key.get_result.settlement_point.as_ref()
                            == Some(&removed_key.unpack.settlement_point)
                        && removed_key.found_observation.operation
                            == removed_key.get_action.point
                        && removed_key.found_observation.pair_binding
                            == removed_key.get_result.binding
                        && same_literal_key
                        && root_settlements_match
                };
                let removed_key_receipt_profile_consistent = removed_key_checked_miss
                    || cfg.removed_key_checked_miss_receipt.is_none();
                exact_unpack_result_join
                    && removed_key_receipt_profile_consistent
                    && ((exact_direct_get_set_lineage
                        && (exact_checked_get_key_payload || exact_computed_key_payload))
                        || exact_static_key_selected_payload_authority
                        || exact_removed_key_checked_miss)
            });

            let exact_profile_source_lineage = if direct_tuple_get_unpack {
                owner == &direct_tuple_get_owner
                    && direct_tuple_get_source_receipt.is_some_and(|(_, source)| {
                        source.unpack_operation == *call
                            && source.unpack_conversion_kind
                                == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                            && Some(&source.unpack_target_type) == target_type
                            && matches!(
                                program.resolved_types.get(&source.unpack_source_type),
                                Some(crate::core::ResolvedType::DynamicAny { .. })
                            )
                    })
            } else if set_get_any_result_i32 {
                owner == &set_get_any_result_i32_owner
                    && target_type == Some(&i32_type)
                    && set_get_any_result_i32_source_receipt.is_some_and(|(_, source)| {
                        source.get_unpack_operation == *call
                            && source.get_unpack_conversion_kind
                                == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                            && source.get_unpack_target_type == i32_type
                            && matches!(
                                program.resolved_types.get(&source.get_unpack_source_type),
                                Some(crate::core::ResolvedType::DynamicAny { .. })
                            )
                    })
            } else if set_get_any_i32 {
                owner == &set_get_any_i32_owner
                    && target_type == Some(&i32_type)
                    && set_get_any_i32_source_receipt.is_some_and(|(_, source)| {
                        source.unpack_operation == *call
                            && source.unpack_conversion_kind
                                == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                            && source.unpack_target_type == i32_type
                    })
            } else if set_get_any_string {
                owner == &set_get_any_string_owner
                    && target_type == Some(&string_type)
                    && set_get_any_string_source_receipt.is_some_and(|(_, source)| {
                        source.unpack_operation == *call
                            && source.unpack_conversion_kind
                                == crate::core::ir::CheckedConversionKind::DynamicAnyUnpack
                            && source.unpack_target_type == string_type
                            && matches!(
                                program.resolved_types.get(&source.unpack_source_type),
                                Some(crate::core::ResolvedType::DynamicAny { .. })
                            )
                    })
            } else if dynamic_keys_get {
                exact_dynamic_keys_get_candidate_set
                    && exact_cfg
                    && exact_dynamic_keys_get_selected_payload_authority
            } else if zero_miss_i32 {
                exact_zero_miss_i32_candidate_set && exact_cfg
            } else if multi_unpack_count.is_some() {
                exact_multi_profile_candidate_set
                    && exact_cfg
                    && (!(clone_isolation || checked_get_multi)
                        || exact_static_key_selected_payload_authority)
            } else if map_root_get_any_caller {
                exact_map_root_get_any_caller
            } else {
                ((checked_get || computed_key_get || removed_key_checked_miss)
                    && exact_source_lineage)
                    || exact_static_key_selected_payload_authority
            };
            if !candidate_census_is_exact
                || !(checked_get
                    || computed_key_get
                    || dynamic_keys_get
                    || removed_key_checked_miss
                    || map_root_get_any_caller
                    || direct_tuple_get_unpack
                    || zero_miss_i32
                    || clone_isolation
                    || set_get_any_string
                    || set_get_any_result_i32
                    || set_get_any_i32
                    || exact_static_key_selected_payload_authority)
                || !exact_builtin_call
                || (!exact_cfg
                    && !direct_tuple_get_unpack
                    && !set_get_any_i32
                    && !set_get_any_string
                    && !set_get_any_result_i32)
                || !exact_profile_source_lineage
            {
                let span = site
                    .map(|site| site.origin.user_span())
                    .or_else(|| {
                        program
                            .node_meta
                            .get(call)
                            .map(|meta| meta.origin.user_span())
                    })
                    .unwrap_or(Span::UNKNOWN);
                errors.push(Diagnostic::error_code(
                    crate::diagnostic::codes::E0200,
                    "Map/Any unpack requires an exact profile-matched GetAny receipt with a selected payload",
                    span,
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn stabilize_expression_types(
    program: &CheckedProgram,
    ephemeral: &EphemeralExpressionTypes,
) -> Result<StableExpressionTypes, Vec<Diagnostic>> {
    let index = ExpressionKeyIndex::build(program, ephemeral.keys());
    let mut errors = Vec::new();
    let mut stable = BTreeMap::new();
    for (owner, expression_types) in ephemeral {
        // Same-owner duplicate keys are a global invariant — report them even
        // when the key never surfaces in this owner's expression types.
        index.report_duplicates(program, &owner.0, &mut errors);

        let mut owner_types = BTreeMap::new();
        for (key, ty) in expression_types {
            let hits = index.owned(&owner.0, key);
            let node_id = match hits.last() {
                Some(node_id) => *node_id,
                None => {
                    // Missing here: consult foreign owners. Exactly one hit is
                    // the callee-owned-default re-check case — skipped.
                    let foreign_count = index.foreign(&owner.0, key).count();
                    if foreign_count == 1 {
                        continue;
                    }
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: checker expression in '{}' has no stable NodeId",
                            owner.0
                        ),
                        program
                            .node_meta
                            .get(owner)
                            .map(|meta| meta.origin.user_span())
                            .unwrap_or(Span::UNKNOWN),
                    ));
                    continue;
                }
            };
            owner_types.insert(node_id.clone(), ty.clone());
        }
        stable.insert(owner.clone(), owner_types);
    }
    if errors.is_empty() {
        Ok(stable)
    } else {
        Err(errors)
    }
}

fn stabilize_session_actions(
    program: &CheckedProgram,
    ephemeral: &BTreeMap<
        NodeId,
        BTreeMap<ExpressionTypeKey, crate::core::checker::flow::CheckedSessionAction>,
    >,
) -> Result<BTreeMap<NodeId, crate::core::ResolvedSessionAction>, Vec<Diagnostic>> {
    let mut stable = BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, actions) in ephemeral {
        let owner_prefix = format!("{}/", owner.0);
        let keys = program
            .node_meta
            .values()
            .filter(|meta| meta.node_id.0.starts_with(&owner_prefix))
            .filter_map(|meta| {
                meta.expression_key
                    .as_ref()
                    .map(|key| (key.clone(), meta.node_id.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        for (key, action) in actions {
            let Some(call) = keys.get(key) else {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: session action in '{}' has no stable call NodeId",
                        owner.0
                    ),
                    program
                        .node_meta
                        .get(owner)
                        .map(|meta| meta.origin.user_span())
                        .unwrap_or(Span::UNKNOWN),
                ));
                continue;
            };
            let before =
                match crate::core::SessionResidualId::new(format_session_type(&action.before)) {
                    Ok(before) => before,
                    Err(error) => {
                        errors.push(Diagnostic::error(
                            format!("TOOL-RESOLUTION-001: {error}"),
                            program.node_meta[call].origin.user_span(),
                        ));
                        continue;
                    }
                };
            let after_text = if action.terminal {
                "closed".to_string()
            } else {
                format_session_type(&action.after)
            };
            let after = match crate::core::SessionResidualId::new(after_text) {
                Ok(after) => after,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!("TOOL-RESOLUTION-001: {error}"),
                        program.node_meta[call].origin.user_span(),
                    ));
                    continue;
                }
            };
            let fact = crate::core::ResolvedSessionAction {
                endpoint: action.endpoint.clone(),
                before,
                after,
                terminal: action.terminal,
            };
            if stable.insert(call.clone(), fact).is_some() {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: call '{}' has duplicate session actions",
                        call.0
                    ),
                    program.node_meta[call].origin.user_span(),
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(stable)
    } else {
        Err(errors)
    }
}

/// Resolve a checker-owned transparent alias chain to a primitive type.
///
/// This is shared by canonical signature construction and route admission.
/// Keeping the lookup here means consumers agree on the same alias identity,
/// ambiguity, and generic-arity rules instead of inspecting declaration ASTs
/// independently.  Only aliases whose complete target chain is primitive are
/// resolved; cyclic, ambiguous, generic, and structural aliases fail closed.
pub fn transparent_alias_primitive(
    name: &str,
    program: &crate::core::CheckedProgram,
) -> Option<crate::core::ir::PrimitiveType> {
    transparent_alias_primitive_inner(name, program, &mut std::collections::BTreeSet::new())
}

fn transparent_alias_primitive_inner(
    name: &str,
    program: &crate::core::CheckedProgram,
    seen: &mut std::collections::BTreeSet<String>,
) -> Option<crate::core::ir::PrimitiveType> {
    if let Some(primitive) = crate::core::ir::PrimitiveType::from_language_name(name) {
        return Some(primitive);
    }
    if !seen.insert(name.to_owned()) {
        return None;
    }
    let definitions = program
        .type_defs()
        .values()
        .filter(|definition| {
            matches!(
                &definition.declaration.kind,
                crate::ast::TypeDefKind::Alias(_)
            ) && (definition.qualified_name == name
                || definition.qualified_name.rsplit("::").next() == Some(name))
        })
        .collect::<Vec<_>>();
    let definition = (definitions.len() == 1).then(|| definitions[0])?;
    // Generic aliases require an instantiated type argument before their
    // target can be interpreted.  Keeping them out of this primitive-only
    // shortcut preserves the checker-owned arity boundary and prevents a
    // malformed/partially-resolved spelling from becoming an FFI scalar.
    if !definition.generic_parameters.is_empty() {
        return None;
    }
    let crate::ast::TypeDefKind::Alias(target) = &definition.declaration.kind else {
        return None;
    };
    match target.unlocated() {
        crate::ast::Type::Name(target, arguments) if arguments.is_empty() => {
            transparent_alias_primitive_inner(target, program, seen)
        }
        _ => None,
    }
}

fn build_canonical_function_signatures(
    program: &CheckedProgram,
    expression_types: &StableExpressionTypes,
    file: &File,
) -> Result<CanonicalFunctionArtifacts, Vec<Diagnostic>> {
    fn register_nominal(
        catalog: &mut BTreeMap<String, std::collections::BTreeSet<String>>,
        qualified_name: &str,
        identity: &NodeId,
    ) {
        let mut keys = vec![qualified_name.to_string()];
        if let Some(short) = qualified_name.rsplit("::").next() {
            keys.push(short.to_string());
        }
        for key in keys {
            catalog.entry(key).or_default().insert(identity.0.clone());
        }
    }

    /// Resolve a name only when the checker-owned catalog contains one exact
    /// type-bearing declaration. Flow containers are excluded below; their
    /// concrete states, rather than the container name, are value types.
    fn resolve_nominal(
        catalog: &BTreeMap<String, std::collections::BTreeSet<String>>,
        name: &str,
    ) -> Option<crate::core::ResolvedTypeName> {
        let candidates = catalog.get(name)?;
        if candidates.len() == 1 {
            let identity = candidates.iter().next()?.clone();
            return crate::core::NominalTypeId::new(identity)
                .ok()
                .map(crate::core::ResolvedTypeName::Nominal);
        }
        None
    }

    fn builtin_nominal(name: &str) -> Option<crate::core::NominalTypeId> {
        const BUILTIN_NOMINALS: &[&str] = &[
            "AST",
            "AtomicBool",
            "AtomicI32",
            "AtomicI64",
            "Channel",
            "ExecResult",
            "Fault",
            "Future",
            "List",
            "Map",
            "MemoryDump",
            "Mutex",
            "MutexGuard",
            "Option",
            "PanicPayload",
            "PeerFault",
            "Range",
            "Record",
            "Result",
            "SessionChan",
            "Set",
            "StatResult",
            "SystemTrace",
            "SystemToken",
            "TokenChannel",
            "Tuple",
            "Type",
            "TypeInfo",
            "session_chan",
            // v0.36.38: `dual X` — the session-protocol expression form used
            // by the hi end of session_pair::<S>() (SessionChan<dual X>). It
            // is an ANNOTATION identity only: the value type of a SessionChan
            // never lowers its type arguments (opaque i64 handle), so "dual"
            // never becomes a runtime type. Registered here so the type
            // table can intern SessionChan<dual X> without demanding a user
            // declaration.
            "dual",
            // v0.34.10a (golden §7.6): quote!/ast_eval exchange AST values.
        ];
        BUILTIN_NOMINALS
            .contains(&name)
            .then(|| crate::core::NominalTypeId::new(format!("builtin:type:{name}")))
            .transpose()
            .ok()
            .flatten()
    }

    let mut nominal_catalog = BTreeMap::new();
    for definition in program.type_defs.values() {
        register_nominal(
            &mut nominal_catalog,
            &definition.qualified_name,
            &definition.node_id,
        );
    }
    for actor in program.actors.values() {
        register_nominal(&mut nominal_catalog, &actor.qualified_name, &actor.node_id);
    }
    for flow in program.flows.values() {
        for state in flow.states.values() {
            register_nominal(
                &mut nominal_catalog,
                &format!("{}::{}", flow.id.0, state.id.name),
                &state.node_id,
            );
            // 0.36.6 (裁决 1 跨 flow 补全): the checker types Fault-sink call
            // results as `flow::<flow>::Fault` (so field access resolves the
            // per-flow StateId/EventId field types). Key the surface-qualified
            // form WITHOUT the short-name alias (register_nominal would add a
            // bare `Fault`/state-name key and make previously-unique short
            // names ambiguous across flows).
            nominal_catalog
                .entry(format!("flow::{}::{}", flow.id.0, state.id.name))
                .or_default()
                .insert(state.node_id.0.clone());
        }
    }
    for session in program.sessions.values() {
        register_nominal(
            &mut nominal_catalog,
            &session.qualified_name,
            &session.node_id,
        );
    }
    for capability in program.capabilities.values() {
        register_nominal(
            &mut nominal_catalog,
            &capability.qualified_name,
            &capability.node_id,
        );
    }
    for trait_def in program.traits.values() {
        register_nominal(
            &mut nominal_catalog,
            &trait_def.qualified_name,
            &trait_def.node_id,
        );
    }

    let mut types = crate::core::ResolvedTypeTable::new();
    let capabilities =
        crate::core::ResolvedTypeCapabilities::with_dynamic_any("type.dynamic_value")
            .map_err(|error| vec![Diagnostic::error(error.to_string(), Span::UNKNOWN)])?;
    // Statement and structural block nodes are always typed as unit in
    // ResolvedBody, even when no callable signature mentions unit directly.
    let unit = ZonkedTy::from_resolved(Type::Name("unit".into(), Vec::new()))
        .expect("unit is a fully resolved primitive type");
    types
        .intern_zonked(
            &unit,
            &capabilities,
            crate::core::ResolvedTypeName::primitive,
        )
        .map_err(|error| vec![Diagnostic::error(error.to_string(), Span::UNKNOWN)])?;
    let ids = NodeIdBuilder::new(program.source_registry());
    let mut signatures = BTreeMap::new();
    let mut node_types = BTreeMap::new();
    let mut type_operands = BTreeMap::new();
    let mut type_arguments = BTreeMap::new();
    let mut field_types = BTreeMap::new();
    let mut state_field_types = BTreeMap::new();
    let mut resolved_variants = BTreeMap::new();
    let mut type_targets = BTreeMap::new();
    let mut errors = Vec::new();

    let mut functions = program.functions.values().collect::<Vec<_>>();
    functions.sort_by(|left, right| left.node_id.cmp(&right.node_id));

    // 0.39.136 perf: one precomputed pass replaces three per-function
    // node_meta × functions double scans (each with a format! allocation per
    // pair) that made actor-heavy files superquadratic (10k actors ≈ 160 s in
    // canon-sig building alone). A meta lands in a function's bucket only when
    // EXACTLY ONE function prefix matches — the original `starts_with` +
    // "no other nested function" semantics.
    let mut uniquely_owned: std::collections::HashMap<&NodeId, Vec<&NodeMeta>> =
        std::collections::HashMap::new();
    {
        // 0.39.136 perf: ownership was resolved by testing EVERY function
        // prefix per binding-carrying meta — O(metas × functions), exploding
        // on actor-dense files. A function prefix `{f}/` matches a node id
        // exactly when it equals one of its `/`-delimited prefixes, so per
        // meta only its own depth-many boundaries need a hash lookup.
        let mut fn_by_prefix: std::collections::HashMap<String, &NodeId> =
            std::collections::HashMap::new();
        for id in program.functions.keys() {
            fn_by_prefix.insert(format!("{}/", id.0), id);
        }
        for (meta_id, meta) in program.node_meta.iter() {
            if meta.shared_binding.is_none()
                && meta.ref_binding.is_none()
                && meta.type_operand.is_none()
            {
                continue;
            }
            let nid = meta_id.0.as_str();
            let mut owner: Option<&NodeId> = None;
            let mut count = 0usize;
            for (boundary, ch) in nid.char_indices() {
                if ch != '/' {
                    continue;
                }
                if let Some(&fid) = fn_by_prefix.get(&nid[..boundary + 1]) {
                    count += 1;
                    if count > 1 {
                        break;
                    }
                    owner = Some(fid);
                }
            }
            if count == 1 {
                uniquely_owned
                    .entry(owner.expect("count == 1 implies an owner"))
                    .or_default()
                    .push(meta);
            }
        }
    }
    for function in functions {
        let Some((parameter_types, result_type)) =
            program.zonked_function_types.get(&function.node_id)
        else {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: function '{}' has no checker-finalized signature",
                    function.qualified_name
                ),
                function.origin.user_span(),
            ));
            continue;
        };
        if parameter_types.len() != function.param_decls.len() {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: function '{}' canonical parameter count mismatch",
                    function.qualified_name
                ),
                function.origin.user_span(),
            ));
            continue;
        }

        let mut generic_names = BTreeMap::new();
        let mut generic_parameters = Vec::new();
        let mut generic_intern_failed = false;
        for (name, id) in &function.generic_binders {
            if let Err(error) =
                types.intern_resolved(crate::core::ResolvedType::GenericParameter(id.clone()))
            {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: generic parameter '{}' has no canonical identity: {}",
                        id.0, error
                    ),
                    function.origin.user_span(),
                ));
                generic_intern_failed = true;
            }
            if !program.node_meta.contains_key(id) {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: generic parameter '{}' is absent from NodeMeta",
                        id.0
                    ),
                    function.origin.user_span(),
                ));
            }
            generic_names.insert(name.clone(), id.clone());
            generic_parameters.push(id.clone());
        }

        let module = function
            .qualified_name
            .rsplit_once("::")
            .map(|(module, _)| module);
        let mut resolve_name = |name: &str| {
            if let Some(primitive) = crate::core::ResolvedTypeName::primitive(name) {
                return Some(primitive);
            }
            if let Some(parameter) = generic_names.get(name) {
                return Some(crate::core::ResolvedTypeName::GenericParameter(
                    parameter.clone(),
                ));
            }
            if let Some(module) = module {
                let qualified = format!("{module}::{name}");
                if let Some(candidates) = nominal_catalog.get(&qualified) {
                    if candidates.len() == 1 {
                        let identity = candidates.iter().next()?.clone();
                        return crate::core::NominalTypeId::new(identity)
                            .ok()
                            .map(crate::core::ResolvedTypeName::Nominal);
                    }
                }
            }
            if let Some(primitive) = transparent_alias_primitive_inner(
                name,
                program,
                &mut std::collections::BTreeSet::new(),
            ) {
                return Some(crate::core::ResolvedTypeName::Primitive(primitive));
            }
            if let Some(resolved) = resolve_nominal(&nominal_catalog, name) {
                return Some(resolved);
            }
            builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal)
        };

        let mut canonical_parameter_types = Vec::with_capacity(parameter_types.len());
        let mut signature_failed = generic_intern_failed;
        for ty in parameter_types {
            match types.intern_zonked(ty, &capabilities, &mut resolve_name) {
                Ok(ty) => canonical_parameter_types.push(ty),
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: function '{}' parameter type is not canonical: {}",
                            function.qualified_name, error
                        ),
                        function.origin.user_span(),
                    ));
                    signature_failed = true;
                }
            }
        }
        let canonical_result =
            match types.intern_zonked(result_type, &capabilities, &mut resolve_name) {
                Ok(ty) => ty,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: function '{}' result type is not canonical: {}",
                            function.qualified_name, error
                        ),
                        function.origin.user_span(),
                    ));
                    continue;
                }
            };
        if signature_failed {
            continue;
        }

        if let Some(expressions) = expression_types.get(&function.node_id) {
            for (node_id, ty) in expressions {
                match types.intern_zonked(ty, &capabilities, &mut resolve_name) {
                    Ok(ty) => {
                        node_types.insert(node_id.clone(), ty);
                    }
                    Err(error) => errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: expression '{}' type is not canonical: {}",
                            node_id.0, error
                        ),
                        program
                            .node_meta
                            .get(node_id)
                            .map(|meta| meta.origin.user_span())
                            .unwrap_or_else(|| function.origin.user_span()),
                    )),
                }
                if let Some(type_operand) = program
                    .node_meta
                    .get(node_id)
                    .and_then(|meta| meta.type_operand.as_ref())
                {
                    match ZonkedTy::from_resolved(type_operand.clone()) {
                        Ok(operand) => match types.intern_zonked(
                            &operand,
                            &capabilities,
                            &mut resolve_name,
                        ) {
                            Ok(operand) => {
                                type_operands.insert(node_id.clone(), operand);
                            }
                            Err(error) => errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: explicit type operand '{}' is not canonical: {error}",
                                    node_id.0
                                ),
                                program.node_meta[node_id].origin.user_span(),
                            )),
                        },
                        Err(error) => errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: explicit type operand '{}' is not zonked: {error}",
                                node_id.0
                            ),
                            program.node_meta[node_id].origin.user_span(),
                        )),
                    }
                }
                if let Some(arguments) = program
                    .node_meta
                    .get(node_id)
                    .map(|meta| meta.type_arguments.as_slice())
                    .filter(|arguments| !arguments.is_empty())
                {
                    let mut canonical = Vec::with_capacity(arguments.len());
                    let mut failed = false;
                    for argument in arguments {
                        let zonked = match ZonkedTy::from_resolved(argument.clone()) {
                            Ok(argument) => argument,
                            Err(error) => {
                                errors.push(Diagnostic::error(
                                    format!(
                                        "TOOL-RESOLUTION-001: generic argument at '{}' is not zonked: {error}",
                                        node_id.0
                                    ),
                                    program.node_meta[node_id].origin.user_span(),
                                ));
                                failed = true;
                                continue;
                            }
                        };
                        match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                            Ok(argument) => canonical.push(argument),
                            Err(error) => {
                                errors.push(Diagnostic::error(
                                    format!(
                                        "TOOL-RESOLUTION-001: generic argument at '{}' is not canonical: {error}",
                                        node_id.0
                                    ),
                                    program.node_meta[node_id].origin.user_span(),
                                ));
                                failed = true;
                            }
                        }
                    }
                    if !failed {
                        type_arguments.insert(node_id.clone(), canonical);
                    }
                }
            }

            let expression_types_by_key = expressions
                .iter()
                .filter_map(|(node_id, ty)| {
                    program
                        .node_meta
                        .get(node_id)
                        .and_then(|meta| meta.expression_key.clone())
                        .map(|key| (key, ty))
                })
                .collect::<BTreeMap<_, _>>();
            let shared_bindings: Vec<(NodeId, crate::ast::SharedKind, ExpressionTypeKey)> =
                uniquely_owned
                    .get(&function.node_id)
                    .map(|metas: &Vec<&NodeMeta>| {
                        metas
                            .iter()
                            .filter_map(|meta| {
                                meta.shared_binding
                                    .as_ref()
                                    .map(|(kind, key)| (meta.node_id.clone(), *kind, key.clone()))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
            for (node_id, kind, initializer_key) in shared_bindings {
                let Some(initializer) = expression_types_by_key.get(&initializer_key) else {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: shared binding '{}' has no checker-finalized initializer type",
                            node_id.0
                        ),
                        program.node_meta[&node_id].origin.user_span(),
                    ));
                    continue;
                };
                let binding = match canonical_shared_binding_type(kind, initializer) {
                    Ok(binding) => binding,
                    Err(message) => {
                        errors.push(Diagnostic::error(
                            format!("TOOL-RESOLUTION-001: {message}"),
                            program.node_meta[&node_id].origin.user_span(),
                        ));
                        continue;
                    }
                };
                match types.intern_zonked(&binding, &capabilities, &mut resolve_name) {
                    Ok(binding) => {
                        node_types.insert(node_id, binding);
                    }
                    Err(error) => errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: shared binding '{}' type is not canonical: {error}",
                            node_id.0
                        ),
                        program.node_meta[&node_id].origin.user_span(),
                    )),
                }
            }

            let ref_bindings = uniquely_owned
                .get(&function.node_id)
                .map(|metas| {
                    metas
                        .iter()
                        .filter_map(|meta| {
                            meta.ref_binding
                                .as_ref()
                                .map(|key| (meta.node_id.clone(), key.clone()))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for (node_id, initializer_key) in ref_bindings {
                let Some(initializer) = expression_types_by_key.get(&initializer_key) else {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: reference binding '{}' has no checker-finalized initializer type",
                            node_id.0
                        ),
                        program.node_meta[&node_id].origin.user_span(),
                    ));
                    continue;
                };
                let binding = match canonical_reference_binding_type(initializer) {
                    Ok(binding) => binding,
                    Err(message) => {
                        errors.push(Diagnostic::error(
                            format!("TOOL-RESOLUTION-001: {message}"),
                            program.node_meta[&node_id].origin.user_span(),
                        ));
                        continue;
                    }
                };
                match types.intern_zonked(&binding, &capabilities, &mut resolve_name) {
                    Ok(binding) => {
                        node_types.insert(node_id, binding);
                    }
                    Err(error) => errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: reference binding '{}' type is not canonical: {error}",
                            node_id.0
                        ),
                        program.node_meta[&node_id].origin.user_span(),
                    )),
                }
            }
        }

        // Persist every explicit type annotation owned by the callable, not
        // only type operands attached to expressions. ResolvedBody uses this
        // table to type local bindings and conversions without consulting raw
        // `ast::Type` after construction.
        let annotated_types = uniquely_owned
            .get(&function.node_id)
            .map(|metas| {
                metas
                    .iter()
                    .filter_map(|meta| {
                        meta.type_operand
                            .as_ref()
                            .map(|annotation| (meta.node_id.clone(), annotation.clone()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (node_id, annotation) in annotated_types {
            if matches!(
                annotation.unlocated(),
                Type::Infer | Type::TypeVar(_) | Type::ForAll(_, _) | Type::TyErr
            ) {
                continue;
            }
            let zonked = match ZonkedTy::from_resolved(annotation) {
                Ok(annotation) => annotation,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: callable annotation '{}' is not zonked: {error}",
                            node_id.0
                        ),
                        program.node_meta[&node_id].origin.user_span(),
                    ));
                    continue;
                }
            };
            match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                Ok(annotation) => {
                    type_operands.insert(node_id, annotation);
                }
                Err(error) => errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: callable annotation '{}' is not canonical: {error}",
                        node_id.0
                    ),
                    program.node_meta[&node_id].origin.user_span(),
                )),
            }
        }

        let parameters = function
            .param_decls
            .iter()
            .zip(canonical_parameter_types)
            .map(|(parameter, ty)| {
                let id = ids.anonymous(
                    &function.node_id,
                    "decl.parameter",
                    &format!("parameter.{}", stable_id_fragment(&parameter.name)),
                    usable_span(parameter.meta.span),
                    parameter.meta.origin,
                    &mut errors,
                );
                if !program.node_meta.contains_key(&id) {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: parameter '{}' is absent from NodeMeta",
                            id.0
                        ),
                        function.origin.user_span(),
                    ));
                }
                crate::core::ResolvedParameter {
                    id: crate::core::ResolvedParameterId(id),
                    name: parameter.name.clone(),
                    ty,
                    mutable: parameter.mut_,
                    permission: parameter.borrow.map(|permission| match permission {
                        crate::ast::ParamBorrow::View => crate::core::Permission::View,
                        crate::ast::ParamBorrow::Mutate => crate::core::Permission::Mutate,
                    }),
                    has_default: parameter.default_value.is_some(),
                }
            })
            .collect();
        let mut effects = function.effects.clone();
        effects.sort();
        effects.dedup();
        let effects = effects
            .into_iter()
            .filter_map(|effect| match crate::core::EffectId::new(effect) {
                Ok(effect) => Some(effect),
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!("TOOL-RESOLUTION-001: {error}"),
                        function.origin.user_span(),
                    ));
                    None
                }
            })
            .collect();
        let signature = crate::core::ResolvedSignature {
            owner: function.node_id.clone(),
            generic_parameters,
            parameters,
            result: canonical_result,
            effects,
        };
        if let Err(signature_errors) = signature.validate(&types) {
            errors.extend(signature_errors.into_iter().map(|error| {
                Diagnostic::error(
                    format!("TOOL-RESOLUTION-001: {error}"),
                    function.origin.user_span(),
                )
            }));
        } else {
            signatures.insert(function.node_id.clone(), signature);
        }
    }

    // Trait methods are declarations without executable bodies, but their
    // source-level ABI is still checker-owned data. Keep them outside the
    // executable signature directory so body/CFG consumers cannot mistake a
    // vtable contract for a callable. Intern the complete signature here,
    // while the canonical type table and checked name resolver are available.
    let mut trait_signatures = BTreeMap::new();
    for item in &file.items {
        let Item::Trait(trait_def) = item else {
            continue;
        };
        let Some(resolved_trait) = program.trait_def(&trait_def.name) else {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: trait '{}' has no checked declaration",
                    trait_def.name
                ),
                trait_def.meta.span,
            ));
            continue;
        };
        if resolved_trait.methods.len() != trait_def.methods.len()
            || resolved_trait.method_owners.len() != trait_def.methods.len()
        {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: trait '{}' method identity coverage differs",
                    trait_def.name
                ),
                trait_def.meta.span,
            ));
            continue;
        }
        let trait_owner = resolved_trait.node_id.clone();
        let trait_binders =
            callable_generic_binders(&trait_def.generics, &trait_owner, &ids, &mut errors);
        for (method_index, method) in trait_def.methods.iter().enumerate() {
            let method_owner = resolved_trait.method_owners[method_index].clone();
            let mut generic_binders = trait_binders.clone();
            generic_binders.extend(callable_generic_binders(
                &method.generics,
                &method_owner,
                &ids,
                &mut errors,
            ));
            let mut generic_names = BTreeMap::new();
            let mut generic_parameters = Vec::with_capacity(generic_binders.len());
            for (name, binder) in generic_binders {
                if !program.node_meta.contains_key(&binder) {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: trait generic parameter '{}' is absent from NodeMeta",
                            binder.0
                        ),
                        method.meta.span,
                    ));
                }
                generic_names.insert(name, binder.clone());
                generic_parameters.push(binder);
            }
            let module = trait_def.name.rsplit_once("::").map(|(module, _)| module);
            let mut resolve_name = |name: &str| {
                if let Some(primitive) = crate::core::ResolvedTypeName::primitive(name) {
                    return Some(primitive);
                }
                if let Some(parameter) = generic_names.get(name) {
                    return Some(crate::core::ResolvedTypeName::GenericParameter(
                        parameter.clone(),
                    ));
                }
                if let Some(module) = module {
                    let qualified = format!("{module}::{name}");
                    if let Some(candidates) = nominal_catalog.get(&qualified) {
                        if candidates.len() == 1 {
                            let identity = candidates.iter().next()?.clone();
                            return crate::core::NominalTypeId::new(identity)
                                .ok()
                                .map(crate::core::ResolvedTypeName::Nominal);
                        }
                    }
                }
                if let Some(primitive) = transparent_alias_primitive_inner(
                    name,
                    program,
                    &mut std::collections::BTreeSet::new(),
                ) {
                    return Some(crate::core::ResolvedTypeName::Primitive(primitive));
                }
                if let Some(resolved) = resolve_nominal(&nominal_catalog, name) {
                    return Some(resolved);
                }
                builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal)
            };
            let mut parameters = Vec::with_capacity(method.params.len());
            let mut failed = false;
            for parameter in &method.params {
                let parameter_id = ids.anonymous(
                    &method_owner,
                    "decl.parameter",
                    &format!("parameter.{}", stable_id_fragment(&parameter.name)),
                    usable_span(parameter.meta.span),
                    parameter.meta.origin,
                    &mut errors,
                );
                if !program.node_meta.contains_key(&parameter_id) {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: trait method parameter '{}' is absent from NodeMeta",
                            parameter_id.0
                        ),
                        parameter.meta.span,
                    ));
                }
                let zonked = match ZonkedTy::from_resolved(parameter.ty.clone()) {
                    Ok(zonked) => zonked,
                    Err(error) => {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: trait method '{}' parameter '{}' is not zonked: {error}",
                                method.name, parameter.name
                            ),
                            parameter.meta.span,
                        ));
                        failed = true;
                        continue;
                    }
                };
                let ty = match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                    Ok(ty) => ty,
                    Err(error) => {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: trait method '{}' parameter '{}' is not canonical: {error}",
                                method.name, parameter.name
                            ),
                            parameter.meta.span,
                        ));
                        failed = true;
                        continue;
                    }
                };
                parameters.push(crate::core::ResolvedParameter {
                    id: crate::core::ResolvedParameterId(parameter_id),
                    name: parameter.name.clone(),
                    ty,
                    mutable: parameter.mut_,
                    permission: parameter.borrow.map(|permission| match permission {
                        crate::ast::ParamBorrow::View => crate::core::Permission::View,
                        crate::ast::ParamBorrow::Mutate => crate::core::Permission::Mutate,
                    }),
                    has_default: parameter.default_value.is_some(),
                });
            }
            let result_type = method
                .ret
                .clone()
                .unwrap_or_else(|| Type::Name("unit".into(), Vec::new()));
            let result = match ZonkedTy::from_resolved(result_type) {
                Ok(zonked) => {
                    match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                        Ok(result) => Some(result),
                        Err(error) => {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: trait method '{}' result is not canonical: {error}",
                                    method.name
                                ),
                                method.meta.span,
                            ));
                            None
                        }
                    }
                }
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: trait method '{}' result is not zonked: {error}",
                            method.name
                        ),
                        method.meta.span,
                    ));
                    None
                }
            };
            if failed {
                continue;
            }
            let Some(result) = result else {
                continue;
            };
            let signature = crate::core::ResolvedSignature {
                owner: method_owner.clone(),
                generic_parameters,
                parameters,
                result,
                effects: Vec::new(),
            };
            if let Err(signature_errors) = signature.validate(&types) {
                errors.extend(signature_errors.into_iter().map(|error| {
                    Diagnostic::error(format!("TOOL-RESOLUTION-001: {error}"), method.meta.span)
                }));
            } else if trait_signatures.insert(method_owner, signature).is_some() {
                errors.push(Diagnostic::error(
                    "TOOL-RESOLUTION-001: duplicate canonical trait method owner",
                    method.meta.span,
                ));
            }
        }
    }

    // Extern declarations are callable signatures without a ResolvedBody, so
    // their ABI-only parameter types may never occur in expression_types (for
    // example an `f64` declaration called with an i32 widening argument).  The
    // canonical FFI receipt still needs those declaration TypeDescs.  Intern
    // every extern parameter/result here while the resolved name catalog is
    // available; otherwise MIR materialization would fail with a misleading
    // "primitive type is absent" error before any backend can apply the
    // checker-approved conversion receipt.
    let mut resolve_extern_name = |name: &str| {
        if let Some(primitive) = crate::core::ResolvedTypeName::primitive(name) {
            return Some(primitive);
        }
        if let Some(primitive) =
            transparent_alias_primitive_inner(name, program, &mut std::collections::BTreeSet::new())
        {
            return Some(crate::core::ResolvedTypeName::Primitive(primitive));
        }
        resolve_nominal(&nominal_catalog, name)
            .or_else(|| builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal))
    };
    let mut extern_blocks = program.extern_blocks.values().collect::<Vec<_>>();
    extern_blocks.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for block in extern_blocks {
        for signature in CheckedProgram::extern_signatures_sorted(block) {
            let mut declaration_types = signature
                .typed_params
                .iter()
                .map(|(_, ty, _)| ("parameter", ty.clone()))
                .collect::<Vec<_>>();
            declaration_types.push((
                "result",
                signature
                    .ret_type
                    .clone()
                    .unwrap_or_else(|| Type::Name("unit".into(), Vec::new())),
            ));
            for (role, ty) in declaration_types {
                let zonked = match ZonkedTy::from_resolved(ty) {
                    Ok(ty) => ty,
                    Err(error) => {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: extern '{}' {} type is not zonked: {error}",
                                signature.name, role
                            ),
                            signature.span,
                        ));
                        continue;
                    }
                };
                if let Err(error) =
                    types.intern_zonked(&zonked, &capabilities, &mut resolve_extern_name)
                {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: extern '{}' {} type is not canonical: {error}",
                            signature.name, role
                        ),
                        signature.span,
                    ));
                }
            }
        }
    }

    // Declaration expressions are typed roots even when no executable
    // callable exists (extern predicates, constant/actor initializers).
    // Optional capture failures leave their roots Deferred; never replace them
    // with guessed i64 types or turn an old legal declaration into a rejection.
    for (owner, expressions) in expression_types {
        if program.functions.contains_key(owner)
            || program
                .transitions
                .values()
                .any(|transition| &transition.node_id == owner)
        {
            continue;
        }
        for (node, ty) in expressions {
            if let Ok(canonical) = types.intern_zonked(ty, &capabilities, &mut resolve_extern_name)
            {
                node_types.insert(node.clone(), canonical);
            }
        }
    }

    let mut transitions = program.transitions.values().collect::<Vec<_>>();
    transitions.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for transition in transitions {
        let Some(flow_declaration) = program.flows.get(&transition.id.flow) else {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: transition '{}' has no owning Flow declaration",
                    transition.node_id.0
                ),
                transition.span,
            ));
            continue;
        };
        let flow_generic_names = flow_declaration
            .generic_parameters
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();
        let flow_generic_binders = flow_declaration
            .generic_parameters
            .iter()
            .map(|(_, binder)| binder.clone())
            .collect::<Vec<_>>();
        if transition.params.len() != transition.parameter_ids.len() {
            errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: transition '{}' parameter identity count mismatch",
                    transition.node_id.0
                ),
                transition.span,
            ));
            continue;
        }
        for parameter in
            std::iter::once(&transition.source_parameter_id).chain(transition.parameter_ids.iter())
        {
            if !program.node_meta.contains_key(parameter) {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: transition parameter '{}' is absent from NodeMeta",
                        parameter.0
                    ),
                    transition.span,
                ));
            }
        }
        let module = transition
            .id
            .flow
            .0
            .rsplit_once("::")
            .map(|(module, _)| module);
        let mut resolve_name = |name: &str| {
            if let Some(primitive) = crate::core::ResolvedTypeName::primitive(name) {
                return Some(primitive);
            }
            if let Some(parameter) = flow_generic_names.get(name) {
                return Some(crate::core::ResolvedTypeName::GenericParameter(
                    parameter.clone(),
                ));
            }
            let flow_qualified = format!("{}::{name}", transition.id.flow.0);
            if let Some(candidates) = nominal_catalog.get(&flow_qualified) {
                if candidates.len() == 1 {
                    return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                        .ok()
                        .map(crate::core::ResolvedTypeName::Nominal);
                }
            }
            if let Some(module) = module {
                let qualified = format!("{module}::{name}");
                if let Some(candidates) = nominal_catalog.get(&qualified) {
                    if candidates.len() == 1 {
                        return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                            .ok()
                            .map(crate::core::ResolvedTypeName::Nominal);
                    }
                }
            }
            if let Some(primitive) = transparent_alias_primitive_inner(
                name,
                program,
                &mut std::collections::BTreeSet::new(),
            ) {
                return Some(crate::core::ResolvedTypeName::Primitive(primitive));
            }
            if let Some(resolved) = resolve_nominal(&nominal_catalog, name) {
                return Some(resolved);
            }
            builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal)
        };
        let source_name = format!("{}::{}", transition.id.flow.0, transition.id.source.name);
        let source_zonked = match ZonkedTy::from_resolved(Type::Name(source_name, Vec::new())) {
            Ok(ty) => ty,
            Err(error) => {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: transition '{}' source type is not zonked: {error}",
                        transition.node_id.0
                    ),
                    transition.span,
                ));
                continue;
            }
        };
        let source = match types.intern_zonked(&source_zonked, &capabilities, &mut resolve_name) {
            Ok(ty) => ty,
            Err(error) => {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: transition '{}' source type is not canonical: {error}",
                        transition.node_id.0
                    ),
                    transition.span,
                ));
                continue;
            }
        };
        let mut parameters = vec![crate::core::ResolvedParameter {
            id: crate::core::ResolvedParameterId(transition.source_parameter_id.clone()),
            name: "self".into(),
            ty: source,
            mutable: false,
            permission: Some(crate::core::Permission::Consume),
            has_default: false,
        }];
        let mut failed = false;
        for ((name, ty), parameter_id) in transition.params.iter().zip(&transition.parameter_ids) {
            let zonked = match ZonkedTy::from_resolved(ty.clone()) {
                Ok(ty) => ty,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' parameter '{}' is not zonked: {error}",
                            transition.node_id.0, name
                        ),
                        transition.span,
                    ));
                    failed = true;
                    continue;
                }
            };
            match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                Ok(ty) => parameters.push(crate::core::ResolvedParameter {
                    id: crate::core::ResolvedParameterId(parameter_id.clone()),
                    name: name.clone(),
                    ty,
                    mutable: false,
                    permission: None,
                    has_default: false,
                }),
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' parameter '{}' is not canonical: {error}",
                            transition.node_id.0, name
                        ),
                        transition.span,
                    ));
                    failed = true;
                }
            }
        }
        if failed {
            continue;
        }
        let result = if transition.targets.len() > 1 {
            let flow = match crate::core::NominalTypeId::new(format!(
                "flow:{}",
                transition.id.flow.0
            )) {
                Ok(flow) => flow,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' Flow identity is invalid: {error}",
                            transition.node_id.0
                        ),
                        transition.span,
                    ));
                    continue;
                }
            };
            let states = transition
                .targets
                .iter()
                .map(|target| {
                    crate::core::NominalTypeId::new(format!(
                        "state:{}::{}",
                        target.flow.0, target.name
                    ))
                })
                .collect::<Result<Vec<_>, _>>();
            match states.and_then(|states| types.intern_flow_state_set(flow, states)) {
                Ok(result) => result,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' target set is not canonical: {error}",
                            transition.node_id.0
                        ),
                        transition.span,
                    ));
                    continue;
                }
            }
        } else {
            let result_type = transition
                .targets
                .first()
                .map(|target| Type::Name(format!("{}::{}", target.flow.0, target.name), Vec::new()))
                .unwrap_or_else(|| Type::Name("unit".into(), Vec::new()));
            match ZonkedTy::from_resolved(result_type) {
                Ok(ty) => match types.intern_zonked(&ty, &capabilities, &mut resolve_name) {
                    Ok(ty) => ty,
                    Err(error) => {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: transition '{}' result type is not canonical: {error}",
                                transition.node_id.0
                            ),
                            transition.span,
                        ));
                        continue;
                    }
                },
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' result type is not zonked: {error}",
                            transition.node_id.0
                        ),
                        transition.span,
                    ));
                    continue;
                }
            }
        };
        let result = if let Some(fails_ty) = &transition.fails {
            let source_type = Type::Name(
                format!("{}::{}", transition.id.flow.0, transition.id.source.name),
                Vec::new(),
            );
            let source_zonked = match ZonkedTy::from_resolved(source_type) {
                Ok(ty) => ty,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' fails source type is not zonked: {error}",
                            transition.node_id.0
                        ),
                        transition.span,
                    ));
                    continue;
                }
            };
            let source_id = match types.intern_zonked(
                &source_zonked,
                &capabilities,
                &mut resolve_name,
            ) {
                Ok(ty) => ty,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' fails source type is not canonical: {error}",
                            transition.node_id.0
                        ),
                        transition.span,
                    ));
                    continue;
                }
            };
            let fails_zonked = match ZonkedTy::from_resolved(fails_ty.clone()) {
                Ok(ty) => ty,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' fails error type is not zonked: {error}",
                            transition.node_id.0
                        ),
                        transition.span,
                    ));
                    continue;
                }
            };
            let fails_id = match types.intern_zonked(
                &fails_zonked,
                &capabilities,
                &mut resolve_name,
            ) {
                Ok(ty) => ty,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' fails error type is not canonical: {error}",
                            transition.node_id.0
                        ),
                        transition.span,
                    ));
                    continue;
                }
            };
            let error_tuple = match types.intern_resolved(crate::core::ir::ResolvedType::Tuple(
                vec![source_id, fails_id],
            )) {
                Ok(ty) => ty,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' fails tuple type is not canonical: {error}",
                            transition.node_id.0
                        ),
                        transition.span,
                    ));
                    continue;
                }
            };
            match types.intern_resolved(crate::core::ir::ResolvedType::Result {
                ok: result,
                error: error_tuple,
            }) {
                Ok(ty) => ty,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition '{}' fails Result type is not canonical: {error}",
                            transition.node_id.0
                        ),
                        transition.span,
                    ));
                    continue;
                }
            }
        } else {
            result
        };
        if let Some(expressions) = expression_types.get(&transition.node_id) {
            for (node_id, ty) in expressions {
                match types.intern_zonked(ty, &capabilities, &mut resolve_name) {
                    Ok(ty) => {
                        node_types.insert(node_id.clone(), ty);
                    }
                    Err(error) => errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition expression '{}' type is not canonical: {error}",
                            node_id.0
                        ),
                        program
                            .node_meta
                            .get(node_id)
                            .map(|meta| meta.origin.user_span())
                            .unwrap_or(transition.span),
                    )),
                }
                if let Some(type_operand) = program
                    .node_meta
                    .get(node_id)
                    .and_then(|meta| meta.type_operand.as_ref())
                {
                    match ZonkedTy::from_resolved(type_operand.clone()) {
                        Ok(operand) => match types.intern_zonked(
                            &operand,
                            &capabilities,
                            &mut resolve_name,
                        ) {
                            Ok(operand) => {
                                type_operands.insert(node_id.clone(), operand);
                            }
                            Err(error) => errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: transition type operand '{}' is not canonical: {error}",
                                    node_id.0
                                ),
                                program.node_meta[node_id].origin.user_span(),
                            )),
                        },
                        Err(error) => errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: transition type operand '{}' is not zonked: {error}",
                                node_id.0
                            ),
                            program.node_meta[node_id].origin.user_span(),
                        )),
                    }
                }
                if let Some(arguments) = program
                    .node_meta
                    .get(node_id)
                    .map(|meta| meta.type_arguments.as_slice())
                    .filter(|arguments| !arguments.is_empty())
                {
                    let mut canonical = Vec::with_capacity(arguments.len());
                    let mut failed = false;
                    for argument in arguments {
                        let zonked = match ZonkedTy::from_resolved(argument.clone()) {
                            Ok(argument) => argument,
                            Err(error) => {
                                errors.push(Diagnostic::error(
                                    format!(
                                        "TOOL-RESOLUTION-001: transition generic argument at '{}' is not zonked: {error}",
                                        node_id.0
                                    ),
                                    program.node_meta[node_id].origin.user_span(),
                                ));
                                failed = true;
                                continue;
                            }
                        };
                        match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                            Ok(argument) => canonical.push(argument),
                            Err(error) => {
                                errors.push(Diagnostic::error(
                                    format!(
                                        "TOOL-RESOLUTION-001: transition generic argument at '{}' is not canonical: {error}",
                                        node_id.0
                                    ),
                                    program.node_meta[node_id].origin.user_span(),
                                ));
                                failed = true;
                            }
                        }
                    }
                    if !failed {
                        type_arguments.insert(node_id.clone(), canonical);
                    }
                }
            }
        }
        // Persist every explicit type annotation owned by the transition,
        // not only type operands attached to expressions. ResolvedBody uses
        // this table to type local bindings and conversions without
        // consulting raw `ast::Type` after construction. Without this,
        // `let xs: List<string> = []` inside a Flow transition body fails
        // with TOOL-RESOLUTION-001 ("explicit annotation has no
        // checker-canonical type").
        let owner_prefix = format!("{}/", transition.node_id.0);
        let annotated_types = program
            .node_meta
            .iter()
            .filter_map(|(node_id, meta)| {
                meta.type_operand
                    .as_ref()
                    .filter(|_| node_id.0.starts_with(&owner_prefix))
                    .map(|annotation| (node_id.clone(), annotation.clone()))
            })
            .collect::<Vec<_>>();
        for (node_id, annotation) in annotated_types {
            if matches!(
                annotation.unlocated(),
                Type::Infer | Type::TypeVar(_) | Type::ForAll(_, _) | Type::TyErr
            ) {
                continue;
            }
            let zonked = match ZonkedTy::from_resolved(annotation) {
                Ok(annotation) => annotation,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: transition annotation '{}' is not zonked: {error}",
                            node_id.0
                        ),
                        program.node_meta[&node_id].origin.user_span(),
                    ));
                    continue;
                }
            };
            match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                Ok(annotation) => {
                    type_operands.insert(node_id.clone(), annotation);
                }
                Err(error) => errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: transition annotation '{}' is not canonical: {error}",
                        node_id.0
                    ),
                    program.node_meta[&node_id].origin.user_span(),
                )),
            }
        }
        let signature = crate::core::ResolvedSignature {
            owner: transition.node_id.clone(),
            generic_parameters: flow_generic_binders,
            parameters,
            result,
            effects: Vec::new(),
        };
        if let Err(signature_errors) = signature.validate(&types) {
            errors.extend(signature_errors.into_iter().map(|error| {
                Diagnostic::error(format!("TOOL-RESOLUTION-001: {error}"), transition.span)
            }));
        } else {
            signatures.insert(transition.node_id.clone(), signature);
        }
    }

    let mut definitions = program.type_defs.values().collect::<Vec<_>>();
    definitions.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for definition in definitions {
        let generic_names = definition
            .generic_parameters
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();
        let module = definition
            .qualified_name
            .rsplit_once("::")
            .map(|(module, _)| module);
        let mut resolve_name = |name: &str| {
            if let Some(primitive) = crate::core::ResolvedTypeName::primitive(name) {
                return Some(primitive);
            }
            if let Some(parameter) = generic_names.get(name) {
                return Some(crate::core::ResolvedTypeName::GenericParameter(
                    parameter.clone(),
                ));
            }
            if let Some(module) = module {
                let qualified = format!("{module}::{name}");
                if let Some(candidates) = nominal_catalog.get(&qualified) {
                    if candidates.len() == 1 {
                        return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                            .ok()
                            .map(crate::core::ResolvedTypeName::Nominal);
                    }
                }
            }
            if let Some(primitive) = transparent_alias_primitive_inner(
                name,
                program,
                &mut std::collections::BTreeSet::new(),
            ) {
                return Some(crate::core::ResolvedTypeName::Primitive(primitive));
            }
            if let Some(resolved) = resolve_nominal(&nominal_catalog, name) {
                return Some(resolved);
            }
            builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal)
        };
        match &definition.declaration.kind {
            crate::ast::TypeDefKind::Record(fields) | crate::ast::TypeDefKind::Union(fields) => {
                for field in fields {
                    let field_id = ids.anonymous(
                        &definition.node_id,
                        "decl.field",
                        &format!("field.{}", stable_id_fragment(&field.name)),
                        usable_span(field.meta.span),
                        field.meta.origin,
                        &mut errors,
                    );
                    canonicalize_declaration_member(
                        field_id,
                        &field.name,
                        &field.ty,
                        definition,
                        &mut DeclarationMemberContext {
                            types: &mut types,
                            capabilities: &capabilities,
                            resolve_name: &mut resolve_name,
                            field_types: &mut field_types,
                            errors: &mut errors,
                        },
                    );
                }
            }
            crate::ast::TypeDefKind::Enum(variants) => {
                for variant in variants {
                    let Some(variant_id) = definition.variant_ids.get(&variant.name).cloned()
                    else {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: variant '{}::{}' has no stable identity",
                                definition.qualified_name, variant.name
                            ),
                            definition.origin.user_span(),
                        ));
                        continue;
                    };
                    let mut members = Vec::new();
                    let shape = match &variant.payload {
                        Some(crate::ast::VariantPayload::Tuple(payload)) => {
                            let role_batch = type_sibling_roles("payload.element", payload);
                            for index in 0..payload.len() {
                                let role = &role_batch[index];
                                let meta = payload[index].meta();
                                let member_id = ids.anonymous(
                                    &variant_id,
                                    type_kind(&payload[index]),
                                    role,
                                    meta.and_then(|meta| usable_span(meta.span)),
                                    meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User),
                                    &mut errors,
                                );
                                canonicalize_declaration_member(
                                    member_id.clone(),
                                    &format!("{}[{index}]", variant.name),
                                    &payload[index],
                                    definition,
                                    &mut DeclarationMemberContext {
                                        types: &mut types,
                                        capabilities: &capabilities,
                                        resolve_name: &mut resolve_name,
                                        field_types: &mut field_types,
                                        errors: &mut errors,
                                    },
                                );
                                if let Some(ty) = field_types.get(&member_id).cloned() {
                                    members.push(ResolvedVariantMember {
                                        node_id: member_id,
                                        name: format!("_{index}"),
                                        ty,
                                    });
                                }
                            }
                            ResolvedVariantShape::Tuple
                        }
                        Some(crate::ast::VariantPayload::Record(fields)) => {
                            for field in fields {
                                let member_id = ids.anonymous(
                                    &variant_id,
                                    "decl.field",
                                    &format!("payload.field.{}", stable_id_fragment(&field.name)),
                                    usable_span(field.meta.span),
                                    field.meta.origin,
                                    &mut errors,
                                );
                                canonicalize_declaration_member(
                                    member_id.clone(),
                                    &field.name,
                                    &field.ty,
                                    definition,
                                    &mut DeclarationMemberContext {
                                        types: &mut types,
                                        capabilities: &capabilities,
                                        resolve_name: &mut resolve_name,
                                        field_types: &mut field_types,
                                        errors: &mut errors,
                                    },
                                );
                                if let Some(ty) = field_types.get(&member_id).cloned() {
                                    members.push(ResolvedVariantMember {
                                        node_id: member_id,
                                        name: field.name.clone(),
                                        ty,
                                    });
                                }
                            }
                            ResolvedVariantShape::Record
                        }
                        None => ResolvedVariantShape::Unit,
                    };
                    let schema = ResolvedVariantSchema {
                        node_id: variant_id.clone(),
                        owner: definition.node_id.clone(),
                        name: variant.name.clone(),
                        shape,
                        members,
                    };
                    if resolved_variants.insert(variant_id, schema).is_some() {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: duplicate canonical variant '{}::{}'",
                                definition.qualified_name, variant.name
                            ),
                            definition.origin.user_span(),
                        ));
                    }
                }
            }
            crate::ast::TypeDefKind::Alias(target) | crate::ast::TypeDefKind::Newtype(target) => {
                let zonked = match ZonkedTy::from_resolved(target.clone()) {
                    Ok(target) => target,
                    Err(error) => {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: type target '{}' is not zonked: {error}",
                                definition.qualified_name
                            ),
                            definition.origin.user_span(),
                        ));
                        continue;
                    }
                };
                match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                    Ok(target) => {
                        type_targets.insert(definition.node_id.clone(), target);
                    }
                    Err(error) => errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: type target '{}' is not canonical: {error}",
                            definition.qualified_name
                        ),
                        definition.origin.user_span(),
                    )),
                }
            }
        }
    }

    let mut actors = program.actors.values().collect::<Vec<_>>();
    actors.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for actor in actors {
        // The declaration itself owns a handle type even when it has no
        // methods or spawn expression. Canonical actor descriptors must not
        // depend on incidental expression/signature interning.
        match crate::core::NominalTypeId::new(actor.node_id.0.clone()).and_then(|item| {
            types.intern_resolved(crate::core::ResolvedType::Nominal {
                item,
                arguments: Vec::new(),
                is_linear: false,
            })
        }) {
            Ok(_) => {}
            Err(error) => errors.push(Diagnostic::error(
                format!("TOOL-RESOLUTION-001: actor handle type is not canonical: {error}"),
                actor.origin.user_span(),
            )),
        }
        let module = actor
            .qualified_name
            .rsplit_once("::")
            .map(|(module, _)| module);
        let mut resolve_name = |name: &str| {
            if let Some(primitive) = crate::core::ResolvedTypeName::primitive(name) {
                return Some(primitive);
            }
            if let Some(module) = module {
                let qualified = format!("{module}::{name}");
                if let Some(candidates) = nominal_catalog.get(&qualified) {
                    if candidates.len() == 1 {
                        return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                            .ok()
                            .map(crate::core::ResolvedTypeName::Nominal);
                    }
                }
            }
            if let Some(primitive) = transparent_alias_primitive_inner(
                name,
                program,
                &mut std::collections::BTreeSet::new(),
            ) {
                return Some(crate::core::ResolvedTypeName::Primitive(primitive));
            }
            if let Some(resolved) = resolve_nominal(&nominal_catalog, name) {
                return Some(resolved);
            }
            builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal)
        };
        for (name, field_type, _) in &actor.fields {
            let Some(field_id) = actor.field_ids.get(name) else {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: actor '{}' field '{}' has no stable declaration identity",
                        actor.qualified_name, name
                    ),
                    actor.origin.user_span(),
                ));
                continue;
            };
            if !program.node_meta.contains_key(field_id) {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: actor field '{}' is absent from NodeMeta",
                        field_id.0
                    ),
                    actor.origin.user_span(),
                ));
                continue;
            }
            let zonked = match ZonkedTy::from_resolved(field_type.clone()) {
                Ok(ty) => ty,
                Err(error) => {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: field '{}' in actor '{}' is not zonked: {error}",
                            name, actor.qualified_name
                        ),
                        actor.origin.user_span(),
                    ));
                    continue;
                }
            };
            match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                Ok(ty) => {
                    field_types.insert(field_id.clone(), ty);
                }
                Err(error) => errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: field '{}' in actor '{}' is not canonical: {error}",
                        name, actor.qualified_name
                    ),
                    actor.origin.user_span(),
                )),
            }
        }
    }

    let mut flows = program.flows.values().collect::<Vec<_>>();
    flows.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for flow in flows {
        let flow_generic_names = flow
            .generic_parameters
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();
        let module = flow.id.0.rsplit_once("::").map(|(module, _)| module);
        let mut resolve_name = |name: &str| {
            if let Some(primitive) = crate::core::ResolvedTypeName::primitive(name) {
                return Some(primitive);
            }
            if let Some(parameter) = flow_generic_names.get(name) {
                return Some(crate::core::ResolvedTypeName::GenericParameter(
                    parameter.clone(),
                ));
            }
            if let Some(module) = module {
                let qualified = format!("{module}::{name}");
                if let Some(candidates) = nominal_catalog.get(&qualified) {
                    if candidates.len() == 1 {
                        return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                            .ok()
                            .map(crate::core::ResolvedTypeName::Nominal);
                    }
                }
            }
            if let Some(primitive) = transparent_alias_primitive_inner(
                name,
                program,
                &mut std::collections::BTreeSet::new(),
            ) {
                return Some(crate::core::ResolvedTypeName::Primitive(primitive));
            }
            if let Some(resolved) = resolve_nominal(&nominal_catalog, name) {
                return Some(resolved);
            }
            builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal)
        };
        let mut states = flow.states.values().collect::<Vec<_>>();
        states.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        for state in states {
            for (name, field_type) in &state.payload {
                let Some(field_id) = state.field_ids.get(name) else {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: state '{}::{}' field '{}' has no stable declaration identity",
                            flow.id.0, state.id.name, name
                        ),
                        state.origin.user_span(),
                    ));
                    continue;
                };
                if !program.node_meta.contains_key(field_id) {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: state field '{}' is absent from NodeMeta",
                            field_id.0
                        ),
                        state.origin.user_span(),
                    ));
                    continue;
                }
                let zonked = match ZonkedTy::from_resolved(field_type.clone()) {
                    Ok(ty) => ty,
                    Err(error) => {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: field '{}' in state '{}::{}' is not zonked: {error}",
                                name, flow.id.0, state.id.name
                            ),
                            state.origin.user_span(),
                        ));
                        continue;
                    }
                };
                match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                    Ok(ty) => {
                        field_types.insert(field_id.clone(), ty);
                    }
                    Err(error) => errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: field '{}' in state '{}::{}' is not canonical: {error}",
                            name, flow.id.0, state.id.name
                        ),
                        state.origin.user_span(),
                    )),
                }
            }
        }
    }

    for owner in [
        "builtin:type:MemoryDump",
        "builtin:type:PanicPayload",
        "builtin:type:PeerFault",
        "builtin:type:SystemTrace",
        "builtin:type:ExecResult",
        "builtin:type:StatResult",
    ] {
        let Some(schema) = builtin_record_schema(owner) else {
            continue;
        };
        let mut resolve_name = |name: &str| {
            crate::core::ResolvedTypeName::primitive(name)
                .or_else(|| builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal))
        };
        for (field, type_name) in schema {
            let zonked = ZonkedTy::from_resolved(Type::Name((*type_name).into(), Vec::new()))
                .expect("builtin record schemas contain finalized type names");
            match types.intern_zonked(&zonked, &capabilities, &mut resolve_name) {
                Ok(ty) => {
                    field_types.insert(NodeId(format!("{owner}/field:{field}")), ty);
                }
                Err(error) => errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: builtin field '{owner}.{field}' is not canonical: {error}"
                    ),
                    Span::UNKNOWN,
                )),
            }
        }
    }

    // Canonical payload types are required for two consumers: multi-target
    // transition receipts need every target state's layout, and a `runs Flow`
    // Actor needs its root state's payload when opening the actor domain.
    // Without the latter, an otherwise valid one-target Flow loses the root
    // field type before the checker-owned Actor descriptor is built.
    let mut states_requiring_payload_types: BTreeMap<String, BTreeSet<String>> = {
        let mut map = BTreeMap::new();
        for transition in program.transitions.values() {
            if transition.targets.len() > 1 {
                map.entry(transition.id.flow.0.clone())
                    .or_insert_with(BTreeSet::new)
                    .extend(transition.targets.iter().map(|target| target.name.clone()));
            }
        }
        map
    };
    for item in &file.items {
        let crate::ast::Item::Actor(actor) = item else {
            continue;
        };
        let Some(flow_name) = actor.runs_flow.as_deref() else {
            continue;
        };
        let Some(flow) = file.items.iter().find_map(|candidate| match candidate {
            crate::ast::Item::Flow(flow) if flow.name == flow_name => Some(flow),
            _ => None,
        }) else {
            continue;
        };
        let Some(root) = flow.states.iter().find(|state| state.name != "Fault") else {
            continue;
        };
        states_requiring_payload_types
            .entry(flow_name.to_owned())
            .or_default()
            .insert(root.name.clone());
    }
    for flow in program.flows.values() {
        let Some(target_states) = states_requiring_payload_types.get(&flow.id.0) else {
            continue;
        };
        let flow_generic_names = flow
            .generic_parameters
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();
        let module = flow.id.0.rsplit_once("::").map(|(module, _)| module);
        let mut resolve_state_name = |name: &str| {
            if let Some(primitive) = crate::core::ResolvedTypeName::primitive(name) {
                return Some(primitive);
            }
            if let Some(parameter) = flow_generic_names.get(name) {
                return Some(crate::core::ResolvedTypeName::GenericParameter(
                    parameter.clone(),
                ));
            }
            let flow_qualified = format!("{}::{name}", flow.id.0);
            if let Some(candidates) = nominal_catalog.get(&flow_qualified) {
                if candidates.len() == 1 {
                    return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                        .ok()
                        .map(crate::core::ResolvedTypeName::Nominal);
                }
            }
            if let Some(module) = module {
                let qualified = format!("{module}::{name}");
                if let Some(candidates) = nominal_catalog.get(&qualified) {
                    if candidates.len() == 1 {
                        return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                            .ok()
                            .map(crate::core::ResolvedTypeName::Nominal);
                    }
                }
            }
            if let Some(resolved) = resolve_nominal(&nominal_catalog, name) {
                return Some(resolved);
            }
            builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal)
        };
        for state in flow.states.values() {
            if !target_states.contains(&state.id.name) {
                continue;
            }
            for (field_name, field_ty) in &state.payload {
                let Some(field_id) = state.field_ids.get(field_name) else {
                    continue;
                };
                let zonked = match ZonkedTy::from_resolved(field_ty.clone()) {
                    Ok(zonked) => zonked,
                    Err(error) => {
                        errors.push(Diagnostic::error(
                            format!(
                                "TOOL-RESOLUTION-001: state '{}'::{} field '{}' is not zonked: {error}",
                                flow.id.0, state.id.name, field_name
                            ),
                            state.origin.user_span(),
                        ));
                        continue;
                    }
                };
                match types.intern_zonked(&zonked, &capabilities, &mut resolve_state_name) {
                    Ok(ty) => {
                        state_field_types.insert(field_id.clone(), ty);
                    }
                    Err(error) => errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: state '{}'::{} field '{}' is not canonical: {error}",
                            flow.id.0, state.id.name, field_name
                        ),
                        state.origin.user_span(),
                    )),
                }
            }
        }
    }
    if let Err(type_errors) = types.validate() {
        errors.extend(type_errors.into_iter().map(|error| {
            Diagnostic::error(format!("TOOL-RESOLUTION-001: {error}"), Span::UNKNOWN)
        }));
    }
    if errors.is_empty() {
        Ok((
            types,
            signatures,
            node_types,
            type_operands,
            field_types,
            resolved_variants,
            type_targets,
            type_arguments,
            state_field_types,
            trait_signatures,
        ))
    } else {
        Err(errors)
    }
}

struct DeclarationMemberContext<'a, R> {
    types: &'a mut crate::core::ResolvedTypeTable,
    capabilities: &'a crate::core::ResolvedTypeCapabilities,
    resolve_name: &'a mut R,
    field_types: &'a mut BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    errors: &'a mut Vec<Diagnostic>,
}

fn canonicalize_declaration_member<R>(
    member_id: NodeId,
    member_name: &str,
    member_type: &Type,
    definition: &ResolvedTypeDef,
    context: &mut DeclarationMemberContext<'_, R>,
) where
    R: FnMut(&str) -> Option<crate::core::ResolvedTypeName>,
{
    let zonked = match ZonkedTy::from_resolved(member_type.clone()) {
        Ok(ty) => ty,
        Err(error) => {
            context.errors.push(Diagnostic::error(
                format!(
                    "TOOL-RESOLUTION-001: member '{}' in '{}' is not zonked: {error}",
                    member_name, definition.qualified_name
                ),
                definition.origin.user_span(),
            ));
            return;
        }
    };
    match context
        .types
        .intern_zonked(&zonked, context.capabilities, &mut *context.resolve_name)
    {
        Ok(ty) => {
            context.field_types.insert(member_id, ty);
        }
        Err(error) => context.errors.push(Diagnostic::error(
            format!(
                "TOOL-RESOLUTION-001: member '{}' in '{}' is not canonical: {error}",
                member_name, definition.qualified_name
            ),
            definition.origin.user_span(),
        )),
    }
}

fn contains_unresolved_type(ty: &Type) -> bool {
    // C3 (audit 2026-08-03): `Any` removed — it is a legitimate internal
    // polymorphic marker (builtin `map_get` returns `(bool, Any)`), not an
    // unresolved type. 0.34.10 (golden §2.4) removed it from user syntax, so
    // the only producers are internal signatures (stdlib maps/set wrappers,
    // builtin map ops). Rejecting it here failed stdlib trait signatures
    // with TOOL-RESOLUTION-001 before codegen could even run.
    crate::core::type_folder::type_any(ty, &|t| match t {
        Type::Infer | Type::TypeVar(_) | Type::TyErr => true,
        Type::ForAll(_, _) => true,
        Type::Name(name, _) => name == "_" || name == "unknown",
        _ => false,
    })
}

#[cfg(test)]
mod tests;
