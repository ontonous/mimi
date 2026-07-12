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
///
/// v0.29.17: collects all flow-state record shapes first so injected
/// reset/recover/Fault bodies can default nested subflow payloads correctly.
pub fn expand_file(file: &mut File) {
    let shapes = collect_record_shapes(&file.items);
    expand_items(&mut file.items, &shapes);
}

/// Collect unqualified state-name → payload fields for every flow state in `items`.
/// First declaration wins (same rule as checker unqualified state registration).
fn collect_record_shapes(items: &[Item]) -> HashMap<String, Vec<Field>> {
    let mut shapes = HashMap::new();
    collect_record_shapes_items(items, &mut shapes);
    shapes
}

fn collect_record_shapes_items(items: &[Item], shapes: &mut HashMap<String, Vec<Field>>) {
    for item in items {
        match item {
            Item::Flow(flow) => {
                for state in &flow.states {
                    if state.name == "Fault" {
                        continue;
                    }
                    shapes
                        .entry(state.name.clone())
                        .or_insert_with(|| state.payload.clone().unwrap_or_default());
                }
            }
            Item::Module(m) => collect_record_shapes_items(&m.items, shapes),
            Item::Type(td) => {
                if let TypeDefKind::Record(fields) = &td.kind {
                    shapes
                        .entry(td.name.clone())
                        .or_insert_with(|| fields.clone());
                }
            }
            _ => {}
        }
    }
}

fn expand_items(items: &mut [Item], shapes: &HashMap<String, Vec<Field>>) {
    for item in items.iter_mut() {
        match item {
            Item::Flow(flow) => expand_flow_with_shapes(flow, shapes),
            Item::Module(m) => expand_items(&mut m.items, shapes),
            _ => {}
        }
    }
}

/// Expand a single flow: ensure Fault exists, inject missing (state, event) → Fault,
/// and inject system verbs `reset` / `recover` (v0.29.13) when not user-defined.
pub fn expand_flow(flow: &mut FlowDef) {
    // Standalone path (unit tests): only this flow's shapes are known.
    let mut shapes = HashMap::new();
    for state in &flow.states {
        if state.name != "Fault" {
            shapes
                .entry(state.name.clone())
                .or_insert_with(|| state.payload.clone().unwrap_or_default());
        }
    }
    expand_flow_with_shapes(flow, &shapes);
}

