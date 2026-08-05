//! Session Types compiler skeleton (v0.29.19).
//!
//! Provides:
//! - dualization of linear session types (`!T.S` ↔ `?T.S`, `end` ↔ `end`)
//! - well-formedness / name resolution for `session Name = ...` declarations
//! - compile-time order checking for channel endpoints typed as
//!   `SessionChan<S>`: `session_send` / `session_recv` / `session_close`
//!   must follow the residual protocol prefix.
//!
//! Full channel-runtime integration (endpoint allocation, dual pairing) is
//! deferred; this module is the static skeleton AGENTS.md §13.18.3 targets.

use crate::ast::{SessionType, Type};
use std::collections::HashMap;

use crate::core::helpers::types_compatible;

fn fmt_type(ty: &Type) -> String {
    match ty.unlocated() {
        Type::Name(n, args) if args.is_empty() => n.clone(),
        Type::Name(n, args) => {
            let inner: Vec<String> = args.iter().map(fmt_type).collect();
            format!("{}[{}]", n, inner.join(", "))
        }
        Type::Option(i) => format!("Option[{}]", fmt_type(i)),
        Type::Result(a, b) => format!("Result[{}, {}]", fmt_type(a), fmt_type(b)),
        Type::Tuple(es) => {
            let inner: Vec<String> = es.iter().map(fmt_type).collect();
            format!("({})", inner.join(", "))
        }
        _ => format!("{:?}", ty),
    }
}

/// Compute the dual of a session type.
///
/// ```text
/// dual(!T . S) = ?T . dual(S)
/// dual(?T . S) = !T . dual(S)
/// dual(end)    = end
/// dual(dual(S))= S            (involution, simplified)
/// dual(Name)   = dual(Name)   (kept symbolic until resolved)
/// ```
pub fn dual(s: &SessionType) -> SessionType {
    match s.unlocated() {
        SessionType::Send(t, cont) => SessionType::Recv(t.clone(), Box::new(dual(cont))),
        SessionType::Recv(t, cont) => SessionType::Send(t.clone(), Box::new(dual(cont))),
        SessionType::End => SessionType::End,
        SessionType::Dual(inner) => {
            // dual(dual(S)) = S (involution). Nested duals collapse.
            match inner.as_ref().unlocated() {
                SessionType::Dual(inner2) => dual(inner2),
                other => other.clone(),
            }
        }
        SessionType::Name(n) => SessionType::Dual(Box::new(SessionType::Name(n.clone()))),
        SessionType::Located { .. } => unreachable!("unlocated session type"),
    }
}

/// Resolve named session references and expand `dual(...)` using `env`.
/// Returns `None` if a name is unknown **or** if a circular session
/// definition is encountered (caller emits a diagnostic).
///
/// X-2 (full-audit 2026-08-05 §3.10): the previous guard only caught direct
/// self-reference (`session A = A`); a cycle of length >= 2 such as
/// `session A = B; session B = A` recursed forever and overflowed the
/// compiler stack (user-source DoS on every check/build/run path). A
/// visited-set now detects cycles at any depth and fails closed with `None`.
pub fn resolve(s: &SessionType, env: &HashMap<String, SessionType>) -> Option<SessionType> {
    let mut visiting: std::collections::HashSet<String> = std::collections::HashSet::new();
    resolve_inner(s, env, &mut visiting)
}

