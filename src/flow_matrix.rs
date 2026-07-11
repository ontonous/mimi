//! Flow transfer-matrix auto-completion (+1 Fault fallback).
//!
//! For every user-defined flow, the compiler expands the N×M matrix of
//! (state, event) pairs. Any combination not written by the programmer is
//! filled with an implicit `transition event(State) -> Fault` that returns a
//! Fault payload (or the user-defined Fault shape with field defaults).
//!
//! This is the v0.29.10 foundation for the Fault absorbing state. Later
//! versions (0.29.11+) deepen Fault semantics (auto-drop, mailbox short-circuit,
//! SystemTrace, Reset/Recover).

use crate::ast::*;
use std::collections::{HashMap, HashSet};

/// Expand every `flow` in `file` in place (including nested modules).
pub fn expand_file(file: &mut File) {
    expand_items(&mut file.items);
}

fn expand_items(items: &mut [Item]) {
    for item in items.iter_mut() {
        match item {
            Item::Flow(flow) => expand_flow(flow),
            Item::Module(m) => expand_items(&mut m.items),
            _ => {}
        }
    }
}

/// Expand a single flow: ensure Fault exists, inject missing (state, event) → Fault,
/// and inject system verbs `reset` / `recover` (v0.29.13) when not user-defined.
pub fn expand_flow(flow: &mut FlowDef) {
    // Always ensure Fault exists so recovery verbs have a source state.
    ensure_fault_state(flow);

    // Event name → params (first definition wins; overloads should share params).
    // Exclude system verbs from the N×M matrix — they only apply from Fault.
    let mut events: HashMap<String, Vec<Param>> = HashMap::new();
    for t in &flow.transitions {
        if t.name == "reset" || t.name == "recover" {
            continue;
        }
        events.entry(t.name.clone()).or_insert_with(|| t.params.clone());
    }

    // Already-defined (from_state, event) pairs — never override user code.
    let defined: HashSet<(String, String)> = flow
        .transitions
        .iter()
        .map(|t| (t.from_state.clone(), t.name.clone()))
        .collect();

    let state_names: Vec<String> = flow.states.iter().map(|s| s.name.clone()).collect();

    let mut fallbacks: Vec<TransitionDef> = Vec::new();
    for state in &state_names {
        for (event, params) in &events {
            if defined.contains(&(state.clone(), event.clone())) {
                continue;
            }
            let body = fault_return_body(flow, state, event);
            fallbacks.push(TransitionDef {
                name: event.clone(),
                from_state: state.clone(),
                params: params.clone(),
                to_states: vec!["Fault".to_string()],
                body: Some(body),
                pos: (0, 0),
                is_fallback: true,
            });
        }
    }
    flow.transitions.extend(fallbacks);

    // v0.29.13: inject reset / recover from Fault → root state.
    inject_system_verbs(flow);
}

/// Root (initial) state of a flow: first non-Fault declared state.
fn root_state_name(flow: &FlowDef) -> Option<String> {
    flow.states
        .iter()
        .find(|s| s.name != "Fault")
        .map(|s| s.name.clone())
}

