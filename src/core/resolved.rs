use crate::ast::{AstOrigin, File, FlowDef, Item, Type};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use std::collections::HashMap;

pub const RESOLVED_IR_VERSION: &str = "mimi-resolved-ir-1";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

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
    flows: HashMap<FlowId, ResolvedFlow>,
    transitions: HashMap<TransitionId, ResolvedTransition>,
    backend_requirements: Vec<CapabilityRequirement>,
}

impl<'a> CheckedProgram<'a> {
    pub(crate) fn from_checked_file(file: &'a File) -> Result<Self, Vec<Diagnostic>> {
        let mut transitions = HashMap::new();
        let mut flows = HashMap::new();
        let mut backend_requirements = Vec::new();
        let mut errors = Vec::new();
        collect_items(
            &file.items,
            "",
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
            flows,
            transitions,
            backend_requirements,
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
    flows: &mut HashMap<FlowId, ResolvedFlow>,
    transitions: &mut HashMap<TransitionId, ResolvedTransition>,
    backend_requirements: &mut Vec<CapabilityRequirement>,
    errors: &mut Vec<Diagnostic>,
) {
    for item in items {
        match item {
            Item::Module(def) => {
                let qualified = qualify(module, &def.name);
                collect_items(
                    &def.items,
                    &qualified,
                    flows,
                    transitions,
                    backend_requirements,
                    errors,
                );
            }
            Item::Flow(flow) => collect_flow(
                flow,
                &qualify(module, &flow.name),
                flows,
                transitions,
                backend_requirements,
                errors,
            ),
            _ => {}
        }
    }
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
                .collect();
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
}

fn qualify(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{}::{}", module, name)
    }
}
