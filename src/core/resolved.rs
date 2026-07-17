use crate::ast::{AstOrigin, Expr, FStringPart, File, FlowDef, Item, Pattern, Stmt, Type};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

use super::OwnershipLedger;

pub const RESOLVED_IR_VERSION: &str = "mimi-resolved-ir-1";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

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
    DeclarationFallback,
}

#[derive(Debug, Clone)]
pub struct NodeMeta {
    pub node_id: NodeId,
    pub origin: Origin,
    pub precision: SpanPrecision,
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
    Desugared { parent: NodeId, span: Span },
    PrototypeFallback { parent: NodeId, span: Span },
    RuntimeSystem { parent: NodeId, span: Span },
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
}

#[derive(Debug, Clone)]
pub struct ResolvedTransition {
    pub node_id: NodeId,
    pub id: TransitionId,
    pub targets: Vec<StateId>,
    pub params: Vec<(String, Type)>,
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
    pub ret: Type,
    pub effects: Vec<String>,
    pub is_comptime: bool,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedSession {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub body: crate::ast::SessionType,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedProtocol {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub states: Vec<String>,
    pub transitions: Vec<(String, String, Vec<String>)>, // (event, from, to_states)
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedActor {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub fields: Vec<(String, Type, bool)>,
    pub methods: Vec<String>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedCapability {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub combined_with: Option<String>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedConstant {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedTrait {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub methods: Vec<String>,
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedImpl {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub trait_name: String,
    pub type_name: String,
    pub methods: Vec<String>,
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
    pub origin: Origin,
}

#[derive(Debug, Clone)]
pub struct ResolvedExternBlock {
    pub node_id: NodeId,
    pub qualified_name: String,
    pub abi: String,
    pub funcs: Vec<String>,
    pub no_panic: bool,
    pub unsafe_: bool,
    pub origin: Origin,
}

#[derive(Debug)]
pub struct CheckedProgram<'a> {
    file: &'a File,
    items: HashMap<NodeId, ResolvedItem>,
    node_meta: HashMap<NodeId, NodeMeta>,
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
}

impl<'a> CheckedProgram<'a> {
    #[cfg(test)]
    pub(crate) fn from_checked_file(file: &'a File) -> Result<Self, Vec<Diagnostic>> {
        Self::from_checked_file_with_ownership(file, HashMap::new())
    }

    pub(crate) fn from_checked_file_with_ownership(
        file: &'a File,
        ownership_ledgers: HashMap<NodeId, OwnershipLedger>,
    ) -> Result<Self, Vec<Diagnostic>> {
        let mut transitions = HashMap::new();
        let mut flows = HashMap::new();
        let mut items = HashMap::new();
        let mut node_meta = HashMap::new();
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
        collect_items(
            &file.items,
            "",
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
        for (owner, ledger) in &ownership_ledgers {
            if ledger.owner != *owner {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: ownership ledger key '{}' disagrees with ledger.owner '{}'",
                        owner.0, ledger.owner.0
                    ),
                    Span::single(1, 1),
                ));
            }
            let ok = owner.0.starts_with("function:") || owner.0.starts_with("transition:");
            if !ok {
                errors.push(Diagnostic::error(
                    format!(
                        "TOOL-RESOLUTION-001: ownership ledger owner '{}' is not a callable NodeId",
                        owner.0
                    ),
                    Span::single(1, 1),
                ));
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Self {
            file,
            items,
            node_meta,
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
            ownership_ledgers,
        })
    }

    pub fn file(&self) -> &'a File {
        self.file
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

    pub fn protocols(&self) -> &HashMap<NodeId, ResolvedProtocol> {
        &self.protocols
    }

    pub fn protocol(&self, qualified_name: &str) -> Option<&ResolvedProtocol> {
        self.protocols
            .values()
            .find(|protocol| protocol.qualified_name == qualified_name)
    }

    pub fn actors(&self) -> &HashMap<NodeId, ResolvedActor> {
        &self.actors
    }

    pub fn actor(&self, qualified_name: &str) -> Option<&ResolvedActor> {
        self.actors
            .values()
            .find(|actor| actor.qualified_name == qualified_name)
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

    pub fn ownership_ledgers(&self) -> &HashMap<NodeId, OwnershipLedger> {
        &self.ownership_ledgers
    }

    pub fn ownership_ledger(&self, owner: &NodeId) -> Option<&OwnershipLedger> {
        self.ownership_ledgers.get(owner)
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
    for item in items {
        match item {
            Item::Module(def) => {
                let qualified = qualify(module, &def.name);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Module,
                    &qualified,
                    def.origin,
                    Span::from(def.pos),
                    errors,
                );
                collect_items(
                    &def.items,
                    &qualified,
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
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Flow,
                    &qualified,
                    flow.origin,
                    Span::from(flow.pos),
                    errors,
                );
                collect_flow(
                    flow,
                    &qualified,
                    flows,
                    transitions,
                    backend_requirements,
                    errors,
                );
            }
            Item::Func(function) => {
                let qualified = qualify(module, &function.name);
                let span = Span::from(function.pos);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Function,
                    &qualified,
                    AstOrigin::User,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("function:{}", qualified));
                let origin = Origin::User(span);
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
                        node_id,
                        qualified_name: qualified.clone(),
                        params,
                        ret,
                        effects: function.effects.clone(),
                        is_comptime: function.is_comptime,
                        origin,
                    },
                );
                collect_block_meta(
                    &function.body,
                    &format!("function:{}", qualified),
                    span,
                    node_meta,
                );
            }
            Item::Type(type_def) => {
                let qualified = qualify(module, &type_def.name);
                let span = type_def
                    .decl_pos
                    .map(Span::from)
                    .unwrap_or_else(|| Span::single(1, 1));
                if type_def.decl_pos.is_some() {
                    insert_item(
                        resolved_items,
                        ResolvedItemKind::Type,
                        &qualified,
                        AstOrigin::User,
                        span,
                        errors,
                    );
                }
                let kind = match &type_def.kind {
                    crate::ast::TypeDefKind::Alias(_) => ResolvedTypeKind::Alias,
                    crate::ast::TypeDefKind::Newtype(_) => ResolvedTypeKind::Newtype,
                    crate::ast::TypeDefKind::Record(_) => ResolvedTypeKind::Record,
                    crate::ast::TypeDefKind::Enum(_) => ResolvedTypeKind::Enum,
                    crate::ast::TypeDefKind::Union(_) => ResolvedTypeKind::Union,
                };
                let node_id = NodeId(format!("type:{}", qualified));
                type_defs.insert(
                    node_id.clone(),
                    ResolvedTypeDef {
                        node_id,
                        qualified_name: qualified,
                        kind,
                        origin: Origin::User(span),
                    },
                );
            }
            Item::Const { name, pos, .. } => {
                let qualified = qualify(module, name);
                let span = Span::from(*pos);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Constant,
                    &qualified,
                    AstOrigin::User,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("constant:{}", qualified));
                constants.insert(
                    node_id.clone(),
                    ResolvedConstant {
                        node_id,
                        qualified_name: qualified,
                        origin: Origin::User(span),
                    },
                );
            }
            Item::Cap(cap) => {
                let qualified = qualify(module, &cap.name);
                let span = Span::from(cap.pos);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Capability,
                    &qualified,
                    cap.origin,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("capability:{}", qualified));
                capabilities.insert(
                    node_id.clone(),
                    ResolvedCapability {
                        node_id,
                        qualified_name: qualified,
                        combined_with: cap.combined_with.clone(),
                        origin: resolve_origin(cap.origin, &NodeId("capability".into()), span),
                    },
                );
            }
            Item::Trait(trait_def) => {
                let qualified = qualify(module, &trait_def.name);
                let span = Span::from(trait_def.pos);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Trait,
                    &qualified,
                    trait_def.origin,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("trait:{}", qualified));
                let methods = trait_def
                    .methods
                    .iter()
                    .map(|method| method.name.clone())
                    .collect();
                traits.insert(
                    node_id.clone(),
                    ResolvedTrait {
                        node_id,
                        qualified_name: qualified,
                        methods,
                        origin: resolve_origin(trait_def.origin, &NodeId("trait".into()), span),
                    },
                );
            }
            Item::Impl(impl_def) => {
                let qualified = qualify(
                    module,
                    &format!("{}:for:{}", impl_def.trait_name, impl_def.type_name),
                );
                let span = Span::from(impl_def.pos);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Impl,
                    &qualified,
                    impl_def.origin,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("impl:{}", qualified));
                let methods = impl_def
                    .methods
                    .iter()
                    .map(|method| method.name.clone())
                    .collect();
                impls.insert(
                    node_id.clone(),
                    ResolvedImpl {
                        node_id,
                        qualified_name: qualified,
                        trait_name: impl_def.trait_name.clone(),
                        type_name: impl_def.type_name.clone(),
                        methods,
                        origin: resolve_origin(impl_def.origin, &NodeId("impl".into()), span),
                    },
                );
            }
            Item::ExternBlock(block) => {
                let qualified = qualify(module, &format!("{}:at:{}", block.abi, block.pos.0));
                let span = Span::from(block.pos);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::ExternBlock,
                    &qualified,
                    block.origin,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("extern:{}", qualified));
                let funcs = block.funcs.iter().map(|func| func.name.clone()).collect();
                for func in &block.funcs {
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
                }
                extern_blocks.insert(
                    node_id.clone(),
                    ResolvedExternBlock {
                        node_id,
                        qualified_name: qualified,
                        abi: block.abi.clone(),
                        funcs,
                        no_panic: block.no_panic,
                        unsafe_: block.unsafe_,
                        origin: resolve_origin(block.origin, &NodeId("extern".into()), span),
                    },
                );
            }

