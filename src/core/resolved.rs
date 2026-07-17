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
    pub origin: Origin,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ResolvedFlow {
    pub node_id: NodeId,
    pub id: FlowId,
    pub states: HashMap<String, ResolvedState>,
    pub transitions: Vec<TransitionId>,
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

#[derive(Debug)]
pub struct CheckedProgram<'a> {
    file: &'a File,
    items: HashMap<NodeId, ResolvedItem>,
    node_meta: HashMap<NodeId, NodeMeta>,
    flows: HashMap<FlowId, ResolvedFlow>,
    transitions: HashMap<TransitionId, ResolvedTransition>,
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
        let mut backend_requirements = Vec::new();
        let mut errors = Vec::new();
        collect_items(
            &file.items,
            "",
            &mut items,
            &mut node_meta,
            &mut flows,
            &mut transitions,
            &mut backend_requirements,
            &mut errors,
        );
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Self {
            file,
            items,
            node_meta,
            flows,
            transitions,
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
        BackendProfile::Interpreter => true,
        BackendProfile::Native => !matches!(capability, "flow.multi_target" | "flow.transactional"),
        BackendProfile::Verifier => false,
        BackendProfile::Component => false,
    }
}

fn collect_items(
    items: &[Item],
    module: &str,
    resolved_items: &mut HashMap<NodeId, ResolvedItem>,
    node_meta: &mut HashMap<NodeId, NodeMeta>,
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
                insert_item(
                    resolved_items,
                    ResolvedItemKind::Function,
                    &qualified,
                    AstOrigin::User,
                    Span::from(function.pos),
                    errors,
                );
                collect_block_meta(
                    &function.body,
                    &format!("function:{}", qualified),
                    Span::from(function.pos),
                    node_meta,
                );
            }
            Item::Type(type_def) => {
                if let Some(pos) = type_def.decl_pos {
                    insert_item(
                        resolved_items,
                        ResolvedItemKind::Type,
                        &qualify(module, &type_def.name),
                        AstOrigin::User,
                        Span::from(pos),
                        errors,
                    );
                }
            }
            Item::Const { name, pos, .. } => insert_item(
                resolved_items,
                ResolvedItemKind::Constant,
                &qualify(module, name),
                AstOrigin::User,
                Span::from(*pos),
                errors,
            ),
            Item::Cap(cap) => insert_item(
                resolved_items,
                ResolvedItemKind::Capability,
                &qualify(module, &cap.name),
                cap.origin,
                Span::from(cap.pos),
                errors,
            ),
            Item::Trait(trait_def) => insert_item(
                resolved_items,
                ResolvedItemKind::Trait,
                &qualify(module, &trait_def.name),
                trait_def.origin,
                Span::from(trait_def.pos),
                errors,
            ),
            Item::Impl(impl_def) => insert_item(
                resolved_items,
                ResolvedItemKind::Impl,
                &qualify(
                    module,
                    &format!("{}:for:{}", impl_def.trait_name, impl_def.type_name),
                ),
                impl_def.origin,
                Span::from(impl_def.pos),
                errors,
            ),
            Item::ExternBlock(block) => insert_item(
                resolved_items,
                ResolvedItemKind::ExternBlock,
                &qualify(module, &format!("{}:at:{}", block.abi, block.pos.0)),
                block.origin,
                Span::from(block.pos),
                errors,
            ),
            Item::Actor(actor) => insert_item(
                resolved_items,
                ResolvedItemKind::Actor,
                &qualify(module, &actor.name),
                actor.origin,
                Span::from(actor.pos),
                errors,
            ),
            Item::Protocol(protocol) => insert_item(
                resolved_items,
                ResolvedItemKind::Protocol,
                &qualify(module, &protocol.name),
                protocol.origin,
                Span::from(protocol.pos),
                errors,
            ),
            Item::Session(session) => insert_item(
                resolved_items,
                ResolvedItemKind::Session,
                &qualify(module, &session.name),
                session.origin,
                Span::from(session.pos),
                errors,
            ),
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
    let resolved_flow = ResolvedFlow {
        node_id: flow_node_id.clone(),
        id: flow_id.clone(),
        states,
        transitions: flow_transition_ids,
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
        break
    }
    drop(f)
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