fn resolve_inner(
    s: &SessionType,
    env: &HashMap<String, SessionType>,
    visiting: &mut std::collections::HashSet<String>,
) -> Option<SessionType> {
    match s.unlocated() {
        SessionType::Send(t, cont) => {
            let c = resolve_inner(cont, env, visiting)?;
            Some(SessionType::Send(t.clone(), Box::new(c)))
        }
        SessionType::Recv(t, cont) => {
            let c = resolve_inner(cont, env, visiting)?;
            Some(SessionType::Recv(t.clone(), Box::new(c)))
        }
        SessionType::End => Some(SessionType::End),
        SessionType::Name(n) => {
            // X-2: re-entering a name that is currently being expanded means
            // the definitions are circular — fail closed instead of recursing.
            if !visiting.insert(n.clone()) {
                return None;
            }
            let result = match env.get(n) {
                Some(body) => resolve_inner(body, env, visiting),
                None => None,
            };
            // Path-based stack: pop on exit so the same name reached through
            // an independent (non-cyclic) branch can still be expanded.
            visiting.remove(n);
            result
        }
        SessionType::Dual(inner) => {
            let r = resolve_inner(inner, env, visiting)?;
            Some(dual(&r))
        }
        SessionType::Located { .. } => unreachable!("unlocated session type"),
    }
}

/// Collect every session name referenced anywhere inside `st` (recursively).
fn referenced_names(st: &SessionType, out: &mut std::collections::HashSet<String>) {
    match st.unlocated() {
        SessionType::Send(_, cont) | SessionType::Recv(_, cont) => referenced_names(cont, out),
        SessionType::Dual(inner) => referenced_names(inner, out),
        SessionType::Name(n) => {
            out.insert(n.clone());
        }
        SessionType::End => {}
        SessionType::Located { .. } => unreachable!("unlocated session type"),
    }
}

/// Detect a circular session definition in `env` (X-2).
///
/// Returns one offending cycle as a name path whose first and last elements
/// are equal (e.g. `["A", "B", "A"]`), suitable for a user-facing diagnostic.
/// Returns `None` when the session definitions are acyclic.
///
/// Implemented as an iterative three-color DFS over the name-reference graph
/// so a hostile chain of thousands of declarations cannot overflow the stack
/// either.
pub fn detect_session_cycle(env: &HashMap<String, SessionType>) -> Option<Vec<String>> {
    // Build the reference graph: name -> referenced names that exist in `env`
    // (unknown names terminate resolution, so they cannot form a cycle).
    // Edges are owned strings: the scratch set below is reused per node, so
    // references into it must not leak into the graph.
    let mut refs: HashMap<&str, Vec<String>> = HashMap::new();
    let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, body) in env {
        names.clear();
        referenced_names(body, &mut names);
        let edges: Vec<String> = names
            .iter()
            .filter(|n| env.contains_key(n.as_str()))
            .cloned()
            .collect();
        refs.insert(name.as_str(), edges);
    }

    // 0 = white (unvisited), 1 = gray (on current path), 2 = black (done).
    let mut color: HashMap<&str, u8> = HashMap::new();
    // Outlives the DFS frames: stack/path entries may borrow from it.
    let no_edges: Vec<String> = Vec::new();
    for start in env.keys() {
        if color.get(start.as_str()).copied().unwrap_or(0) != 0 {
            continue;
        }
        color.insert(start.as_str(), 1);
        let mut stack: Vec<(&str, usize)> = vec![(start.as_str(), 0)];
        let mut path: Vec<&str> = vec![start.as_str()];
        loop {
            // Copy the frame out so `stack` can be mutated below (NLL).
            let Some(&(node, idx)) = stack.last() else {
                break;
            };
            let edges = refs.get(node).unwrap_or(&no_edges);
            if idx < edges.len() {
                let next: &str = edges[idx].as_str();
                stack.last_mut().expect("stack non-empty").1 += 1;
                match color.get(next).copied().unwrap_or(0) {
                    0 => {
                        color.insert(next, 1);
                        stack.push((next, 0));
                        path.push(next);
                    }
                    1 => {
                        // Gray node on the current path: extract the cycle.
                        let pos = path
                            .iter()
                            .position(|n| *n == next)
                            .expect("gray node must be on the current path");
                        let mut cycle: Vec<String> =
                            path[pos..].iter().map(|n| n.to_string()).collect();
                        cycle.push(next.to_string());
                        return Some(cycle);
                    }
                    _ => {} // black: subtree fully explored, no cycle there.
                }
            } else {
                color.insert(node, 2);
                stack.pop();
                path.pop();
            }
        }
    }
    None
}