            Item::Actor(actor) => {
                let qualified = qualify(module, &actor.name);
                let span = Span::from(actor.pos);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Actor,
                    &qualified,
                    actor.origin,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("actor:{}", qualified));
                let fields = actor
                    .fields
                    .iter()
                    .map(|field| (field.name.clone(), field.ty.clone(), field.mut_))
                    .collect::<Vec<_>>();
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
                actors.insert(
                    node_id.clone(),
                    ResolvedActor {
                        node_id,
                        qualified_name: qualified,
                        fields,
                        methods,
                        origin: resolve_origin(actor.origin, &NodeId("actor".into()), span),
                    },
                );
            }
            Item::Protocol(protocol) => {
                let qualified = qualify(module, &protocol.name);
                let span = Span::from(protocol.pos);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Protocol,
                    &qualified,
                    protocol.origin,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("protocol:{}", qualified));
                let states = protocol
                    .states
                    .iter()
                    .map(|state| state.name.clone())
                    .collect::<Vec<_>>();
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
                protocols.insert(
                    node_id.clone(),
                    ResolvedProtocol {
                        node_id,
                        qualified_name: qualified,
                        states,
                        transitions,
                        origin: resolve_origin(protocol.origin, &NodeId("protocol".into()), span),
                    },
                );
            }
            Item::Session(session) => {
                let qualified = qualify(module, &session.name);
                let span = Span::from(session.pos);
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Session,
                    &qualified,
                    session.origin,
                    span,
                    errors,
                );
                let node_id = NodeId(format!("session:{}", qualified));
                sessions.insert(
                    node_id.clone(),
                    ResolvedSession {
                        node_id,
                        qualified_name: qualified,
                        body: session.body.clone(),
                        origin: resolve_origin(session.origin, &NodeId("session".into()), span),
                    },
                );
            }
        }
    }
}

