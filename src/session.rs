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
    match ty {
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
    match s {
        SessionType::Send(t, cont) => SessionType::Recv(t.clone(), Box::new(dual(cont))),
        SessionType::Recv(t, cont) => SessionType::Send(t.clone(), Box::new(dual(cont))),
        SessionType::End => SessionType::End,
        SessionType::Dual(inner) => {
            // dual(dual(S)) = S (involution). Nested duals collapse.
            match inner.as_ref() {
                SessionType::Dual(inner2) => dual(inner2),
                other => other.clone(),
            }
        }
        SessionType::Name(n) => SessionType::Dual(Box::new(SessionType::Name(n.clone()))),
    }
}

/// Resolve named session references and expand `dual(...)` using `env`.
/// Returns `None` if a name is unknown (caller emits a diagnostic).
pub fn resolve(s: &SessionType, env: &HashMap<String, SessionType>) -> Option<SessionType> {
    match s {
        SessionType::Send(t, cont) => {
            let c = resolve(cont, env)?;
            Some(SessionType::Send(t.clone(), Box::new(c)))
        }
        SessionType::Recv(t, cont) => {
            let c = resolve(cont, env)?;
            Some(SessionType::Recv(t.clone(), Box::new(c)))
        }
        SessionType::End => Some(SessionType::End),
        SessionType::Name(n) => {
            let body = env.get(n)?;
            // Avoid infinite recursion on self-referential sessions by only
            // expanding one level of Name; dual/send/recv continue resolve.
            match body {
                SessionType::Name(n2) if n2 == n => Some(SessionType::Name(n.clone())),
                other => resolve(other, env),
            }
        }
        SessionType::Dual(inner) => {
            let r = resolve(inner, env)?;
            Some(dual(&r))
        }
    }
}

/// Structural equality of session types after dual-normalization.
pub fn session_eq(a: &SessionType, b: &SessionType) -> bool {
    match (a, b) {
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
    let residual = match residual {
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
    match s {
        SessionType::Send(t, cont) => {
            format!("!{} . {}", fmt_type(t), fmt_session(cont))
        }
        SessionType::Recv(t, cont) => {
            format!("?{} . {}", fmt_type(t), fmt_session(cont))
        }
        SessionType::Dual(inner) => format!("dual({})", fmt_session(inner)),
        SessionType::Name(n) => n.clone(),
        SessionType::End => "end".to_string(),
    }
}

/// Extract session type from a `SessionChan<S>` / `SessionChan` type name.
///
/// Conventions:
/// - `SessionChan` with type-arg list of length 1 → the arg is a session name
///   encoded as `Type::Name(session_name, [])`
/// - bare `SessionChan` → unknown / untracked
pub fn session_from_chan_type(ty: &Type) -> Option<String> {
    match ty {
        Type::Name(n, args) if n == "SessionChan" || n == "session_chan" => {
            if let Some(Type::Name(s, _)) = args.first() {
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
                assert!(matches!(t, Type::Name(n, _) if n == "i32"));
                match *cont {
                    SessionType::Send(t2, cont2) => {
                        assert!(matches!(t2, Type::Name(n, _) if n == "string"));
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
        assert!(matches!(t1, Some(Type::Name(n, _)) if n == "i32"));
        let (r2, t2) = apply_action(&r1, SessionAction::Recv).unwrap();
        assert!(matches!(t2, Some(Type::Name(n, _)) if n == "string"));
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
