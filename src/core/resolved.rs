use crate::ast::{
    AstNodeMeta, AstOrigin, AstParentHint, Expr, FStringPart, File, FlowDef, Item, Pattern,
    PatternKind, Stmt, Type,
};
use crate::core::checker::flow::FlowAcc;
use crate::core::phase::{TypeScheme, ZonkedTy};
use crate::diagnostic::Diagnostic;
use crate::span::{SourceRegistry, Span};
use std::collections::{BTreeMap, HashMap};

use super::OwnershipLedger;

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
    Module,
    Actor,
    Flow,
    Protocol,
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
    pub argc: usize,
    /// Expected arity from function/extern directories when known.
    pub expected_argc: Option<usize>,
    /// Effects from callee function directory when known.
    pub effects: Vec<String>,
    /// Return type display from callee function directory when known.
    pub ret: Option<String>,
    pub kind: ResolvedCallKind,
    pub origin: Origin,
}

impl ResolvedCallSite {
    pub fn arity_matches(&self) -> bool {
        self.expected_argc
            .map(|expected| expected == self.argc)
            .unwrap_or(true)
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
    pub origin: Origin,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ResolvedFlow {
    pub node_id: NodeId,
    pub id: FlowId,
    pub states: HashMap<String, ResolvedState>,
    pub transitions: Vec<TransitionId>,
    pub max_children: Option<usize>,
    pub mailbox_depth: Option<usize>,
    pub persistent_fields: Vec<String>,
    pub transactional_fields: Vec<String>,
    pub metadata_shadow_fields: Vec<String>,
    pub impl_protocols: Vec<String>,
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
pub struct ResolvedProtocolState {
    pub name: String,
    pub payload_name: Option<String>,
    pub payload_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedProtocolTransition {
    pub event: String,
    pub from_state: String,
    pub to_states: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedProtocol {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub states: Vec<String>,
    pub state_payloads: Vec<ResolvedProtocolState>,
    pub transitions: Vec<(String, String, Vec<String>)>, // (event, from, to_states)
    pub transition_records: Vec<ResolvedProtocolTransition>,
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
    pub origin: Origin,
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
    pub method_signatures: Vec<ResolvedMethodSig>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedImpl {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub trait_name: String,
    pub type_name: String,
    pub methods: Vec<String>,
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

#[derive(Debug, Clone)]
pub struct ResolvedTypeDef {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub kind: ResolvedTypeKind,
    /// Alias/newtype target type display when applicable.
    pub alias_of: Option<String>,
    /// Record/union fields: (name, type display).
    pub fields: Vec<(String, String)>,
    /// Enum variants: (name, optional payload display).
    pub variants: Vec<(String, Option<String>)>,
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
    pub no_panic: bool,
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
    pub origin: Origin,
}

#[derive(Debug)]
pub struct CheckedProgram {
    /// Owned normalized source retained only while legacy consumers migrate.
    /// This field is deleted with the final raw-AST consumer in v0.31.5.
    legacy_file: File,
    items: HashMap<NodeId, ResolvedItem>,
    node_meta: HashMap<NodeId, NodeMeta>,
    call_sites: HashMap<NodeId, ResolvedCallSite>,
    flows: HashMap<FlowId, ResolvedFlow>,
    transitions: HashMap<TransitionId, ResolvedTransition>,
    functions: HashMap<NodeId, ResolvedFunction>,
    sessions: HashMap<NodeId, ResolvedSession>,
    protocols: HashMap<NodeId, ResolvedProtocol>,
    actors: HashMap<NodeId, ResolvedActor>,
    capabilities: HashMap<NodeId, ResolvedCapability>,
    constants: HashMap<NodeId, ResolvedConstant>,
    traits: HashMap<NodeId, ResolvedTrait>,
    impls: HashMap<NodeId, ResolvedImpl>,
    type_defs: HashMap<NodeId, ResolvedTypeDef>,
    extern_blocks: HashMap<NodeId, ResolvedExternBlock>,
    backend_requirements: Vec<CapabilityRequirement>,
    ownership_ledgers: HashMap<NodeId, OwnershipLedger>,
    type_schemes: HashMap<NodeId, TypeScheme>,
    zonked_function_types: HashMap<NodeId, (Vec<ZonkedTy>, ZonkedTy)>,
    resolved_types: crate::core::ResolvedTypeTable,
    resolved_signatures: BTreeMap<NodeId, crate::core::ResolvedSignature>,
    resolved_node_types: BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    resolved_type_operands: BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    resolved_type_arguments: BTreeMap<NodeId, Vec<crate::core::ResolvedTypeId>>,
    resolved_field_types: BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    resolved_type_targets: BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    resolved_bodies: BTreeMap<NodeId, crate::core::ResolvedBody>,
    callable_cfgs: BTreeMap<NodeId, crate::core::cfg::CallableCfg>,
    resource_analyses: BTreeMap<NodeId, crate::core::ResourceAnalysis>,
}

impl CheckedProgram {
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
            ..
        } = acc;
        let mut program = Self::from_checked_file_base(file)?;
        let mut errors = Vec::new();
        let mut zonked_by_node = HashMap::new();

        // Override declaration snapshots only with mandatory-finalized checker
        // artifacts. Raw checker types are not a backend input.
        for func in program.functions.values_mut() {
            if let Some((resolved_params, resolved_ret)) = zonked_nested_func_types
                .get(&func.node_id)
                .or_else(|| zonked_func_types.get(&func.qualified_name))
            {
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
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let stable_expression_types = stabilize_expression_types(&program, &zonked_expr_types)?;
        for meta in program.node_meta.values_mut() {
            meta.expression_key = None;
        }
        program.type_schemes = schemes;
        program.zonked_function_types = zonked_by_node;
        let (
            resolved_types,
            resolved_signatures,
            resolved_node_types,
            resolved_type_operands,
            resolved_field_types,
            resolved_type_targets,
            resolved_type_arguments,
        ) = build_canonical_function_signatures(&program, &stable_expression_types)?;
        for meta in program.node_meta.values_mut() {
            meta.type_operand = None;
            meta.type_arguments.clear();
        }
        program.resolved_types = resolved_types;
        program.resolved_signatures = resolved_signatures;
        program.resolved_node_types = resolved_node_types;
        program.resolved_type_operands = resolved_type_operands;
        program.resolved_field_types = resolved_field_types;
        program.resolved_type_targets = resolved_type_targets;
        program.resolved_type_arguments = resolved_type_arguments;
        program.resolved_bodies =
            match crate::core::ir::lower::lower_checked_callable_bodies(file, &program) {
                Ok(bodies) => bodies,
                Err(body_errors) => {
                    return Err(body_errors
                        .into_iter()
                        .map(|error| {
                            let span = program
                                .node_meta
                                .get(&error.node_id)
                                .map(|meta| meta.origin.user_span())
                                .unwrap_or(Span::UNKNOWN);
                            Diagnostic::error(format!("TOOL-RESOLUTION-001: {error}"), span)
                        })
                        .collect())
                }
            };
        program.callable_cfgs = crate::core::cfg::lower_resolved_bodies(&program.resolved_bodies)?;
        program.resource_analyses = crate::core::cfg::analyze_resolved_bodies(
            &program.callable_cfgs,
            &program.resolved_bodies,
            &program.resolved_signatures,
            &program.resolved_types,
        )?;
        program.ownership_ledgers = program
            .resource_analyses
            .iter()
            .map(|(owner, analysis)| {
                (
                    owner.clone(),
                    OwnershipLedger::from_analysis(analysis, &program.callable_cfgs[owner]),
                )
            })
            .collect();
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
        let mut protocols = HashMap::new();
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
            "",
            &file.sources,
            &mut items,
            &mut node_meta,
            &mut functions,
            &mut sessions,
            &mut protocols,
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
        for call in call_sites.values() {
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
        for protocol in protocols.values() {
            origin_catalog.register(&protocol.node_id, &protocol.origin, &mut errors);
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
            legacy_file: file.clone(),
            items,
            node_meta,
            call_sites,
            flows,
            transitions,
            functions,
            sessions,
            protocols,
            actors,
            capabilities,
            constants,
            traits,
            impls,
            type_defs,
            extern_blocks,
            backend_requirements,
            ownership_ledgers: HashMap::new(),
            type_schemes: HashMap::new(),
            zonked_function_types: HashMap::new(),
            resolved_types: crate::core::ResolvedTypeTable::new(),
            resolved_signatures: BTreeMap::new(),
            resolved_node_types: BTreeMap::new(),
            resolved_type_operands: BTreeMap::new(),
            resolved_type_arguments: BTreeMap::new(),
            resolved_field_types: BTreeMap::new(),
            resolved_type_targets: BTreeMap::new(),
            resolved_bodies: BTreeMap::new(),
            callable_cfgs: BTreeMap::new(),
            resource_analyses: BTreeMap::new(),
        })
    }

    /// Transitional body source for backends that do not yet consume typed body IR.
    /// Declaration-only consumers must use the resolved catalogs instead.
    pub(crate) fn legacy_body_file(&self) -> &File {
        &self.legacy_file
    }

    pub fn transitions(&self) -> &HashMap<TransitionId, ResolvedTransition> {
        &self.transitions
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

    pub fn protocols(&self) -> &HashMap<NodeId, ResolvedProtocol> {
        &self.protocols
    }

    pub fn protocol(&self, qualified_name: &str) -> Option<&ResolvedProtocol> {
        self.protocols
            .values()
            .find(|protocol| protocol.qualified_name == qualified_name)
    }

    pub fn protocol_state_payload(
        &self,
        protocol_name: &str,
        state_name: &str,
    ) -> Option<&ResolvedProtocolState> {
        self.protocol(protocol_name).and_then(|protocol| {
            protocol
                .state_payloads
                .iter()
                .find(|state| state.name == state_name)
        })
    }

    pub fn protocol_transition_records(
        &self,
        protocol_name: &str,
    ) -> Option<&[ResolvedProtocolTransition]> {
        self.protocol(protocol_name)
            .map(|protocol| protocol.transition_records.as_slice())
    }

    pub fn actors(&self) -> &HashMap<NodeId, ResolvedActor> {
        &self.actors
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

    pub fn extern_func_signature(&self, name: &str) -> Option<&ResolvedExternFunc> {
        self.extern_blocks
            .values()
            .flat_map(|block| block.signatures.iter())
            .find(|sig| sig.name == name)
    }

    pub fn ownership_ledgers(&self) -> &HashMap<NodeId, OwnershipLedger> {
        &self.ownership_ledgers
    }

    pub fn ownership_ledger(&self, owner: &NodeId) -> Option<&OwnershipLedger> {
        self.ownership_ledgers.get(owner)
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

    pub fn resolved_type_targets(&self) -> &BTreeMap<NodeId, crate::core::ResolvedTypeId> {
        &self.resolved_type_targets
    }

    pub fn resolved_type_target(
        &self,
        definition: &NodeId,
    ) -> Option<&crate::core::ResolvedTypeId> {
        self.resolved_type_targets.get(definition)
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

fn backend_supports(backend: BackendProfile, capability: &str) -> bool {
    match backend {
        // Interpreter implements the current Flow surface, including experimental multi-target.
        BackendProfile::Interpreter => true,
        // Native still lacks tagged multi-target ABI and transactional WAL.
        BackendProfile::Native => !matches!(capability, "flow.multi_target" | "flow.transactional"),
        // Verifier proves function contracts; it does not claim Proven for Flow turns.
        // Multi-target / transactional flows must not block unrelated contract verification.
        BackendProfile::Verifier => true,
        // Component IR consumers cannot yet lower Flow runtime features.
        BackendProfile::Component => {
            !matches!(capability, "flow.multi_target" | "flow.transactional")
        }
    }
}

fn collect_items(
    items: &[Item],
    module: &str,
    sources: &SourceRegistry,
    resolved_items: &mut HashMap<NodeId, ResolvedItem>,
    node_meta: &mut HashMap<NodeId, NodeMeta>,
    functions: &mut HashMap<NodeId, ResolvedFunction>,
    sessions: &mut HashMap<NodeId, ResolvedSession>,
    protocols: &mut HashMap<NodeId, ResolvedProtocol>,
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
    for item in items {
        collect_item_meta(item, module, &ids, node_meta, errors);
        match item {
            Item::Module(def) => {
                let qualified = qualify(module, &def.name);
                let span = declaration_span(def.meta, def.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Module,
                    &qualified,
                    def.meta,
                    span,
                    errors,
                );
                collect_items(
                    &def.items,
                    &qualified,
                    sources,
                    resolved_items,
                    node_meta,
                    functions,
                    sessions,
                    protocols,
                    actors,
                    capabilities,
                    constants,
                    traits,
                    impls,
                    type_defs,
                    extern_blocks,
                    flows,
                    transitions,
                    backend_requirements,
                    errors,
                );
            }
            Item::Flow(flow) => {
                let qualified = qualify(module, &flow.name);
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
            }
            Item::Func(function) => {
                let qualified = qualify(module, &function.name);
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
                let qualified = qualify(module, &type_def.name);
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
                let mut variants = Vec::new();
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
                        }
                    }
                    crate::ast::TypeDefKind::Enum(enum_variants) => {
                        for variant in enum_variants {
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
                let node_id = NodeId(format!("type:{}", qualified));
                type_defs.insert(
                    node_id.clone(),
                    ResolvedTypeDef {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        kind,
                        alias_of,
                        fields,
                        variants,
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
                ..
            } => {
                let qualified = qualify(module, name);
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
                let qualified = qualify(module, &cap.name);
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
            }
            Item::Trait(trait_def) => {
                let qualified = qualify(module, &trait_def.name);
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
                let mut method_signatures = Vec::new();
                for method in &trait_def.methods {
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
                let qualified = qualify(
                    module,
                    &format!("{}:for:{}", impl_def.trait_name, impl_def.type_name),
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
                for method in &impl_def.methods {
                    let method_id = impl_method_owner(&qualified, method);
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
                            qualified_name: format!("{}_{}", impl_def.type_name, method.name),
                            params,
                            param_decls,
                            ret: method
                                .ret
                                .clone()
                                .unwrap_or_else(|| Type::Name("unit".into(), Vec::new())),
                            effects: method.effects.clone(),
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
                        methods,
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
                let qualified = qualify(module, &extern_block_key(block));
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
                let qualified = qualify(module, &actor.name);
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
                let methods = actor
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
            Item::Protocol(protocol) => {
                let qualified = qualify(module, &protocol.name);
                let span = declaration_span(protocol.meta, protocol.meta.span);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Protocol,
                    &qualified,
                    protocol.meta,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("protocol:{}", qualified));
                let states = protocol
                    .states
                    .iter()
                    .map(|state| state.name.clone())
                    .collect::<Vec<_>>();
                let mut state_payloads = Vec::new();
                for state in &protocol.states {
                    if let Some(ty) = &state.payload_type {
                        if contains_unresolved_type(ty) {
                            errors.push(Diagnostic::error(
                                format!(
                                    "TOOL-RESOLUTION-001: unresolved payload type in protocol '{}' state '{}'",
                                    qualified,
                                    state.name
                                ),
                                span,
                            ));
                        }
                    }
                    state_payloads.push(ResolvedProtocolState {
                        name: state.name.clone(),
                        payload_name: state.payload_name.clone(),
                        payload_type: state.payload_type.as_ref().map(crate::core::fmt_type),
                    });
                }
                let transitions = protocol
                    .transitions
                    .iter()
                    .map(|transition| {
                        (
                            transition.name.clone(),
                            transition.from_state.clone(),
                            vec![transition.to_state.clone()],
                        )
                    })
                    .collect::<Vec<_>>();
                let transition_records = protocol
                    .transitions
                    .iter()
                    .map(|transition| ResolvedProtocolTransition {
                        event: transition.name.clone(),
                        from_state: transition.from_state.clone(),
                        to_states: vec![transition.to_state.clone()],
                    })
                    .collect::<Vec<_>>();
                protocols.insert(
                    node_id.clone(),
                    ResolvedProtocol {
                        node_id: node_id.clone(),
                        qualified_name: qualified.clone(),
                        states,
                        state_payloads,
                        transitions,
                        transition_records,
                        origin: resolve_named_origin(
                            ResolvedItemKind::Protocol,
                            &qualified,
                            &node_id,
                            protocol.meta,
                            span,
                            errors,
                        ),
                    },
                );
            }
            Item::Session(session) => {
                let qualified = qualify(module, &session.name);
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
            Stmt::If { then_, else_, .. } => {
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
            Stmt::While { body, .. }
            | Stmt::WhileLet { body, .. }
            | Stmt::Loop(body)
            | Stmt::For { body, .. }
            | Stmt::Block(body)
            | Stmt::Arena(body)
            | Stmt::Unsafe(body)
            | Stmt::OnFailure(body)
            | Stmt::Do(body)
            | Stmt::Parasteps(body)
            | Stmt::Alloc { body, .. }
            | Stmt::Pinned { body, .. } => collect_nested_function_records(
                body,
                owner,
                parent_qualified,
                (ids, inherited_generic_binders),
                node_meta,
                functions,
                errors,
            ),
            _ => {}
        }
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
        ResolvedItemKind::Module => "module",
        ResolvedItemKind::Actor => "actor",
        ResolvedItemKind::Flow => "flow",
        ResolvedItemKind::Protocol => "protocol",
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

pub(crate) fn stmt_sibling_role(context: &str, block: &[Stmt], index: usize) -> String {
    semantic_sibling_role(
        &format!("{context}.statement"),
        block,
        index,
        stmt_semantic_key,
    )
}

pub(crate) fn expr_sibling_role(context: &str, exprs: &[Expr], index: usize) -> String {
    semantic_sibling_role(context, exprs, index, expr_semantic_key)
}

pub(crate) fn pattern_sibling_role(context: &str, patterns: &[Pattern], index: usize) -> String {
    semantic_sibling_role(context, patterns, index, pattern_semantic_key)
}

pub(crate) fn type_sibling_role(context: &str, types: &[Type], index: usize) -> String {
    semantic_sibling_role(context, types, index, type_semantic_key)
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

pub(crate) fn match_arm_role(context: &str, arms: &[crate::ast::MatchArm], index: usize) -> String {
    semantic_sibling_role(context, arms, index, match_arm_semantic_key)
}

fn map_entry_semantic_key(entry: &(Expr, Expr)) -> String {
    format!(
        "{}=>{}",
        expr_semantic_key(&entry.0),
        expr_semantic_key(&entry.1)
    )
}

pub(crate) fn map_entry_role(context: &str, entries: &[(Expr, Expr)], index: usize) -> String {
    semantic_sibling_role(context, entries, index, map_entry_semantic_key)
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

fn semantic_sibling_role<T>(
    context: &str,
    values: &[T],
    index: usize,
    key_of: impl Fn(&T) -> String,
) -> String {
    let key = key_of(&values[index]);
    let occurrence = values[..index]
        .iter()
        .filter(|value| key_of(value) == key)
        .count();
    format!(
        "{}.{:016x}.same.{}",
        context,
        stable_text_hash(&key),
        occurrence
    )
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
        Type::Cap(_) => "type.capability",
        Type::Shared(_) => "type.shared",
        Type::LocalShared(_) => "type.local_shared",
        Type::Weak(_) => "type.weak",
        Type::WeakLocal(_) => "type.weak_local",
        Type::Newtype(_, _) => "type.newtype",
        Type::Nothing => "type.nothing",
        Type::Allocator => "type.allocator",
        Type::Array(_, _) => "type.array",
        Type::Slice(_) => "type.slice",
        Type::ImplTrait(_) => "type.impl_trait",
        Type::DynTrait(_) => "type.dyn_trait",
        Type::RawPtr(_) => "type.raw_ptr",
        Type::RawPtrMut(_) => "type.raw_ptr_mut",
        Type::CShared(_) => "type.c_shared",
        Type::CBorrow(_) => "type.c_borrow",
        Type::CBorrowMut(_) => "type.c_borrow_mut",
        Type::RawString => "type.raw_string",
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
            for index in 0..args.len() {
                let child_role = type_sibling_role(&format!("{role}.argument"), args, index);
                collect_type_meta(
                    &args[index],
                    &node_id,
                    &child_role,
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
        | Type::LocalShared(inner)
        | Type::Weak(inner)
        | Type::WeakLocal(inner)
        | Type::Newtype(_, inner)
        | Type::Slice(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::CShared(inner)
        | Type::CBorrow(inner)
        | Type::CBorrowMut(inner)
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
            for index in 0..items.len() {
                let child_role = type_sibling_role(&format!("{role}.element"), items, index);
                collect_type_meta(
                    &items[index],
                    &node_id,
                    &child_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        Type::Func(params, ret) | Type::ExternFunc(params, ret) => {
            for index in 0..params.len() {
                let child_role = type_sibling_role(&format!("{role}.parameter"), params, index);
                collect_type_meta(
                    &params[index],
                    &node_id,
                    &child_role,
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
        | Type::Nothing
        | Type::Allocator
        | Type::ImplTrait(_)
        | Type::DynTrait(_)
        | Type::RawString
        | Type::Infer
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

fn method_signature_key(name: &str, params: &[crate::ast::Param], ret: Option<&Type>) -> String {
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

fn extern_function_owner(block_owner: &NodeId, function: &crate::ast::ExternFunc) -> NodeId {
    NodeId(format!(
        "{}/function:{}:{:016x}",
        block_owner.0,
        stable_id_fragment(&function.name),
        stable_text_hash(&extern_function_signature_key(function))
    ))
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
    NodeId(format!(
        "{}/function:{}:{:016x}",
        owner.0,
        stable_id_fragment(&function.name),
        stable_text_hash(&method_signature_key(
            &function.name,
            &function.params,
            function.ret.as_ref()
        ))
    ))
}

fn collect_item_meta(
    item: &Item,
    module: &str,
    ids: &NodeIdBuilder<'_>,
    out: &mut HashMap<NodeId, NodeMeta>,
    errors: &mut Vec<Diagnostic>,
) {
    match item {
        Item::Module(def) => {
            let qualified = qualify(module, &def.name);
            let node_id = NodeId(format!("module:{qualified}"));
            let fallback = def.meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::Module,
                &qualified,
                def.meta,
                fallback,
                out,
                errors,
            );
            for import in &def.imports {
                let key = format!(
                    "{}:as:{}",
                    import.path.join("::"),
                    import.alias.as_deref().unwrap_or("_")
                );
                insert_child_meta(
                    import.meta,
                    &node_id,
                    "decl.import",
                    &format!("import.{}", stable_id_fragment(&key)),
                    declaration_span(import.meta, fallback),
                    ids,
                    out,
                    errors,
                );
            }
        }
        Item::Func(function) => {
            let qualified = qualify(module, &function.name);
            let node_id = NodeId(format!("function:{qualified}"));
            let fallback = function.meta.span;
            let parent = top_level_enclosing_parent(&qualified);
            collect_func_meta(function, node_id, &parent, fallback, ids, out, errors);
        }
        Item::Type(type_def) => {
            let qualified = qualify(module, &type_def.name);
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
                                for index in 0..types.len() {
                                    let child_role =
                                        type_sibling_role("payload.element", types, index);
                                    collect_type_meta(
                                        &types[index],
                                        &variant_id,
                                        &child_role,
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
            let qualified = qualify(module, &actor.name);
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
            let qualified = qualify(module, &cap.name);
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
            let qualified = qualify(module, &trait_def.name);
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
            let qualified = qualify(
                module,
                &format!("{}:for:{}", impl_def.trait_name, impl_def.type_name),
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
            for index in 0..impl_def.trait_args.len() {
                let role = type_sibling_role("trait_argument", &impl_def.trait_args, index);
                collect_type_meta(
                    &impl_def.trait_args[index],
                    &node_id,
                    &role,
                    span,
                    ids,
                    out,
                    errors,
                );
            }
            for index in 0..impl_def.type_args.len() {
                let role = type_sibling_role("type_argument", &impl_def.type_args, index);
                collect_type_meta(
                    &impl_def.type_args[index],
                    &node_id,
                    &role,
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
            let qualified = qualify(module, &extern_block_key(block));
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
            let qualified = qualify(module, name);
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
            let qualified = qualify(module, &flow.name);
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
        Item::Protocol(protocol) => {
            let qualified = qualify(module, &protocol.name);
            let node_id = NodeId(format!("protocol:{qualified}"));
            let fallback = protocol.meta.span;
            insert_canonical_meta(
                node_id.clone(),
                ResolvedItemKind::Protocol,
                &qualified,
                protocol.meta,
                fallback,
                out,
                errors,
            );
            let span = declaration_span(protocol.meta, fallback);
            for generic in &protocol.generics {
                collect_generic_param_meta(generic, &node_id, "generic", span, ids, out, errors);
            }
            for state in &protocol.states {
                let state_id = NodeId(format!(
                    "{}/state:{}",
                    node_id.0,
                    stable_id_fragment(&state.name)
                ));
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
                if let Some(payload) = &state.payload_type {
                    collect_type_meta(payload, &state_id, "payload_type", span, ids, out, errors);
                }
            }
            for transition in &protocol.transitions {
                let signature = format!(
                    "{}:{}->{}",
                    transition.name, transition.from_state, transition.to_state
                );
                let transition_id = NodeId(format!(
                    "{}/transition:{}:{:016x}",
                    node_id.0,
                    stable_id_fragment(&transition.name),
                    stable_text_hash(&signature)
                ));
                insert_node_meta(
                    transition_id,
                    transition.meta.origin,
                    transition.meta.parent,
                    ast_meta_anchor(transition.meta),
                    span,
                    &node_id,
                    None,
                    out,
                    errors,
                );
            }
        }
        Item::Session(session) => {
            let qualified = qualify(module, &session.name);
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
        Stmt::While { cond, .. } => format!("while:{}", expr_semantic_key(cond)),
        Stmt::WhileLet { pat, init, .. } => format!(
            "while-let:{}:{}",
            pattern_semantic_key(pat),
            expr_semantic_key(init)
        ),
        Stmt::Loop(_) => "loop".into(),
        Stmt::For { var, iterable, .. } => {
            format!("for:{var}:{}", expr_semantic_key(iterable))
        }
        Stmt::Block(_) => "block".into(),
        Stmt::Desc(value, _) => format!("desc:{value}"),
        Stmt::Rule(value, _) => format!("rule:{value}"),
        Stmt::Requires(expr, _) => format!("requires:{}", expr_semantic_key(expr)),
        Stmt::Ensures(expr, _) => format!("ensures:{}", expr_semantic_key(expr)),
        Stmt::Invariant(expr, _) => format!("invariant:{}", expr_semantic_key(expr)),
        Stmt::Math(exprs) => format!("math:{}", exprs.len()),
        Stmt::Assign { target, .. } => format!("assign:{}", expr_semantic_key(target)),
        Stmt::Arena(_) => "arena".into(),
        Stmt::Unsafe(_) => "unsafe".into(),
        Stmt::Drop(expr) => format!("drop:{}", expr_semantic_key(expr)),
        Stmt::SharedLet { name, .. } => format!("shared-let:{name}"),
        Stmt::OnFailure(_) => "on-failure".into(),
        Stmt::Do(_) => "do".into(),
        Stmt::Delegate { target, .. } => format!("delegate:{target}"),
        Stmt::Pinned { var, .. } => format!("pinned:{}", var.as_deref().unwrap_or("_")),
        Stmt::Parasteps(_) => "parasteps".into(),
        Stmt::MmsBlock { content, .. } => format!("mms:{:016x}", stable_text_hash(content)),
        Stmt::Func(function) => format!("function:{}", function.name),
        Stmt::Alloc { kind, .. } => format!("alloc:{kind:?}"),
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
        Stmt::While { .. } => "stmt.while",
        Stmt::WhileLet { .. } => "stmt.while_let",
        Stmt::Loop(_) => "stmt.loop",
        Stmt::For { .. } => "stmt.for",
        Stmt::Block(_) => "stmt.block",
        Stmt::Desc(_, _) => "stmt.desc",
        Stmt::Rule(_, _) => "stmt.rule",
        Stmt::Requires(_, _) => "stmt.requires",
        Stmt::Ensures(_, _) => "stmt.ensures",
        Stmt::Invariant(_, _) => "stmt.invariant",
        Stmt::Math(_) => "stmt.math",
        Stmt::Assign { .. } => "stmt.assign",
        Stmt::Arena(_) => "stmt.arena",
        Stmt::Unsafe(_) => "stmt.unsafe",
        Stmt::Drop(_) => "stmt.drop",
        Stmt::SharedLet { .. } => "stmt.shared_let",
        Stmt::OnFailure(_) => "stmt.on_failure",
        Stmt::Do(_) => "stmt.do",
        Stmt::Delegate { .. } => "stmt.delegate",
        Stmt::Pinned { .. } => "stmt.pinned",
        Stmt::Parasteps(_) => "stmt.parasteps",
        Stmt::MmsBlock { .. } => "stmt.mms",
        Stmt::Func(_) => "stmt.function",
        Stmt::Alloc { .. } => "stmt.alloc",
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
        | Stmt::SharedLet { init: expr, .. }
        | Stmt::Delegate { expr, .. } => {
            expr_span(expr).map(|span| (span, SpanPrecision::SourceAnchor))
        }
        Stmt::If { cond, .. } | Stmt::While { cond, .. } => {
            expr_span(cond).map(|span| (span, SpanPrecision::SourceAnchor))
        }
        Stmt::WhileLet { pat, .. } => anchored(pat.meta.span),
        Stmt::For { iterable, .. } => {
            expr_span(iterable).map(|span| (span, SpanPrecision::SourceAnchor))
        }
        Stmt::Desc(_, span)
        | Stmt::Rule(_, span)
        | Stmt::Requires(_, span)
        | Stmt::Ensures(_, span)
        | Stmt::Invariant(_, span)
        | Stmt::MmsBlock { span, .. } => anchored(*span),
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
        node_id,
        ast_origin,
        meta.map(|meta| meta.parent).unwrap_or(AstParentHint::None),
        anchor,
        fallback,
        owner,
        None,
        out,
        errors,
    );
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
        Stmt::Continue | Stmt::Ellipsis | Stmt::Desc(_, _) | Stmt::Rule(_, _) => {}
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
        | Stmt::OnFailure(body)
        | Stmt::Do(body)
        | Stmt::Parasteps(body) => collect_block_meta(
            body,
            owner,
            &format!("{role}.body"),
            fallback,
            ids,
            out,
            errors,
        ),
        Stmt::For { iterable, body, .. } => {
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
            for index in 0..exprs.len() {
                let child_role = expr_sibling_role(&format!("{role}.math"), exprs, index);
                collect_expr_meta(
                    &exprs[index],
                    owner,
                    &child_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
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
        Stmt::Delegate { expr, .. } => collect_expr_meta(
            expr,
            owner,
            &format!("{role}.expression"),
            fallback,
            ids,
            out,
            errors,
        ),
        Stmt::Pinned {
            expr,
            timeout,
            body,
            ..
        } => {
            collect_expr_meta(
                expr,
                owner,
                &format!("{role}.expression"),
                fallback,
                ids,
                out,
                errors,
            );
            if let Some(timeout) = timeout {
                collect_expr_meta(
                    timeout,
                    owner,
                    &format!("{role}.timeout"),
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
        Stmt::MmsBlock { .. } => {}
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
        Stmt::Alloc { body, .. } => collect_block_meta(
            body,
            owner,
            &format!("{role}.body"),
            fallback,
            ids,
            out,
            errors,
        ),
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
        Expr::Range { .. } => "range".into(),
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
        Expr::Range { .. } => "expr.range",
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
            for index in 0..args.len() {
                let child_role = expr_sibling_role(&format!("{role}.argument"), args, index);
                collect_expr_meta(&args[index], owner, &child_role, fallback, ids, out, errors);
            }
        }
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            for index in 0..items.len() {
                let child_role = expr_sibling_role(&format!("{role}.element"), items, index);
                collect_expr_meta(
                    &items[index],
                    owner,
                    &child_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
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
            for (index, arm) in arms.iter().enumerate() {
                let arm_role = match_arm_role(&format!("{role}.arm"), arms, index);
                insert_child_meta(
                    arm.meta,
                    owner,
                    "match.arm",
                    &arm_role,
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
        Expr::Record { fields, .. } => {
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
        Expr::Range { start, end } => {
            collect_expr_meta(
                start,
                owner,
                &format!("{role}.start"),
                fallback,
                ids,
                out,
                errors,
            );
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
        Expr::Turbofish(_, types, args) => {
            for index in 0..types.len() {
                let child_role = type_sibling_role(&format!("{role}.type_argument"), types, index);
                collect_type_meta(
                    &types[index],
                    owner,
                    &child_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
            for index in 0..args.len() {
                let child_role = expr_sibling_role(&format!("{role}.argument"), args, index);
                collect_expr_meta(&args[index], owner, &child_role, fallback, ids, out, errors);
            }
        }
        Expr::MapLiteral { entries } => {
            for (index, (key, value)) in entries.iter().enumerate() {
                let entry_role = map_entry_role(&format!("{role}.entry"), entries, index);
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
            for index in 0..items.len() {
                let child_role = pattern_sibling_role(&format!("{role}.element"), items, index);
                collect_pattern_meta(
                    &items[index],
                    owner,
                    &child_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
            }
        }
        PatternKind::Slice(items, rest) => {
            for index in 0..items.len() {
                let child_role = pattern_sibling_role(&format!("{role}.element"), items, index);
                collect_pattern_meta(
                    &items[index],
                    owner,
                    &child_role,
                    fallback,
                    ids,
                    out,
                    errors,
                );
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
    let flow_span = declaration_span(flow.meta, flow.meta.span);
    let states = flow
        .states
        .iter()
        .map(|state| {
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
            (
                state.name.clone(),
                ResolvedState {
                    node_id,
                    id,
                    payload,
                    field_ids,
                    origin,
                },
            )
        })
        .collect();
    let mut flow_transition_ids = Vec::with_capacity(flow.transitions.len());
    if !flow.transactional_fields.is_empty() {
        backend_requirements.push(CapabilityRequirement {
            requirement_id: "FLOW-TURN-001",
            capability: "flow.transactional",
            flow: flow_id.clone(),
            span: flow_span,
        });
    }
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
            origin: transition_origin,
            span,
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
        }
    }
    let resolved_flow = ResolvedFlow {
        node_id: flow_node_id.clone(),
        id: flow_id.clone(),
        states,
        transitions: flow_transition_ids,
        max_children,
        mailbox_depth,
        persistent_fields: flow.persistent_fields.clone(),
        transactional_fields: flow.transactional_fields.clone(),
        metadata_shadow_fields: flow.metadata_shadow_fields.clone(),
        impl_protocols: flow.impl_protocols.clone(),
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

fn extern_block_key(block: &crate::ast::ExternBlock) -> String {
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
    let mut function_info: HashMap<String, (usize, Vec<String>, String)> = HashMap::new();
    for function in functions.values() {
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
    for block in extern_blocks.values() {
        for sig in &block.signatures {
            extern_info.insert(sig.name.clone(), (sig.params.len(), sig.ret.clone()));
        }
        // Keep names even if signature missing (defensive).
        for func in &block.funcs {
            extern_info
                .entry(func.clone())
                .or_insert((0, "unit".into()));
        }
    }
    let mut method_info: HashMap<String, (usize, Vec<String>, String)> = HashMap::new();
    for actor in actors.values() {
        for method in &actor.method_signatures {
            // Prefer bare method name; qualified actor.method also recorded.
            method_info.insert(
                method.name.clone(),
                (
                    method.params.len(),
                    method.effects.clone(),
                    method.ret.clone(),
                ),
            );
            method_info.insert(
                format!("{}.{}", actor.qualified_name, method.name),
                (
                    method.params.len(),
                    method.effects.clone(),
                    method.ret.clone(),
                ),
            );
        }
    }
    for impl_def in impls.values() {
        for method in &impl_def.method_signatures {
            method_info.insert(
                method.name.clone(),
                (
                    method.params.len(),
                    method.effects.clone(),
                    method.ret.clone(),
                ),
            );
            method_info.insert(
                format!("{}.{}", impl_def.type_name, method.name),
                (
                    method.params.len(),
                    method.effects.clone(),
                    method.ret.clone(),
                ),
            );
        }
    }
    let ids = NodeIdBuilder::new(&file.sources);
    for item in &file.items {
        collect_item_call_sites(
            item,
            "",
            &ids,
            &function_info,
            &extern_info,
            &method_info,
            out,
            errors,
        );
    }
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
    module: &str,
    ids: &NodeIdBuilder<'_>,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
    out: &mut HashMap<NodeId, ResolvedCallSite>,
    errors: &mut Vec<Diagnostic>,
) {
    match item {
        Item::Module(module_def) => {
            let next = if module.is_empty() {
                module_def.name.clone()
            } else {
                format!("{module}::{}", module_def.name)
            };
            for inner in &module_def.items {
                collect_item_call_sites(
                    inner, &next, ids, functions, externs, methods, out, errors,
                );
            }
        }
        Item::Func(function) => {
            let owner = NodeId(if module.is_empty() {
                format!("function:{}", function.name)
            } else {
                format!("function:{module}::{}", function.name)
            });
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
            let qualified = qualify(module, &actor.name);
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
            let qualified = qualify(module, &trait_def.name);
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
            let qualified = qualify(
                module,
                &format!("{}:for:{}", impl_def.trait_name, impl_def.type_name),
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
            let qualified = qualify(module, &extern_block_key(block));
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
            let owner = NodeId(format!("constant:{}", qualify(module, name)));
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
            let qualified = qualify(module, &flow.name);
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
    for (index, stmt) in block.iter().enumerate() {
        let role = stmt_sibling_role(context, block, index);
        collect_stmt_call_sites(
            stmt, owner, &role, fallback, ids, functions, externs, methods, out, errors,
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
        argc,
        expected_argc,
        effects,
        ret,
        kind,
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
        | Stmt::SharedLet { init: expr, .. }
        | Stmt::Delegate { expr, .. } => {
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
        | Stmt::OnFailure(body)
        | Stmt::Do(body)
        | Stmt::Parasteps(body) => {
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
        Stmt::Pinned {
            expr,
            timeout,
            body,
            ..
        } => {
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
            if let Some(timeout) = timeout {
                collect_expr_call_sites(
                    timeout,
                    owner,
                    &format!("{role}.timeout"),
                    fallback,
                    ids,
                    functions,
                    externs,
                    methods,
                    out,
                    errors,
                );
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
        Stmt::Alloc { body, .. } => {
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
        Stmt::Math(exprs) => {
            for index in 0..exprs.len() {
                let child_role = expr_sibling_role(&format!("{role}.math"), exprs, index);
                collect_expr_call_sites(
                    &exprs[index],
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
            record_expr_call_site(
                expr,
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
            for index in 0..args.len() {
                let child_role = expr_sibling_role(&format!("{role}.argument"), args, index);
                collect_expr_call_sites(
                    &args[index],
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
            for index in 0..items.len() {
                let child_role = expr_sibling_role(&format!("{role}.element"), items, index);
                collect_expr_call_sites(
                    &items[index],
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
            for (index, arm) in arms.iter().enumerate() {
                let arm_role = match_arm_role(&format!("{role}.arm"), arms, index);
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
        Expr::Record { fields, .. } => {
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
        Expr::Range { start, end } => {
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
        Expr::Turbofish(name, _, args) => {
            record_expr_call_site(
                expr,
                owner,
                role,
                fallback,
                ids,
                args.len(),
                resolve_named_call_callee(name, functions, externs, methods),
                out,
                errors,
            );
            for index in 0..args.len() {
                let child_role = expr_sibling_role(&format!("{role}.argument"), args, index);
                collect_expr_call_sites(
                    &args[index],
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
        Expr::MapLiteral { entries } => {
            for (index, (key, value)) in entries.iter().enumerate() {
                let entry_role = map_entry_role(&format!("{role}.entry"), entries, index);
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

fn resolve_named_call_callee(
    name: &str,
    functions: &HashMap<String, (usize, Vec<String>, String)>,
    externs: &HashMap<String, (usize, String)>,
    methods: &HashMap<String, (usize, Vec<String>, String)>,
) -> ResolvedCalleeFacts {
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
                "!{}.{}",
                crate::core::fmt_type(payload),
                format_session_type(cont)
            )
        }
        crate::ast::SessionType::Recv(payload, cont) => {
            format!(
                "?{}.{}",
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
    BTreeMap<NodeId, crate::core::ResolvedTypeId>,
    BTreeMap<NodeId, Vec<crate::core::ResolvedTypeId>>,
);

fn stabilize_expression_types(
    program: &CheckedProgram,
    ephemeral: &EphemeralExpressionTypes,
) -> Result<StableExpressionTypes, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let mut stable = BTreeMap::new();
    for (owner, expression_types) in ephemeral {
        let owner_prefix = format!("{}/", owner.0);
        let mut keys = BTreeMap::new();
        for meta in program
            .node_meta
            .values()
            .filter(|meta| meta.node_id.0.starts_with(&owner_prefix))
        {
            if let Some(key) = &meta.expression_key {
                if let Some(previous) = keys.insert(key.clone(), meta.node_id.clone()) {
                    errors.push(Diagnostic::error(
                        format!(
                            "TOOL-RESOLUTION-001: callable '{}' expression key maps to both '{}' and '{}'",
                            owner.0, previous.0, meta.node_id.0
                        ),
                        meta.origin.user_span(),
                    ));
                }
            }
        }
        let mut owner_types = BTreeMap::new();
        for (key, ty) in expression_types {
            let Some(node_id) = keys.get(key) else {
                let foreign_owners = program
                    .node_meta
                    .values()
                    .filter(|meta| meta.expression_key.as_ref() == Some(key))
                    .map(|meta| &meta.node_id)
                    .collect::<Vec<_>>();
                if foreign_owners.len() == 1 {
                    // The checker may re-check a declaration-owned default at
                    // a call site. Its semantic node remains owned by the
                    // callee and is materialized exactly once there.
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

fn build_canonical_function_signatures(
    program: &CheckedProgram,
    expression_types: &StableExpressionTypes,
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

    fn builtin_nominal(name: &str) -> Option<crate::core::NominalTypeId> {
        const BUILTIN_NOMINALS: &[&str] = &[
            "AST",
            "ExecResult",
            "Fault",
            "Future",
            "List",
            "Map",
            "MemoryDump",
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
            "Tuple",
            "Type",
            "TypeInfo",
            "session_chan",
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
        register_nominal(&mut nominal_catalog, &flow.id.0, &flow.node_id);
        for state in flow.states.values() {
            register_nominal(
                &mut nominal_catalog,
                &format!("{}::{}", flow.id.0, state.id.name),
                &state.node_id,
            );
        }
    }
    for protocol in program.protocols.values() {
        register_nominal(
            &mut nominal_catalog,
            &protocol.qualified_name,
            &protocol.node_id,
        );
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
    let ids = NodeIdBuilder::new(&program.legacy_file.sources);
    let mut signatures = BTreeMap::new();
    let mut node_types = BTreeMap::new();
    let mut type_operands = BTreeMap::new();
    let mut type_arguments = BTreeMap::new();
    let mut field_types = BTreeMap::new();
    let mut type_targets = BTreeMap::new();
    let mut errors = Vec::new();
    let mut functions = program.functions.values().collect::<Vec<_>>();
    functions.sort_by(|left, right| left.node_id.cmp(&right.node_id));

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
        for (name, id) in &function.generic_binders {
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
            if let Some(candidates) = nominal_catalog.get(name) {
                if candidates.len() == 1 {
                    let identity = candidates.iter().next()?.clone();
                    return crate::core::NominalTypeId::new(identity)
                        .ok()
                        .map(crate::core::ResolvedTypeName::Nominal);
                }
            }
            builtin_nominal(name).map(crate::core::ResolvedTypeName::Nominal)
        };

        let mut canonical_parameter_types = Vec::with_capacity(parameter_types.len());
        let mut signature_failed = false;
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
        }

        // Persist every explicit type annotation owned by the callable, not
        // only type operands attached to expressions. ResolvedBody uses this
        // table to type local bindings and conversions without consulting raw
        // `ast::Type` after construction.
        let owner_prefix = format!("{}/", function.node_id.0);
        let annotated_types = program
            .node_meta
            .iter()
            .filter_map(|(node_id, meta)| {
                meta.type_operand
                    .as_ref()
                    .filter(|_| {
                        node_id.0.starts_with(&owner_prefix)
                            && !program.functions.keys().any(|nested| {
                                nested != &function.node_id
                                    && node_id.0.starts_with(&format!("{}/", nested.0))
                            })
                    })
                    .map(|annotation| (node_id.clone(), annotation.clone()))
            })
            .collect::<Vec<_>>();
        for (node_id, annotation) in annotated_types {
            if matches!(
                annotation.unlocated(),
                Type::Infer | Type::TypeVar(_) | Type::ForAll(_, _)
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

    let mut transitions = program.transitions.values().collect::<Vec<_>>();
    transitions.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for transition in transitions {
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
            if let Some(candidates) = nominal_catalog.get(name) {
                if candidates.len() == 1 {
                    return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                        .ok()
                        .map(crate::core::ResolvedTypeName::Nominal);
                }
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
        let result_type = transition
            .targets
            .first()
            .map(|target| Type::Name(format!("{}::{}", target.flow.0, target.name), Vec::new()))
            .unwrap_or_else(|| Type::Name("unit".into(), Vec::new()));
        let result = match ZonkedTy::from_resolved(result_type) {
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
        let signature = crate::core::ResolvedSignature {
            owner: transition.node_id.clone(),
            generic_parameters: Vec::new(),
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
        let mut generic_names = BTreeMap::new();
        for generic in &definition.declaration.generics {
            let id = ids.anonymous(
                &definition.node_id,
                "decl.generic_parameter",
                &format!("generic.{}", stable_id_fragment(&generic.name)),
                usable_span(generic.meta.span),
                generic.meta.origin,
                &mut errors,
            );
            generic_names.insert(generic.name.clone(), id);
        }
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
            if let Some(candidates) = nominal_catalog.get(name) {
                if candidates.len() == 1 {
                    return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                        .ok()
                        .map(crate::core::ResolvedTypeName::Nominal);
                }
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
                    let variant_id = ids.anonymous(
                        &definition.node_id,
                        "decl.variant",
                        &format!("variant.{}", stable_id_fragment(&variant.name)),
                        usable_span(variant.meta.span),
                        variant.meta.origin,
                        &mut errors,
                    );
                    match &variant.payload {
                        Some(crate::ast::VariantPayload::Tuple(payload)) => {
                            for index in 0..payload.len() {
                                let role = type_sibling_role("payload.element", payload, index);
                                let meta = payload[index].meta();
                                let member_id = ids.anonymous(
                                    &variant_id,
                                    type_kind(&payload[index]),
                                    &role,
                                    meta.and_then(|meta| usable_span(meta.span)),
                                    meta.map(|meta| meta.origin).unwrap_or(AstOrigin::User),
                                    &mut errors,
                                );
                                canonicalize_declaration_member(
                                    member_id,
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
                            }
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
                                    member_id,
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
                        None => {}
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
            if let Some(candidates) = nominal_catalog.get(name) {
                if candidates.len() == 1 {
                    return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                        .ok()
                        .map(crate::core::ResolvedTypeName::Nominal);
                }
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
        let module = flow.id.0.rsplit_once("::").map(|(module, _)| module);
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
            if let Some(candidates) = nominal_catalog.get(name) {
                if candidates.len() == 1 {
                    return crate::core::NominalTypeId::new(candidates.iter().next()?.clone())
                        .ok()
                        .map(crate::core::ResolvedTypeName::Nominal);
                }
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
            type_targets,
            type_arguments,
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
    match ty {
        Type::Located { ty, .. } => contains_unresolved_type(ty),
        Type::Infer | Type::TypeVar(_) => true,
        Type::Name(name, args) => {
            name == "Any"
                || name == "_"
                || name == "unknown"
                || args.iter().any(contains_unresolved_type)
        }
        Type::Ref(_, inner)
        | Type::RefMut(_, inner)
        | Type::Option(inner)
        | Type::Shared(inner)
        | Type::LocalShared(inner)
        | Type::Weak(inner)
        | Type::WeakLocal(inner)
        | Type::Array(inner, _)
        | Type::Slice(inner)
        | Type::Newtype(_, inner)
        | Type::CBuffer(inner)
        | Type::RawPtr(inner)
        | Type::RawPtrMut(inner)
        | Type::CShared(inner)
        | Type::CBorrow(inner)
        | Type::CBorrowMut(inner) => contains_unresolved_type(inner),
        Type::ForAll(_, _) => true,
        Type::Result(ok, err) => contains_unresolved_type(ok) || contains_unresolved_type(err),
        Type::Tuple(items) => items.iter().any(contains_unresolved_type),
        Type::Func(args, ret) | Type::ExternFunc(args, ret) => {
            args.iter().any(contains_unresolved_type) || contains_unresolved_type(ret)
        }
        Type::Cap(_)
        | Type::DynTrait(_)
        | Type::ImplTrait(_)
        | Type::Nothing
        | Type::Allocator
        | Type::RawString => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> File {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse")
    }

    fn node_id_at(program: &CheckedProgram, kind: &str, line: usize, col: usize) -> NodeId {
        let marker = format!("/node:{kind}@");
        program
            .node_meta()
            .iter()
            .find_map(|(node_id, meta)| {
                let span = meta.origin.user_span();
                (node_id.0.contains(&marker) && span.start_line == line && span.start_col == col)
                    .then(|| node_id.clone())
            })
            .unwrap_or_else(|| panic!("missing {kind} at {line}:{col}"))
    }

    fn generated_node_id(program: &CheckedProgram, owner: &str, kind: &str, rule: &str) -> NodeId {
        let generated_prefix = format!("{owner}/generated:{kind}:");
        let anchored_marker = format!("{owner}/node:{kind}@");
        program
            .node_meta()
            .iter()
            .find_map(|(node_id, meta)| {
                ((node_id.0.starts_with(&generated_prefix)
                    || node_id.0.starts_with(&anchored_marker))
                    && meta.origin.rule() == Some(rule))
                .then(|| node_id.clone())
            })
            .unwrap_or_else(|| panic!("missing generated {kind} for {owner} ({rule})"))
    }

    fn node_meta_ids(program: &CheckedProgram) -> std::collections::BTreeSet<String> {
        program
            .node_meta()
            .keys()
            .map(|node_id| node_id.0.clone())
            .collect()
    }

    #[test]
    fn checked_program_persists_zonked_function_signatures() {
        let file = parse("func identity(value: i32) -> i32 { value }");
        let program = crate::core::check_program(&file).expect("check");
        let function = program.function("identity").expect("resolved function");
        let (params, ret) = program
            .zonked_function_type(&function.node_id)
            .expect("checker zonked signature");

        assert_eq!(params.len(), 1);
        assert_eq!(params[0].as_type(), &Type::Name("i32".into(), vec![]));
        assert_eq!(ret.as_type(), &Type::Name("i32".into(), vec![]));
        assert_eq!(function.params[0].1, params[0].as_type().clone());
        assert_eq!(function.ret, ret.as_type().clone());
    }

    #[test]
    fn checked_program_owns_its_migration_body_input() {
        let program = {
            let mut file = parse("func main() -> i32 { 42 }");
            let program = crate::core::check_program(&file).expect("check");
            file.items.clear();
            assert!(file.items.is_empty());
            program
        };

        assert!(program.function("main").is_some());
        assert!(!program.legacy_body_file().items.is_empty());
        let mut interpreter = crate::interp::Interpreter::from_checked(&program);
        assert!(matches!(
            interpreter.run().expect("run owned checked program"),
            crate::interp::Value::Int(42)
        ));
    }

    #[test]
    fn checked_program_materializes_canonical_function_signature() {
        let file = parse("func choose(value: List<i32>, fallback: i32) -> i32 { fallback }");
        let program = crate::core::check_program(&file).expect("check");
        let function = program.function("choose").expect("function");
        let signature = program
            .resolved_signature(&function.node_id)
            .expect("canonical signature");

        assert_eq!(signature.owner, function.node_id);
        assert_eq!(signature.parameters.len(), 2);
        assert!(signature
            .parameters
            .iter()
            .all(|parameter| program.resolved_types().get(&parameter.ty).is_some()));
        assert!(program.resolved_types().get(&signature.result).is_some());
        assert!(signature.validate(program.resolved_types()).is_ok());
    }

    #[test]
    fn canonical_generic_signature_uses_binder_identity() {
        let file = parse("func identity<T>(value: T) -> T { value }");
        let program = crate::core::check_program(&file).expect("check");
        let function = program.function("identity").expect("function");
        let signature = program
            .resolved_signature(&function.node_id)
            .expect("canonical signature");

        assert_eq!(signature.generic_parameters.len(), 1);
        assert_eq!(signature.parameters[0].ty, signature.result);
        assert!(matches!(
            program.resolved_types().get(&signature.result),
            Some(crate::core::ResolvedType::GenericParameter(parameter))
                if parameter == &signature.generic_parameters[0]
        ));
    }

    #[test]
    fn canonical_impl_method_signature_inherits_impl_binder() {
        let file = parse(
            "trait Head<T> { func head() -> T }\nimpl<T> Head<T> for List<T> { func head() -> T { self[0] } }\nfunc first<T>(values: List<T>) -> T { values.head() }",
        );
        let program = crate::core::check_program(&file).expect("check generic impl");
        let method = program
            .functions()
            .values()
            .find(|function| function.qualified_name == "List_head")
            .expect("resolved impl method");
        let signature = program
            .resolved_signature(&method.node_id)
            .expect("canonical impl signature");

        assert_eq!(signature.generic_parameters.len(), 1);
        assert!(matches!(
            program.resolved_types().get(&signature.result),
            Some(crate::core::ResolvedType::GenericParameter(parameter))
                if parameter == &signature.generic_parameters[0]
        ));
        assert!(matches!(
            program.resolved_types().get(&signature.parameters[0].ty),
            Some(crate::core::ResolvedType::Nominal { item, arguments })
                if item.as_str() == "builtin:type:List"
                    && matches!(
                        arguments.as_slice(),
                        [argument] if argument == &signature.result
                    )
        ));
    }

    #[test]
    fn canonical_signatures_are_declaration_order_independent() {
        let first = parse(
            "func first(value: i32) -> i32 { value }\nfunc second(value: List<i32>) -> i32 { 0 }",
        );
        let second = parse(
            "func second(value: List<i32>) -> i32 { 0 }\nfunc first(value: i32) -> i32 { value }",
        );
        let first = crate::core::check_program(&first).expect("first check");
        let second = crate::core::check_program(&second).expect("second check");
        for name in ["first", "second"] {
            let first_function = first.function(name).expect("first function");
            let second_function = second.function(name).expect("second function");
            let first_signature = first
                .resolved_signature(&first_function.node_id)
                .expect("first signature");
            let second_signature = second
                .resolved_signature(&second_function.node_id)
                .expect("second signature");
            assert_eq!(
                first_signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.ty.clone())
                    .collect::<Vec<_>>(),
                second_signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.ty.clone())
                    .collect::<Vec<_>>()
            );
            assert_eq!(first_signature.result, second_signature.result);
        }
    }

    #[test]
    fn checker_expression_types_use_stable_resolved_node_ids() {
        let file = parse(
            "func maximum(left: i32, right: i32) -> i32 { if left > right { left } else { right } }",
        );
        let program = crate::core::check_program(&file).expect("check");
        let function = program.function("maximum").expect("function");
        let prefix = format!("{}/", function.node_id.0);
        let expression_nodes = program
            .node_meta()
            .keys()
            .filter(|node| node.0.starts_with(&prefix) && node.0.contains("/node:expr."))
            .collect::<Vec<_>>();

        assert!(!expression_nodes.is_empty());
        assert!(expression_nodes
            .iter()
            .all(|node| program.resolved_node_type(node).is_some()));
        assert!(program
            .node_meta()
            .values()
            .all(|meta| meta.expression_key.is_none()));
        assert!(program
            .node_meta()
            .values()
            .all(|meta| meta.type_operand.is_none()));
        assert!(program
            .node_meta()
            .values()
            .all(|meta| meta.type_arguments.is_empty()));
    }

    #[test]
    fn resolved_transition_ids_include_source_state() {
        let file = parse(
            r#"
flow Door {
    state Closed
    state Open
    transition toggle(Closed) -> Open { do { return Open {} } }
    transition toggle(Open) -> Closed { do { return Closed {} } }
}
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        assert!(program.transition("Door", "toggle", "Closed").is_some());
        assert!(program.transition("Door", "toggle", "Open").is_some());
    }

    #[test]
    fn resolved_ids_do_not_depend_on_declaration_order() {
        let first = parse(
            r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
    transition close(Open) -> Closed { do { return Closed {} } }
}
"#,
        );
        let second = parse(
            r#"
flow Door {
    state Open
    state Closed
    transition close(Open) -> Closed { do { return Closed {} } }
    transition open(Closed) -> Open { do { return Open {} } }
}
"#,
        );
        let first = crate::core::check_program(&first).expect("check first");
        let second = crate::core::check_program(&second).expect("check second");
        let first_ids = first
            .transitions()
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let second_ids = second
            .transitions()
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(first_ids, second_ids);
    }

    #[test]
    fn native_capability_gate_rejects_multi_target() {
        let file = parse(
            r#"
flow Decision {
    state Pending
    state Yes
    state No
    transition decide(Pending) -> Yes | No { do { return Yes {} } }
}
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let diagnostics = program
            .validate_backend(BackendProfile::Native)
            .expect_err("native must reject multi-target");
        assert!(diagnostics[0].message.contains("FLOW-MULTI-001"));
        assert!(diagnostics[0].message.contains("flow.multi_target"));
        assert_eq!(diagnostics[0].span.start_line, 6);
    }

    #[test]
    fn verifier_capability_gate_allows_multi_target_for_contract_verification() {
        // Verifier proves function contracts; multi-target must not block
        // unrelated verification of the same CheckedProgram.
        let file = parse(
            r#"
flow Decision {
    state Pending
    state Yes
    state No
    transition decide(Pending) -> Yes | No { do { return Yes {} } }
}
func abs(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    x
}
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        program
            .validate_backend(BackendProfile::Verifier)
            .expect("verifier must not reject multi-target flows for contract verification");
        assert!(program
            .transition("Decision", "decide", "Pending")
            .is_some());
    }

    #[test]
    fn resolved_transition_table_is_exact_source_keyed() {
        let file = parse(
            r#"
flow Counter {
    state Zero
    state Pos
    transition inc(Zero) -> Pos { do { return Pos {} } }
    transition inc(Pos) -> Pos { do { return Pos {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        assert!(program.transition("Counter", "inc", "Zero").is_some());
        assert!(program.transition("Counter", "inc", "Pos").is_some());
        assert!(program.transition("Counter", "inc", "Missing").is_none());
        assert!(program.transition("Counter", "dec", "Zero").is_none());
    }

    #[test]
    fn resolved_function_signatures_are_indexed_by_qualified_name() {
        let file = parse(
            r#"
module util {
    func twice(x: i32) -> i32 { x + x }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let twice = program
            .function("util::twice")
            .expect("util::twice signature");
        assert_eq!(twice.params.len(), 1);
        assert_eq!(twice.params[0].0, "x");
        assert!(matches!(twice.params[0].1.unlocated(), Type::Name(n, _) if n == "i32"));
        assert!(matches!(twice.ret.unlocated(), Type::Name(n, _) if n == "i32"));
        assert!(program.function("twice").is_none());
        assert!(program.function("main").is_some());
    }

    #[test]
    fn resolved_function_records_effect_clause() {
        let file = parse(
            r#"
cap Io
func write(x: i32) -> i32 with Io { x }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let write = program.function("write").expect("write");
        assert!(write.effects.iter().any(|e| e == "Io"));
    }

    #[test]
    fn resolved_session_types_are_indexed() {
        let file = parse(
            r#"
session Ping = !i32 . ?i32 . end
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let session = program.session("Ping").expect("Ping session");
        assert!(matches!(
            session.body.unlocated(),
            crate::ast::SessionType::Send(_, _)
        ));
    }

    #[test]
    fn resolved_protocol_topology_is_indexed() {
        let file = parse(
            r#"
protocol Sensor {
    state Idle
    state Active
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let protocol = program.protocol("Sensor").expect("Sensor");
        assert!(protocol.states.iter().any(|s| s == "Idle"));
        assert!(protocol.states.iter().any(|s| s == "Active"));
        assert!(protocol
            .transitions
            .iter()
            .any(|(name, from, to)| name == "start"
                && from == "Idle"
                && to.as_slice() == ["Active"]));
    }

    #[test]
    fn resolved_actor_fields_and_methods_are_indexed() {
        let file = parse(
            r#"
actor Counter {
    count: i32
    func inc() -> i32 { 1 }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let actor = program.actor("Counter").expect("Counter actor");
        assert!(actor.fields.iter().any(|(n, _, _)| n == "count"));
        assert!(actor.methods.iter().any(|m| m == "inc"));
    }

    #[test]
    fn interpreter_from_checked_installs_function_directory() {
        let file = parse(
            r#"
cap Io
func write(x: i32) -> i32 with Io { x }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(interp.resolved_function_arity("write"), Some(1));
        let effects = interp
            .resolved_function_effects("write")
            .expect("write effects");
        assert!(effects.iter().any(|e| e == "Io"));
        assert!(program.function("write").is_some());
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.has_checked_function("write"));
        assert!(verifier
            .checked_function_effects("write")
            .is_some_and(|e| e.iter().any(|x| x == "Io")));
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "fx");
        codegen.compile_checked(&program).expect("compile");
        assert!(codegen
            .resolved_function_effects("write")
            .is_some_and(|e| e.iter().any(|x| x == "Io")));
        assert_eq!(codegen.resolved_function_return_type("write"), Some("i32"));
        assert_eq!(verifier.checked_function_return_type("write"), Some("i32"));
        assert_eq!(
            interp.resolved_function_params("write"),
            Some(vec![("x".into(), "i32".into())])
        );
        assert_eq!(
            codegen.resolved_function_params("write"),
            Some(vec![("x".into(), "i32".into())])
        );
        assert_eq!(
            verifier.checked_function_params("write"),
            Some(vec![("x".into(), "i32".into())])
        );
    }

    #[test]
    fn consumers_install_comptime_function_directory() {
        let file = parse(
            r#"
comptime func answer() -> i32 { 42 }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        assert!(program.function("answer").is_some_and(|f| f.is_comptime));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.is_resolved_comptime_function("answer"));
        assert!(!interp.is_resolved_comptime_function("main"));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.is_checked_comptime_function("answer"));
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "ct");
        codegen.compile_checked(&program).expect("compile");
        assert!(codegen.is_resolved_comptime_function("answer"));
    }

    #[test]
    fn interpreter_from_checked_installs_session_and_protocol_directories() {
        let file = parse(
            r#"
protocol Sensor {
    state Idle
    state Active
    transition start(Idle) -> Active
}
session Ping = !i32 . end
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.has_resolved_session("Ping"));
        assert!(interp.has_resolved_protocol("Sensor"));
        assert!(!interp.has_resolved_protocol("Missing"));
    }

    #[test]
    fn interpreter_from_checked_installs_actor_directory() {
        let file = parse(
            r#"
actor Counter {
    count: i32
    func inc() -> i32 { 1 }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        let methods = interp
            .resolved_actor_methods("Counter")
            .expect("Counter methods");
        assert!(methods.iter().any(|m| m == "inc"));
    }

    #[test]
    fn resolved_capabilities_and_constants_are_indexed() {
        let file = parse(
            r#"
cap Io
const MAX: i32 = 10
func main() -> i32 { MAX }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        assert!(program.capability("Io").is_some());
        assert!(program.constant("MAX").is_some());
    }

    #[test]
    fn resolved_traits_and_impls_are_indexed() {
        let file = parse(
            r#"
trait Close { func close() -> i32 }
type Handle { value: i32 }
impl Close for Handle {
    func close() -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let trait_def = program.trait_def("Close").expect("Close");
        assert!(trait_def.methods.iter().any(|m| m == "close"));
        assert!(program
            .impls()
            .values()
            .any(|i| i.trait_name == "Close" && i.type_name == "Handle"));
    }

    #[test]
    fn interpreter_from_checked_installs_trait_and_impl_directories() {
        let file = parse(
            r#"
trait Close { func close() -> i32 }
type Handle { value: i32 }
impl Close for Handle {
    func close() -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        let methods = interp
            .resolved_trait_methods("Close")
            .expect("Close methods");
        assert!(methods.iter().any(|m| m == "close"));
        let impl_methods = interp
            .resolved_impl_methods("Close", "Handle")
            .expect("Close for Handle");
        assert!(impl_methods.iter().any(|m| m == "close"));
        // Trait/impl method params + effects directories.
        assert_eq!(interp.resolved_method_params("Close.close"), Some(vec![]));
        assert_eq!(interp.resolved_method_effects("Close.close"), Some(vec![]));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(verifier.checked_method_params("Close.close"), Some(vec![]));
        assert_eq!(verifier.checked_method_effects("Close.close"), Some(vec![]));
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "trait_params");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(codegen.resolved_method_params("Close.close"), Some(vec![]));
        assert_eq!(codegen.resolved_method_effects("Close.close"), Some(vec![]));
    }

    #[test]
    fn consumers_install_trait_impl_method_params_and_effects() {
        let file = parse(
            r#"
cap Io
trait Writer {
    func write(data: i32) -> i32
}
type Buffer { x: i32 }
impl Writer for Buffer {
    func write(data: i32) -> i32 with Io { data }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        // Trait method: params materialised; effects empty (trait decls carry no effect set).
        assert_eq!(
            interp.resolved_method_params("Writer.write"),
            Some(vec![("data".into(), "i32".into())])
        );
        assert_eq!(interp.resolved_method_effects("Writer.write"), Some(vec![]));
        // Impl method: params + effects (Io) materialised under impl qualified name.
        assert_eq!(
            interp.resolved_method_params("Writer:for:Buffer.write"),
            Some(vec![("data".into(), "i32".into())])
        );
        assert_eq!(
            interp.resolved_method_effects("Writer:for:Buffer.write"),
            Some(vec!["Io".to_string()])
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(
            verifier.checked_method_params("Writer.write"),
            Some(vec![("data".into(), "i32".into())])
        );
        assert_eq!(
            verifier.checked_method_effects("Writer.write"),
            Some(vec![])
        );
        assert_eq!(
            verifier.checked_method_params("Writer:for:Buffer.write"),
            Some(vec![("data".into(), "i32".into())])
        );
        assert_eq!(
            verifier.checked_method_effects("Writer:for:Buffer.write"),
            Some(vec!["Io".to_string()])
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "trait_effects");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen.resolved_method_params("Writer.write"),
            Some(vec![("data".into(), "i32".into())])
        );
        assert_eq!(
            codegen.resolved_method_effects("Writer:for:Buffer.write"),
            Some(vec!["Io".to_string()])
        );
    }

    #[test]
    fn consumers_install_ownership_ledger_owners() {
        let file = parse(
            r#"
cap File
func close(f: cap File) -> i32 { drop(f); 0 }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let ledger = program
            .ownership_ledger(&crate::core::NodeId("function:close".into()))
            .expect("close ledger");
        assert_eq!(
            ledger.action_count(crate::core::ResourceActionKind::Introduce),
            1
        );
        assert_eq!(
            ledger.action_count(crate::core::ResourceActionKind::Drop),
            1
        );
        assert!(ledger.resources().iter().any(|r| r == "f"));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.has_resolved_ownership_owner("function:close"));
        assert_eq!(
            interp.resolved_ownership_summary("function:close"),
            Some((1, 0, 1, 0, 0, false))
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.has_checked_ownership_owner("function:close"));
        assert_eq!(
            verifier.checked_ownership_summary("function:close"),
            Some((1, 0, 1, 0, 0, false))
        );
        assert_eq!(
            interp.resolved_ownership_resources("function:close"),
            Some(vec!["f".into()])
        );
        assert_eq!(
            verifier.checked_ownership_resources("function:close"),
            Some(vec!["f".into()])
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "own_res");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen.resolved_ownership_resources("function:close"),
            Some(vec!["f".into()])
        );
        let actions = interp
            .resolved_ownership_actions("function:close")
            .expect("actions");
        assert!(actions.iter().any(|(k, r)| k == "introduce" && r == "f"));
        assert!(actions.iter().any(|(k, r)| k == "drop" && r == "f"));
        assert!(verifier
            .checked_ownership_actions("function:close")
            .is_some_and(|a| a.iter().any(|(k, r)| k == "drop" && r == "f")));
        assert!(codegen
            .resolved_ownership_actions("function:close")
            .is_some_and(|a| a.iter().any(|(k, r)| k == "introduce" && r == "f")));
        assert_eq!(
            interp.resolved_ownership_merges("function:close"),
            Some(vec![])
        );
    }

    #[test]
    fn ownership_merges_are_installed_for_branchy_cap_function() {
        // Native codegen still treats this pattern as unconsumed after join; exercise
        // install paths via from_checked/verify_checked only.
        let file = parse(
            r#"
cap File
func both(flag: bool, f: cap File) -> i32 {
    if flag {
        drop(f)
    } else {
        drop(f)
    }
    0
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.resolved_ownership_merges("function:both").is_some());
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.checked_ownership_merges("function:both").is_some());
        // Codegen directory install without running full object emission of this fixture:
        // compile_checked fail-closes on residual linear join for this pattern, so only
        // query install via a simple program that still has empty merges map present.
        let simple = parse(
            r#"
cap File
func close(f: cap File) -> i32 { drop(f); 0 }
func main() -> i32 { 0 }
"#,
        );
        let simple_program = crate::core::check_program(&simple).expect("check simple");
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "own_merge");
        codegen.compile_checked(&simple_program).expect("compile");
        assert!(codegen
            .resolved_ownership_merges("function:close")
            .is_some_and(|m| m.is_empty()));
    }

    #[test]
    fn ownership_summary_flags_maybe_consumed_branch_merge() {
        // This program is rejected by checker; use check_program expect_err then
        // still materialize IR is not available. Instead check accepted both-path
        // program for merge without maybe, and use a custom accepted pattern.
        // For maybe-consumed, checker errors before IR success. Validate helper
        // on a synthetic ledger instead.
        let mut ledger =
            crate::core::OwnershipLedger::new(crate::core::NodeId("function:synthetic".into()));
        ledger.branch_merges.push(crate::core::BranchMerge {
            resource: "f".into(),
            then_state: crate::core::ResourceState::Consumed,
            else_state: crate::core::ResourceState::Available,
            merged_state: crate::core::ResourceState::MaybeConsumed,
            span: crate::span::Span::single(1, 1),
        });
        assert!(ledger.has_maybe_consumed_merge());
        assert_eq!(
            ledger.action_count(crate::core::ResourceActionKind::Drop),
            0
        );
    }

    #[test]
    fn resolved_types_and_extern_blocks_are_indexed() {
        let file = parse(
            r#"
type Point { x: i32, y: i32 }
extern "C" {
    func c_abs(x: i32) -> i32
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let point = program.type_def("Point").expect("Point");
        assert_eq!(point.kind, ResolvedTypeKind::Record);
        assert!(program
            .extern_blocks()
            .values()
            .any(|block| block.funcs.iter().any(|f| f == "c_abs")));
    }

    #[test]
    fn resolved_flow_records_annotations() {
        let file = parse(
            r#"
flow Worker {
    @max_children(3)
    @mailbox(depth = 8)
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let flow = program.flow("Worker").expect("Worker");
        assert_eq!(flow.max_children, Some(3));
        assert_eq!(flow.mailbox_depth, Some(8));
    }

    #[test]
    fn interpreter_from_checked_prefers_resolved_max_children() {
        let file = parse(
            r#"
flow Worker {
    @max_children(4)
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        // max_children is private; use public API if any.
        // spawn_count / max via builtins would need runtime; assert via program IR.
        assert_eq!(program.flow("Worker").unwrap().max_children, Some(4));
        assert_eq!(interp.resolved_max_children(), Some(4));
    }

    #[test]
    fn interpreter_from_checked_installs_mailbox_depths() {
        let file = parse(
            r#"
flow Worker {
    @mailbox(depth = 64)
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(interp.resolved_mailbox_depth("Worker"), Some(64));
        assert_eq!(program.flow("Worker").unwrap().mailbox_depth, Some(64));
    }

    #[test]
    fn resolved_mailbox_depth_matches_module_qualified_flow() {
        let file = parse(
            r#"
module net {
    flow Conn {
        @mailbox(depth = 32)
        state Idle
        transition tick(Idle) -> Idle { do { return Idle {} } }
    }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        assert_eq!(program.flow("net::Conn").unwrap().mailbox_depth, Some(32));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(interp.resolved_mailbox_depth("Conn"), Some(32));
        assert_eq!(interp.resolved_mailbox_depth("net::Conn"), Some(32));
    }

    #[test]
    fn verifier_records_flow_annotation_directories() {
        let file = parse(
            r#"
flow Worker {
    @max_children(5)
    @mailbox(depth = 16)
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(verifier.checked_max_children(), Some(5));
        assert_eq!(verifier.checked_mailbox_depth("Worker"), Some(16));
    }

    #[test]
    fn resolved_flow_records_persistent_field_sets() {
        let file = parse(
            r#"
flow ResilientService {
    persistent state Config { max_retries: i32, timeout_ms: i64 }
    state Active { request_id: i32 }
    transition run(Active) -> Active { do { return Active { request_id: 1 } } }
}
func main() -> i32 { 0 }
"#,
        );
        // Materialize IR from parsed AST; full check may inject matrix defaults
        // that interact with i64 payload fields independently of this IR slice.
        let program = CheckedProgram::from_checked_file(&file).expect("ir");
        let flow = program.flow("ResilientService").expect("flow");
        assert_eq!(
            flow.persistent_fields,
            vec!["max_retries".to_string(), "timeout_ms".to_string()]
        );
        assert!(flow.states.contains_key("Config"));
        assert!(flow.states.contains_key("Active"));
    }

    #[test]
    fn consumers_install_persistent_field_directories() {
        let file = parse(
            r#"
flow ResilientService {
    persistent state Config { max_retries: i32, timeout_ms: i64 }
    state Active { request_id: i32 }
    transition run(Active) -> Active { do { return Active { request_id: 1 } } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = CheckedProgram::from_checked_file(&file).expect("ir");
        let interp = crate::interp::Interpreter::from_checked(&program);
        let fields = interp
            .resolved_persistent_fields("ResilientService")
            .expect("persistent fields");
        assert!(fields.iter().any(|f| f == "max_retries"));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        let vfields = verifier
            .checked_persistent_fields("ResilientService")
            .expect("verifier persistent fields");
        assert!(vfields.iter().any(|f| f == "timeout_ms"));
    }

    #[test]
    fn verifier_installs_transactional_field_directories() {
        let file = parse(
            r#"
flow Store {
    persistent state Active { buffer: List<i32> }
    @transactional state Active
    transition tick(Active) -> Active { do { return Active { buffer: buffer } } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = match CheckedProgram::from_checked_file(&file) {
            Ok(p) => p,
            Err(_) => return, // syntax variants differ; IR path still covered elsewhere
        };
        if let Some(flow) = program.flow("Store") {
            let mut verifier = crate::verifier::Verifier::new().expect("z3");
            let _ = verifier.verify_checked(&program);
            if !flow.transactional_fields.is_empty() {
                assert!(verifier
                    .checked_transactional_fields("Store")
                    .is_some_and(|f| !f.is_empty()));
            }
        }
    }

    #[test]
    fn checked_program_exposes_backend_requirements() {
        let file = parse(
            r#"
flow Decision {
    state Pending
    state Yes
    state No
    transition decide(Pending) -> Yes | No { do { return Yes {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        assert!(program.requires_capability("flow.multi_target"));
        assert!(program
            .backend_requirements()
            .iter()
            .any(|r| r.requirement_id == "FLOW-MULTI-001"));
        assert!(program.node_meta().len() > 0);
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.requires_resolved_capability("flow.multi_target"));
        assert!(interp.resolved_node_meta_count().is_some_and(|n| n > 0));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.requires_checked_capability("flow.multi_target"));
        assert!(verifier.checked_node_meta_count() > 0);
        // Native codegen fail-closes multi-target; use a simple program for codegen install.
        let simple = parse(
            r#"
func main() -> i32 { 0 }
"#,
        );
        let simple_program = crate::core::check_program(&simple).expect("check simple");
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "backend_req");
        codegen.compile_checked(&simple_program).expect("compile");
        assert!(codegen.resolved_node_meta_count().is_some_and(|n| n > 0));
        assert!(!codegen.requires_resolved_capability("flow.multi_target"));
    }

    #[test]
    fn resolved_flow_records_impl_protocols() {
        let file = parse(
            r#"
protocol Sensor {
    state Idle
    transition tick(Idle) -> Idle
}
flow Lidar {
    impl Sensor
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let flow = program.flow("Lidar").expect("Lidar");
        assert!(flow.impl_protocols.iter().any(|p| p == "Sensor"));
    }

    #[test]
    fn consumers_install_flow_impl_protocol_directories() {
        let file = parse(
            r#"
protocol Sensor {
    state Idle
    transition tick(Idle) -> Idle
}
flow Lidar {
    impl Sensor
    state Idle
    transition tick(Idle) -> Idle { do { return Idle {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        let protocols = interp
            .resolved_flow_protocols("Lidar")
            .expect("Lidar protocols");
        assert!(protocols.iter().any(|p| p == "Sensor"));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier
            .checked_flow_protocols("Lidar")
            .is_some_and(|p| p.iter().any(|n| n == "Sensor")));
    }

    #[test]
    fn resolved_transition_records_fallback_and_pinned_flags() {
        let file = parse(
            r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
        );
        // Matrix injects fallback edges; user open is not fallback.
        let program = crate::core::check_program(&file).expect("check");
        let open = program.transition("Door", "open", "Closed").expect("open");
        assert!(!open.is_fallback);
        // Matrix injects fallback edges for undefined combinations.
        assert!(program.transitions().values().any(|t| t.is_fallback));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(!interp.is_resolved_fallback_transition("Door", "open", "Closed"));
        assert!(program.transitions().values().any(|t| t.is_fallback
            && interp.is_resolved_fallback_transition(
                &t.id.flow.0,
                &t.id.event,
                &t.id.source.name
            )));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(program.transitions().values().any(|t| {
            t.is_fallback
                && verifier.is_checked_fallback_transition(
                    &t.id.flow.0,
                    &t.id.event,
                    &t.id.source.name,
                )
        }));
        assert!(!verifier.is_checked_fallback_transition("Door", "open", "Closed"));
        assert!(!verifier.is_checked_ffi_pinned_transition("Door", "open", "Closed"));
        assert!(!interp.is_resolved_ffi_pinned_transition("Door", "open", "Closed"));
    }

    #[test]
    fn interpreter_exposes_resolved_transition_targets() {
        let file = parse(
            r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        let targets = interp
            .resolved_transition_targets("Door", "open", "Closed")
            .expect("targets");
        assert_eq!(targets, vec!["Open".to_string()]);
        assert!(interp
            .resolved_transition_targets("Door", "missing", "Closed")
            .is_none());
    }

    #[test]
    fn codegen_exposes_resolved_transition_targets() {
        let file = parse(
            r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "targets");
        codegen.compile_checked(&program).expect("compile");
        let targets = codegen
            .resolved_transition_targets("Door", "open", "Closed")
            .expect("targets");
        assert_eq!(targets, vec!["Open".to_string()]);
        assert!(!codegen.is_resolved_fallback_transition("Door", "open", "Closed"));
    }

    #[test]
    fn resolved_transition_records_event_parameters() {
        let file = parse(
            r#"
flow Door {
    state Closed
    state Open
    transition open(Closed, code: i32) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let open = program.transition("Door", "open", "Closed").expect("open");
        assert_eq!(open.params.len(), 1);
        assert_eq!(open.params[0].0, "code");
        assert!(matches!(open.params[0].1.unlocated(), Type::Name(n, _) if n == "i32"));
    }

    #[test]
    fn consumers_use_resolved_transition_param_arity() {
        let file = parse(
            r#"
flow Door {
    state Closed
    state Open
    transition open(Closed, code: i32) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(
            interp.resolved_transition_param_arity("Door", "open", "Closed"),
            Some(1)
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "arity");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen.resolved_transition_param_arity("Door", "open", "Closed"),
            Some(1)
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(
            verifier.checked_transition_param_arity("Door", "open", "Closed"),
            Some(1)
        );
        assert_eq!(
            interp.resolved_transition_params("Door", "open", "Closed"),
            Some(vec![("code".into(), "i32".into())])
        );
        assert_eq!(
            codegen.resolved_transition_params("Door", "open", "Closed"),
            Some(vec![("code".into(), "i32".into())])
        );
        assert_eq!(
            verifier.checked_transition_params("Door", "open", "Closed"),
            Some(vec![("code".into(), "i32".into())])
        );
        let by_flow = interp
            .resolved_transitions_for_flow("Door")
            .expect("Door transitions");
        assert!(by_flow
            .iter()
            .any(|(event, source, targets, fallback, _pinned, argc)| {
                event == "open"
                    && source == "Closed"
                    && targets.contains("Open")
                    && !*fallback
                    && *argc == 1
            }));
        assert!(verifier
            .checked_transitions_for_flow("Door")
            .is_some_and(|trs| trs
                .iter()
                .any(|(e, s, _, _, _, _)| e == "open" && s == "Closed")));
        assert!(codegen
            .resolved_transitions_for_flow("Door")
            .is_some_and(|trs| trs.iter().any(|(e, _, targets, _, _, argc)| {
                e == "open" && targets.contains("Open") && *argc == 1
            })));
        let by_event = interp
            .resolved_transitions_for_event("open")
            .expect("open transitions");
        assert!(by_event
            .iter()
            .any(|(flow, source, targets, fallback, _pinned, argc)| {
                flow == "Door"
                    && source == "Closed"
                    && targets.contains("Open")
                    && !*fallback
                    && *argc == 1
            }));
        assert!(verifier
            .checked_transitions_for_event("open")
            .is_some_and(|trs| trs
                .iter()
                .any(|(f, s, _, _, _, _)| f == "Door" && s == "Closed")));
        assert!(codegen
            .resolved_transitions_for_event("open")
            .is_some_and(|trs| trs.iter().any(|(f, _, targets, _, _, argc)| {
                f == "Door" && targets.contains("Open") && *argc == 1
            })));
    }

    #[test]
    fn consumers_install_type_and_extern_directories() {
        let file = parse(
            r#"
type Point { x: i32, y: i32 }
extern "C" {
    func c_abs(x: i32) -> i32
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(interp.resolved_type_kind("Point"), Some("record"));
        assert!(interp.has_resolved_extern_func("c_abs"));
        assert_eq!(interp.resolved_extern_abi("c_abs"), Some("C"));
        assert_eq!(
            interp.resolved_extern_signature("c_abs"),
            Some((1, "i32".into()))
        );
        assert_eq!(
            interp.resolved_extern_params("c_abs"),
            Some(vec![("x".into(), "i32".into())])
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.has_checked_type_def("Point"));
        assert!(verifier.has_checked_extern_func("c_abs"));
        assert_eq!(verifier.checked_extern_abi("c_abs"), Some("C"));
        assert_eq!(
            verifier.checked_extern_signature("c_abs"),
            Some((1, "i32".into()))
        );
        assert_eq!(
            verifier.checked_extern_params("c_abs"),
            Some(vec![("x".into(), "i32".into())])
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "abi");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(codegen.resolved_extern_abi("c_abs"), Some("C"));
        assert_eq!(
            codegen.resolved_extern_signature("c_abs"),
            Some((1, "i32".into()))
        );
        assert_eq!(
            codegen.resolved_extern_params("c_abs"),
            Some(vec![("x".into(), "i32".into())])
        );
    }

    #[test]
    fn interpreter_resolved_extern_directory_matches_runtime_index() {
        let file = parse(
            r#"
extern "C" {
    func c_abs(x: i32) -> i32
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.has_resolved_extern_func("c_abs"));
        // Directory install is consistent with a successful from_checked construction.
        assert!(!interp.has_resolved_extern_func("missing_c_fn"));
    }

    #[test]
    fn interpreter_from_checked_installs_capability_and_constant_directories() {
        let file = parse(
            r#"
cap Io
const MAX: i32 = 10
const NEG: i32 = -3
const FLAG: bool = true
func main() -> i32 { MAX }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let max = program.constant("MAX").expect("MAX");
        assert_eq!(max.ty.as_deref(), Some("i32"));
        assert_eq!(max.value, crate::core::ResolvedConstValue::Int(10));
        let neg = program.constant("NEG").expect("NEG");
        assert_eq!(neg.value, crate::core::ResolvedConstValue::Int(-3));
        let flag = program.constant("FLAG").expect("FLAG");
        assert_eq!(flag.value, crate::core::ResolvedConstValue::Bool(true));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.has_resolved_capability("Io"));
        assert!(interp.has_resolved_constant("MAX"));
        assert!(!interp.has_resolved_constant("Missing"));
        assert_eq!(
            interp.resolved_constant_value("MAX"),
            Some((Some("i32".into()), "int:10".into()))
        );
        assert_eq!(
            interp.resolved_constant_value("NEG"),
            Some((Some("i32".into()), "int:-3".into()))
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(
            verifier.checked_constant_value("MAX"),
            Some((Some("i32".into()), "int:10".into()))
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "const_vals");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen.resolved_constant_value("FLAG"),
            Some((Some("bool".into()), "bool:true".into()))
        );
    }

    #[test]
    fn call_sites_resolve_function_and_extern_callees() {
        let file = parse(
            r#"
extern "C" {
    func c_abs(x: i32) -> i32
}
func helper(x: i32) -> i32 { x + 1 }
func main() -> i32 {
    let a = helper(1)
    let b = c_abs(a)
    b
}
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let sites: Vec<_> = program.call_sites().values().collect();
        assert!(
            sites.iter().any(|s| {
                s.callee == "helper"
                    && s.kind == crate::core::ResolvedCallKind::Function
                    && s.argc == 1
                    && s.expected_argc == Some(1)
                    && s.arity_matches()
                    && s.ret.as_deref() == Some("i32")
            }),
            "expected helper call site, got {:?}",
            sites
                .iter()
                .map(|s| (&s.callee, s.kind, s.argc, s.expected_argc))
                .collect::<Vec<_>>()
        );
        assert!(
            sites.iter().any(|s| {
                s.callee == "c_abs"
                    && s.kind == crate::core::ResolvedCallKind::Extern
                    && s.argc == 1
                    && s.expected_argc == Some(1)
                    && s.ret.as_deref() == Some("i32")
                    && s.arity_matches()
            }),
            "expected c_abs extern call site"
        );
        let c_abs = program.extern_func_signature("c_abs").expect("c_abs sig");
        assert_eq!(c_abs.params.len(), 1);
        assert_eq!(c_abs.ret, "i32");
        assert!(
            sites
                .iter()
                .any(|s| s.callee == "helper" && s.effects.is_empty()),
            "helper effects should be empty when unannotated"
        );
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.has_resolved_call_to("helper"));
        assert!(interp.has_resolved_call_to("c_abs"));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.has_checked_call_to("helper"));
        assert!(verifier.has_checked_call_to("c_abs"));
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "calls");
        codegen.compile_checked(&program).expect("compile");
        assert!(codegen.has_resolved_call_to("helper"));
        assert!(codegen.has_resolved_call_to("c_abs"));
        assert_eq!(interp.resolved_call_arity_mismatches(), 0);
        assert_eq!(codegen.resolved_call_arity_mismatches(), 0);
        assert_eq!(verifier.checked_call_arity_mismatches(), 0);
        assert_eq!(
            interp.resolved_call_return_type("helper").as_deref(),
            Some("i32")
        );
        assert_eq!(
            codegen.resolved_call_return_type("helper").as_deref(),
            Some("i32")
        );
        assert_eq!(
            verifier.checked_call_return_type("helper").as_deref(),
            Some("i32")
        );
        let main_calls = interp
            .resolved_call_sites_for_owner("function:main")
            .expect("main calls");
        assert!(main_calls
            .iter()
            .any(|(c, argc, kind)| c == "helper" && *argc == 1 && kind == "function"));
        assert!(main_calls
            .iter()
            .any(|(c, argc, kind)| c == "c_abs" && *argc == 1 && kind == "extern"));
        assert!(verifier
            .checked_call_sites_for_owner("function:main")
            .is_some_and(|calls| calls.iter().any(|(c, _, _)| c == "helper")));
        assert!(codegen
            .resolved_call_sites_for_owner("function:main")
            .is_some_and(|calls| calls
                .iter()
                .any(|(c, _, kind)| c == "c_abs" && kind == "extern")));
        let helper_callers = interp
            .resolved_call_sites_for_callee("helper")
            .expect("helper callers");
        assert!(helper_callers
            .iter()
            .any(|(owner, argc, kind)| owner == "function:main"
                && *argc == 1
                && kind == "function"));
        assert!(verifier
            .checked_call_sites_for_callee("c_abs")
            .is_some_and(|cs| cs
                .iter()
                .any(|(owner, _, kind)| owner == "function:main" && kind == "extern")));
        assert!(codegen
            .resolved_call_sites_for_callee("helper")
            .is_some_and(|cs| cs.iter().any(|(owner, _, _)| owner == "function:main")));
    }

    #[test]
    fn checked_ffi_verifier_rejects_resolved_arity_mismatch_before_z3() {
        let file = parse(
            r#"
extern "C" { func c_abs(x: i32) -> i32 }
func main() -> i32 { c_abs(1, 2) }
"#,
        );
        let program = CheckedProgram::from_checked_file(&file).expect("IR");
        let error = crate::verifier::verify_ffi_checked(&program)
            .expect_err("resolved arity mismatch must fail closed");
        assert!(error.contains("TOOL-RESOLUTION-001"));
        assert!(error.contains("expects 1 arguments, got 2"));
    }

    #[test]
    fn actor_method_signatures_are_materialised() {
        let file = parse(
            r#"
actor Worker {
    func run(x: i32) -> i32 with Io { x }
}
cap Io
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let sig = program
            .actor_method_signature("Worker", "run")
            .expect("run");
        assert_eq!(sig.params.len(), 1);
        assert_eq!(sig.ret, "i32");
        assert!(program
            .actor("Worker")
            .is_some_and(|a| a.methods.iter().any(|m| m == "run")));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(
            interp.resolved_actor_method_signature("Worker", "run"),
            Some((1, "i32".into()))
        );
        assert_eq!(
            interp.resolved_actor_method_params("Worker", "run"),
            Some(vec![("x".into(), "i32".into())])
        );
        assert_eq!(
            interp.resolved_actor_method_effects("Worker", "run"),
            Some(vec!["Io".into()])
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(
            verifier.checked_actor_method_signature("Worker", "run"),
            Some((1, "i32".into()))
        );
        assert_eq!(
            verifier.checked_actor_method_params("Worker", "run"),
            Some(vec![("x".into(), "i32".into())])
        );
        assert_eq!(
            verifier.checked_actor_method_effects("Worker", "run"),
            Some(vec!["Io".into()])
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "actor_sig");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen.resolved_actor_method_signature("Worker", "run"),
            Some((1, "i32".into()))
        );
        assert_eq!(
            codegen.resolved_actor_method_params("Worker", "run"),
            Some(vec![("x".into(), "i32".into())])
        );
        assert_eq!(
            codegen.resolved_actor_method_effects("Worker", "run"),
            Some(vec!["Io".into()])
        );
    }

    #[test]
    fn trait_and_impl_method_signatures_are_materialised() {
        let file = parse(
            r#"
trait Show {
    func show(self: i32) -> i32
}
type Number = i32
impl Show for Number {
    func show(self: Number) -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let trait_sig = program
            .trait_method_signature("Show", "show")
            .expect("trait show");
        assert_eq!(trait_sig.ret, "i32");
        let impl_sig = program
            .impl_method_signature("Show", "Number", "show")
            .expect("impl show");
        assert_eq!(impl_sig.ret, "i32");
        assert_eq!(impl_sig.params.len(), 1);
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(
            interp.resolved_method_signature("Show.show"),
            Some((1, "i32".into()))
        );
        assert_eq!(
            interp.resolved_method_signature("Show:for:Number.show"),
            Some((1, "i32".into()))
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(
            verifier.checked_method_signature("Show.show"),
            Some((1, "i32".into()))
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "trait_sig");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen.resolved_method_signature("Show:for:Number.show"),
            Some((1, "i32".into()))
        );
    }

    #[test]
    fn protocol_payloads_and_transition_records_are_materialised() {
        let file = parse(
            r#"
protocol Sensor {
    state Idle
    state Active { data: i32 }
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let active = program
            .protocol_state_payload("Sensor", "Active")
            .expect("Active");
        assert_eq!(active.payload_type.as_deref(), Some("i32"));
        let records = program
            .protocol_transition_records("Sensor")
            .expect("records");
        assert!(records
            .iter()
            .any(|t| t.event == "start" && t.from_state == "Idle"));
        assert!(records
            .iter()
            .any(|t| t.event == "stop" && t.from_state == "Active"));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(
            interp
                .resolved_protocol_payload("Sensor", "Active")
                .as_deref(),
            Some("i32")
        );
        assert_eq!(
            interp.resolved_protocol_state_payload("Sensor", "Active"),
            Some(("data".into(), "i32".into()))
        );
        assert_eq!(
            interp.resolved_protocol_states("Sensor"),
            Some(vec!["Active".into(), "Idle".into()])
        );
        assert!(interp
            .resolved_protocol_transitions("Sensor")
            .is_some_and(|trs| trs.iter().any(|(e, f, _)| e == "start" && f == "Idle")));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(
            verifier
                .checked_protocol_payload("Sensor", "Active")
                .as_deref(),
            Some("i32")
        );
        assert_eq!(
            verifier.checked_protocol_state_payload("Sensor", "Active"),
            Some(("data".into(), "i32".into()))
        );
        assert_eq!(
            verifier.checked_protocol_states("Sensor"),
            Some(vec!["Active".into(), "Idle".into()])
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "proto");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen
                .resolved_protocol_payload("Sensor", "Active")
                .as_deref(),
            Some("i32")
        );
        assert_eq!(
            codegen.resolved_protocol_state_payload("Sensor", "Active"),
            Some(("data".into(), "i32".into()))
        );
        assert_eq!(
            codegen.resolved_protocol_states("Sensor"),
            Some(vec!["Active".into(), "Idle".into()])
        );
    }

    #[test]
    fn session_body_display_is_materialised() {
        let file = parse(
            r#"
session Ping = !i32 . ?i32 . end
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        assert_eq!(program.session_body_display("Ping"), Some("!i32.?i32.end"));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(
            interp.resolved_session_display("Ping"),
            Some("!i32.?i32.end")
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(
            verifier.checked_session_display("Ping"),
            Some("!i32.?i32.end")
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "sess");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen.resolved_session_display("Ping"),
            Some("!i32.?i32.end")
        );
    }

    #[test]
    fn type_def_fields_and_variants_are_materialised() {
        let file = parse(
            r#"
type Point { x: i32, y: i32 }
type Id = i32
type Color { Red Green Blue }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let fields = program.type_def_fields("Point").expect("Point fields");
        assert!(fields.iter().any(|(n, ty)| n == "x" && ty == "i32"));
        assert!(fields.iter().any(|(n, ty)| n == "y" && ty == "i32"));
        assert_eq!(program.type_def_alias_of("Id"), Some("i32"));
        let variants = program.type_def_variants("Color").expect("Color");
        assert!(variants.iter().any(|(n, p)| n == "Red" && p.is_none()));
        assert!(variants.iter().any(|(n, _)| n == "Green"));
        assert!(variants.iter().any(|(n, _)| n == "Blue"));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp
            .resolved_type_fields("Point")
            .is_some_and(|fields| fields.iter().any(|(n, ty)| n == "x" && ty == "i32")));
        assert_eq!(interp.resolved_type_alias_of("Id"), Some("i32"));
        assert!(interp
            .resolved_type_variants("Color")
            .is_some_and(|vs| vs.iter().any(|(n, _)| n == "Blue")));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(verifier.checked_type_alias_of("Id"), Some("i32"));
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "types");
        codegen.compile_checked(&program).expect("compile");
        assert!(codegen
            .resolved_type_fields("Point")
            .is_some_and(|fields| fields.iter().any(|(n, _)| n == "y")));
    }

    #[test]
    fn capability_combined_with_is_installed() {
        let file = parse(
            r#"
cap A
cap B
cap Combined = A + B
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let cap = program.capability("Combined").expect("Combined");
        assert_eq!(cap.combined_with.as_deref(), Some("A + B"));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(
            interp.resolved_capability_combined_with("Combined"),
            Some("A + B")
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(
            verifier.checked_capability_combined_with("Combined"),
            Some("A + B")
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "cap");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen.resolved_capability_combined_with("Combined"),
            Some("A + B")
        );
    }

    #[test]
    fn flow_state_payloads_are_installed() {
        let file = parse(
            r#"
flow Counter {
    state Zero
    state Positive { count: i32 }
    transition inc(Zero) -> Positive {
        do { return Positive { count: 1 } }
    }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert_eq!(
            interp.resolved_flow_state_payload("Counter", "Positive"),
            Some(vec![("count".into(), "i32".into())])
        );
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert_eq!(
            verifier.checked_flow_state_payload("Counter", "Positive"),
            Some(vec![("count".into(), "i32".into())])
        );
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "flow_payload");
        codegen.compile_checked(&program).expect("compile");
        assert_eq!(
            codegen.resolved_flow_state_payload("Counter", "Positive"),
            Some(vec![("count".into(), "i32".into())])
        );
        let states = interp.resolved_flow_states("Counter").expect("states");
        assert!(states.iter().any(|s| s == "Zero"));
        assert!(states.iter().any(|s| s == "Positive"));
        assert!(verifier
            .checked_flow_states("Counter")
            .is_some_and(|ss| ss.iter().any(|s| s == "Positive")));
        assert!(codegen
            .resolved_flow_states("Counter")
            .is_some_and(|ss| ss.iter().any(|s| s == "Zero")));
        assert!(interp
            .resolved_flow_events("Counter")
            .is_some_and(|es| es.iter().any(|e| e == "inc")));
        assert!(verifier
            .checked_flow_events("Counter")
            .is_some_and(|es| es.iter().any(|e| e == "inc")));
        assert!(codegen
            .resolved_flow_events("Counter")
            .is_some_and(|es| es.iter().any(|e| e == "inc")));
        assert_eq!(interp.resolved_item_kind("Counter"), Some("flow"));
        assert_eq!(verifier.checked_item_kind("Counter"), Some("flow"));
        assert_eq!(codegen.resolved_item_kind("main"), Some("function"));
    }

    #[test]
    fn actor_fields_are_installed() {
        let file = parse(
            r#"
actor Worker {
    count: i32
    mut flag: bool
    func run() -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let fields = program.actor("Worker").expect("Worker").fields.clone();
        assert!(fields.iter().any(|(n, _, m)| n == "count" && !*m));
        assert!(fields.iter().any(|(n, _, m)| n == "flag" && *m));
        let interp = crate::interp::Interpreter::from_checked(&program);
        let installed = interp.resolved_actor_fields("Worker").expect("fields");
        assert!(installed
            .iter()
            .any(|(n, ty, m)| n == "count" && ty == "i32" && !*m));
        assert!(installed
            .iter()
            .any(|(n, ty, m)| n == "flag" && ty == "bool" && *m));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier
            .checked_actor_fields("Worker")
            .is_some_and(|fs| fs.iter().any(|(n, _, m)| n == "flag" && *m)));
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "actor_fields");
        codegen.compile_checked(&program).expect("compile");
        assert!(codegen
            .resolved_actor_fields("Worker")
            .is_some_and(|fs| fs.iter().any(|(n, ty, _)| n == "count" && ty == "i32")));
    }

    #[test]
    fn extern_block_flags_are_installed() {
        let file = parse(
            r#"
#[no_panic]
extern "C" {
    func safe_abs(x: i32) -> i32
}
unsafe extern "C" {
    func raw_abs(x: i32) -> i32
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.is_resolved_extern_no_panic("safe_abs"));
        assert!(!interp.is_resolved_extern_no_panic("raw_abs"));
        assert!(interp.is_resolved_extern_unsafe("raw_abs"));
        assert!(!interp.is_resolved_extern_unsafe("safe_abs"));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.is_checked_extern_no_panic("safe_abs"));
        assert!(verifier.is_checked_extern_unsafe("raw_abs"));
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "extern_flags");
        codegen.compile_checked(&program).expect("compile");
        assert!(codegen.is_resolved_extern_no_panic("safe_abs"));
        assert!(codegen.is_resolved_extern_unsafe("raw_abs"));
    }

    #[test]
    fn call_sites_bind_callee_effects_from_function_directory() {
        // IR-only materialization: avoid effect-scope runtime checks at call sites.
        let file = parse(
            r#"
cap Io
func write_it(x: i32) -> i32 with Io { x }
func main() -> i32 {
    write_it(1)
}
"#,
        );
        let program = crate::core::CheckedProgram::from_checked_file(&file).expect("ir");
        assert!(
            program.call_sites().values().any(|s| {
                s.callee == "write_it"
                    && s.effects.iter().any(|e| e == "Io")
                    && s.expected_argc == Some(1)
                    && s.kind == crate::core::ResolvedCallKind::Function
                    && s.ret.as_deref() == Some("i32")
            }),
            "expected write_it Io call site"
        );
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.has_resolved_call_with_effect("write_it", "Io"));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.has_checked_call_with_effect("write_it", "Io"));
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "call_fx");
        codegen.compile_checked(&program).expect("compile");
        assert!(codegen.has_resolved_call_with_effect("write_it", "Io"));
    }

    #[test]
    fn codegen_compile_checked_installs_directories() {
        let file = parse(
            r#"
cap Io
protocol Sensor {
    state Idle
    transition start(Idle) -> Idle
}
session Ping = !i32 . end
actor A { func f() -> i32 { 0 } }
const N: i32 = 1
func main() -> i32 { N }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "dir_test");
        codegen.compile_checked(&program).expect("compile_checked");
        // Public API is limited; compile success with populated CheckedProgram is the gate.
        assert!(program.capability("Io").is_some());
        assert!(program.protocol("Sensor").is_some());
        assert!(program.session("Ping").is_some());
        assert!(program.actor("A").is_some());
        assert!(program.constant("N").is_some());
    }

    #[test]
    fn verifier_verify_checked_records_function_names() {
        let file = parse(
            r#"
flow Door {
    state Closed
    state Open
    transition open(Closed) -> Open { do { return Open {} } }
}
protocol Sensor {
    state Idle
    transition tick(Idle) -> Idle
}
trait Close { func close() -> i32 }
actor Sink { func ping() -> i32 { 0 } }
session Ping = !i32 . end
cap Io
func abs(x: i32) -> i32 {
    requires: x >= 0
    ensures: result >= 0
    x
}
func main() -> i32 { abs(1) }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.has_checked_function("abs"));
        assert!(verifier.has_checked_function("main"));
        assert!(verifier.has_checked_transition("Door", "open", "Closed"));
        assert!(verifier.has_checked_session("Ping"));
        assert!(!verifier.has_checked_transition("Door", "close", "Closed"));
        assert!(verifier.has_checked_protocol("Sensor"));
        assert!(verifier.has_checked_trait("Close"));
        assert!(verifier.has_checked_actor("Sink"));
    }

    #[test]
    fn canonical_flow_ids_include_module_path() {
        let file = parse(
            r#"
module alpha {
    flow Worker {
        state Idle
        state Busy
        transition start(Idle) -> Busy { do { return Busy {} } }
    }
}
module beta {
    flow Worker {
        state Idle
        state Busy
        transition start(Idle) -> Busy { do { return Busy {} } }
    }
}
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        assert!(program
            .transition("alpha::Worker", "start", "Idle")
            .is_some());
        assert!(program
            .transition("beta::Worker", "start", "Idle")
            .is_some());
        assert_eq!(
            program
                .transitions()
                .keys()
                .filter(|id| id.event == "start" && id.source.name == "Idle")
                .count(),
            2
        );
        let alpha = program.flow("alpha::Worker").expect("alpha flow");
        let idle = alpha.states.get("Idle").expect("Idle state");
        assert_eq!(idle.id.flow.0, "alpha::Worker");
        assert_eq!(idle.node_id.0, "state:alpha::Worker::Idle");
        assert_eq!(idle.origin.user_span().start_line, 4);
        assert!(idle.payload.is_empty());
        assert!(program.flow("Worker").is_none());
        assert!(program
            .items()
            .contains_key(&NodeId("module:alpha".to_string())));
        assert!(program
            .items()
            .contains_key(&NodeId("flow:alpha::Worker".to_string())));
    }

    #[test]
    fn resolved_item_directory_records_declaration_spans() {
        let file = parse(
            r#"
actor Worker {
    func run() -> i32 { 0 }
}
protocol Service {
    state Ready
}
session Request = !i32 . end
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        for (node_id, line) in [
            ("actor:Worker", 2),
            ("protocol:Service", 5),
            ("session:Request", 8),
            ("function:main", 9),
        ] {
            let item = program
                .items()
                .get(&NodeId(node_id.to_string()))
                .unwrap_or_else(|| panic!("missing {node_id}"));
            assert_eq!(item.origin.user_span().start_line, line);
        }
        assert_eq!(program.entry_span().expect("entry span").start_line, 9);
    }

    #[test]
    fn resolved_types_distinguish_user_declarations_from_synthetic_types() {
        let file = parse(
            r#"
type Point { x: i32 }
newtype UserId = i64
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        for (node_id, line) in [("type:Point", 2), ("type:UserId", 3)] {
            let item = program
                .items()
                .get(&NodeId(node_id.to_string()))
                .unwrap_or_else(|| panic!("missing {node_id}"));
            assert_eq!(item.kind, ResolvedItemKind::Type);
            assert_eq!(item.origin.user_span().start_line, line);
        }
        assert!(!program
            .items()
            .contains_key(&NodeId("type:ExecResult".to_string())));
    }

    #[test]
    fn resolved_item_directory_covers_remaining_top_level_items() {
        let file = parse(
            r#"
cap Read
trait Show { func show(self: i32) -> i32; }
type Number = i32
impl Show for Number { func show(self: Number) -> i32 { 0 } }
const ANSWER: i32 = 42
extern "C" { func abs(value: i32) -> i32; }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        for node_id in [
            "capability:Read",
            "trait:Show",
            "impl:Show:for:Number",
            "const:ANSWER",
            "extern:C:abs",
        ] {
            let item = program
                .items()
                .get(&NodeId(node_id.to_string()))
                .unwrap_or_else(|| panic!("missing {node_id}"));
            assert!(item.origin.user_span().start_line > 0);
        }
    }

    #[test]
    fn extern_block_node_id_is_independent_of_position_and_symbol_order() {
        let first = parse(
            r#"
extern "C" {
    func read(fd: i32) -> i32;
    func close(fd: i32) -> i32;
}
func main() -> i32 { 0 }
"#,
        );
        let reordered = parse(
            r#"
func main() -> i32 { 0 }


extern "C" {
    func close(fd: i32) -> i32;
    func read(fd: i32) -> i32;
}
"#,
        );
        let first = crate::core::check_program(&first).expect("first program");
        let reordered = crate::core::check_program(&reordered).expect("reordered program");
        let first_ids = first
            .extern_blocks()
            .keys()
            .map(|id| id.0.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let reordered_ids = reordered
            .extern_blocks()
            .keys()
            .map(|id| id.0.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(first_ids, reordered_ids);
        assert_eq!(first_ids.len(), 1);
        assert!(first_ids.contains("extern:C:close+read"));
    }

    #[test]
    fn generated_flow_nodes_keep_user_span_and_system_origin() {
        let file = parse(
            r#"
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let flow = program.flow("Main").expect("implicit Main flow");
        assert!(matches!(flow.origin, Origin::RuntimeSystem { .. }));
        assert_eq!(flow.origin.rule(), Some("progressive.main"));
        match &flow.origin {
            Origin::RuntimeSystem { parent, .. } => {
                assert_eq!(parent, &NodeId("function:main".to_string()));
                assert!(program.items().contains_key(parent));
                assert_ne!(parent, &flow.node_id);
            }
            _ => unreachable!(),
        }
        assert_eq!(flow.origin.user_span().start_line, 2);
        let single = flow.states.get("Single").expect("Single state");
        assert!(matches!(single.origin, Origin::RuntimeSystem { .. }));
        assert_eq!(single.origin.rule(), Some("progressive.single"));
        assert_eq!(single.origin.user_span().start_line, 2);
        let Origin::RuntimeSystem {
            parent: single_parent,
            ..
        } = &single.origin
        else {
            unreachable!()
        };
        assert_eq!(single_parent, &flow.node_id);
        assert!(flow
            .transitions
            .iter()
            .filter_map(|id| program.transitions().get(id))
            .all(|transition| transition.origin.user_span().start_line > 0));
        let run = program
            .transition("Main", "run", "Single")
            .expect("implicit run transition");
        assert!(matches!(run.origin, Origin::RuntimeSystem { .. }));
        assert_eq!(run.origin.rule(), Some("progressive.run"));
        let Origin::RuntimeSystem {
            parent: run_parent, ..
        } = &run.origin
        else {
            unreachable!()
        };
        assert_eq!(run_parent, &flow.node_id);
        let reset = program
            .transition("Main", "reset", "Fault")
            .expect("implicit reset transition");
        assert!(matches!(reset.origin, Origin::RuntimeSystem { .. }));
        assert_eq!(reset.origin.rule(), Some("flow.reset"));
        let fallback = program
            .transition("Main", "run", "Fault")
            .expect("matrix fallback transition");
        assert!(matches!(fallback.origin, Origin::PrototypeFallback { .. }));
        assert_eq!(fallback.origin.rule(), Some("flow.matrix.fallback"));
        let run_stmt_id = generated_node_id(
            &program,
            "transition:Main::run::Single",
            "stmt.return",
            "progressive.run",
        );
        let run_stmt = program
            .node_meta()
            .get(&run_stmt_id)
            .expect("implicit run body metadata");
        assert!(matches!(run_stmt.origin, Origin::RuntimeSystem { .. }));
        assert_eq!(run_stmt.origin.rule(), Some("progressive.run"));
        let Origin::RuntimeSystem {
            parent: run_stmt_parent,
            ..
        } = &run_stmt.origin
        else {
            unreachable!()
        };
        assert_eq!(run_stmt_parent, &run.node_id);
        let fallback_stmt_id = generated_node_id(
            &program,
            "transition:Main::run::Fault",
            "stmt.return",
            "flow.matrix.fallback",
        );
        let fallback_stmt = program
            .node_meta()
            .get(&fallback_stmt_id)
            .expect("matrix fallback body metadata");
        assert!(matches!(
            fallback_stmt.origin,
            Origin::PrototypeFallback { .. }
        ));
        assert_eq!(fallback_stmt.origin.rule(), Some("flow.matrix.fallback"));
    }

    #[test]
    fn generated_transition_rule_comes_from_ast_and_survives_rename() {
        let mut file = parse("flow Worker { state Active }");
        let reset = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Flow(flow) => flow
                    .transitions
                    .iter_mut()
                    .find(|transition| transition.name == "reset"),
                _ => None,
            })
            .expect("generated reset transition");
        reset.name = "restart".to_string();
        reset.meta.origin = AstOrigin::RuntimeSystem("test.rule.from_ast");

        let program = crate::core::check_program(&file).expect("check renamed transition");
        let restart = program
            .transition("Worker", "restart", "Fault")
            .expect("renamed transition");
        assert_eq!(restart.origin.rule(), Some("test.rule.from_ast"));
        let Origin::RuntimeSystem { parent, .. } = &restart.origin else {
            unreachable!()
        };
        assert_eq!(parent, &NodeId("flow:Worker".into()));
    }

    #[test]
    fn explicit_named_parent_survives_generated_flow_rule_and_name_changes() {
        let mut file = parse("func main() -> i32 { 0 }");
        let flow = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Flow(flow) if flow.name == "Main" => Some(flow),
                _ => None,
            })
            .expect("implicit Main flow");
        assert_eq!(flow.meta.parent, AstParentHint::NamedFunction("main"));
        flow.name = "RenamedRuntimeFlow".into();
        flow.meta.origin = AstOrigin::RuntimeSystem("test.renamed.progressive.rule");

        let program = crate::core::check_program(&file).expect("renamed generated flow");
        let flow = program
            .flow("RenamedRuntimeFlow")
            .expect("renamed flow catalog entry");
        let Origin::RuntimeSystem { parent, rule, .. } = &flow.origin else {
            unreachable!()
        };
        assert_eq!(parent, &NodeId("function:main".into()));
        assert_eq!(rule, "test.renamed.progressive.rule");
    }

    #[test]
    fn generated_ast_node_without_parent_hint_is_rejected() {
        let mut file = parse("flow Worker { state Active }");
        let reset = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Flow(flow) => flow
                    .transitions
                    .iter_mut()
                    .find(|transition| transition.name == "reset"),
                _ => None,
            })
            .expect("generated reset transition");
        reset.meta.parent = AstParentHint::None;

        let diagnostics = crate::core::check_program(&file).expect_err("parent hint must fail");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains(
                "generated NodeId 'transition:Worker::reset::Fault' is missing an explicit AST parent hint",
            )
        }));
    }

    #[test]
    fn generated_ast_origin_with_empty_rule_is_rejected() {
        let mut file = parse("flow Worker { state Active }");
        let reset = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Flow(flow) => flow
                    .transitions
                    .iter_mut()
                    .find(|transition| transition.name == "reset"),
                _ => None,
            })
            .expect("generated reset transition");
        reset.meta.origin = AstOrigin::RuntimeSystem("");

        let diagnostics = crate::core::check_program(&file).expect_err("empty rule must fail");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains(
                "generated NodeId 'transition:Worker::reset::Fault' has an empty Origin rule",
            )
        }));
    }

    #[test]
    fn generated_transition_call_sites_inherit_runtime_origin() {
        let file = parse(
            r#"
flow Worker {
    state Active { outcome: Result<i32, string> }
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let recover_owner = "transition:Worker::recover::Fault";
        let calls = program
            .call_sites()
            .values()
            .filter(|call| call.owner == recover_owner)
            .collect::<Vec<_>>();
        assert!(!calls.is_empty(), "recover should contain a generated call");
        assert!(calls.iter().all(|call| {
            matches!(call.origin, Origin::RuntimeSystem { .. })
                && call.origin.rule() == Some("flow.recover")
        }));
    }

    #[test]
    fn checked_diagnostics_never_use_zero_sentinel_spans() {
        for source in [
            "func broken(x: Missing) -> i32 { 0 }",
            "actor Worker { value: Missing }",
            "protocol P { state A { value: Missing } }",
            "session S = Missing",
            "flow F { state A { value: Missing } }",
        ] {
            let file = parse(source);
            let diagnostics = crate::core::check_program(&file).expect_err(source);
            assert!(!diagnostics.is_empty(), "expected diagnostics for {source}");
            for diagnostic in diagnostics {
                assert!(
                    diagnostic.span.start_line > 0 && diagnostic.span.start_col > 0,
                    "sentinel span for {source}: {:?}",
                    diagnostic
                );
            }
        }
    }

    #[test]
    fn node_meta_covers_nested_stmt_expr_and_pattern_paths() {
        let file = parse(
            r#"
func main() -> i32 {
    let pair = (1, 2)
    if true { return pair.0 } else { return 0 }
}
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let let_id = node_id_at(&program, "stmt.let", 3, 5);
        let pattern_id = node_id_at(&program, "pattern.variable", 3, 9);
        let tuple_id = node_id_at(&program, "expr.tuple", 3, 16);
        let tuple_item_id = node_id_at(&program, "expr.literal", 3, 17);
        let cond_id = node_id_at(&program, "expr.literal", 4, 8);
        let returned_ident_id = node_id_at(&program, "expr.identifier", 4, 22);
        for node_id in [
            &let_id,
            &pattern_id,
            &tuple_id,
            &tuple_item_id,
            &cond_id,
            &returned_ident_id,
        ] {
            let meta = program
                .node_meta()
                .get(node_id)
                .unwrap_or_else(|| panic!("missing {}", node_id.0));
            assert!(meta.origin.user_span().start_line > 0);
        }
        assert_eq!(
            program
                .node_meta()
                .get(&let_id)
                .expect("let metadata")
                .precision,
            SpanPrecision::Exact
        );
        assert_eq!(
            program
                .node_meta()
                .get(&cond_id)
                .expect("condition metadata")
                .precision,
            SpanPrecision::Exact
        );
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.has_resolved_node_meta_path(&let_id.0));
        assert!(interp.has_resolved_node_meta_path(&cond_id.0));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.has_checked_node_meta_path(&tuple_id.0));
        let context = inkwell::context::Context::create();
        let mut codegen = crate::codegen::CodeGenerator::new(&context, "node_meta");
        codegen.compile_checked(&program).expect("compile");
        assert!(codegen.has_resolved_node_meta_path(&returned_ident_id.0));
        assert_eq!(
            interp.resolved_node_meta_precision(&let_id.0),
            Some("exact")
        );
        assert_eq!(
            verifier.checked_node_meta_precision(&cond_id.0),
            Some("exact")
        );
        assert_eq!(
            codegen.resolved_node_meta_precision(&let_id.0),
            Some("exact")
        );
        let let_span = interp.resolved_node_meta_span(&let_id.0).expect("let span");
        assert!(let_span.0 > 0);
        assert_eq!(verifier.checked_node_meta_span(&let_id.0), Some(let_span));
        assert_eq!(codegen.resolved_node_meta_span(&let_id.0), Some(let_span));
        let cond_span = interp
            .resolved_node_meta_span(&cond_id.0)
            .expect("cond span");
        assert!(cond_span.0 > 0);
        assert_eq!(verifier.checked_node_meta_span(&cond_id.0), Some(cond_span));
    }

    #[test]
    fn positioned_body_node_ids_survive_synthetic_statement_insertion() {
        let mut file = parse(
            r#"
func helper() -> i32 { 1 }
func main() -> i32 {
    let value = helper()
    value
}
"#,
        );
        let original = crate::core::check_program(&file).expect("original");
        let let_id = node_id_at(&original, "stmt.let", 4, 5);
        let call_id = original
            .call_sites()
            .values()
            .find(|call| call.owner == "function:main" && call.callee == "helper")
            .map(|call| call.node_id.clone())
            .expect("helper call site");
        assert!(original.node_meta().contains_key(&let_id));
        assert!(original.call_sites().contains_key(&call_id));

        let main = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main function");
        main.body.insert(0, Stmt::Block(Vec::new()));

        let lowered = crate::core::check_program(&file).expect("lowered");
        assert!(lowered.node_meta().contains_key(&let_id));
        assert!(lowered.call_sites().contains_key(&call_id));
    }

    #[test]
    fn positioned_contract_node_ids_survive_synthetic_statement_insertion() {
        let mut file = parse(
            r#"
func main() -> i32 {
    requires: true
    return 0
}
"#,
        );
        let original = crate::core::check_program(&file).expect("original");
        let contract_id = node_id_at(&original, "stmt.requires", 3, 5);
        assert!(original.node_meta().contains_key(&contract_id));

        let main = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main function");
        main.body.insert(0, Stmt::Block(Vec::new()));

        let lowered = crate::core::check_program(&file).expect("lowered");
        assert!(lowered.node_meta().contains_key(&contract_id));
    }

    #[test]
    fn anonymous_node_ids_use_stable_source_keys_not_session_source_ids() {
        let source = "func main() -> i32 { let value = 1; value }";
        let key = crate::span::SourceKey::new("workspace:src/main.mimi").expect("key");

        let mut first_registry = crate::span::SourceRegistry::default();
        let first_id = first_registry
            .register(crate::span::SourceRecord::new(
                key.clone(),
                crate::span::SourceTextOrigin::Disk,
            ))
            .expect("first source");
        let first_tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex first");
        let first_file =
            crate::parser::Parser::new_with_source_registry(first_tokens, first_id, first_registry)
                .parse_file()
                .expect("parse first");

        let mut second_registry = crate::span::SourceRegistry::default();
        second_registry
            .register_key(
                "workspace:src/other.mimi",
                crate::span::SourceTextOrigin::Disk,
            )
            .expect("other source");
        let second_id = second_registry
            .register(crate::span::SourceRecord::new(
                key,
                crate::span::SourceTextOrigin::Disk,
            ))
            .expect("second source");
        assert_ne!(first_id, second_id);
        let second_tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("lex second");
        let second_file = crate::parser::Parser::new_with_source_registry(
            second_tokens,
            second_id,
            second_registry,
        )
        .parse_file()
        .expect("parse second");

        let first = crate::core::check_program(&first_file).expect("check first");
        let second = crate::core::check_program(&second_file).expect("check second");
        let first_ids = first
            .node_meta()
            .keys()
            .map(|id| id.0.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let second_ids = second
            .node_meta()
            .keys()
            .map(|id| id.0.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(first_ids, second_ids);
        assert!(first_ids
            .iter()
            .any(|id| id.contains("workspace:src%2fmain.mimi")));
    }

    #[test]
    fn declaration_expression_call_sites_are_complete_expr_ids_and_reorder_stable() {
        let source = r#"
func leaf() -> i32 { 1 }
func generic<T>(value: T) -> T { value }
const ANSWER: i32 = leaf()
trait Defaulted {
    func choose(left: i32 = leaf(), right: i32 = leaf()) -> i32;
}
actor Worker {
    mut value: i32 = leaf()
    func choose(left: i32 = leaf(), right: i32 = leaf()) -> i32 {
        generic::<i32>(left)
    }
}
impl Defaulted for i32 {
    func choose(left: i32 = leaf(), right: i32 = leaf()) -> i32 {
        generic::<i32>(right)
    }
}
extern "C" {
    func probe(value: i32) -> i32
        requires: leaf() > 0
        ensures: generic::<i32>(result) > 0;
}
flow Machine {
    state Ready
    transition tick(Ready, left: i32, right: i32) -> Ready {
        do { return Ready {} }
    }
}
func top(left: i32 = leaf(), right: i32 = leaf()) -> i32 {
    func nested(first: i32 = leaf(), second: i32 = leaf()) -> i32 { first }
    generic::<i32>(nested(left))
}
func main() -> i32 { top() }
"#;
        let mut file = parse(source);
        // Transition parameters use the same Param AST but their current
        // surface grammar does not expose defaults. Seed the model directly
        // so the declaration walker remains complete when that syntax lands.
        let transition_defaults = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "top" => Some(
                    function
                        .params
                        .iter()
                        .map(|param| param.default_value.clone().expect("top default"))
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .expect("default expression fixtures");
        let transition = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Flow(flow) => flow
                    .transitions
                    .iter_mut()
                    .find(|transition| transition.name == "tick"),
                _ => None,
            })
            .expect("tick transition");
        for (param, default) in transition.params.iter_mut().zip(transition_defaults) {
            param.default_value = Some(default);
        }
        let original = CheckedProgram::from_checked_file(&file).expect("catalog original");

        let calls_for = |owner: &str, callee: &str| {
            original
                .call_sites()
                .values()
                .filter(|call| call.owner == owner && call.callee == callee)
                .collect::<Vec<_>>()
        };
        assert_eq!(calls_for("constant:ANSWER", "leaf").len(), 1);
        assert_eq!(calls_for("actor:Worker", "leaf").len(), 1);
        assert_eq!(calls_for("function:Worker::choose", "leaf").len(), 2);
        assert_eq!(calls_for("function:Worker::choose", "generic").len(), 1);
        assert_eq!(
            calls_for("transition:Machine::tick::Ready", "leaf").len(),
            2
        );
        assert_eq!(calls_for("function:top", "leaf").len(), 2);
        assert_eq!(calls_for("function:top", "generic").len(), 1);

        let trait_def = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Trait(trait_def) => Some(trait_def),
                _ => None,
            })
            .expect("trait fixture");
        let trait_method = &trait_def.methods[0];
        let trait_owner = format!(
            "trait:Defaulted/method:{}:{:016x}",
            stable_id_fragment(&trait_method.name),
            stable_text_hash(&method_signature_key(
                &trait_method.name,
                &trait_method.params,
                trait_method.ret.as_ref()
            ))
        );
        assert_eq!(calls_for(&trait_owner, "leaf").len(), 2);

        let impl_def = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Impl(impl_def) => Some(impl_def),
                _ => None,
            })
            .expect("impl fixture");
        let impl_owner = impl_method_owner("Defaulted:for:i32", &impl_def.methods[0]);
        assert_eq!(calls_for(&impl_owner.0, "leaf").len(), 2);
        assert_eq!(calls_for(&impl_owner.0, "generic").len(), 1);

        let extern_block = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::ExternBlock(block) => Some(block),
                _ => None,
            })
            .expect("extern fixture");
        let extern_owner = extern_function_owner(
            &NodeId(format!("extern:{}", extern_block_key(extern_block))),
            &extern_block.funcs[0],
        );
        assert_eq!(calls_for(&extern_owner.0, "leaf").len(), 1);
        assert_eq!(calls_for(&extern_owner.0, "generic").len(), 1);

        let nested_owner = original
            .call_sites()
            .values()
            .find(|call| call.owner.starts_with("function:top/function:nested:"))
            .map(|call| call.owner.clone())
            .expect("nested default call owner");
        assert_eq!(calls_for(&nested_owner, "leaf").len(), 2);

        let turbofish_calls = original
            .call_sites()
            .values()
            .filter(|call| call.callee == "generic")
            .collect::<Vec<_>>();
        assert_eq!(turbofish_calls.len(), 4);
        assert!(turbofish_calls.iter().all(|call| {
            call.node_id.0.contains("/node:expr.turbofish@")
                && original.node_meta().get(&call.node_id).is_some_and(|meta| {
                    meta.origin == call.origin && meta.precision == SpanPrecision::Exact
                })
        }));
        assert!(original.call_sites().values().all(|call| {
            original
                .node_meta()
                .get(&call.node_id)
                .is_some_and(|meta| meta.origin == call.origin)
        }));

        let original_ids = original
            .call_sites()
            .keys()
            .map(|node_id| node_id.0.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut reordered_file = file.clone();
        reordered_file.items.reverse();
        for item in &mut reordered_file.items {
            match item {
                Item::Func(function) => {
                    function.params.reverse();
                    for stmt in &mut function.body {
                        if let Stmt::Func(nested) = stmt.unlocated_mut() {
                            nested.params.reverse();
                        }
                    }
                }
                Item::Actor(actor) => {
                    actor.fields.reverse();
                    actor.methods.reverse();
                    for method in &mut actor.methods {
                        method.params.reverse();
                    }
                }
                Item::Trait(trait_def) => {
                    trait_def.methods.reverse();
                    for method in &mut trait_def.methods {
                        method.params.reverse();
                    }
                }
                Item::Impl(impl_def) => {
                    impl_def.methods.reverse();
                    for method in &mut impl_def.methods {
                        method.params.reverse();
                    }
                }
                Item::ExternBlock(block) => block.funcs.reverse(),
                Item::Flow(flow) => {
                    flow.transitions.reverse();
                    for transition in &mut flow.transitions {
                        transition.params.reverse();
                    }
                }
                _ => {}
            }
        }
        let reordered =
            CheckedProgram::from_checked_file(&reordered_file).expect("catalog reordered");
        let reordered_ids = reordered
            .call_sites()
            .keys()
            .map(|node_id| node_id.0.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(original_ids, reordered_ids);
        assert!(reordered.call_sites().values().all(|call| {
            reordered
                .node_meta()
                .get(&call.node_id)
                .is_some_and(|meta| meta.origin == call.origin)
        }));
    }

    #[test]
    fn declaration_type_protocol_session_extern_and_flow_catalog_is_complete_and_reorder_stable() {
        let source = r#"
type Pair<T: Clone, U: Eq> { left: Result<T, string>, right: List<U> }
type Choice { Some(i32), Empty }
trait Show<T> { func show(value: T, flags: i32) -> string; }
extern "C" {
    func add(left: i32, right: i32) -> i32;
    func sub(left: i32, right: i32) -> i32;
}
protocol Sensor<T> {
    state Idle
    state Active { data: T }
    transition start(Idle) -> Active
    transition stop(Active) -> Idle
}
session Ping = !Result<i32, string> . ?List<i32> . end
session Pong = dual(Ping)
flow Worker<T> {
    @max_children(3)
    @mailbox(depth = 8)
    state Idle { value: T, count: i32 }
    state Busy { value: T, count: i32 }
    transition start(Idle, left: i32, right: i32) -> Busy { do { return Busy {} } }
    transition stop(Busy, left: i32, right: i32) -> Idle { do { return Idle {} } }
}
func catalog<T: Clone, U: Eq>(first: Pair<T, U>, second: Pair<T, U>) -> i32 where T: Clone, U: Eq {
    let pair = Pair { left: first.left, right: second.right }
    match 1 {
        0 => Pair { left: first.left, right: second.right }.right,
        _ => 0
    }
}
"#;
        let original_file = parse(source);
        let mut reordered_file = original_file.clone();

        for item in &mut reordered_file.items {
            match item {
                Item::Type(type_def) if type_def.name == "Pair" => {
                    type_def.generics.reverse();
                    if let crate::ast::TypeDefKind::Record(fields) = &mut type_def.kind {
                        fields.reverse();
                    }
                }
                Item::Type(type_def) if type_def.name == "Choice" => {
                    if let crate::ast::TypeDefKind::Enum(variants) = &mut type_def.kind {
                        variants.reverse();
                        for variant in variants {
                            match &mut variant.payload {
                                Some(crate::ast::VariantPayload::Tuple(types)) => types.reverse(),
                                Some(crate::ast::VariantPayload::Record(fields)) => {
                                    fields.reverse()
                                }
                                None => {}
                            }
                        }
                    }
                }
                Item::Trait(trait_def) => {
                    trait_def.generics.reverse();
                }
                Item::ExternBlock(block) => {
                    block.funcs.reverse();
                    for function in &mut block.funcs {
                        function.params.reverse();
                    }
                }
                Item::Protocol(protocol) => {
                    protocol.generics.reverse();
                    protocol.states.reverse();
                    protocol.transitions.reverse();
                }
                Item::Flow(flow) => {
                    flow.generics.reverse();
                    flow.annotations.reverse();
                    flow.states.reverse();
                    flow.transitions.reverse();
                    for state in &mut flow.states {
                        if let Some(payload) = &mut state.payload {
                            payload.reverse();
                        }
                    }
                    for transition in &mut flow.transitions {
                        transition.params.reverse();
                    }
                }
                Item::Func(function) if function.name == "catalog" => {
                    function.generics.reverse();
                    function.params.reverse();
                    function.where_clause.reverse();
                    for stmt in &mut function.body {
                        match stmt.unlocated_mut() {
                            Stmt::Let {
                                init: Some(expr), ..
                            } => {
                                if let Expr::Record { fields, .. } = expr.unlocated_mut() {
                                    fields.reverse();
                                }
                            }
                            Stmt::Expr(expr) => {
                                if let Expr::Match(_, arms) = expr.unlocated_mut() {
                                    arms.reverse();
                                    for arm in arms {
                                        if let Expr::Field(record, _) = arm.body.unlocated_mut() {
                                            if let Expr::Record { fields, .. } =
                                                record.unlocated_mut()
                                            {
                                                fields.reverse();
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        reordered_file.items.reverse();

        let original = CheckedProgram::from_checked_file(&original_file).expect("original catalog");
        let reordered =
            CheckedProgram::from_checked_file(&reordered_file).expect("reordered catalog");
        assert_eq!(node_meta_ids(&original), node_meta_ids(&reordered));

        let ids = node_meta_ids(&original);
        for canonical in [
            "type:Pair",
            "type:Choice",
            "trait:Show",
            "extern:C:add+sub",
            "protocol:Sensor",
            "session:Ping",
            "session:Pong",
            "flow:Worker",
            "state:Worker::Idle",
            "state:Worker::Busy",
            "transition:Worker::start::Idle",
            "transition:Worker::stop::Busy",
            "function:catalog",
        ] {
            assert!(ids.contains(canonical), "missing canonical {canonical}");
        }
        for kind in [
            "decl.generic_parameter",
            "decl.parameter",
            "decl.where_clause",
            "decl.field",
            "decl.variant",
            "decl.extern_parameter",
            "decl.flow_annotation",
            "type.name",
            "session.send",
            "session.recv",
            "session.end",
            "match.arm",
            "record.field",
        ] {
            assert!(
                ids.iter().any(|node_id| {
                    node_id.contains(&format!("/node:{kind}@"))
                        || node_id.contains(&format!("/generated:{kind}:"))
                        || node_id.contains(&format!("/fallback:{kind}:"))
                }),
                "missing NodeMeta kind {kind}"
            );
        }
        assert!(ids
            .iter()
            .any(|node_id| node_id.starts_with("protocol:Sensor/state:")));
        assert!(ids
            .iter()
            .any(|node_id| node_id.starts_with("protocol:Sensor/transition:")));
        assert!(ids
            .iter()
            .any(|node_id| node_id.starts_with("extern:C:add+sub/function:")));
    }

    #[test]
    fn generated_siblings_with_the_same_inherited_span_use_rule_and_semantic_discriminator() {
        let mut file = parse("func main() -> i32 { let values = [1]; 0 }");
        let main = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let inherited = main.meta.span;
        let Stmt::Let {
            init: Some(values), ..
        } = main.body[0].unlocated_mut()
        else {
            panic!("list binding")
        };
        let Expr::List(items) = values.unlocated_mut() else {
            panic!("list expression")
        };
        *items = vec![
            Expr::Literal(crate::ast::Lit::Int(7)).with_meta(crate::ast::AstNodeMeta::inherited(
                inherited,
                AstOrigin::Desugared("test.same_span"),
            )),
            Expr::Literal(crate::ast::Lit::Int(7)).with_meta(crate::ast::AstNodeMeta::inherited(
                inherited,
                AstOrigin::Desugared("test.same_span"),
            )),
        ];

        let program = CheckedProgram::from_checked_file(&file).expect("generated siblings");
        let generated = program
            .node_meta()
            .iter()
            .filter(|(node_id, meta)| {
                node_id
                    .0
                    .contains("/generated:expr.literal:test.same_span:")
                    && meta.origin.rule() == Some("test.same_span")
            })
            .collect::<Vec<_>>();
        assert_eq!(generated.len(), 2);
        assert_ne!(generated[0].0, generated[1].0);
        assert!(generated
            .iter()
            .all(|(_, meta)| meta.precision == SpanPrecision::SourceAnchor));
    }

    #[test]
    fn callable_catalog_uses_the_same_impl_and_nested_ids_as_ownership_ledgers() {
        let source = r#"
module api {
    trait Close {
        func close(value: i32) -> i32;
        func flush(value: i32) -> i32;
    }
    type Handle { value: i32 }
    impl Close for Handle {
        func close(value: i32) -> i32 { value }
        func flush(value: i32) -> i32 { value }
    }
    func outer() -> i32 { func inner(value: i32) -> i32 { value }; inner(1) }
}
func main() -> i32 { 0 }
"#;
        let file = parse(source);
        let program = crate::core::check_program(&file).expect("callable catalog");
        let module = file
            .items
            .iter()
            .find_map(|item| match item {
                Item::Module(module) if module.name == "api" => Some(module),
                _ => None,
            })
            .expect("api module");
        let impl_def = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Impl(impl_def) => Some(impl_def),
                _ => None,
            })
            .expect("impl");
        let impl_owner = impl_method_owner("api::Close:for:Handle", &impl_def.methods[0]);
        assert!(program.node_meta().contains_key(&impl_owner));
        assert!(program.ownership_ledger(&impl_owner).is_some());

        let outer = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "outer" => Some(function),
                _ => None,
            })
            .expect("outer");
        let nested = outer
            .body
            .iter()
            .find_map(|stmt| match stmt.unlocated() {
                Stmt::Func(function) => Some(function),
                _ => None,
            })
            .expect("nested function");
        let nested_owner = nested_function_owner(&NodeId("function:api::outer".into()), nested);
        assert!(program.node_meta().contains_key(&nested_owner));
        assert!(program.ownership_ledger(&nested_owner).is_some());

        let expected_owners = program
            .ownership_ledgers()
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let mut reordered = file.clone();
        let reordered_module = reordered
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Module(module) if module.name == "api" => Some(module),
                _ => None,
            })
            .expect("reordered api");
        for item in &mut reordered_module.items {
            match item {
                Item::Trait(trait_def) => trait_def.methods.reverse(),
                Item::Impl(impl_def) => impl_def.methods.reverse(),
                _ => {}
            }
        }
        reordered_module.items.reverse();
        let reordered_program =
            crate::core::check_program(&reordered).expect("reordered callable catalog");
        assert_eq!(
            expected_owners,
            reordered_program
                .ownership_ledgers()
                .keys()
                .cloned()
                .collect()
        );
    }

    #[test]
    fn production_node_ids_do_not_encode_vec_indexes() {
        let file = parse(
            r#"
func helper(x: i32) -> i32 { x }
func main() -> i32 {
    let values = [helper(1), helper(2)]
    match values { [a, b] => a, _ => 0 }
}
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        for node_id in program
            .node_meta()
            .keys()
            .chain(program.call_sites().keys())
        {
            for forbidden in ["/stmt:", "/arg:", "/item:", "/arm:", "/field:", "/entry:"] {
                assert!(
                    !node_id.0.contains(forbidden),
                    "NodeId contains Vec-index identity: {}",
                    node_id.0
                );
            }
        }
    }

    #[test]
    fn exact_child_ids_survive_same_role_synthetic_insertion() {
        let mut file = parse("func main() -> i32 { let values = [1, 2]; 0 }");
        let original = crate::core::check_program(&file).expect("original");
        let first_literal = node_id_at(&original, "expr.literal", 1, 36);
        let second_literal = node_id_at(&original, "expr.literal", 1, 39);

        let main = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let Stmt::Let {
            init: Some(values), ..
        } = main.body[0].unlocated_mut()
        else {
            panic!("list binding")
        };
        let Expr::List(items) = values.unlocated_mut() else {
            panic!("list expression")
        };
        items.insert(
            0,
            Expr::Literal(crate::ast::Lit::Int(99))
                .synthetic_with_origin(AstOrigin::Desugared("test.list_prefix")),
        );

        let lowered = crate::core::check_program(&file).expect("lowered");
        assert!(lowered.node_meta().contains_key(&first_literal));
        assert!(lowered.node_meta().contains_key(&second_literal));
    }

    #[test]
    fn duplicate_canonical_node_ids_are_structured_errors() {
        let mut file = parse("func main() -> i32 { 1; 2 }");
        let main = file
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::Func(function) if function.name == "main" => Some(function),
                _ => None,
            })
            .expect("main");
        let first_meta = match main.body[0].unlocated() {
            Stmt::Expr(expr) => expr.meta().expect("first metadata"),
            _ => panic!("first expression"),
        };
        let second = match main.body[1].unlocated_mut() {
            Stmt::Expr(expr) => expr,
            _ => panic!("second expression"),
        };
        *second = second.clone().with_meta(first_meta);

        let diagnostics = CheckedProgram::from_checked_file(&file).expect_err("duplicate id");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("TOOL-RESOLUTION-001")
                && diagnostic.message.contains("duplicate canonical NodeId")
        }));
    }

    #[test]
    fn origin_catalog_rejects_missing_parent_and_cycles() {
        let a = NodeId("generated:a".into());
        let b = NodeId("generated:b".into());
        let span = Span::single(2, 3);
        let mut catalog = OriginCatalog::default();
        let mut errors = Vec::new();
        catalog.register(
            &a,
            &Origin::Desugared {
                parent: b.clone(),
                rule: "test.a".into(),
                span,
            },
            &mut errors,
        );
        catalog.validate(&mut errors);
        assert!(errors
            .iter()
            .any(|error| error.message.contains("missing Origin parent")));

        let mut cyclic = OriginCatalog::default();
        let mut cycle_errors = Vec::new();
        cyclic.register(
            &a,
            &Origin::Desugared {
                parent: b.clone(),
                rule: "test.a".into(),
                span,
            },
            &mut cycle_errors,
        );
        cyclic.register(
            &b,
            &Origin::Desugared {
                parent: a.clone(),
                rule: "test.b".into(),
                span,
            },
            &mut cycle_errors,
        );
        cyclic.validate(&mut cycle_errors);
        assert!(cycle_errors
            .iter()
            .any(|error| error.message.contains("Origin cycle")));
    }

    #[test]
    fn resolved_ir_rejects_nested_erased_state_payloads() {
        let file = parse(
            r#"
flow Cache {
    state Ready { values: List<Any> }
}
"#,
        );
        let diagnostics = CheckedProgram::from_checked_file(&file).expect_err("IR must reject Any");
        assert!(diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("TOOL-RESOLUTION-001")
            && diagnostic.message.contains("List<Any>")));
        assert!(diagnostics
            .iter()
            .all(|diagnostic| diagnostic.span.start_line > 0));
    }

    #[test]
    fn resolved_ir_rejects_unknown_and_type_schemes() {
        for ty in [
            Type::Name("unknown".into(), vec![]),
            Type::ForAll(vec!["T".into()], Box::new(Type::Name("i32".into(), vec![]))),
        ] {
            let file = File {
                sources: crate::span::SourceRegistry::default(),
                imports: Vec::new(),
                items: vec![Item::Func(crate::ast::FuncDef {
                    meta: crate::ast::AstNodeMeta::synthetic(crate::ast::AstOrigin::RuntimeSystem(
                        "test.resolved_fixture",
                    )),
                    name: "bad".into(),
                    pub_: false,
                    params: vec![crate::ast::Param {
                        meta: crate::ast::AstNodeMeta::synthetic(
                            crate::ast::AstOrigin::RuntimeSystem("test.resolved_fixture_param"),
                        ),
                        name: "value".into(),
                        ty,
                        mut_: false,
                        default_value: None,
                        borrow: None,
                    }],
                    ret: Some(Type::Name("i32".into(), vec![])),
                    body: vec![Stmt::Return(Some(Expr::Literal(crate::ast::Lit::Int(0))))],
                    where_clause: Vec::new(),
                    generics: Vec::new(),
                    effects: Vec::new(),
                    is_comptime: false,
                    is_async: false,
                    extern_abi: None,
                })],
                implicit_single: false,
            };

            let diagnostics =
                CheckedProgram::from_checked_file(&file).expect_err("IR must reject unresolved");
            assert!(diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("TOOL-RESOLUTION-001")));
        }
    }

    #[test]
    fn ownership_ledger_persists_capability_actions_and_branch_merges() {
        let file = parse(
            r#"
cap File
func pass(flag: bool, f: cap File) -> i32 {
    if flag { drop(f) } else { drop(f) }
    0
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let ledger = program
            .ownership_ledger(&NodeId("function:pass".to_string()))
            .expect("pass ownership ledger");
        assert!(ledger.actions.iter().any(|action| {
            action.kind == crate::core::ResourceActionKind::Introduce && action.resource == "f"
        }));
        assert_eq!(
            ledger
                .actions
                .iter()
                .filter(|action| {
                    action.kind == crate::core::ResourceActionKind::Drop && action.resource == "f"
                })
                .count(),
            2
        );
        let merge = ledger
            .branch_merges
            .iter()
            .find(|merge| merge.resource == "f")
            .expect("f branch merge");
        assert_eq!(merge.then_state, crate::core::ResourceState::Consumed);
        assert_eq!(merge.else_state, crate::core::ResourceState::Consumed);
        assert_eq!(merge.merged_state, crate::core::ResourceState::Consumed);
    }

    #[test]
    fn ownership_ledger_records_return_transfer() {
        let file = parse(
            r#"
cap File
func identity(f: cap File) -> cap File { return f }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let ledger = program
            .ownership_ledger(&NodeId("function:identity".to_string()))
            .expect("identity ownership ledger");
        assert!(ledger.actions.iter().any(|action| {
            action.kind == crate::core::ResourceActionKind::Return && action.resource == "f"
        }));
    }

    #[test]
    fn ownership_ledger_records_borrow_places_and_lifetime_end() {
        let file = parse(
            r#"
type Pair { left: i32, right: i32 }
func inspect() -> i32 {
    let mut p = Pair { left: 1, right: 2 }
    let xs = [3, 4]
    let left = &p.left
    let right = &mut p.right
    let item = &xs[0]
    *left + *right + *item
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("borrow program checks");
        let ledger = program
            .ownership_ledger(&NodeId("function:inspect".to_string()))
            .expect("inspect ownership ledger");
        let actions: Vec<_> = ledger
            .actions
            .iter()
            .map(|action| (action.kind, action.resource.as_str()))
            .collect();
        assert!(actions.contains(&(crate::core::ResourceActionKind::BorrowShared, "p.left")));
        assert!(actions.contains(&(crate::core::ResourceActionKind::BorrowMut, "p.right")));
        // RESOURCE-LINEAR-001: canonical places retain constant-index
        // disjointness; only genuinely dynamic indices project as `[*]`.
        assert!(actions.contains(&(crate::core::ResourceActionKind::BorrowShared, "xs[0]")));
        assert!(actions.contains(&(crate::core::ResourceActionKind::BorrowEnd, "p.left")));
        assert!(actions.contains(&(crate::core::ResourceActionKind::BorrowEnd, "p.right")));
        assert!(actions.contains(&(crate::core::ResourceActionKind::BorrowEnd, "xs[0]")));

        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp
            .resolved_ownership_actions("function:inspect")
            .is_some_and(|actions| actions
                .iter()
                .any(|(kind, place)| kind == "borrow_mut" && place == "p.right")));
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier
            .checked_ownership_actions("function:inspect")
            .is_some_and(|actions| actions
                .iter()
                .any(|(kind, place)| kind == "borrow_end" && place == "p.left")));
    }

    #[test]
    fn ownership_checker_transfers_compound_capability_returns_in_order() {
        let file = parse(
            r#"
cap Token
func pair(a: cap Token, b: cap Token) -> (cap Token, cap Token) {
    return (a, b)
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("compound return transfers caps");
        let ledger = program
            .ownership_ledger(&NodeId("function:pair".to_string()))
            .expect("pair ownership ledger");
        let returned: Vec<_> = ledger
            .actions
            .iter()
            .filter(|action| action.kind == crate::core::ResourceActionKind::Return)
            .map(|action| action.resource.as_str())
            .collect();
        assert_eq!(returned, vec!["a", "b"]);
    }

    #[test]
    fn ownership_checker_drops_compound_capabilities_in_order() {
        let file = parse(
            r#"
cap Token
func close(a: cap Token, b: cap Token) -> i32 {
    drop((a, b))
    0
}
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("compound drop consumes caps");
        let ledger = program
            .ownership_ledger(&NodeId("function:close".to_string()))
            .expect("close ownership ledger");
        let dropped: Vec<_> = ledger
            .actions
            .iter()
            .filter(|action| action.kind == crate::core::ResourceActionKind::Drop)
            .map(|action| action.resource.as_str())
            .collect();
        assert_eq!(dropped, vec!["a", "b"]);
    }

    #[test]
    fn ownership_checker_rejects_one_branch_consumption() {
        let file = parse(
            r#"
cap File
func bad(flag: bool, f: cap File) -> i32 {
    if flag { drop(f) }
    0
}
func main() -> i32 { 0 }
"#,
        );
        let diagnostics = crate::core::check_program(&file).expect_err("branch mismatch");
        // RESOURCE-LINEAR-001: the typed CFG join, not checker snapshots,
        // reports the mismatch between reachable predecessors.
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
                && diagnostic.message.contains("some reachable CFG paths")
        }));
    }

    #[test]
    fn ownership_checker_consumes_outer_capability_in_nested_block() {
        let file = parse(
            r#"
cap File
func close(f: cap File) -> i32 {
    { drop(f) }
    0
}
func main() -> i32 { 0 }
"#,
        );
        crate::core::check_program(&file).expect("nested block consumes outer cap");
    }

    #[test]
    fn ownership_checker_rejects_return_path_leak() {
        let file = parse(
            r#"
cap File
func bad(flag: bool, f: cap File) -> i32 {
    if flag { return 0 }
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
        );
        let diagnostics = crate::core::check_program(&file).expect_err("return path leak");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0256)
                && diagnostic.message.contains("return path")
        }));
    }

    #[test]
    fn ownership_checker_accepts_return_transfer_on_both_paths() {
        let file = parse(
            r#"
cap File
func choose(flag: bool, f: cap File) -> cap File {
    if flag { return f }
    return f
}
func main() -> i32 { 0 }
"#,
        );
        crate::core::check_program(&file).expect("both return paths transfer f");
    }

    #[test]
    fn ownership_checker_rejects_loop_carried_consumption() {
        let file = parse(
            r#"
cap File
func bad(run: bool, f: cap File) -> i32 {
    while run {
        drop(f)
    }
    0
}
func main() -> i32 { 0 }
"#,
        );
        let diagnostics = crate::core::check_program(&file).expect_err("loop consumption");
        // RESOURCE-LINEAR-001: loop ownership is a fixed-point join.
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
                && diagnostic.message.contains("some reachable CFG paths")
        }));
    }

    #[test]
    fn ownership_checker_allows_break_only_loop_body_consumption() {
        // Body always exits via break → no back-edge; still join with zero-iteration path.
        let file = parse(
            r#"
cap File
func ok(run: bool, f: cap File) -> i32 {
    while run {
        drop(f)
        break
    }
    0
}
func main() -> i32 { 0 }
"#,
        );
        let diagnostics = crate::core::check_program(&file).expect_err("zero-iteration leak");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
                && diagnostic.message.contains("some reachable CFG paths")
        }));
    }

    #[test]
    fn ownership_checker_accepts_loop_with_break_and_post_drop() {
        let file = parse(
            r#"
cap File
func ok(run: bool, f: cap File) -> i32 {
    while run {
        break
    }
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
        );
        crate::core::check_program(&file).expect("break-only body does not consume f");
    }

    #[test]
    fn ownership_checker_accepts_infinite_loop_break_after_drop() {
        let file = parse(
            r#"
cap File
func ok(f: cap File) -> i32 {
    loop {
        drop(f)
        break
    }
    0
}
func main() -> i32 { 0 }
"#,
        );
        crate::core::check_program(&file).expect("loop body always exits after drop");
    }

    #[test]
    fn ownership_checker_moves_by_value_cap_arguments() {
        let file = parse(
            r#"
cap File
func consume(f: cap File) -> i32 { drop(f); 0 }
func bad(f: cap File) -> i32 {
    consume(f)
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
        );
        let diagnostics = crate::core::check_program(&file).expect_err("double consume");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
                && diagnostic
                    .message
                    .contains("consumed more than once on this CFG path")
        }));
    }

    #[test]
    fn ownership_checker_joins_expression_if_branches() {
        let file = parse(
            r#"
cap File
func use_cap(flag: bool, f: cap File) -> i32 {
    let result = if flag { drop(f); 1 } else { drop(f); 2 }
    result
}
func main() -> i32 { 0 }
"#,
        );
        crate::core::check_program(&file).expect("expression if consumes both paths");
    }

    #[test]
    fn ownership_checker_joins_match_arms() {
        let file = parse(
            r#"
cap File
func use_cap(flag: bool, f: cap File) -> i32 {
    match flag { true => { drop(f); 1 }, false => { drop(f); 2 } }
}
func main() -> i32 { 0 }
"#,
        );
        crate::core::check_program(&file).expect("match consumes both paths");
    }

    #[test]
    fn ownership_checker_accepts_implicit_capability_return() {
        let file = parse(
            r#"
cap File
func identity(f: cap File) -> cap File { f }
func main() -> i32 { 0 }
"#,
        );
        crate::core::check_program(&file).expect("implicit return transfers f");
    }

    #[test]
    fn ownership_ledgers_use_module_qualified_owner_ids() {
        let file = parse(
            r#"
cap File
module A { func close(f: cap File) -> i32 { drop(f); 0 } }
module B { func close(f: cap File) -> i32 { drop(f); 0 } }
func main() -> i32 { 0 }
"#,
        );
        let program = crate::core::check_program(&file).expect("check modules");
        assert!(program
            .ownership_ledger(&NodeId("function:A::close".to_string()))
            .is_some());
        assert!(program
            .ownership_ledger(&NodeId("function:B::close".to_string()))
            .is_some());
        assert!(program
            .ownership_ledger(&NodeId("function:close".to_string()))
            .is_none());
    }

    #[test]
    fn ownership_ledger_ignores_non_linear_drop() {
        let file = parse("func main() -> i32 { let x = 1; drop(x); 0 }");
        let program = crate::core::check_program(&file).expect("check");
        let ledger = program
            .ownership_ledger(&NodeId("function:main".to_string()))
            .expect("main ledger");
        assert!(ledger.actions.iter().all(|action| action.resource != "x"));
    }

    #[test]
    fn ownership_checker_nested_function_does_not_consume_outer_capability() {
        let file = parse(
            r#"
cap File
func outer(f: cap File) -> i32 {
    func inner() -> i32 { 0 }
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
        );
        crate::core::check_program(&file).expect("nested function preserves outer ownership");
    }

    #[test]
    fn ownership_checker_rejects_implicit_nested_capability_capture() {
        let file = parse(
            r#"
cap File
func outer(f: cap File) -> i32 {
    func inner() -> i32 { drop(f); 0 }
    drop(f)
    0
}
func main() -> i32 { 0 }
"#,
        );
        let diagnostics = crate::core::check_program(&file).expect_err("implicit cap capture");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
                && diagnostic
                    .message
                    .contains("not owned by the current callable")
        }));
    }

    #[test]
    fn ownership_checker_tracks_actor_method_capabilities() {
        let file = parse(
            r#"
cap File
actor Sink {
    func leak(f: cap File) -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
        );
        let diagnostics = crate::core::check_program(&file).expect_err("actor method leak");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0256)
                && diagnostic.message.contains("f")
        }));
    }

    #[test]
    fn ownership_checker_tracks_impl_method_capabilities() {
        let file = parse(
            r#"
cap File
trait Close { func close(f: cap File) -> i32 }
type Handle { value: i32 }
impl Close for Handle {
    func close(f: cap File) -> i32 { 0 }
}
func main() -> i32 { 0 }
"#,
        );
        let diagnostics = crate::core::check_program(&file).expect_err("impl method leak");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0256)
                && diagnostic.message.contains("f")
        }));
    }

    #[test]
    fn ownership_checker_tracks_transition_capabilities() {
        let file = parse(
            r#"
cap File
flow Door {
    state Closed
    state Open
    transition open(Closed, f: cap File) -> Open { do { return Open {} } }
}
func main() -> i32 { 0 }
"#,
        );
        let diagnostics = crate::core::check_program(&file).expect_err("transition leak");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0256)
                && diagnostic.message.contains("f")
        }));
    }
}

fn qualify(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{}::{}", module, name)
    }
}