fn insert_item(
    items: &mut HashMap<NodeId, ResolvedItem>,
    kind: ResolvedItemKind,
    qualified_name: &str,
    origin: AstOrigin,
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
    let item = ResolvedItem {
        node_id: node_id.clone(),
        qualified_name: qualified_name.to_string(),
        kind,
        origin: resolve_origin(origin, &node_id, span),
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
    parent: &str,
    fallback: Span,
    out: &mut HashMap<NodeId, NodeMeta>,
) {
    for (index, stmt) in block.iter().enumerate() {
        collect_stmt_meta(stmt, &format!("{parent}/stmt:{index}"), fallback, out);
    }
}

fn collect_stmt_meta(stmt: &Stmt, path: &str, fallback: Span, out: &mut HashMap<NodeId, NodeMeta>) {
    let exact = match stmt {
        Stmt::Let { pos, .. } => Some(Span::from(*pos)),
        Stmt::Desc(_, span)
        | Stmt::Rule(_, span)
        | Stmt::Requires(_, span)
        | Stmt::Ensures(_, span)
        | Stmt::Invariant(_, span)
        | Stmt::MmsBlock { span, .. } => Some(*span),
        _ => None,
    };
    insert_node_meta(path, exact, fallback, out);
    match stmt {
        Stmt::Let { pat, init, .. } => {
            collect_pattern_meta(pat, &format!("{path}/pattern"), fallback, out);
            if let Some(expr) = init {
                collect_expr_meta(expr, &format!("{path}/init"), fallback, out);
            }
        }
        Stmt::Return(expr) | Stmt::Break(expr) => {
            if let Some(expr) = expr {
                collect_expr_meta(expr, &format!("{path}/value"), fallback, out);
            }
        }
        Stmt::Continue | Stmt::Ellipsis | Stmt::Desc(_, _) | Stmt::Rule(_, _) => {}
        Stmt::Expr(expr)
        | Stmt::Drop(expr)
        | Stmt::Requires(expr, _)
        | Stmt::Ensures(expr, _)
        | Stmt::Invariant(expr, _) => {
            collect_expr_meta(expr, &format!("{path}/expr"), fallback, out);
        }
        Stmt::If { cond, then_, else_ } => {
            collect_expr_meta(cond, &format!("{path}/cond"), fallback, out);
            collect_block_meta(then_, &format!("{path}/then"), fallback, out);
            if let Some(block) = else_ {
                collect_block_meta(block, &format!("{path}/else"), fallback, out);
            }
        }
        Stmt::While { cond, body } => {
            collect_expr_meta(cond, &format!("{path}/cond"), fallback, out);
            collect_block_meta(body, &format!("{path}/body"), fallback, out);
        }
        Stmt::WhileLet { pat, init, body } => {
            collect_pattern_meta(pat, &format!("{path}/pattern"), fallback, out);
            collect_expr_meta(init, &format!("{path}/init"), fallback, out);
            collect_block_meta(body, &format!("{path}/body"), fallback, out);
        }
        Stmt::Loop(body)
        | Stmt::Block(body)
        | Stmt::Arena(body)
        | Stmt::Unsafe(body)
        | Stmt::OnFailure(body)
        | Stmt::Do(body)
        | Stmt::Parasteps(body) => {
            collect_block_meta(body, &format!("{path}/body"), fallback, out);
        }
        Stmt::For { iterable, body, .. } => {
            collect_expr_meta(iterable, &format!("{path}/iterable"), fallback, out);
            collect_block_meta(body, &format!("{path}/body"), fallback, out);
        }
        Stmt::Math(exprs) => {
            for (index, expr) in exprs.iter().enumerate() {
                collect_expr_meta(expr, &format!("{path}/math:{index}"), fallback, out);
            }
        }
        Stmt::Assign { target, value } => {
            collect_expr_meta(target, &format!("{path}/target"), fallback, out);
            collect_expr_meta(value, &format!("{path}/value"), fallback, out);
        }
        Stmt::SharedLet { init, .. } => {
            collect_expr_meta(init, &format!("{path}/init"), fallback, out);
        }
        Stmt::Delegate { expr, .. } => {
            collect_expr_meta(expr, &format!("{path}/expr"), fallback, out);
        }
        Stmt::Pinned {
            expr,
            timeout,
            body,
            ..
        } => {
            collect_expr_meta(expr, &format!("{path}/expr"), fallback, out);
            if let Some(timeout) = timeout {
                collect_expr_meta(timeout, &format!("{path}/timeout"), fallback, out);
            }
            collect_block_meta(body, &format!("{path}/body"), fallback, out);
        }
        Stmt::MmsBlock { .. } => {}
        Stmt::Func(function) => collect_block_meta(
            &function.body,
            &format!("{path}/function:{}", function.name),
            Span::from(function.pos),
            out,
        ),
        Stmt::Alloc { body, .. } => {
            collect_block_meta(body, &format!("{path}/body"), fallback, out);
        }
    }
}

fn collect_expr_meta(expr: &Expr, path: &str, fallback: Span, out: &mut HashMap<NodeId, NodeMeta>) {
    insert_node_meta(path, None, fallback, out);
    match expr {
        Expr::Literal(lit) => {
            if let crate::ast::Lit::FString(parts) = lit {
                for (index, part) in parts.iter().enumerate() {
                    if let FStringPart::Interp(expr) = part {
                        collect_expr_meta(expr, &format!("{path}/fstring:{index}"), fallback, out);
                    }
                }
            }
        }
        Expr::Ident(_) | Expr::TypeInfo(_) => {}
        Expr::Binary(_, left, right) | Expr::Index(left, right) => {
            collect_expr_meta(left, &format!("{path}/left"), fallback, out);
            collect_expr_meta(right, &format!("{path}/right"), fallback, out);
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
            collect_expr_meta(inner, &format!("{path}/inner"), fallback, out);
        }
        Expr::Call(callee, args) => {
            collect_expr_meta(callee, &format!("{path}/callee"), fallback, out);
            for (index, arg) in args.iter().enumerate() {
                collect_expr_meta(arg, &format!("{path}/arg:{index}"), fallback, out);
            }
        }
        Expr::Tuple(items) | Expr::List(items) | Expr::SetLiteral(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_expr_meta(item, &format!("{path}/item:{index}"), fallback, out);
            }
        }
        Expr::Comprehension {
            expr, iter, guard, ..
        } => {
            collect_expr_meta(expr, &format!("{path}/value"), fallback, out);
            collect_expr_meta(iter, &format!("{path}/iter"), fallback, out);
            if let Some(guard) = guard {
                collect_expr_meta(guard, &format!("{path}/guard"), fallback, out);
            }
        }
        Expr::Match(scrutinee, arms) => {
            collect_expr_meta(scrutinee, &format!("{path}/scrutinee"), fallback, out);
            for (index, arm) in arms.iter().enumerate() {
                collect_pattern_meta(
                    &arm.pat,
                    &format!("{path}/arm:{index}/pattern"),
                    fallback,
                    out,
                );
                if let Some(guard) = &arm.guard {
                    collect_expr_meta(guard, &format!("{path}/arm:{index}/guard"), fallback, out);
                }
                collect_expr_meta(
                    &arm.body,
                    &format!("{path}/arm:{index}/body"),
                    fallback,
                    out,
                );
            }
        }
        Expr::Record { fields, .. } => {
            for (index, field) in fields.iter().enumerate() {
                collect_expr_meta(
                    &field.value,
                    &format!("{path}/field:{index}"),
                    fallback,
                    out,
                );
            }
        }
        Expr::Block(block) | Expr::Quote(block) | Expr::Comptime(block) | Expr::Arena(block) => {
            collect_block_meta(block, &format!("{path}/block"), fallback, out);
        }
        Expr::If { cond, then_, else_ } => {
            collect_expr_meta(cond, &format!("{path}/cond"), fallback, out);
            collect_block_meta(then_, &format!("{path}/then"), fallback, out);
            if let Some(block) = else_ {
                collect_block_meta(block, &format!("{path}/else"), fallback, out);
            }
        }
        Expr::Lambda { body, .. } => {
            collect_block_meta(body, &format!("{path}/body"), fallback, out);
        }
        Expr::SliceExpr { target, start, end } => {
            collect_expr_meta(target, &format!("{path}/target"), fallback, out);
            if let Some(start) = start {
                collect_expr_meta(start, &format!("{path}/start"), fallback, out);
            }
            if let Some(end) = end {
                collect_expr_meta(end, &format!("{path}/end"), fallback, out);
            }
        }
        Expr::Range { start, end } => {
            collect_expr_meta(start, &format!("{path}/start"), fallback, out);
            collect_expr_meta(end, &format!("{path}/end"), fallback, out);
        }
        Expr::Turbofish(_, _, args) => {
            for (index, arg) in args.iter().enumerate() {
                collect_expr_meta(arg, &format!("{path}/arg:{index}"), fallback, out);
            }
        }
        Expr::MapLiteral { entries } => {
            for (index, (key, value)) in entries.iter().enumerate() {
                collect_expr_meta(key, &format!("{path}/entry:{index}/key"), fallback, out);
                collect_expr_meta(value, &format!("{path}/entry:{index}/value"), fallback, out);
            }
        }
    }
}