fn expand_flow_with_shapes(flow: &mut FlowDef, shapes: &HashMap<String, Vec<Field>>) {
    // Always ensure Fault exists so recovery verbs have a source state.
    ensure_fault_state(flow);

    // Event name → params (first definition wins; overloads should share params).
    // Exclude system verbs from the N×M matrix:
    // - reset/recover only apply from Fault
    // - peer_fault is injected per-state (v0.29.20) rather than N×M-expanded
    let mut events: HashMap<String, Vec<Param>> = HashMap::new();
    for t in &flow.transitions {
        if t.name == "reset" || t.name == "recover" || t.name == "peer_fault" {
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
            let body = fault_return_body(flow, state, event, shapes);
            fallbacks.push(TransitionDef {
                name: event.clone(),
                from_state: state.clone(),
                params: params.clone(),
                to_states: vec!["Fault".to_string()],
                body: Some(body),
                pos: (0, 0),
                is_fallback: true,
                is_ffi_pinned: false,
            });
        }
    }
    flow.transitions.extend(fallbacks);

    // v0.29.13: inject reset / recover from Fault → root state.
    inject_system_verbs(flow, shapes);
    // v0.29.20: inject peer_fault(State) → Fault for every non-Fault state
    // that does not already define peer_fault (user self-loop breaks the chain).
    inject_peer_fault_verbs(flow, shapes);
    // v0.29.42: inject FFI_Pinned enter/exit/crash transitions when user
    // explicitly declares `state FFI_Pinned { ... }` in a flow.
    inject_ffi_pinned_transitions(flow, shapes);
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
fn rebuild_root_body(
    flow: &FlowDef,
    root: &str,
    keep_persistent: bool,
    shapes: &HashMap<String, Vec<Field>>,
) -> Block {
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
                        default_type_value(&f.ty, shapes)
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

/// Default expression for a type used by injected reset/recover/Fault bodies.
///
/// v0.29.17: when `ty` names a known record/state shape (including nested
/// subflow state payloads), emit a zeroed record literal instead of unit.
fn default_type_value(ty: &Type, shapes: &HashMap<String, Vec<Field>>) -> Expr {
    default_type_value_depth(ty, shapes, 0)
}

const MAX_DEFAULT_NESTING: usize = 8;

fn default_type_value_depth(
    ty: &Type,
    shapes: &HashMap<String, Vec<Field>>,
    depth: usize,
) -> Expr {
    match ty {
        Type::Name(n, _) if n == "string" || n == "String" => {
            Expr::Literal(Lit::String(String::new()))
        }
        Type::Name(n, _) if n == "i32" || n == "i64" || n == "Int" => Expr::Literal(Lit::Int(0)),
        Type::Name(n, _) if n == "f32" || n == "f64" || n == "Float" => {
            Expr::Literal(Lit::Float(0.0))
        }
        Type::Name(n, _) if n == "bool" || n == "Bool" => Expr::Literal(Lit::Bool(false)),
        Type::Name(n, _) if n == "SystemTrace" => system_trace_expr("", "", ""),
        Type::Name(n, _) if shapes.contains_key(n) => {
            if depth >= MAX_DEFAULT_NESTING {
                // Cycle / pathological depth — fall back to unit rather than stack overflow.
                return Expr::Literal(Lit::Unit);
            }
            // Nested state / record payload: zero every field recursively.
            let fields = shapes.get(n).cloned().unwrap_or_default();
            Expr::Record {
                ty: Some(n.clone()),
                fields: fields
                    .into_iter()
                    .map(|f| RecordFieldExpr {
                        name: f.name,
                        value: default_type_value_depth(&f.ty, shapes, depth + 1),
                    })
                    .collect(),
            }
        }
        Type::Name(n, args) if n == "List" || n == "list" => {
            // Empty list default (args ignored for the empty literal).
            let _ = args;
            Expr::List(vec![])
        }
        _ => Expr::Literal(Lit::Unit),
    }
}

/// Inject `peer_fault` → Fault for every non-Fault state missing a user handler.
///
/// v0.29.20 PeerFault cascade default: unhandled peer disconnect becomes Fault.
/// User-written `transition peer_fault(State) -> State` (self-loop) or any
/// other target is never overridden — that is the explicit break-chain form.
fn inject_peer_fault_verbs(flow: &mut FlowDef, shapes: &HashMap<String, Vec<Field>>) {
    let states: Vec<String> = flow
        .states
        .iter()
        .filter(|s| s.name != "Fault")
        .map(|s| s.name.clone())
        .collect();
    // Collect (from_state) that already have a user/injected peer_fault.
    let defined: HashSet<String> = flow
        .transitions
        .iter()
        .filter(|t| t.name == "peer_fault")
        .map(|t| t.from_state.clone())
        .collect();

    let mut injected = Vec::new();
    for state in states {
        if defined.contains(&state) {
            continue;
        }
        // Default cascade: peer_fault(State) → Fault with SystemTrace payload.
        let body = fault_return_body(flow, &state, "peer_fault", shapes);
        injected.push(TransitionDef {
            name: "peer_fault".to_string(),
            from_state: state,
            params: vec![],
            to_states: vec!["Fault".to_string()],
            body: Some(body),
            pos: (0, 0),
            is_fallback: true,
            is_ffi_pinned: false,
        });
    }
    flow.transitions.extend(injected);
}

/// v0.29.42: Inject FFI_Pinned enter/exit/crash transitions when the user
/// explicitly declares `state FFI_Pinned { ... }` in a flow.
///
/// White-paper §4.2: "对于需要精细控制 FFI 生命周期的底层库作者，显式的
/// FFI_Pinned 状态声明仍然可用，且与块级语法糖完全共存"
///
/// Injects:
///   - `transition enter_ffi(Active) -> FFI_Pinned` — payload passthrough
///   - `transition exit_ffi(FFI_Pinned) -> Active` — payload passthrough
///   - `transition ffi_crash(FFI_Pinned) -> Fault` — fallback (is_fallback=true)
///
/// User-written transitions from FFI_Pinned are never overridden.
fn inject_ffi_pinned_transitions(
    flow: &mut FlowDef,
    shapes: &HashMap<String, Vec<Field>>,
) {
    // Only act if the user explicitly declared an FFI_Pinned state.
    if !flow.states.iter().any(|s| s.name == "FFI_Pinned") {
        return;
    }

    // Determine the "Active" state: first non-Fault, non-FFI_Pinned state.
    let active = flow
        .states
        .iter()
        .find(|s| s.name != "Fault" && s.name != "FFI_Pinned")
        .map(|s| s.name.clone());
    let Some(active) = active else {
        return; // No active state to wire transitions from/to.
    };

    // Build payload-passthrough body: return Target { field: self.field, ... }
    let make_passthrough_body = |target: &str| -> Block {
        let fields = flow
            .states
            .iter()
            .find(|s| s.name == target)
            .and_then(|s| s.payload.as_ref())
            .map(|payload| {
                payload
                    .iter()
                    .map(|f| RecordFieldExpr {
                        name: f.name.clone(),
                        value: Expr::Field(
                            Box::new(Expr::Ident("self".to_string())),
                            f.name.clone(),
                        ),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        vec![Stmt::Return(Some(Expr::Record {
            ty: Some(target.to_string()),
            fields,
        }))]
    };

    // enter_ffi: Active → FFI_Pinned (if not already user-defined)
    let has_enter = flow
        .transitions
        .iter()
        .any(|t| t.name == "enter_ffi" && t.from_state == active);
    if !has_enter {
        flow.transitions.push(TransitionDef {
            name: "enter_ffi".to_string(),
            from_state: active.clone(),
            params: vec![],
            to_states: vec!["FFI_Pinned".to_string()],
            body: Some(make_passthrough_body("FFI_Pinned")),
            pos: (0, 0),
            is_fallback: false,
            is_ffi_pinned: true,
        });
    }

    // exit_ffi: FFI_Pinned → Active (if not already user-defined)
    let has_exit = flow
        .transitions
        .iter()
        .any(|t| t.name == "exit_ffi" && t.from_state == "FFI_Pinned");
    if !has_exit {
        flow.transitions.push(TransitionDef {
            name: "exit_ffi".to_string(),
            from_state: "FFI_Pinned".to_string(),
            params: vec![],
            to_states: vec![active.clone()],
            body: Some(make_passthrough_body(&active)),
            pos: (0, 0),
            is_fallback: false,
            is_ffi_pinned: true,
        });
    }

    // ffi_crash: FFI_Pinned → Fault (fallback — always injected if missing)
    let has_crash = flow
        .transitions
        .iter()
        .any(|t| t.name == "ffi_crash" && t.from_state == "FFI_Pinned");
    if !has_crash {
        let body = fault_return_body(flow, "FFI_Pinned", "ffi_crash", shapes);
        flow.transitions.push(TransitionDef {
            name: "ffi_crash".to_string(),
            from_state: "FFI_Pinned".to_string(),
            params: vec![],
            to_states: vec!["Fault".to_string()],
            body: Some(body),
            pos: (0, 0),
            is_fallback: true,
            is_ffi_pinned: true,
        });
    }
}
fn inject_system_verbs(flow: &mut FlowDef, shapes: &HashMap<String, Vec<Field>>) {
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
        let body = rebuild_root_body(flow, &root, false, shapes);
        flow.transitions.push(TransitionDef {
            name: "reset".to_string(),
            from_state: "Fault".to_string(),
            params: vec![],
            to_states: vec![root.clone()],
            body: Some(body),
            pos: (0, 0),
            is_fallback: true,
            is_ffi_pinned: false,
        });
    }
    if !has_recover {
        // recover: rebuild root, pulling persistent fields from Fault shadow copy.
        // When no persistent fields exist, recover == reset (still provided for API).
        // H2: codegen uses this body as-is (no separate dirty check). Interp may
        // further degrade to reset when non-transactional persistent fields were
        // dirtied mid-turn (`persistent_dirty_for_recover`); that path needs a
        // live WAL snapshot which only the interpreter maintains.
        let keep = !flow.persistent_fields.is_empty();
        let body = rebuild_root_body(flow, &root, keep, shapes);
        flow.transitions.push(TransitionDef {
            name: "recover".to_string(),
            from_state: "Fault".to_string(),
            params: vec![],
            to_states: vec![root],
            body: Some(body),
            pos: (0, 0),
            is_fallback: true,
            is_ffi_pinned: false,
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
            // M2-fix: when the persistent field type cannot be resolved from
            // any state payload, default to a unit type instead of i32.
            // Previously defaulted to i32, which silently created a type mismatch
            // if the field was actually a different type.
            .unwrap_or_else(|| Type::Name("unit".to_string(), vec![]));
        payload.push(Field {
            name: name.clone(),
            ty,
        });
    }
}

/// Build a `SystemTrace { last_state_name, unexpected_event, snapshot, memory_dump, panic_payload }` record.
/// v0.29.39: added memory_dump + panic_payload structured sub-records.
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
            // v0.29.39: MemoryDump
            RecordFieldExpr {
                name: "memory_dump".to_string(),
                value: Expr::Record {
                    ty: Some("MemoryDump".to_string()),
                    fields: vec![
                        RecordFieldExpr {
                            name: "fields".to_string(),
                            value: Expr::Literal(Lit::String(String::new())),
                        },
                        RecordFieldExpr {
                            name: "count".to_string(),
                            value: Expr::Literal(Lit::Int(0)),
                        },
                    ],
                },
            },
            // v0.29.39: PanicPayload
            RecordFieldExpr {
                name: "panic_payload".to_string(),
                value: Expr::Record {
                    ty: Some("PanicPayload".to_string()),
                    fields: vec![
                        RecordFieldExpr {
                            name: "error_type".to_string(),
                            value: Expr::Literal(Lit::String(event.to_string())),
                        },
                        RecordFieldExpr {
                            name: "file".to_string(),
                            value: Expr::Literal(Lit::String(String::new())),
                        },
                        RecordFieldExpr {
                            name: "line".to_string(),
                            value: Expr::Literal(Lit::Int(0)),
                        },
                        RecordFieldExpr {
                            name: "stack".to_string(),
                            value: Expr::Literal(Lit::String(snapshot.to_string())),
                        },
                    ],
                },
            },
        ],
    }
}

/// Build `return Fault { ... }` matching the Fault state's payload shape.
/// Persistent fields are shadowed from `self.<name>` so recover can restore them.
fn fault_return_body(
    flow: &FlowDef,
    from_state: &str,
    event: &str,
    shapes: &HashMap<String, Vec<Field>>,
) -> Block {
    // M1-fix: When Fault→Fault (i.e. the Fault state receives another event),
    // preserve the existing SystemTrace by reading from self.trace instead
    // of constructing a new one.
    let is_fault_to_fault = from_state == "Fault";
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
                    } else if is_fault_to_fault
                        && matches!(
                            f.name.as_str(),
                            "trace" | "last_state" | "unexpected_event" | "snapshot"
                        )
                    {
                        // M1-fix: Fault→Fault preserves the original SystemTrace
                        // by reading from self instead of constructing a new one.
                        Expr::Field(Box::new(Expr::Ident("self".to_string())), f.name.clone())
                    } else {
                        default_field_value(&f.name, &f.ty, from_state, event, &snapshot, shapes)
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
    shapes: &HashMap<String, Vec<Field>>,
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
        // v0.29.39: MemoryDump field in SystemTrace
        "memory_dump" => {
            if matches!(ty, Type::Name(n, _) if n == "MemoryDump") {
                return Expr::Record {
                    ty: Some("MemoryDump".to_string()),
                    fields: vec![
                        RecordFieldExpr {
                            name: "fields".to_string(),
                            value: Expr::Literal(Lit::String(String::new())),
                        },
                        RecordFieldExpr {
                            name: "count".to_string(),
                            value: Expr::Literal(Lit::Int(0)),
                        },
                    ],
                };
            }
        }
        // v0.29.39: PanicPayload field in SystemTrace
        "panic_payload" => {
            if matches!(ty, Type::Name(n, _) if n == "PanicPayload") {
                return Expr::Record {
                    ty: Some("PanicPayload".to_string()),
                    fields: vec![
                        RecordFieldExpr {
                            name: "error_type".to_string(),
                            value: Expr::Literal(Lit::String(event.to_string())),
                        },
                        RecordFieldExpr {
                            name: "file".to_string(),
                            value: Expr::Literal(Lit::String(String::new())),
                        },
                        RecordFieldExpr {
                            name: "line".to_string(),
                            value: Expr::Literal(Lit::Int(0)),
                        },
                        RecordFieldExpr {
                            name: "stack".to_string(),
                            value: Expr::Literal(Lit::String(snapshot.to_string())),
                        },
                    ],
                };
            }
        }
        _ => {}
    }
    // Shared path: scalars + nested record/state shapes (v0.29.17).
    default_type_value(ty, shapes)
}

/// Build a runtime Fault value with full SystemTrace (used by panic→Fault path).
/// v0.29.39: SystemTrace now includes memory_dump + panic_payload.
/// Persistent field shadows are filled by the caller via `shadow_persistent_into_fault`.
pub fn make_fault_value(from_state: &str, event: &str, snapshot: &str) -> crate::interp::Value {
    use crate::interp::Value;
    use std::collections::HashMap;

    // v0.29.39: PanicPayload sub-record
    let mut panic_payload = HashMap::new();
    panic_payload.insert("error_type".to_string(), Value::String(event.to_string()));
    panic_payload.insert("file".to_string(), Value::String(String::new()));
    panic_payload.insert("line".to_string(), Value::Int(0));
    panic_payload.insert("stack".to_string(), Value::String(snapshot.to_string()));

    // v0.29.39: MemoryDump sub-record (field summary)
    // v0.29.44: populated with actual from_state field names when available
    let mut memory_dump = HashMap::new();
    let dump_fields = format!("from_state={};event={}", from_state, event);
    memory_dump.insert("fields".to_string(), Value::String(dump_fields));
    memory_dump.insert("count".to_string(), Value::Int(2));

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
    trace.insert(
        "memory_dump".to_string(),
        Value::Record(Some("MemoryDump".to_string()), memory_dump),
    );
    trace.insert(
        "panic_payload".to_string(),
        Value::Record(Some("PanicPayload".to_string()), panic_payload),
    );

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


/// Build a PeerFault record value (v0.29.20).
pub fn make_peer_fault_value(peer_id: &str, reason: &str) -> crate::interp::Value {
    use crate::interp::Value;
    use std::collections::HashMap;
    let mut fields = HashMap::new();
    fields.insert("peer_id".to_string(), Value::String(peer_id.to_string()));
    fields.insert("reason".to_string(), Value::String(reason.to_string()));
    Value::Record(Some("PeerFault".to_string()), fields)
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
                is_ffi_pinned: false,
            }],
            impl_protocols: vec![],
            persistent_fields: vec![],
            transactional_fields: vec![],
            metadata_shadow_fields: vec![],
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
            is_ffi_pinned: false,
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