/// Build `return Root { fields... }` using defaults, overlaying `self.<persistent>`
/// when `keep_persistent` is true (recover path).
fn rebuild_root_body(flow: &FlowDef, root: &str, keep_persistent: bool) -> Block {
    let fields = flow
        .states
        .iter()
        .find(|s| s.name == root)
        .and_then(|s| s.payload.as_ref())
        .map(|payload| {
            payload
                .iter()
                .map(|f| {
                    let value = if keep_persistent && flow.persistent_fields.contains(&f.name) {
                        // Pull surviving value off the Fault payload (shadowed there).
                        Expr::Field(Box::new(Expr::Ident("self".to_string())), f.name.clone())
                    } else {
                        default_type_value(&f.ty)
                    };
                    RecordFieldExpr {
                        name: f.name.clone(),
                        value,
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    vec![Stmt::Return(Some(Expr::Record {
        ty: Some(root.to_string()),
        fields,
    }))]
}

fn default_type_value(ty: &Type) -> Expr {
    match ty {
        Type::Name(n, _) if n == "string" || n == "String" => {
            Expr::Literal(Lit::String(String::new()))
        }
        Type::Name(n, _) if n == "i32" || n == "i64" || n == "Int" => Expr::Literal(Lit::Int(0)),
        Type::Name(n, _) if n == "f32" || n == "f64" || n == "Float" => {
            Expr::Literal(Lit::Float(0.0))
        }
        Type::Name(n, _) if n == "bool" || n == "Bool" => Expr::Literal(Lit::Bool(false)),
        Type::Name(n, _) if n == "SystemTrace" => {
            system_trace_expr("", "", "")
        }
        _ => Expr::Literal(Lit::Unit),
    }
}

/// Inject `reset` and `recover` transitions from Fault → root when absent.
fn inject_system_verbs(flow: &mut FlowDef) {
    let Some(root) = root_state_name(flow) else {
        return;
    };
    let has_reset = flow
        .transitions
        .iter()
        .any(|t| t.name == "reset" && t.from_state == "Fault");
    let has_recover = flow
        .transitions
        .iter()
        .any(|t| t.name == "recover" && t.from_state == "Fault");

    if !has_reset {
        // reset: rebuild root from type defaults (SystemTrace destroyed by not copying).
        let body = rebuild_root_body(flow, &root, false);
        flow.transitions.push(TransitionDef {
            name: "reset".to_string(),
            from_state: "Fault".to_string(),
            params: vec![],
            to_states: vec![root.clone()],
            body: Some(body),
            pos: (0, 0),
            is_fallback: true,
        });
    }
    if !has_recover {
        // recover: rebuild root, pulling persistent fields from Fault shadow copy.
        // When no persistent fields exist, recover == reset (still provided for API).
        let keep = !flow.persistent_fields.is_empty();
        let body = rebuild_root_body(flow, &root, keep);
        flow.transitions.push(TransitionDef {
            name: "recover".to_string(),
            from_state: "Fault".to_string(),
            params: vec![],
            to_states: vec![root],
            body: Some(body),
            pos: (0, 0),
            is_fallback: true,
        });
    }
}

/// Ensure a `Fault` state exists.
///
/// v0.29.12 SystemTrace shape:
/// ```text
/// state Fault {
///   last_state: string,       // alias: last_state_name
///   unexpected_event: string, // event name or "panic:<type>"
///   snapshot: string,         // compact reason / stack summary
///   trace: SystemTrace,       // structured { last_state_name, unexpected_event, snapshot }
///   // + shadow copies of flow.persistent_fields for recover (v0.29.13)
/// }
/// ```
/// Flat fields keep existing MCDD / dual-backend tests working; `trace` is the
/// structured view for user `match self.trace` recovery paths.
fn ensure_fault_state(flow: &mut FlowDef) {
    if !flow.states.iter().any(|s| s.name == "Fault") {
        flow.states.push(StateDef {
            name: "Fault".to_string(),
            payload: Some(vec![
                Field {
                    name: "last_state".to_string(),
                    ty: Type::Name("string".to_string(), vec![]),
                },
                Field {
                    name: "unexpected_event".to_string(),
                    ty: Type::Name("string".to_string(), vec![]),
                },
                Field {
                    name: "snapshot".to_string(),
                    ty: Type::Name("string".to_string(), vec![]),
                },
                Field {
                    name: "trace".to_string(),
                    ty: Type::Name("SystemTrace".to_string(), vec![]),
                },
            ]),
        });
    }
    // Attach persistent-field shadows so recover can read them off Fault.
    attach_persistent_shadows(flow);
}

/// Add each `persistent_fields` entry onto Fault payload if missing.
fn attach_persistent_shadows(flow: &mut FlowDef) {
    if flow.persistent_fields.is_empty() {
        return;
    }
    // Resolve types from any state that declares the field.
    let mut types: HashMap<String, Type> = HashMap::new();
    for s in &flow.states {
        if let Some(payload) = &s.payload {
            for f in payload {
                if flow.persistent_fields.contains(&f.name) {
                    types.entry(f.name.clone()).or_insert_with(|| f.ty.clone());
                }
            }
        }
    }
    let fault = match flow.states.iter_mut().find(|s| s.name == "Fault") {
        Some(s) => s,
        None => return,
    };
    let payload = fault.payload.get_or_insert_with(Vec::new);
    for name in &flow.persistent_fields {
        if payload.iter().any(|f| &f.name == name) {
            continue;
        }
        let ty = types
            .get(name)
            .cloned()
            .unwrap_or_else(|| Type::Name("i32".to_string(), vec![]));
        payload.push(Field {
            name: name.clone(),
            ty,
        });
    }
}

/// Build a `SystemTrace { last_state_name, unexpected_event, snapshot }` record.
pub fn system_trace_expr(from_state: &str, event: &str, snapshot: &str) -> Expr {
    Expr::Record {
        ty: Some("SystemTrace".to_string()),
        fields: vec![
            RecordFieldExpr {
                name: "last_state_name".to_string(),
                value: Expr::Literal(Lit::String(from_state.to_string())),
            },
            RecordFieldExpr {
                name: "unexpected_event".to_string(),
                value: Expr::Literal(Lit::String(event.to_string())),
            },
            RecordFieldExpr {
                name: "snapshot".to_string(),
                value: Expr::Literal(Lit::String(snapshot.to_string())),
            },
        ],
    }
}

/// Build `return Fault { ... }` matching the Fault state's payload shape.
/// Persistent fields are shadowed from `self.<name>` so recover can restore them.
fn fault_return_body(flow: &FlowDef, from_state: &str, event: &str) -> Block {
    let snapshot = format!("undefined transition {}({})", event, from_state);
    // Fields available on the from-state payload (for persistent shadowing).
    let from_fields: HashSet<String> = flow
        .states
        .iter()
        .find(|s| s.name == from_state)
        .and_then(|s| s.payload.as_ref())
        .map(|p| p.iter().map(|f| f.name.clone()).collect())
        .unwrap_or_default();

    let fields = flow
        .states
        .iter()
        .find(|s| s.name == "Fault")
        .and_then(|s| s.payload.as_ref())
        .map(|payload| {
            payload
                .iter()
                .map(|f| {
                    let value = if flow.persistent_fields.contains(&f.name)
                        && from_fields.contains(&f.name)
                    {
                        // Shadow copy from abandoned state.
                        Expr::Field(Box::new(Expr::Ident("self".to_string())), f.name.clone())
                    } else {
                        default_field_value(&f.name, &f.ty, from_state, event, &snapshot)
                    };
                    RecordFieldExpr {
                        name: f.name.clone(),
                        value,
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    vec![Stmt::Return(Some(Expr::Record {
        ty: Some("Fault".to_string()),
        fields,
    }))]
}

fn default_field_value(
    field: &str,
    ty: &Type,
    from_state: &str,
    event: &str,
    snapshot: &str,
) -> Expr {
    // Prefer SystemTrace semantics for well-known field names.
    match field {
        "last_state" | "last_state_name" => {
            return Expr::Literal(Lit::String(from_state.to_string()));
        }
        "unexpected_event" => {
            return Expr::Literal(Lit::String(event.to_string()));
        }
        "snapshot" => {
            return Expr::Literal(Lit::String(snapshot.to_string()));
        }
        "trace" => {
            // Structured SystemTrace record (v0.29.12).
            if matches!(ty, Type::Name(n, _) if n == "SystemTrace" || n == "string" || n == "String")
            {
                if matches!(ty, Type::Name(n, _) if n == "SystemTrace") {
                    return system_trace_expr(from_state, event, snapshot);
                }
            }
            // User-defined Fault { trace: string } — encode a compact reason.
            return Expr::Literal(Lit::String(snapshot.to_string()));
        }
        _ => {}
    }
    match ty {
        Type::Name(n, _) if n == "SystemTrace" => system_trace_expr(from_state, event, snapshot),
        Type::Name(n, _) if n == "string" || n == "String" => {
            Expr::Literal(Lit::String(String::new()))
        }
        Type::Name(n, _) if n == "i32" || n == "i64" || n == "Int" => Expr::Literal(Lit::Int(0)),
        Type::Name(n, _) if n == "f32" || n == "f64" || n == "Float" => {
            Expr::Literal(Lit::Float(0.0))
        }
        Type::Name(n, _) if n == "bool" || n == "Bool" => Expr::Literal(Lit::Bool(false)),
        // Best-effort: empty unit for unknown shapes (type checker will report if bad).
        _ => Expr::Literal(Lit::Unit),
    }
}

/// Build a runtime Fault value with full SystemTrace (used by panic→Fault path).
/// Persistent field shadows are filled by the caller via `shadow_persistent_into_fault`.
pub fn make_fault_value(from_state: &str, event: &str, snapshot: &str) -> crate::interp::Value {
    use crate::interp::Value;
    use std::collections::HashMap;

    let mut trace = HashMap::new();
    trace.insert(
        "last_state_name".to_string(),
        Value::String(from_state.to_string()),
    );
    trace.insert(
        "unexpected_event".to_string(),
        Value::String(event.to_string()),
    );
    trace.insert("snapshot".to_string(), Value::String(snapshot.to_string()));

    let mut fields = HashMap::new();
    fields.insert(
        "last_state".to_string(),
        Value::String(from_state.to_string()),
    );
    fields.insert(
        "unexpected_event".to_string(),
        Value::String(event.to_string()),
    );
    fields.insert("snapshot".to_string(), Value::String(snapshot.to_string()));
    fields.insert(
        "trace".to_string(),
        Value::Record(Some("SystemTrace".to_string()), trace),
    );
    Value::Record(Some("Fault".to_string()), fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_flow() -> FlowDef {
        FlowDef {
            name: "Counter".to_string(),
            pub_: false,
            generics: vec![],
            annotations: vec![],
            states: vec![
                StateDef {
                    name: "Zero".to_string(),
                    payload: Some(vec![Field {
                        name: "count".to_string(),
                        ty: Type::Name("i32".to_string(), vec![]),
                    }]),
                },
                StateDef {
                    name: "Positive".to_string(),
                    payload: Some(vec![Field {
                        name: "count".to_string(),
                        ty: Type::Name("i32".to_string(), vec![]),
                    }]),
                },
            ],
            transitions: vec![TransitionDef {
                name: "inc".to_string(),
                from_state: "Zero".to_string(),
                params: vec![],
                to_states: vec!["Positive".to_string()],
                body: Some(vec![Stmt::Return(Some(Expr::Record {
                    ty: Some("Positive".to_string()),
                    fields: vec![RecordFieldExpr {
                        name: "count".to_string(),
                        value: Expr::Literal(Lit::Int(1)),
                    }],
                }))]),
                pos: (1, 1),
                is_fallback: false,
            }],
            impl_protocols: vec![],
            persistent_fields: vec![],
        }
    }

    #[test]
    fn injects_fault_and_missing_cells() {
        let mut flow = sample_flow();
        expand_flow(&mut flow);
        assert!(
            flow.states.iter().any(|s| s.name == "Fault"),
            "Fault state must be injected"
        );
        // Defined: Zero+inc. Missing: Positive+inc, Fault+inc + reset/recover.
        let fallbacks: Vec<_> = flow.transitions.iter().filter(|t| t.is_fallback).collect();
        assert!(fallbacks.len() >= 4, "expected ≥4 fallbacks, got {:?}", fallbacks);
        assert!(fallbacks.iter().any(|t| t.from_state == "Positive" && t.name == "inc"));
        assert!(fallbacks.iter().any(|t| t.from_state == "Fault" && t.name == "inc"));
        assert!(fallbacks.iter().any(|t| t.name == "reset" && t.from_state == "Fault"));
        assert!(fallbacks.iter().any(|t| t.name == "recover" && t.from_state == "Fault"));
        // User transition preserved.
        assert!(
            flow.transitions
                .iter()
                .any(|t| !t.is_fallback && t.from_state == "Zero" && t.name == "inc")
        );
    }

    #[test]
    fn does_not_override_user_fault_transition() {
        let mut flow = sample_flow();
        flow.states.push(StateDef {
            name: "Fault".to_string(),
            payload: Some(vec![Field {
                name: "trace".to_string(),
                ty: Type::Name("string".to_string(), vec![]),
            }]),
        });
        flow.transitions.push(TransitionDef {
            name: "inc".to_string(),
            from_state: "Positive".to_string(),
            params: vec![],
            to_states: vec!["Fault".to_string()],
            body: Some(vec![Stmt::Return(Some(Expr::Record {
                ty: Some("Fault".to_string()),
                fields: vec![RecordFieldExpr {
                    name: "trace".to_string(),
                    value: Expr::Literal(Lit::String("user".into())),
                }],
            }))]),
            pos: (2, 1),
            is_fallback: false,
        });
        expand_flow(&mut flow);
        // Positive+inc is user-defined — not a fallback.
        let pos_inc = flow
            .transitions
            .iter()
            .find(|t| t.from_state == "Positive" && t.name == "inc")
            .expect("Positive+inc");
        assert!(!pos_inc.is_fallback);
        // Event fallbacks: only Fault+inc (plus reset/recover system verbs).
        let event_fb: Vec<_> = flow
            .transitions
            .iter()
            .filter(|t| t.is_fallback && t.name == "inc")
            .collect();
        assert_eq!(event_fb.len(), 1);
        assert_eq!(event_fb[0].from_state, "Fault");
        assert!(flow.transitions.iter().any(|t| t.name == "reset" && t.is_fallback));
    }
}