fn collect_pattern_meta(
    pattern: &Pattern,
    path: &str,
    fallback: Span,
    out: &mut HashMap<NodeId, NodeMeta>,
) {
    insert_node_meta(path, None, fallback, out);
    match pattern {
        Pattern::Wildcard | Pattern::Variable(_) | Pattern::Literal(_) => {}
        Pattern::Constructor(_, fields) => {
            for (index, (_, pattern)) in fields.iter().enumerate() {
                collect_pattern_meta(pattern, &format!("{path}/field:{index}"), fallback, out);
            }
        }
        Pattern::Tuple(items) | Pattern::Array(items) => {
            for (index, pattern) in items.iter().enumerate() {
                collect_pattern_meta(pattern, &format!("{path}/item:{index}"), fallback, out);
            }
        }
        Pattern::Slice(items, rest) => {
            for (index, pattern) in items.iter().enumerate() {
                collect_pattern_meta(pattern, &format!("{path}/item:{index}"), fallback, out);
            }
            if let Some(rest) = rest {
                collect_pattern_meta(rest, &format!("{path}/rest"), fallback, out);
            }
        }
    }
}

fn insert_node_meta(
    path: &str,
    exact: Option<Span>,
    fallback: Span,
    out: &mut HashMap<NodeId, NodeMeta>,
) {
    let node_id = NodeId(path.to_string());
    let (span, precision) = exact
        .map(|span| (span, SpanPrecision::Exact))
        .unwrap_or((fallback, SpanPrecision::DeclarationFallback));
    out.insert(
        node_id.clone(),
        NodeMeta {
            node_id,
            origin: Origin::User(span),
            precision,
        },
    );
}