/// Structural equality of session types after dual-normalization.
pub fn session_eq(a: &SessionType, b: &SessionType) -> bool {
    match (a.unlocated(), b.unlocated()) {
        (SessionType::End, SessionType::End) => true,
        (SessionType::Send(ta, ca), SessionType::Send(tb, cb)) => {
            types_compatible(ta, tb) && session_eq(ca, cb)
        }
        (SessionType::Recv(ta, ca), SessionType::Recv(tb, cb)) => {
            types_compatible(ta, tb) && session_eq(ca, cb)
        }
        (SessionType::Name(a), SessionType::Name(b)) => a == b,
        (SessionType::Dual(a), SessionType::Dual(b)) => session_eq(a, b),
        // dual(S) vs expanded dual — compare dual(a) to b
        (SessionType::Dual(a), b) => session_eq(&dual(a), b),
        (a, SessionType::Dual(b)) => session_eq(a, &dual(b)),
        _ => false,
    }
}

/// Action performed on a session endpoint.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionAction {
    /// `session_send(ch, value)` — consumes a `!T` prefix
    Send,
    /// `session_recv(ch)` — consumes a `?T` prefix
    Recv,
    /// `session_close(ch)` — requires residual `end`
    Close,
}

/// Error from applying an action to a residual session type.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionOrderError {
    /// Expected send (`!T`) but residual is something else
    ExpectedSend { residual: String },
    /// Expected recv (`?T`) but residual is something else
    ExpectedRecv { residual: String },
    /// Expected `end` for close, but residual is non-end
    ExpectedEnd { residual: String },
    /// Action on an already-closed / ended session
    AlreadyEnded,
}

/// Apply a session action to a residual protocol, returning the new residual.
///
/// This is the compile-time order checker core: each `session_send` /
/// `session_recv` / `session_close` call advances the endpoint's residual.
pub fn apply_action(
    residual: &SessionType,
    action: SessionAction,
) -> Result<(SessionType, Option<Type>), SessionOrderError> {
    // Normalize dual wrappers first.
    let residual = match residual.unlocated() {
        SessionType::Dual(inner) => dual(inner),
        other => other.clone(),
    };
    match (action, residual) {
        (SessionAction::Send, SessionType::Send(t, cont)) => Ok((*cont, Some(t))),
        (SessionAction::Send, SessionType::End) => Err(SessionOrderError::AlreadyEnded),
        (SessionAction::Send, other) => Err(SessionOrderError::ExpectedSend {
            residual: fmt_session(&other),
        }),
        (SessionAction::Recv, SessionType::Recv(t, cont)) => Ok((*cont, Some(t))),
        (SessionAction::Recv, SessionType::End) => Err(SessionOrderError::AlreadyEnded),
        (SessionAction::Recv, other) => Err(SessionOrderError::ExpectedRecv {
            residual: fmt_session(&other),
        }),
        (SessionAction::Close, SessionType::End) => Ok((SessionType::End, None)),
        (SessionAction::Close, other) => Err(SessionOrderError::ExpectedEnd {
            residual: fmt_session(&other),
        }),
    }
}

/// Human-readable session type formatting for diagnostics.
pub fn fmt_session(s: &SessionType) -> String {
    match s.unlocated() {
        SessionType::Send(t, cont) => {
            format!("!{} . {}", fmt_type(t), fmt_session(cont))
        }
        SessionType::Recv(t, cont) => {
            format!("?{} . {}", fmt_type(t), fmt_session(cont))
        }
        SessionType::Dual(inner) => format!("dual({})", fmt_session(inner)),
        SessionType::Name(n) => n.clone(),
        SessionType::End => "end".to_string(),
        SessionType::Located { .. } => unreachable!("unlocated session type"),
    }
}