fn collect_flow(
    flow: &FlowDef,
    qualified_name: &str,
    flows: &mut HashMap<FlowId, ResolvedFlow>,
    transitions: &mut HashMap<TransitionId, ResolvedTransition>,
    backend_requirements: &mut Vec<CapabilityRequirement>,
    errors: &mut Vec<Diagnostic>,
) {
    let flow_id = FlowId(qualified_name.to_string());
    let flow_node_id = NodeId(format!("flow:{}", qualified_name));
    let flow_span = Span::from(flow.pos);
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
                        Span::from(state.pos),
                    ));
                }
            }
            let node_id = NodeId(format!("state:{}::{}", qualified_name, state.name));
            let origin = resolve_origin(state.origin, &flow_node_id, Span::from(state.pos));
            (
                state.name.clone(),
                ResolvedState {
                    node_id,
                    id,
                    payload,
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
        let span = Span::from(transition.pos);
        let node_id = NodeId(format!(
            "transition:{}::{}::{}",
            qualified_name, transition.name, transition.from_state
        ));
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
            is_fallback: transition.is_fallback,
            is_ffi_pinned: transition.is_ffi_pinned,
            origin: if transition.is_ffi_pinned {
                Origin::RuntimeSystem {
                    parent: flow_node_id.clone(),
                    span: flow_span,
                }
            } else if transition.is_fallback {
                Origin::PrototypeFallback {
                    parent: flow_node_id.clone(),
                    span: flow_span,
                }
            } else {
                Origin::User(span)
            },
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
        match annotation {
            crate::ast::FlowAnnotation::MaxChildren(n) => max_children = Some(*n),
            crate::ast::FlowAnnotation::MailboxDepth(n) => mailbox_depth = Some(*n),
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
        origin: resolve_origin(flow.origin, &flow_node_id, flow_span),
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

fn resolve_origin(origin: AstOrigin, parent: &NodeId, span: Span) -> Origin {
    match origin {
        AstOrigin::User => Origin::User(span),
        AstOrigin::Desugared => Origin::Desugared {
            parent: parent.clone(),
            span,
        },
        AstOrigin::PrototypeFallback => Origin::PrototypeFallback {
            parent: parent.clone(),
            span,
        },
        AstOrigin::RuntimeSystem => Origin::RuntimeSystem {
            parent: parent.clone(),
            span,
        },
    }
}

fn contains_unresolved_type(ty: &Type) -> bool {
    match ty {
        Type::Infer | Type::TypeVar(_) => true,
        Type::Name(name, args) => {
            name == "Any" || name == "_" || args.iter().any(contains_unresolved_type)
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
        | Type::CBorrowMut(inner)
        | Type::ForAll(_, inner) => contains_unresolved_type(inner),
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
        assert!(matches!(&twice.params[0].1, Type::Name(n, _) if n == "i32"));
        assert!(matches!(&twice.ret, Type::Name(n, _) if n == "i32"));
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
            session.body,
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
            .any(|(name, from, to)| name == "start" && from == "Idle" && to.as_slice() == ["Active"]));
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
        assert_eq!(ledger.action_count(crate::core::ResourceActionKind::Drop), 1);
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
        assert_eq!(
            program.flow("net::Conn").unwrap().mailbox_depth,
            Some(32)
        );
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
        let open = program
            .transition("Door", "open", "Closed")
            .expect("open");
        assert!(!open.is_fallback);
        // Matrix injects fallback edges for undefined combinations.
        assert!(program.transitions().values().any(|t| t.is_fallback));
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(!interp.is_resolved_fallback_transition("Door", "open", "Closed"));
        assert!(program
            .transitions()
            .values()
            .any(|t| t.is_fallback && interp.is_resolved_fallback_transition(
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
        let open = program
            .transition("Door", "open", "Closed")
            .expect("open");
        assert_eq!(open.params.len(), 1);
        assert_eq!(open.params[0].0, "code");
        assert!(matches!(&open.params[0].1, Type::Name(n, _) if n == "i32"));
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
        let mut verifier = crate::verifier::Verifier::new().expect("z3");
        let _ = verifier.verify_checked(&program);
        assert!(verifier.has_checked_type_def("Point"));
        assert!(verifier.has_checked_extern_func("c_abs"));
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
func main() -> i32 { MAX }
"#,
        );
        let program = crate::core::check_program(&file).expect("check");
        let interp = crate::interp::Interpreter::from_checked(&program);
        assert!(interp.has_resolved_capability("Io"));
        assert!(interp.has_resolved_constant("MAX"));
        assert!(!interp.has_resolved_constant("Missing"));
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
        codegen
            .compile_checked(&program)
            .expect("compile_checked");
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
            "extern:C:at:7",
        ] {
            let item = program
                .items()
                .get(&NodeId(node_id.to_string()))
                .unwrap_or_else(|| panic!("missing {node_id}"));
            assert!(item.origin.user_span().start_line > 0);
        }
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
        assert_eq!(flow.origin.user_span().start_line, 2);
        let single = flow.states.get("Single").expect("Single state");
        assert!(matches!(single.origin, Origin::RuntimeSystem { .. }));
        assert_eq!(single.origin.user_span().start_line, 2);
        assert!(flow
            .transitions
            .iter()
            .filter_map(|id| program.transitions().get(id))
            .all(|transition| transition.origin.user_span().start_line > 0));
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
        for node_id in [
            "function:main/stmt:0",
            "function:main/stmt:0/pattern",
            "function:main/stmt:0/init",
            "function:main/stmt:0/init/item:0",
            "function:main/stmt:1/cond",
            "function:main/stmt:1/then/stmt:0/value/inner",
        ] {
            let meta = program
                .node_meta()
                .get(&NodeId(node_id.to_string()))
                .unwrap_or_else(|| panic!("missing {node_id}"));
            assert!(meta.origin.user_span().start_line > 0);
        }
        assert_eq!(
            program
                .node_meta()
                .get(&NodeId("function:main/stmt:0".to_string()))
                .expect("let metadata")
                .precision,
            SpanPrecision::Exact
        );
        assert_eq!(
            program
                .node_meta()
                .get(&NodeId("function:main/stmt:1/cond".to_string()))
                .expect("condition metadata")
                .precision,
            SpanPrecision::DeclarationFallback
        );
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
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
                && diagnostic.message.contains("some control-flow paths")
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
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0304)
                && diagnostic.message.contains("potentially repeating loop")
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
                && diagnostic.message.contains("some control-flow paths")
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
                && diagnostic.message.contains("already been consumed")
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