/// Extract session type from a `SessionChan<S>` / `SessionChan` type name.
///
/// Conventions:
/// - `SessionChan` with type-arg list of length 1 → the arg is a session name
///   encoded as `Type::Name(session_name, [])`
/// - bare `SessionChan` → unknown / untracked
pub fn session_from_chan_type(ty: &Type) -> Option<String> {
    match ty.unlocated() {
        Type::Name(n, args) if n == "SessionChan" || n == "session_chan" => {
            if let Some(Type::Name(s, _)) = args.first().map(Type::unlocated) {
                Some(s.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{SessionType, Type};

    fn i32_ty() -> Type {
        Type::Name("i32".into(), vec![])
    }
    fn str_ty() -> Type {
        Type::Name("string".into(), vec![])
    }

    #[test]
    fn dual_swaps_send_recv() {
        // !i32 . ?string . end  →  ?i32 . !string . end
        let s = SessionType::Send(
            i32_ty(),
            Box::new(SessionType::Recv(str_ty(), Box::new(SessionType::End))),
        );
        let d = dual(&s);
        match d {
            SessionType::Recv(t, cont) => {
                assert!(matches!(t.unlocated(), Type::Name(n, _) if n == "i32"));
                match *cont {
                    SessionType::Send(t2, cont2) => {
                        assert!(matches!(t2.unlocated(), Type::Name(n, _) if n == "string"));
                        assert_eq!(*cont2, SessionType::End);
                    }
                    other => panic!("expected Send, got {:?}", other),
                }
            }
            other => panic!("expected Recv, got {:?}", other),
        }
    }

    #[test]
    fn dual_involution() {
        let s = SessionType::Send(
            i32_ty(),
            Box::new(SessionType::Recv(str_ty(), Box::new(SessionType::End))),
        );
        assert!(session_eq(&s, &dual(&dual(&s))));
    }

    #[test]
    fn apply_send_then_recv_then_close() {
        let s = SessionType::Send(
            i32_ty(),
            Box::new(SessionType::Recv(str_ty(), Box::new(SessionType::End))),
        );
        let (r1, t1) = apply_action(&s, SessionAction::Send).unwrap();
        assert!(matches!(t1.as_ref().map(Type::unlocated), Some(Type::Name(n, _)) if n == "i32"));
        let (r2, t2) = apply_action(&r1, SessionAction::Recv).unwrap();
        assert!(
            matches!(t2.as_ref().map(Type::unlocated), Some(Type::Name(n, _)) if n == "string")
        );
        let (r3, _) = apply_action(&r2, SessionAction::Close).unwrap();
        assert_eq!(r3, SessionType::End);
    }

    #[test]
    fn apply_recv_on_send_is_error() {
        let s = SessionType::Send(i32_ty(), Box::new(SessionType::End));
        let err = apply_action(&s, SessionAction::Recv).unwrap_err();
        assert!(matches!(err, SessionOrderError::ExpectedRecv { .. }));
    }

    #[test]
    fn apply_close_on_open_is_error() {
        let s = SessionType::Send(i32_ty(), Box::new(SessionType::End));
        let err = apply_action(&s, SessionAction::Close).unwrap_err();
        assert!(matches!(err, SessionOrderError::ExpectedEnd { .. }));
    }

    #[test]
    fn resolve_named_session() {
        let mut env = HashMap::new();
        env.insert(
            "S".to_string(),
            SessionType::Send(i32_ty(), Box::new(SessionType::End)),
        );
        let r = resolve(&SessionType::Name("S".into()), &env).unwrap();
        match r {
            SessionType::Send(_, cont) => assert_eq!(*cont, SessionType::End),
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn resolve_dual_of_named() {
        let mut env = HashMap::new();
        env.insert(
            "S".to_string(),
            SessionType::Send(i32_ty(), Box::new(SessionType::End)),
        );
        let r = resolve(
            &SessionType::Dual(Box::new(SessionType::Name("S".into()))),
            &env,
        )
        .unwrap();
        match r {
            SessionType::Recv(_, cont) => assert_eq!(*cont, SessionType::End),
            other => panic!("expected Recv, got {:?}", other),
        }
    }
}
